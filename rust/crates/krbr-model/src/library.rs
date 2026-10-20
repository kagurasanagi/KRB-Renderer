//! 3D モデルの読み込み：メモ → ディスクのキャッシュ（ファイル内容の XXH64 ＋分割パラメータがキー）→ 変換器 DLL。
//! キャッシュの名前は C++ 版と同じ規則なので、同じキャッシュを共有できる。

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::{c_char, c_double, c_int, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Instant;

use krbr_core::{to_hex, xxh64};

use crate::mesh_file::{read_mesh_file, MESH_FILE_VERSION};
use crate::ModelMesh;

#[derive(Debug, Clone, Copy)]
pub struct MeshParams {
    /// mm
    pub linear_deflection: f64,
    /// rad
    pub angular_deflection: f64,
}

impl Default for MeshParams {
    fn default() -> Self {
        Self { linear_deflection: 0.01, angular_deflection: 0.35 }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoadResult {
    pub mesh: Option<Arc<ModelMesh>>,
    pub from_cache: bool,
    /// 裏で分割中（`request`）
    pub pending: bool,
    /// ファイル内容の XXH64（16 進）。材質の対応表の照合にも使う
    pub file_hash: String,
    pub error: String,
    pub seconds: f64,
}

// ---------------------------------------------------------------- STEP 変換器 DLL

const STEP_API_VERSION: c_int = 1;

type ToMeshFileFn = unsafe extern "C" fn(*const c_char, c_double, c_double, *const c_char, *mut c_char, usize) -> c_int;
type VersionFn = unsafe extern "C" fn() -> c_int;

struct StepDll {
    _lib: libloading::Library,
    to_mesh_file: ToMeshFileFn,
}

// 安全性：関数ポインタはライブラリと同じ寿命で、ライブラリは一度読んだら解放しない
unsafe impl Send for StepDll {}
unsafe impl Sync for StepDll {}

fn step_dll() -> Result<&'static StepDll, &'static str> {
    static DLL: OnceLock<Result<StepDll, String>> = OnceLock::new();
    DLL.get_or_init(|| {
        // 配布用の一式の dll\step、exe と同じフォルダの step\、開発中は scripts\build.cmd（CMake）の出力（<repo>/build/release/step）
        let mut path = krbr_core::platform::dll_dir("step").join("krbr_step.dll");
        if !path.exists() {
            path = krbr_core::platform::exe_dir().join("step").join("krbr_step.dll");
        }
        if !path.exists() {
            if let Some(dev) = krbr_core::platform::dev_build_dir("step", "krbr_step.dll") {
                path = dev.join("krbr_step.dll");
            }
        }
        if !path.exists() {
            return Err(format!("not found: {}", path.display()));
        }
        // 安全性：DLL は同梱の変換器で、読み込み時に任意の初期化を行わない
        let lib = unsafe {
            // DLL と同じフォルダから OpenCASCADE の DLL を探す
            #[cfg(windows)]
            {
                use libloading::os::windows::{Library, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR};
                Library::load_with_flags(&path, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS)
                    .map(libloading::Library::from)
            }
            #[cfg(not(windows))]
            {
                libloading::Library::new(&path)
            }
        }
        .map_err(|e| format!("cannot load {}: {e}", path.display()))?;
        let (api, file, func) = unsafe {
            let api: libloading::Symbol<VersionFn> = lib.get(b"krbr_step_api_version\0").map_err(|e| e.to_string())?;
            let file: libloading::Symbol<VersionFn> = lib.get(b"krbr_step_mesh_file_version\0").map_err(|e| e.to_string())?;
            let func: libloading::Symbol<ToMeshFileFn> = lib.get(b"krbr_step_to_mesh_file\0").map_err(|e| e.to_string())?;
            (api(), file(), *func)
        };
        if api != STEP_API_VERSION || file != MESH_FILE_VERSION as c_int {
            return Err(format!(
                "krbr_step.dll version mismatch (api {api} / mesh file v{file}; expected {STEP_API_VERSION} / v{MESH_FILE_VERSION})"
            ));
        }
        krbr_core::debug!("STEP converter: {}", path.display());
        Ok(StepDll { _lib: lib, to_mesh_file: func })
    })
    .as_ref()
    .map_err(|e| e.as_str())
}

/// STEP 変換器 DLL が使えるか（使えなければ理由）
pub fn step_support() -> Result<(), &'static str> {
    step_dll().map(|_| ())
}

fn tessellate_step(p: &Path, params: MeshParams) -> Result<ModelMesh, String> {
    let dll = step_dll().map_err(|e| format!("STEP converter unavailable: {e}"))?;
    // 一時ファイルは %TEMP% の直下に置き、読んだら消す（フォルダを作ると、消すときに並列の変換とぶつかり、残りもする）
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("krbr_step_{}_{n}.krbm", std::process::id()));
    let step_c = CString::new(p.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    let out_c = CString::new(tmp.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
    let mut err = vec![0u8; 512];
    // 安全性：引数は NUL 終端の UTF-8 文字列と、長さを添えたバッファ
    let rc = unsafe {
        (dll.to_mesh_file)(step_c.as_ptr(), params.linear_deflection, params.angular_deflection, out_c.as_ptr(), err.as_mut_ptr() as *mut c_char, err.len())
    };
    let result = if rc != 0 {
        let msg = err.iter().position(|&b| b == 0).map(|i| String::from_utf8_lossy(&err[..i]).into_owned()).unwrap_or_default();
        Err(if msg.is_empty() { format!("converter returned {rc}") } else { msg })
    } else {
        read_mesh_file(&tmp).map_err(|e| format!("cannot read converter output: {e}"))
    };
    let _ = std::fs::remove_file(&tmp);
    result
}

// ---------------------------------------------------------------- ModelLibrary

#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub loaded: usize,
    pub cache_hits: usize,
    pub failed: usize,
}

struct Job {
    path: PathBuf,
    cache_file: Option<PathBuf>,
    file_hash: String,
}

#[derive(Default)]
struct State {
    memo: HashMap<PathBuf, LoadResult>,
    queued: BTreeMap<PathBuf, ()>,
    queue: VecDeque<Job>,
    stop: bool,
    completed: usize,
    stats: Stats,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
    params: MeshParams,
}

pub struct ModelLibrary {
    cache_dir: PathBuf,
    use_cache: bool,
    shared: Arc<Shared>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

fn is_step(p: &Path) -> bool {
    matches!(p.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("step") | Some("stp"))
}

impl ModelLibrary {
    pub fn new(cache_dir: PathBuf, params: MeshParams, use_cache: bool) -> Self {
        Self {
            cache_dir,
            use_cache,
            shared: Arc::new(Shared { state: Mutex::new(State::default()), cv: Condvar::new(), params }),
            worker: Mutex::new(None),
        }
    }

    /// 読み込んで返す（同じ実行内では同じパスを 1 回だけ読む）
    pub fn get(&self, p: &Path) -> LoadResult {
        if !is_step(p) {
            let res = LoadResult { error: format!("unsupported model format {}", p.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default()), ..Default::default() };
            let mut st = self.shared.state.lock().unwrap();
            st.stats.failed += 1;
            st.memo.insert(p.to_path_buf(), res.clone());
            return res;
        }
        let (hit, cache_file, file_hash) = self.lookup(p);
        if let Some(r) = hit {
            return r;
        }
        load_now(&self.shared, p, cache_file.as_deref(), &file_hash)
    }

    /// キャッシュにあればすぐ返す。無ければ裏で分割を始めて pending を返す（保存の監視中に使う）
    pub fn request(&self, p: &Path) -> LoadResult {
        if !is_step(p) {
            return self.get(p);
        }
        let (hit, cache_file, file_hash) = self.lookup(p);
        if let Some(r) = hit {
            return r;
        }
        // 分割は裏の 1 本のスレッドで順に行う（OCCT の読み込みを並べない）
        let mut st = self.shared.state.lock().unwrap();
        if !st.queued.contains_key(p) {
            st.queued.insert(p.to_path_buf(), ());
            st.queue.push_back(Job { path: p.to_path_buf(), cache_file, file_hash: file_hash.clone() });
            let mut w = self.worker.lock().unwrap();
            if w.is_none() {
                let shared = Arc::clone(&self.shared);
                *w = Some(std::thread::spawn(move || worker_loop(shared)));
            }
            self.shared.cv.notify_one();
        }
        LoadResult { pending: true, file_hash, ..Default::default() }
    }

    pub fn stats(&self) -> Stats {
        self.shared.state.lock().unwrap().stats
    }

    /// 裏で分割し終えた数（増えたらシーンを作り直す）
    pub fn completed(&self) -> usize {
        self.shared.state.lock().unwrap().completed
    }

    pub fn pending(&self) -> usize {
        self.shared.state.lock().unwrap().queued.len()
    }

    /// メモかディスクのキャッシュにあれば返す。無ければ (None, キャッシュの場所, ハッシュ)
    fn lookup(&self, p: &Path) -> (Option<LoadResult>, Option<PathBuf>, String) {
        if let Some(r) = self.shared.state.lock().unwrap().memo.get(p) {
            return (Some(r.clone()), None, String::new());
        }
        let file_hash = std::fs::read(p).map(|b| to_hex(xxh64(&b))).unwrap_or_default();
        if !self.use_cache || file_hash.is_empty() {
            return (None, None, file_hash);
        }
        // キーはファイル内容のハッシュ＋分割パラメータ＋読み込み器（C++ 版と同じ）
        let params = self.shared.params;
        let param_key = format!("occt-step|{:.6}|{:.6}|v{}", params.linear_deflection, params.angular_deflection, MESH_FILE_VERSION);
        let ph = to_hex(xxh64(param_key.as_bytes()));
        let cache_file = self.cache_dir.join("meshes").join(format!("{file_hash}-{}.krbm", &ph[..8]));
        match read_mesh_file(&cache_file) {
            Ok(m) => {
                krbr_core::info!(
                    "model cache hit: {} -> {}",
                    p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
                    cache_file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
                );
                let res = LoadResult { mesh: Some(Arc::new(m)), from_cache: true, file_hash, ..Default::default() };
                let mut st = self.shared.state.lock().unwrap();
                st.stats.cache_hits += 1;
                st.memo.insert(p.to_path_buf(), res.clone());
                (Some(res), None, String::new())
            }
            Err(_) => (None, Some(cache_file), file_hash),
        }
    }
}

impl Drop for ModelLibrary {
    fn drop(&mut self) {
        self.shared.state.lock().unwrap().stop = true;
        self.shared.cv.notify_all();
        if let Some(w) = self.worker.lock().unwrap().take() {
            let _ = w.join();
        }
    }
}

fn load_now(shared: &Shared, p: &Path, cache_file: Option<&Path>, file_hash: &str) -> LoadResult {
    let t0 = Instant::now();
    let name = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let mut res = LoadResult { file_hash: file_hash.to_string(), ..Default::default() };
    match tessellate_step(p, shared.params) {
        Ok(m) => {
            res.seconds = t0.elapsed().as_secs_f64();
            krbr_core::info!("model loaded: {} ({} tris, {:.2} s){}", name, m.mesh.triangle_count(), res.seconds, if cache_file.is_some() { ", cached" } else { "" });
            if let Some(cf) = cache_file {
                if let Err(e) = crate::mesh_file::write_mesh_file(cf, &m) {
                    krbr_core::warn!("cannot write mesh cache {}: {}", cf.display(), e);
                }
            }
            res.mesh = Some(Arc::new(m));
        }
        Err(e) => {
            res.seconds = t0.elapsed().as_secs_f64();
            krbr_core::warn!("model load failed: {}: {}", name, e);
            res.error = e;
        }
    }
    let mut st = shared.state.lock().unwrap();
    if res.mesh.is_some() {
        st.stats.loaded += 1;
    } else {
        st.stats.failed += 1;
    }
    st.memo.insert(p.to_path_buf(), res.clone());
    res
}

fn worker_loop(shared: Arc<Shared>) {
    krbr_core::platform::lower_thread_priority(); // 裏での分割は低優先度
    loop {
        let job = {
            let mut st = shared.state.lock().unwrap();
            while !st.stop && st.queue.is_empty() {
                st = shared.cv.wait(st).unwrap();
            }
            if st.stop {
                return;
            }
            st.queue.pop_front().unwrap()
        };
        load_now(&shared, &job.path, job.cache_file.as_deref(), &job.file_hash);
        let mut st = shared.state.lock().unwrap();
        st.queued.remove(&job.path);
        st.completed += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_file_name_matches_cpp_rule() {
        // C++: xxh64("occt-step|0.010000|0.350000|v3") の先頭 8 桁がパラメータのキー
        let key = format!("occt-step|{:.6}|{:.6}|v{}", 0.01f64, 0.35f64, MESH_FILE_VERSION);
        assert_eq!(key, "occt-step|0.010000|0.350000|v3");
        let ph = to_hex(xxh64(key.as_bytes()));
        assert_eq!(ph.len(), 16);
    }
}
