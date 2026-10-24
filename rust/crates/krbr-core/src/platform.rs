//! 設定・キャッシュの場所（Windows は `%APPDATA%\krbr`、`%LOCALAPPDATA%\krbr\cache`、ほかは XDG の場所。環境変数で差し替え可）

use std::path::{Path, PathBuf};

fn home_based(xdg: &str, fallback: &str) -> PathBuf {
    if let Ok(v) = std::env::var(xdg) {
        return PathBuf::from(v);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(fallback)
}

/// 全体用キャッシュ（`KRBR_CACHE_DIR` で差し替え可）
pub fn cache_dir() -> PathBuf {
    if let Ok(v) = std::env::var("KRBR_CACHE_DIR") {
        return PathBuf::from(v);
    }
    if cfg!(windows) {
        if let Ok(v) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(v).join("krbr").join("cache");
        }
    }
    home_based("XDG_CACHE_HOME", ".cache").join("krbr")
}

/// 全体設定（`KRBR_CONFIG_DIR` で差し替え可）
pub fn config_dir() -> PathBuf {
    if let Ok(v) = std::env::var("KRBR_CONFIG_DIR") {
        return PathBuf::from(v);
    }
    if cfg!(windows) {
        if let Ok(v) = std::env::var("APPDATA") {
            return PathBuf::from(v).join("krbr");
        }
    }
    home_based("XDG_CONFIG_HOME", ".config").join("krbr")
}

/// 実行ファイルのあるフォルダ（シェーダなどの同梱物を探す）
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 開発中の同梱物の場所：実行ファイルのフォルダから上へたどり、STEP 変換器 DLL の CMake のビルド出力 `<リポジトリ>\build\release\<rel>` に
/// probe があればそのフォルダ（rust\target\release\krbr.exe からも、deps\ に置かれるテストの実行ファイルからも見つかる）
pub fn dev_build_dir(rel: &str, probe: &str) -> Option<PathBuf> {
    exe_dir().ancestors().take(6).map(|a| a.join("build").join("release").join(rel)).find(|d| d.join(probe).exists())
}

/// 配布用の一式の DLL のフォルダ（exe と同じ所の dll\<種類>。step / oidn / vcruntime）
pub fn dll_dir(kind: &str) -> PathBuf {
    exe_dir().join("dll").join(kind)
}

/// dll\vcruntime（Visual C++ ランタイム）を DLL の探し先に足す。起動の直後に一度だけ呼ぶ。
/// STEP 変換器・OIDN は LOAD_LIBRARY_SEARCH_DEFAULT_DIRS で読むので、それらとその依存（OpenCASCADE など）が
/// ここからランタイムを見つける。krbr.exe 自身はランタイムを静的に組み込んでいる（rust/.cargo/config.toml）
pub fn add_dll_search_dirs() {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn AddDllDirectory(dir: *const u16) -> *mut std::ffi::c_void;
        }
        let dir = dll_dir("vcruntime");
        if dir.is_dir() {
            use std::os::windows::ffi::OsStrExt;
            let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
            // 安全性：wide は 0 終端の絶対パス
            unsafe { AddDllDirectory(wide.as_ptr()) };
        }
    }
}

#[cfg(windows)]
mod priority {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        pub fn GetCurrentThread() -> *mut c_void;
        pub fn SetThreadPriority(thread: *mut c_void, priority: i32) -> i32;
        pub fn GetThreadPriority(thread: *mut c_void) -> i32;
        pub fn GetCurrentProcess() -> *mut c_void;
        pub fn SetPriorityClass(process: *mut c_void, class: u32) -> i32;
        pub fn GetPriorityClass(process: *mut c_void) -> u32;
    }
    pub const THREAD_PRIORITY_BELOW_NORMAL: i32 = -1;
    pub const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x4000;
}

/// 今のスレッドの CPU の優先度を下げる（BELOW_NORMAL）。裏の重い処理（読み直し・タイルの作り直し・モデルの分割など）の先頭で呼ぶ。
/// ビューアのウィンドウのスレッドは通常のままなので、ほかのアプリが CPU を使っていても操作への反応は遅れない
pub fn lower_thread_priority() {
    #[cfg(windows)]
    // 安全性：GetCurrentThread は今のスレッドの疑似ハンドル（閉じなくてよい）
    unsafe {
        priority::SetThreadPriority(priority::GetCurrentThread(), priority::THREAD_PRIORITY_BELOW_NORMAL);
    }
}

/// 裏の重い処理（タイルの作り直し・被覆率のラスタライズなど）で並列に動かすスレッドの数：論理コアの半分（最低 1）。
/// 半分にするのは、ビューアのウィンドウのスレッドや KiCad など、ほかのアプリの分を空けておくため。
/// 基板の読み込み（起動・開く）の間（AllCores を持っている間）は、利用者がそれを待っているので、論理コアをすべて使う。
/// 並列の処理を入れ子にしない（外側で並列にしたら、内側は 1 本で動かす）こと
pub fn worker_threads() -> usize {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
    if ALL_CORES.load(std::sync::atomic::Ordering::Relaxed) > 0 {
        n
    } else {
        (n / 2).max(1)
    }
}

static ALL_CORES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// これを持っている間、worker_threads は論理コアをすべて使う（基板の読み込みの間。作業用のスレッドは低優先度のまま）
pub struct AllCores(());

impl AllCores {
    pub fn new() -> Self {
        ALL_CORES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        AllCores(())
    }
}

impl Default for AllCores {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AllCores {
    fn drop(&mut self) {
        ALL_CORES.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// 今のスレッドが lower_thread_priority で下げた優先度か（テスト用）
pub fn thread_priority_is_low() -> bool {
    #[cfg(windows)]
    // 安全性：同上
    unsafe {
        priority::GetThreadPriority(priority::GetCurrentThread()) <= priority::THREAD_PRIORITY_BELOW_NORMAL
    }
    #[cfg(not(windows))]
    false
}

/// プロセス全体の CPU の優先度を下げる（BELOW_NORMAL_PRIORITY_CLASS）。ウィンドウを持たないコマンド（render / watch など）で使う
pub fn lower_process_priority() {
    #[cfg(windows)]
    // 安全性：GetCurrentProcess は今のプロセスの疑似ハンドル
    unsafe {
        priority::SetPriorityClass(priority::GetCurrentProcess(), priority::BELOW_NORMAL_PRIORITY_CLASS);
    }
}

/// プロセスが BELOW_NORMAL か（テスト・ログ用）
pub fn process_priority_is_low() -> bool {
    #[cfg(windows)]
    // 安全性：同上
    unsafe {
        priority::GetPriorityClass(priority::GetCurrentProcess()) == priority::BELOW_NORMAL_PRIORITY_CLASS
    }
    #[cfg(not(windows))]
    false
}

/// ファイルをエクスプローラーで選んだ状態で表示する（無ければ、そのフォルダを開く）
pub fn reveal_in_file_manager(path: &Path) {
    let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let r = if cfg!(windows) {
        if path.is_file() {
            std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn()
        } else {
            std::process::Command::new("explorer").arg(path.parent().unwrap_or(&path)).spawn()
        }
    } else {
        let dir = if path.is_dir() { path.as_path() } else { path.parent().unwrap_or(&path) };
        std::process::Command::new(if cfg!(target_os = "macos") { "open" } else { "xdg-open" }).arg(dir).spawn()
    };
    if let Err(e) = r {
        crate::warn!("cannot open {}: {}", path.display(), e);
    }
}

/// ユーザーの「ドキュメント」フォルダ。Windows は既知のフォルダ（OneDrive などへ移したらその場所。KiCad と同じ決め方）、
/// 取れなければ %USERPROFILE%\Documents。ほかは ~/Documents
pub fn documents_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    if let Some(d) = known_folder::documents() {
        return Some(d);
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })?;
    Some(PathBuf::from(home).join("Documents"))
}

#[cfg(windows)]
mod known_folder {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    #[repr(C)]
    struct Guid(u32, u16, u16, [u8; 8]);
    /// FOLDERID_Documents
    const DOCUMENTS: Guid = Guid(0xFDD39AD0, 0x238F, 0x46AF, [0xAD, 0xB4, 0x6C, 0x85, 0x48, 0x03, 0x69, 0xC7]);

    #[link(name = "shell32")]
    extern "system" {
        fn SHGetKnownFolderPath(id: *const Guid, flags: u32, token: *mut c_void, path: *mut *mut u16) -> i32;
    }
    #[link(name = "ole32")]
    extern "system" {
        fn CoTaskMemFree(p: *mut c_void);
    }

    pub fn documents() -> Option<PathBuf> {
        let mut p: *mut u16 = std::ptr::null_mut();
        // 安全性：p は成功・失敗どちらでも CoTaskMemFree で返す（null でもよい）。成功なら 0 終端の文字列
        unsafe {
            let hr = SHGetKnownFolderPath(&DOCUMENTS, 0, std::ptr::null_mut(), &mut p);
            let out = (hr >= 0 && !p.is_null()).then(|| {
                let len = (0..).take_while(|&i| *p.add(i) != 0).count();
                PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(p, len)))
            });
            CoTaskMemFree(p.cast());
            out
        }
    }
}

/// Windows のレジストリ（読むだけ）。HKEY_LOCAL_MACHINE・HKEY_CURRENT_USER の下のキー
#[cfg(windows)]
pub mod registry {
    use std::ffi::c_void;

    type Hkey = *mut c_void;
    pub const HKEY_CURRENT_USER: isize = 0x8000_0001u32 as i32 as isize;
    pub const HKEY_LOCAL_MACHINE: isize = 0x8000_0002u32 as i32 as isize;
    const KEY_READ: u32 = 0x20019;
    const RRF_RT_REG_SZ: u32 = 0x2;
    const ERROR_MORE_DATA: i32 = 234;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegOpenKeyExW(key: Hkey, sub: *const u16, options: u32, sam: u32, out: *mut Hkey) -> i32;
        fn RegEnumKeyExW(key: Hkey, index: u32, name: *mut u16, name_len: *mut u32, reserved: *mut u32, class: *mut u16, class_len: *mut u32, time: *mut c_void) -> i32;
        fn RegGetValueW(key: Hkey, sub: *const u16, value: *const u16, flags: u32, ty: *mut u32, data: *mut c_void, len: *mut u32) -> i32;
        fn RegCloseKey(key: Hkey) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// 開いたキー（落とすと閉じる）
    pub struct Key(Hkey);

    impl Key {
        /// root（HKEY_*）の下の path を読むために開く。無ければ None
        pub fn open(root: isize, path: &str) -> Option<Key> {
            let mut h: Hkey = std::ptr::null_mut();
            // 安全性：path は 0 終端、h は成功したときだけ使う
            let r = unsafe { RegOpenKeyExW(root as Hkey, wide(path).as_ptr(), 0, KEY_READ, &mut h) };
            (r == 0).then_some(Key(h))
        }

        /// 直下のキーの名前
        pub fn subkeys(&self) -> Vec<String> {
            let mut out = Vec::new();
            for i in 0.. {
                let mut buf = [0u16; 256]; // キーの名前は 255 文字まで
                let mut len = buf.len() as u32;
                // 安全性：buf の長さを len で渡す
                let r = unsafe { RegEnumKeyExW(self.0, i, buf.as_mut_ptr(), &mut len, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
                match r {
                    0 => out.push(String::from_utf16_lossy(&buf[..len as usize])),
                    ERROR_MORE_DATA => continue,
                    _ => break, // ERROR_NO_MORE_ITEMS など
                }
            }
            out
        }

        /// 直下のキー sub の文字列の値 name（無い・文字列でなければ None）
        pub fn string(&self, sub: &str, name: &str) -> Option<String> {
            let (sub, name) = (wide(sub), wide(name));
            let mut bytes: u32 = 0;
            // 安全性：1 回目は大きさだけを聞き、2 回目はその大きさの buf に書かせる
            unsafe {
                if RegGetValueW(self.0, sub.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut bytes) != 0 {
                    return None;
                }
                let mut buf = vec![0u16; (bytes as usize).div_ceil(2)];
                if RegGetValueW(self.0, sub.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut bytes) != 0 {
                    return None;
                }
                let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                Some(String::from_utf16_lossy(&buf[..len]))
            }
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            // 安全性：open で開いたキー
            unsafe { RegCloseKey(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_folder_is_found() {
        assert!(documents_dir().is_some());
    }

    #[test]
    fn worker_thread_priority_is_lowered_without_touching_the_caller() {
        let caller_low = thread_priority_is_low();
        let worker_low = std::thread::spawn(|| {
            lower_thread_priority();
            thread_priority_is_low()
        })
        .join()
        .unwrap();
        assert!(worker_low || !cfg!(windows));
        assert_eq!(thread_priority_is_low(), caller_low); // 呼んだ側のスレッドはそのまま
    }
}
