//! KiCad の線フォントの文字（銅箔・ソルダーマスク・シルク）を kicad-cli で SVG に書き出して取り込む（C++ 版 app/kicad_cli）。
//! kicad-cli に渡すのは、それらの層に線フォントの文字と文字の枠だけを残した基板のコピー（krbr_kicad::stroke_text_only_board）。
//! TrueType の文字・線・図形・パッドは krbr が自分で描くので、そちらには影響しない

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use krbr_core::{debug, warn};
use krbr_geom::shapes::unite;
use krbr_geom::svg_reader::read_kicad_svg;
use krbr_geom::{FillRule, SidePaths};
use krbr_kicad::{stroke_text_only_board, Board, PLOT_LAYERS};

/// kicad-cli 1 回の上限（固まったときに止める）
const TIMEOUT: Duration = Duration::from_secs(120);

/// 表・裏の線フォントの文字（基板座標の多角形。銅箔・マスクの開口・シルク）
#[derive(Debug, Clone, Default)]
pub struct StrokeTextPlot {
    pub side: [Option<Arc<SidePaths>>; 2],
    pub from_cache: bool,
}

/// 線フォントの文字の書き出しに使う kicad-cli と、そのキャッシュ
#[derive(Debug, Clone)]
pub struct KicadCliText {
    pub cli: PathBuf,
    pub cache_dir: PathBuf,
    pub use_cache: bool,
}

impl KicadCliText {
    pub fn plot(&self, board: &Board) -> StrokeTextPlot {
        self.plot_file(&board.file, &board.project_name)
    }

    /// 基板のファイルとプロジェクトの名前から（裏のスレッドへ Board 全体を渡さずに済む）
    pub fn plot_file(&self, board_file: &Path, project_name: &str) -> StrokeTextPlot {
        plot_stroke_text(&self.cli, board_file, project_name, &self.cache_dir, self.use_cache)
    }
}

/// 線フォントの文字の書き出しを裏のスレッドで行う（ビューアと watch で共有）。
/// 書き出し中にまた頼まれたら、終わった後にもう一度書き出す（途中の版の文字を最後の結果にしない）
pub struct StrokeTextJob {
    /// kicad-cli を使わないなら None（request は何もしない）
    kicad: Option<KicadCliText>,
    board_file: PathBuf,
    project_name: String,
    running: Option<mpsc::Receiver<StrokeTextPlot>>,
    rerun: bool,
}

impl StrokeTextJob {
    pub fn new(kicad: Option<KicadCliText>, board_file: &Path, project_name: &str) -> Self {
        Self { kicad, board_file: board_file.to_path_buf(), project_name: project_name.to_string(), running: None, rerun: false }
    }

    /// 書き出し中か
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// 書き出しを頼む（書き出し中なら、終わった後にもう一度）
    pub fn request(&mut self) {
        let Some(k) = self.kicad.clone() else { return };
        if self.running.is_some() {
            self.rerun = true;
            return;
        }
        let (file, name) = (self.board_file.clone(), self.project_name.clone());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // SVG の解析と多角形の和は CPU を使うので、ウィンドウのスレッドより優先度を下げる（ほかの裏の処理と同じ）
            krbr_core::platform::lower_thread_priority();
            let _ = tx.send(k.plot_file(&file, &name));
        });
        self.running = Some(rx);
    }

    /// 書き出しが終わっていれば、その結果（頼まれていた次の書き出しは、ここで始める）
    pub fn poll(&mut self) -> Option<StrokeTextPlot> {
        let rx = self.running.as_ref()?;
        let r = match rx.try_recv() {
            Ok(plot) => Some(plot),
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => {
                warn!("kicad-cli text: the export thread stopped");
                None
            }
        };
        self.running = None;
        if std::mem::take(&mut self.rerun) {
            self.request();
        }
        r
    }
}

/// kicad-cli を探す：設定のパス、インストールされた KiCad の bin（新しい版を優先。プラグインの登録と同じ探し方で、
/// レジストリの InstallLocation と Program Files\KiCad\<版>）、PATH の順
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
        // 版は数で比べる（フォルダ名を文字列で並べると "10.0" が "9.0" より前になる）
        let newest = crate::kicad_plugin::installed_programs().into_iter().map(|(v, dir)| (v, dir.join("bin").join(exe))).filter(|(_, c)| c.is_file()).max_by_key(|(v, _)| *v);
        if let Some((_, p)) = newest {
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

/// 基板の線フォントの文字（銅箔・マスク・シルク）を kicad-cli で書き出して多角形にする。結果は cache_dir/silk にキャッシュする
pub fn plot_stroke_text(cli: &Path, board_file: &Path, project_name: &str, cache_dir: &Path, use_cache: bool) -> StrokeTextPlot {
    let mut out = StrokeTextPlot::default();
    let board_text = match std::fs::read_to_string(board_file) {
        Ok(t) => t,
        Err(e) => {
            warn!("kicad-cli text: {}: {e}", board_file.display());
            return out;
        }
    };
    let dir = board_file.parent().unwrap_or(Path::new("."));
    let pro = dir.join(format!("{project_name}.kicad_pro"));
    let pro_text = std::fs::read_to_string(&pro).ok();
    let cdir = cache_dir.join("silk");

    // キャッシュのキー：渡す基板のコピー（線フォントの文字だけを残したもの）、プロジェクト（文字の変数）、kicad-cli の場所。
    // コピーを作るには基板全体を解析し直す（大きな基板で 0.5 秒前後）ので、元の基板の中身から引ける索引（<元のキー>.key）も残し、
    // 基板が前と同じなら解析を省く
    let tail = format!("{}{}", pro_text.as_deref().unwrap_or(""), cli.to_string_lossy());
    let raw_key = krbr_core::to_hex(krbr_core::xxh64(format!("krbr-stroke-text-raw-v2\n{board_text}{tail}").as_bytes()));
    let index = cdir.join(format!("{raw_key}.key"));
    let mut filtered: Option<String> = None;
    let key = match std::fs::read_to_string(&index).ok().filter(|k| use_cache && k.len() == 16) {
        Some(k) => k,
        None => {
            let f = match stroke_text_only_board(&board_text) {
                Ok(t) => t,
                Err(e) => {
                    warn!("kicad-cli text: {}: {e}", board_file.display());
                    return out;
                }
            };
            let k = krbr_core::to_hex(krbr_core::xxh64(format!("krbr-stroke-text-v2\n{f}{tail}").as_bytes()));
            filtered = Some(f);
            k
        }
    };
    let cached: Vec<PathBuf> = PLOT_LAYERS.iter().map(|(_, shown)| cdir.join(format!("{key}-{shown}.svg"))).collect();
    let mut svg: Vec<Option<String>> = vec![None; PLOT_LAYERS.len()];
    if use_cache && cached.iter().all(|c| c.is_file()) {
        svg = cached.iter().map(|c| std::fs::read_to_string(c).ok()).collect();
        out.from_cache = svg.iter().all(Option::is_some);
    }
    if out.from_cache && filtered.is_some() {
        let _ = std::fs::create_dir_all(&cdir);
        let _ = std::fs::write(&index, &key);
    }
    if !out.from_cache {
        let filtered = match filtered.map(Ok).unwrap_or_else(|| stroke_text_only_board(&board_text)) {
            Ok(t) => t,
            Err(e) => {
                warn!("kicad-cli text: {}: {e}", board_file.display());
                return out;
            }
        };
        svg = export_layers(cli, board_file, &pro, pro_text.as_deref(), &filtered);
        if use_cache && svg.iter().all(Option::is_some) {
            let _ = std::fs::create_dir_all(&cdir);
            for (path, text) in cached.iter().zip(&svg) {
                let _ = std::fs::write(path, text.as_deref().unwrap_or(""));
            }
            let _ = std::fs::write(&index, &key);
        }
    }
    let mut sides = [SidePaths::default(), SidePaths::default()];
    let mut got = [false; 2];
    for ((layer, _), text) in PLOT_LAYERS.iter().zip(&svg) {
        let Some(text) = text else { continue };
        let s = usize::from(layer.starts_with("B."));
        let r = read_kicad_svg(text);
        let paths = unite(&r.paths, FillRule::NonZero);
        debug!("kicad-cli text {layer}: {} elements -> {} paths", r.elements, paths.len());
        let to = if layer.ends_with(".Cu") {
            &mut sides[s].copper
        } else if layer.ends_with(".Mask") {
            &mut sides[s].mask_open
        } else {
            &mut sides[s].silk
        };
        *to = paths;
        got[s] = true;
    }
    for (s, sp) in sides.into_iter().enumerate() {
        if got[s] {
            out.side[s] = Some(Arc::new(sp));
        }
    }
    out
}

/// 線フォントの文字だけを残した基板のコピー（filtered）を、PLOT_LAYERS の層ごとに SVG に書き出す（並びは PLOT_LAYERS と同じ。
/// 書き出せなかった層は None）。1 回の --mode-multi でまとめて書き出し（kicad-cli の起動と基板の読み込みが時間の大半なので）、
/// それで出なかった層（--mode-multi の無い古い kicad-cli など）は層ごとに --mode-single で書き出す
fn export_layers(cli: &Path, board_file: &Path, pro: &Path, pro_text: Option<&str>, filtered: &str) -> Vec<Option<String>> {
    let mut svg: Vec<Option<String>> = vec![None; PLOT_LAYERS.len()];
    let work = work_dir();
    if let Err(e) = std::fs::create_dir_all(&work) {
        warn!("kicad-cli text: {}: {e}", work.display());
        return svg;
    }
    // kicad-cli は基板の横に .kicad_prl を書くので、コピーに対して実行する
    let pcb_copy = work.join(board_file.file_name().unwrap_or_default());
    if let Err(e) = std::fs::write(&pcb_copy, filtered) {
        warn!("kicad-cli text: {}: {e}", pcb_copy.display());
        let _ = std::fs::remove_dir_all(&work);
        return svg;
    }
    if let Some(t) = pro_text {
        let _ = std::fs::write(work.join(pro.file_name().unwrap_or_default()), t);
    }
    let log = work.join("kicad-cli.log");
    let pcb = pcb_copy.to_string_lossy().to_string();
    let run = |args: &[&str], what: &str| -> bool {
        match run_hidden(cli, args, &work, &log) {
            Ok(0) => true,
            Ok(rc) => {
                let msg = std::fs::read_to_string(&log).unwrap_or_default();
                warn!("kicad-cli failed for {what} (exit {rc}): {}", msg.trim());
                false
            }
            Err(e) => {
                warn!("kicad-cli failed for {what}: {e}");
                false
            }
        }
    };
    // --mode-multi は「<基板名>-<層の表示名>.svg」に書く。コピーでは表示名を PLOT_LAYERS の名前にしてある
    let multi = work.join("svg");
    let multi_s = multi.to_string_lossy().to_string();
    let layers = PLOT_LAYERS.map(|(l, _)| l).join(",");
    if run(&["pcb", "export", "svg", "--layers", &layers, "--mode-multi", "--exclude-drawing-sheet", "--page-size-mode", "0", "-o", &multi_s, &pcb], "all layers") {
        let files: Vec<PathBuf> = std::fs::read_dir(&multi).map(|rd| rd.flatten().map(|e| e.path()).collect()).unwrap_or_default();
        for (i, (_, shown)) in PLOT_LAYERS.iter().enumerate() {
            let suffix = format!("-{shown}.svg");
            svg[i] = files.iter().find(|f| f.file_name().is_some_and(|n| n.to_string_lossy().ends_with(&suffix))).and_then(|f| std::fs::read_to_string(f).ok());
        }
    }
    for (i, (layer, _)) in PLOT_LAYERS.iter().enumerate() {
        if svg[i].is_some() {
            continue;
        }
        let path = work.join(format!("{}.svg", layer.replace('.', "_")));
        let path_s = path.to_string_lossy().to_string();
        if run(&["pcb", "export", "svg", "--layers", layer, "--mode-single", "--exclude-drawing-sheet", "--page-size-mode", "0", "-o", &path_s, &pcb], layer) {
            svg[i] = std::fs::read_to_string(&path).ok();
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    svg
}

#[cfg(test)]
mod tests {
    use super::*;
    use krbr_geom::shapes::{path_points, signed_area};
    use krbr_kicad::{Box2, Vec2};

    /// 銅箔（表・裏）・表のマスク・裏のシルクに線フォントの文字を 1 つずつ置いた基板
    const BOARD: &str = r#"(kicad_pcb (version 20260206) (generator "pcbnew") (generator_version "10.0")
  (general (thickness 1.6))
  (paper "A4")
  (layers (0 "F.Cu" signal) (2 "B.Cu" signal) (1 "F.Mask" user) (3 "B.Mask" user) (5 "F.SilkS" user "F.Silkscreen") (7 "B.SilkS" user "B.Silkscreen") (25 "Edge.Cuts" user))
  (setup (pad_to_mask_clearance 0))
  (gr_text "CU" (at 20 20) (layer "F.Cu") (uuid "00000000-0000-0000-0000-000000000001") (effects (font (size 2 2) (thickness 0.3))))
  (gr_text "BCU" (at 60 20) (layer "B.Cu") (uuid "00000000-0000-0000-0000-000000000002") (effects (font (size 2 2) (thickness 0.3)) (justify mirror)))
  (gr_text "MASK" (at 20 50) (layer "F.Mask") (uuid "00000000-0000-0000-0000-000000000003") (effects (font (size 2 2) (thickness 0.3))))
  (gr_text "SILK" (at 60 50) (layer "B.SilkS") (uuid "00000000-0000-0000-0000-000000000004") (effects (font (size 2 2) (thickness 0.3)) (justify mirror)))
)
"#;

    /// 面積と範囲（基板座標）
    fn extent(p: &krbr_geom::Paths) -> (f64, Box2) {
        let mut b = Box2::default();
        for q in p.iter() {
            for v in path_points(q) {
                b.add(v);
            }
        }
        (p.iter().map(signed_area).sum::<f64>().abs(), b)
    }

    /// 文字の層ごとに、置いた位置（基準点の周り 6 mm）に面積のある多角形ができる。文字の無い層は空
    #[test]
    #[ignore = "needs kicad-cli (KiCad 9 or later)"]
    fn plots_stroke_texts_on_copper_mask_and_silk() {
        let cli = find_kicad_cli("").expect("kicad-cli not found");
        let dir = std::env::temp_dir().join(format!("krbr_rust_stroke_text_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("small.kicad_pcb");
        std::fs::write(&file, BOARD).unwrap();
        let t0 = Instant::now();
        let plot = plot_stroke_text(&cli, &file, "small", &dir.join("cache"), true);
        println!("kicad-cli: {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
        let again = plot_stroke_text(&cli, &file, "small", &dir.join("cache"), true);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!plot.from_cache && again.from_cache);

        let [Some(front), Some(back)] = &plot.side else { panic!("missing side") };
        let at = |p: &krbr_geom::Paths, x: f64, y: f64, what: &str| {
            let (area, b) = extent(p);
            assert!(area > 0.5, "{what}: area {area}");
            assert!(b.lo.x > x - 6.0 && b.hi.x < x + 6.0 && b.lo.y > y - 6.0 && b.hi.y < y + 6.0, "{what}: {b:?}");
            assert!(b.intersects(&Box2 { lo: Vec2::new(x - 0.5, y - 0.5), hi: Vec2::new(x + 0.5, y + 0.5) }), "{what}: {b:?}");
        };
        at(&front.copper, 20.0, 20.0, "F.Cu");
        at(&back.copper, 60.0, 20.0, "B.Cu");
        at(&front.mask_open, 20.0, 50.0, "F.Mask");
        at(&back.silk, 60.0, 50.0, "B.SilkS");
        assert!(front.silk.is_empty() && back.mask_open.is_empty());
        // キャッシュから読んでも同じ
        let [Some(f2), Some(_)] = &again.side else { panic!("missing side") };
        assert_eq!(extent(&f2.copper).0, extent(&front.copper).0);
    }
}
