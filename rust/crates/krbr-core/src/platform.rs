//! 設定・キャッシュの場所（C++ 版と同じ：`%APPDATA%\krbr`、`%LOCALAPPDATA%\krbr\cache`。環境変数で差し替え可）

use std::path::PathBuf;

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
