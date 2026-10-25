//! `krbr bench`：GPU を替えたときなどに、同じ条件でレンダリングの速さを測る。
//! 覚えた設定（パネルの設定・視点・環境マップ）は使わず、条件を固定する：内蔵のスタジオ、自動の構図（仰角 35°・方位 45°・画角 30°）、
//! ピンホール、露出 0、乱数の種 0、ノイズ除去なし。PT（RGB）・PT（分光）・BDPT・VCM をそれぞれ測る。
//! 結果は `%APPDATA%\krbr\bench.csv` に追記し、同じ基板・同じ条件で別の GPU の記録があれば、それと比べる。
//! 基板を指定しなければ、同梱のベンチマークの基板（`bench\krbr_bench.kicad_pcb`。scripts/gen_bench_board.py が作る）を使う。
//! 同梱の基板は 3D モデルも同じフォルダに持ち、設定ファイル（基板の .krbr、全体設定の材質と対応表）と kicad-cli を使わないので、
//! どの PC でも同じシーンになる。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use krbr_core::{info, warn};
use krbr_render::{render_offscreen, GpuScene, RenderSettings};
use krbr_scene::{IntegratorKind, LensModel};

use crate::prepare::{frame_params, initial_camera, prepare_scene_and_context, trace_method_name, RenderOptions};

#[derive(Debug, Clone)]
pub struct BenchOptions {
    /// None なら同梱のベンチマークの基板
    pub pcb: Option<PathBuf>,
    pub width: u32,
    pub height: u32,
    /// 場合ごとに測るサンプル数（この前に準備として WARMUP_SPP を流す）
    pub spp: u32,
}

/// 準備のサンプル数（シェーダの準備と GPU のクロックが上がるのを待つ。測らない）
const WARMUP_SPP: u32 = 8;

/// 測る場合：(表示名, 積分器, 分光)
const CASES: [(&str, IntegratorKind, bool); 4] = [("PT", IntegratorKind::Pt, false), ("PT spectral", IntegratorKind::Pt, true), ("BDPT", IntegratorKind::Bdpt, false), ("VCM", IntegratorKind::Vcm, false)];

/// 同梱のベンチマークの基板のファイル名
pub const BUNDLED_BOARD: &str = "krbr_bench.kicad_pcb";

/// 同梱のベンチマークの基板：配布の一式では exe の横の bench\、開発中はリポジトリの bench\（exe のフォルダから上へたどる）
pub fn bundled_board() -> Option<PathBuf> {
    krbr_core::platform::exe_dir().ancestors().take(6).map(|d| d.join("bench").join(BUNDLED_BOARD)).find(|f| f.is_file())
}

const CSV_HEADER: &str = "date,gpu,driver,method,board,board_hash,width,height,spp,max_depth,case,ms_per_spp";

struct Row {
    gpu: String,
    board_hash: String,
    conditions: String,
    case: String,
    ms_per_spp: f64,
}

pub fn cmd_bench(b: &BenchOptions) -> anyhow::Result<()> {
    let bundled = b.pcb.is_none();
    let pcb = match &b.pcb {
        Some(p) => p.clone(),
        None => bundled_board().ok_or_else(|| anyhow::anyhow!("the bundled benchmark board (bench\\{BUNDLED_BOARD}) was not found next to krbr.exe; give a .kicad_pcb"))?,
    };
    let o = RenderOptions {
        pcb,
        hdr: krbr_render::studio_env::STUDIO.into(),
        width: Some(b.width),
        height: Some(b.height),
        spp: Some(b.spp),
        elevation: Some(35.0),
        azimuth: Some(45.0),
        fov: Some(30.0),
        exposure: Some(0.0),
        env_intensity: Some(1.0),
        env_rotation: Some(0.0),
        lens: Some(LensModel::Pinhole),
        integrator: Some(IntegratorKind::Pt),
        spectral: Some(false),
        denoise: Some(false),
        ignore_remembered: true,
        builtin_settings_only: bundled,
        no_kicad_cli: bundled,
        ..Default::default()
    };
    let t0 = Instant::now();
    let (prep, ctx) = prepare_scene_and_context(&o, false);
    let Some(prep) = prep else { anyhow::bail!("scene preparation failed") };
    let ctx = ctx?;
    let prepare_s = t0.elapsed().as_secs_f64();
    if bundled && !prep.scene.missing.is_empty() {
        // 箱で代えたまま測ると、別の PC の記録と比べられない
        anyhow::bail!("{} 3D model(s) of the bundled benchmark board are missing or unreadable (e.g. {}); reinstall krbr", prep.scene.missing.len(), prep.scene.missing[0].spec);
    }
    let cfg = &prep.cfg;
    let cam = initial_camera(&prep, &o, b.width, b.height);
    let method = trace_method_name(&ctx);
    let driver = driver_string(&ctx.info.name, ctx.info.driver_version);
    let mut gpu = GpuScene::new(&ctx);
    gpu.build(&prep.scene, &prep.env, &prep.dist, cfg.scene.bump_intensity).map_err(|e| anyhow::anyhow!("GPU scene: {e}"))?;
    let board_hash = std::fs::read(&prep.board.file).map(|d| krbr_core::to_hex(krbr_core::xxh64(&d))).unwrap_or_default();
    let board_name = prep.board.file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    info!("bench: {} ({}, driver {}), {}x{}, {} spp per case after {} warm-up, max depth {}, scene prepared in {:.1} s", ctx.info.name, method, driver, b.width, b.height, b.spp, WARMUP_SPP, cfg.max_depth, prepare_s);

    let mut results = Vec::new();
    for (name, integrator, spectral) in CASES {
        let mut frame = frame_params(cfg, cam, 0);
        frame.spectral = spectral;
        let mut rs = RenderSettings { width: b.width, height: b.height, spp: WARMUP_SPP, integrator, frame, denoise: None };
        render_offscreen(&ctx, &gpu, &rs).map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
        rs.spp = b.spp;
        let out = render_offscreen(&ctx, &gpu, &rs).map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
        let ms = out.seconds * 1000.0 / b.spp.max(1) as f64;
        info!("bench: {name}: {} spp/s ({ms:.1} ms/spp, {:.1} s for {} spp)", fmt_rate(spp_per_second(ms)), out.seconds, b.spp);
        results.push((name, ms, out.seconds));
    }

    // 記録と比較
    let file = krbr_core::platform::config_dir().join("bench.csv");
    let conditions = format!("{},{},{},{}", b.width, b.height, b.spp, cfg.max_depth);
    let previous = read_rows(&file);
    let date = utc_date_time();
    let mut lines = Vec::new();
    for (name, ms, _) in &results {
        lines.push(format!("{date},{},{driver},{method},{},{board_hash},{conditions},{name},{ms:.3}", csv_field(&ctx.info.name), csv_field(&board_name)));
    }
    if let Err(e) = append_rows(&file, &lines) {
        warn!("{}: {}", file.display(), e);
    }

    println!();
    println!("{} ({}, driver {})", ctx.info.name, method, driver);
    println!("{}: {}x{}, {} spp per case, max depth {}", board_name, b.width, b.height, b.spp, cfg.max_depth);
    println!("{:<12} {:>12} {:>10}   {}", "case", "spp/s", "seconds", "vs. previous GPU");
    for (name, ms, secs) in &results {
        // 同じ基板・同じ条件で、別の GPU の最後の記録
        let prev = previous.iter().rev().find(|r| r.board_hash == board_hash && r.conditions == conditions && r.case == *name && r.gpu != ctx.info.name);
        let cmp = prev.map(|r| format!("{:.2}x faster than {} ({} spp/s)", r.ms_per_spp / ms, r.gpu, fmt_rate(spp_per_second(r.ms_per_spp)))).unwrap_or_default();
        println!("{:<12} {:>12} {:>10.1}   {}", name, fmt_rate(spp_per_second(*ms)), secs, cmp);
    }
    println!("recorded in {}", file.display());
    Ok(())
}

/// 1 秒あたりのサンプル数（表示用。大きいほど速い。CSV には互換のため 1 サンプルあたりの ms のまま残す）
fn spp_per_second(ms_per_spp: f64) -> f64 {
    if ms_per_spp > 0.0 {
        1000.0 / ms_per_spp
    } else {
        0.0
    }
}

/// 有効数字 3 桁ほどで（遅い GPU・大きな解像度で 1 未満になっても 0.0 と出さない）
fn fmt_rate(v: f64) -> String {
    match v {
        v if v >= 100.0 => format!("{v:.0}"),
        v if v >= 10.0 => format!("{v:.1}"),
        v if v >= 1.0 => format!("{v:.2}"),
        _ => format!("{v:.3}"),
    }
}

/// ドライバの版（NVIDIA は 10 ビット・8 ビット・8 ビット・6 ビット、ほかは Vulkan と同じ並び）
fn driver_string(name: &str, v: u32) -> String {
    if name.to_uppercase().contains("NVIDIA") {
        format!("{}.{:02}", v >> 22, (v >> 14) & 0xff)
    } else {
        format!("{}.{}.{}", v >> 22, (v >> 12) & 0x3ff, v & 0xfff)
    }
}

fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// CSV の 1 行を区切る（"..." の中の , と "" に対応）
fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn read_rows(file: &Path) -> Vec<Row> {
    let Ok(text) = std::fs::read_to_string(file) else { return Vec::new() };
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f = split_csv(l);
            (f.len() == 12).then(|| Row { gpu: f[1].clone(), board_hash: f[5].clone(), conditions: f[6..10].join(","), case: f[10].clone(), ms_per_spp: f[11].parse().unwrap_or(0.0) }).filter(|r| r.ms_per_spp > 0.0)
        })
        .collect()
}

fn append_rows(file: &Path, lines: &[String]) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let new = !file.exists();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(file)?;
    if new {
        writeln!(f, "{CSV_HEADER}")?;
    }
    for l in lines {
        writeln!(f, "{l}")?;
    }
    Ok(())
}

/// 今の日時（UTC、ISO 8601 の形）
fn utc_date_time() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (days, rem) = (secs / 86400, secs % 86400);
    // 1970-01-01 からの日数を年月日に（Howard Hinnant の civil_from_days）
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_round_trip_and_comparison_key() {
        let dir = std::env::temp_dir().join(format!("krbr_bench_test_{}", std::process::id()));
        let file = dir.join("bench.csv");
        let _ = std::fs::remove_dir_all(&dir);
        let line = |gpu: &str, case: &str, ms: f64| format!("2026-10-21T00:00:00Z,{},581.57,ray tracing pipeline,a.kicad_pcb,abcd,1920,1080,64,8,{case},{ms:.3}", csv_field(gpu));
        append_rows(&file, &[line("NVIDIA GeForce GTX 1060 6GB", "PT", 180.0), line("GPU, \"quoted\"", "VCM", 400.0)]).unwrap();
        append_rows(&file, &[line("NVIDIA GeForce RTX 3060", "PT", 30.0)]).unwrap();
        let rows = read_rows(&file);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1].gpu, "GPU, \"quoted\"");
        assert_eq!(rows[0].conditions, "1920,1080,64,8");
        assert_eq!((rows[2].case.as_str(), rows[2].ms_per_spp), ("PT", 30.0));
    }

    /// 同梱のベンチマークの基板が、そのフォルダだけで閉じている：3D モデルはどれも ${KIPRJMOD} の中にあり、
    /// kicad-cli の要る文字と塗りの無いゾーンが無い（環境によって結果が変わるものを含まない）
    #[test]
    fn bundled_board_is_self_contained() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bench");
        let board = krbr_kicad::load_board(&dir.join(BUNDLED_BOARD)).expect("bench board");
        assert!(board.footprints.len() > 100 && board.vias.len() > 100);
        assert_eq!(board.stroke_text_count(), 0);
        assert_eq!(board.unfilled_zones, 0);
        let resolver = krbr_kicad::ModelResolver::new(&board.project_dir, &Default::default(), &[]);
        let models_dir = std::fs::canonicalize(dir.join("3dmodels")).unwrap();
        let mut n = 0;
        for m in board.footprints.iter().flat_map(|f| &f.models) {
            assert!(m.path.starts_with("${KIPRJMOD}/3dmodels/"), "{}", m.path);
            let r = resolver.resolve(&m.path);
            assert!(r.found && !r.substituted, "{}", m.path);
            assert!(std::fs::canonicalize(&r.path).unwrap().starts_with(&models_dir), "{}", r.path.display());
            n += 1;
        }
        assert!(n > 100);
    }

    #[test]
    fn date_and_driver() {
        let d = utc_date_time();
        assert_eq!(d.len(), 20);
        assert!(d.starts_with("20") && d.ends_with('Z'));
        // NVIDIA 581.57 = (581 << 22) | (57 << 14)
        assert_eq!(driver_string("NVIDIA GeForce GTX 1060", (581 << 22) | (57 << 14)), "581.57");
        assert_eq!(fmt_rate(spp_per_second(12.5)), "80.0");
        assert_eq!(fmt_rate(spp_per_second(686.116)), "1.46");
        assert_eq!(fmt_rate(spp_per_second(2.0)), "500");
        assert_eq!(fmt_rate(spp_per_second(4000.0)), "0.250");
    }
}
