//! KiCad の線フォントの文字（シルク）を kicad-cli で SVG に書き出して取り込む（C++ 版 app/kicad_cli）。
//! kicad-cli に渡すのは、シルク層に線フォントの文字だけを残した基板のコピー（krbr_kicad::stroke_silk_only_board）。
//! TrueType の文字・線・図形は krbr が自分で描くので、そちらには影響しない

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use krbr_core::{debug, warn};
use krbr_geom::shapes::unite;
use krbr_geom::svg_reader::read_kicad_svg;
use krbr_geom::{FillRule, Paths};
use krbr_kicad::stroke_silk_only_board;

/// kicad-cli 1 回の上限（固まったときに止める）
const TIMEOUT: Duration = Duration::from_secs(120);

/// 表・裏のシルク（基板座標の多角形）
#[derive(Debug, Clone, Default)]
pub struct SilkPlot {
    pub side: [Option<Arc<Paths>>; 2],
    pub from_cache: bool,
}

/// kicad-cli を探す：設定のパス、Program Files\KiCad\*\bin（新しい版を優先）、PATH の順
pub fn find_kicad_cli(configured: &str) -> Option<PathBuf> {
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        if p.is_file() {
            return Some(p);
        }
        warn!("kicad-cli not found at {}", configured);
    }
    let exe = if cfg!(windows) { "kicad-cli.exe" } else { "kicad-cli" };
    if cfg!(windows) {
        let mut found: Vec<PathBuf> = Vec::new();
        for env in ["ProgramFiles", "ProgramW6432"] {
            let Ok(pf) = std::env::var(env) else { continue };
            let Ok(rd) = std::fs::read_dir(Path::new(&pf).join("KiCad")) else { continue };
            found.extend(rd.flatten().map(|e| e.path().join("bin").join(exe)).filter(|c| c.is_file()));
        }
        found.sort();
        if let Some(p) = found.pop() {
            return Some(p);
        }
    } else {
        for d in ["/usr/bin", "/usr/local/bin"] {
            let c = Path::new(d).join(exe);
            if c.is_file() {
                return Some(c);
            }
        }
    }
    std::env::var_os("PATH").and_then(|path| std::env::split_paths(&path).map(|d| d.join(exe)).find(|c| c.is_file()))
}

/// 窓を出さず、優先度を下げて実行する。出力は log に書く。timeout を過ぎたら止める
fn run_hidden(exe: &Path, args: &[&str], work: &Path, log: &Path) -> Result<i32, String> {
    let log_file = std::fs::File::create(log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err_file = log_file.try_clone().map_err(|e| format!("{e}"))?;
    let mut cmd = Command::new(exe);
    cmd.args(args).current_dir(work).stdin(Stdio::null()).stdout(log_file).stderr(err_file);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        cmd.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
    }
    let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", exe.display()))?;
    let t0 = Instant::now();
    loop {
        if let Some(st) = child.try_wait().map_err(|e| format!("{e}"))? {
            return Ok(st.code().unwrap_or(-1));
        }
        if t0.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("timed out after {} s", TIMEOUT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// 作業フォルダ（プロセスと呼び出しごとに別：同時に動いてもぶつからない）
fn work_dir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join(format!("krbr_kicad_{}_{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)))
}

/// 基板の線フォントの文字（シルク）を kicad-cli で書き出して多角形にする。結果は cache_dir/silk にキャッシュする
pub fn plot_stroke_silk(cli: &Path, board_file: &Path, project_name: &str, cache_dir: &Path, use_cache: bool) -> SilkPlot {
    let mut out = SilkPlot::default();
    let board_text = match std::fs::read_to_string(board_file) {
        Ok(t) => t,
        Err(e) => {
            warn!("kicad-cli silk: {}: {e}", board_file.display());
            return out;
        }
    };
    let filtered = match stroke_silk_only_board(&board_text) {
        Ok(t) => t,
        Err(e) => {
            warn!("kicad-cli silk: {}: {e}", board_file.display());
            return out;
        }
    };
    let dir = board_file.parent().unwrap_or(Path::new("."));
    let pro = dir.join(format!("{project_name}.kicad_pro"));
    let pro_text = std::fs::read_to_string(&pro).ok();

    // キャッシュのキー：渡す基板のコピー、プロジェクト（文字の変数）、kicad-cli の場所。
    // 中身が C++ 版（シルク層全体）と違うので、名前を変えて C++ 版のキャッシュとは分ける
    let key_src = format!("krbr-stroke-silk-v1\n{filtered}{}{}", pro_text.as_deref().unwrap_or(""), cli.to_string_lossy());
    let key = krbr_core::to_hex(krbr_core::xxh64(key_src.as_bytes()));
    let cdir = cache_dir.join("silk");
    let cached = [cdir.join(format!("{key}-stroke-F.svg")), cdir.join(format!("{key}-stroke-B.svg"))];
    let mut svg: [Option<String>; 2] = [None, None];
    if use_cache && cached.iter().all(|c| c.is_file()) {
        svg = [std::fs::read_to_string(&cached[0]).ok(), std::fs::read_to_string(&cached[1]).ok()];
        out.from_cache = svg.iter().all(Option::is_some);
    }
    if !out.from_cache {
        let work = work_dir();
        if let Err(e) = std::fs::create_dir_all(&work) {
            warn!("kicad-cli silk: {}: {e}", work.display());
            return out;
        }
        // kicad-cli は基板の横に .kicad_prl を書くので、コピーに対して実行する
        let pcb_copy = work.join(board_file.file_name().unwrap_or_default());
        let ok = std::fs::write(&pcb_copy, &filtered).is_ok();
        if let Some(t) = &pro_text {
            let _ = std::fs::write(work.join(pro.file_name().unwrap_or_default()), t);
        }
        let log = work.join("kicad-cli.log");
        for (s, layer) in ["F.SilkS", "B.SilkS"].iter().enumerate().filter(|_| ok) {
            let svg_path = work.join(format!("{}_silk.svg", if s == 0 { "F" } else { "B" }));
            let args = ["pcb", "export", "svg", "--layers", layer, "--mode-single", "--exclude-drawing-sheet", "--page-size-mode", "0", "-o", &svg_path.to_string_lossy(), &pcb_copy.to_string_lossy()];
            let args: Vec<&str> = args.iter().map(|a| a.as_ref()).collect();
            match run_hidden(cli, &args, &work, &log) {
                Ok(0) => {}
                Ok(rc) => {
                    let msg = std::fs::read_to_string(&log).unwrap_or_default();
                    warn!("kicad-cli failed for {} (exit {}): {}", layer, rc, msg.trim());
                    continue;
                }
                Err(e) => {
                    warn!("kicad-cli failed for {}: {}", layer, e);
                    continue;
                }
            }
            svg[s] = std::fs::read_to_string(&svg_path).ok();
            if let (Some(text), true) = (&svg[s], use_cache) {
                let _ = std::fs::create_dir_all(&cdir);
                let _ = std::fs::write(&cached[s], text);
            }
        }
        let _ = std::fs::remove_dir_all(&work);
    }
    for (s, text) in svg.iter().enumerate() {
        let Some(text) = text else { continue };
        let r = read_kicad_svg(text);
        let paths = unite(&r.paths, FillRule::NonZero);
        debug!("kicad-cli silk {}: {} elements -> {} paths", if s == 0 { "front" } else { "back" }, r.elements, paths.len());
        out.side[s] = Some(Arc::new(paths));
    }
    out
}
