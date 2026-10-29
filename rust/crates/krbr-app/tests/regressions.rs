//! 直したバグの再発を防ぐ GPU のテスト（Vulkan の GPU が要るので、明示したときだけ走らせる）：
//!   cargo test --release -p krbr-app --test regressions -- --ignored --nocapture

mod common;

use std::path::{Path, PathBuf};

use krbr_app::prepare::{frame_params, glow_params, initial_camera, prepare_scene, RenderOptions};
use krbr_render::image_io::{tonemap_rgba8, ImageF};
use krbr_render::{render_offscreen, Context, ContextOptions, GpuScene, RenderSettings};
use krbr_scene::LensModel;

fn bench_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bench")
}

/// 全体設定のフォルダ（KRBR_CONFIG_DIR）を差し替えるので、テストを 1 つずつ走らせる
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 基板の設定ファイル（テストの全体設定のフォルダの中）を書く
fn write_settings(pcb: &Path, text: &str) {
    let file = krbr_scene::board_file::resolve(pcb);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, text).unwrap();
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

fn context() -> Context {
    Context::new(&ContextOptions { low_queue_priority: true, ..Default::default() }).unwrap()
}

/// 描画して、リニアの色・アルベドと設定を返す（krbr render と同じ流れ）
fn render(ctx: &Context, o: &RenderOptions) -> (ImageF, ImageF, krbr_scene::ProjectConfig) {
    let prep = prepare_scene(o).expect("scene");
    let cfg = prep.cfg.clone();
    let cam = initial_camera(&prep, o, cfg.width, cfg.height);
    let mut gpu = GpuScene::new(ctx);
    gpu.build(&prep.scene, &prep.env, &prep.dist, cfg.scene.bump_intensity).unwrap();
    let rs = RenderSettings { width: cfg.width, height: cfg.height, spp: cfg.spp, integrator: cfg.integrator, frame: frame_params(&cfg, cam, o.seed), denoise: None, gpu_limit: Default::default() };
    let out = render_offscreen(ctx, &gpu, &rs).unwrap();
    (out.color, out.albedo, cfg)
}

/// RGB の平均
fn mean(img: &ImageF) -> f64 {
    let s: f64 = img.rgba.chunks_exact(4).map(|p| (p[0] + p[1] + p[2]) as f64).sum();
    s / (img.rgba.len() / 4 * 3) as f64
}

/// 鏡映した（scale が負の）3D モデルの発光面も、鏡映しないときと同じように光る。
/// 以前は光源の一覧の三角形の向きが逆になり、直接サンプリングは裏を光る面として扱って、周りが暗くなっていた
#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn mirrored_emitters_light_like_unmirrored_ones() {
    let _g = serial();
    let ws = std::env::temp_dir().join(format!("krbr_rust_mirrored_emitter_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    common::use_config_dir(&ws.join("config"));
    let ctx = context();
    let text = std::fs::read_to_string(bench_dir().join("krbr_bench.kicad_pcb")).unwrap();
    // D1（LED、(106, 166)）の 3D モデルだけを X 方向に鏡映した基板（LED は左右ほぼ対称なので、形はほとんど変わらない）
    let at = text.find("(at 106 166)").expect("D1");
    let scale = at + text[at..].find("(xyz 1 1 1)").unwrap();
    let mirrored = format!("{}(xyz -1 1 1){}", &text[..scale], &text[scale + "(xyz 1 1 1)".len()..]);
    // 3D モデル（${KIPRJMOD}/3dmodels/...）は基板の横に要る（KIPRJMOD は KiCad と同じく、設定では変えられない）
    copy_dir(&bench_dir().join("3dmodels"), &ws.join("3dmodels"));
    let mut lum = Vec::new();
    for (name, board) in [("plain", &text), ("mirrored", &mirrored)] {
        let pcb = ws.join(format!("{name}.kicad_pcb"));
        std::fs::write(&pcb, board).unwrap();
        // D1 を明るく光らせる
        write_settings(&pcb, "[ref.D1]\nmaterial = \"plastic+emit=#ffffff,20000\"\n");
        let o = RenderOptions { pcb, hdr: "builtin:studio".into(), env_intensity: Some(0.05), spp: Some(256), width: Some(160), height: Some(90), look_at: vec![106.0, 166.0], zoom: Some(12.0), elevation: Some(55.0), ignore_remembered: true, no_kicad_cli: true, ..Default::default() };
        let (color, _, _) = render(&ctx, &o);
        // LED に照らされた周り（直接見えている光る面の画素は、どちらでも同じなので除く）
        let around: Vec<f64> = color.rgba.chunks_exact(4).map(|p| (p[0] + p[1] + p[2]) as f64 / 3.0).filter(|&v| v < 1.0).collect();
        lum.push(around.iter().sum::<f64>() / around.len() as f64);
    }
    let rel = (lum[1] - lum[0]).abs() / lum[0];
    println!("plain {:.5}, mirrored {:.5}, difference {:.2} %", lum[0], lum[1], rel * 100.0);
    assert!(rel < 0.03, "a mirrored emitter lights its surroundings differently: {:.2} %", rel * 100.0);
    let _ = std::fs::remove_dir_all(&ws);
}

/// 口径食（optical_vignetting）で光が通らないサンプルがあっても、光の届いた画素のアルベドの AOV は暗くならない
/// （以前は通らなかったサンプルも数えていたので、一部が遮られた画素は、通った割合だけ暗くなった）。
/// すべてのサンプルが遮られた画素（画面の隅）は光が届かないので 0 のまま
#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn vignetting_does_not_darken_the_albedo() {
    let _g = serial();
    let ws = std::env::temp_dir().join(format!("krbr_rust_aov_vignetting_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    common::use_config_dir(&ws.join("config"));
    let ctx = context();
    let pcb = bench_dir().join("krbr_bench.kicad_pcb");
    let mut shots = Vec::new();
    for vignetting in [0.0, 1.0] {
        write_settings(&pcb, &format!("[lens]\noptical_vignetting = {vignetting}\n"));
        let o = RenderOptions { pcb: pcb.clone(), hdr: "builtin:studio".into(), lens: Some(LensModel::Thin), f_number: Some(1.4), spp: Some(32), width: Some(160), height: Some(90), ignore_remembered: true, no_kicad_cli: true, ..Default::default() };
        let (c, a, _) = render(&ctx, &o);
        shots.push((c, a));
    }
    let sum = |p: &[f32]| (p[0] + p[1] + p[2]) as f64;
    let (mut plain, mut vignetted, mut lit) = (0.0, 0.0, 0usize);
    for (p, v) in shots[0].1.rgba.chunks_exact(4).zip(shots[1].1.rgba.chunks_exact(4)) {
        if sum(v) > 0.0 {
            plain += sum(p);
            vignetted += sum(v);
            lit += 1;
        }
    }
    let rel = (vignetted - plain).abs() / plain;
    println!("albedo of {lit} lit pixels: {plain:.2} -> {vignetted:.2} ({:.2} %); colour {:.4} -> {:.4}", rel * 100.0, mean(&shots[0].0), mean(&shots[1].0));
    assert!(lit > shots[1].1.rgba.len() / 8, "too few pixels receive light");
    assert!(mean(&shots[1].0) < 0.8 * mean(&shots[0].0), "the vignetting should darken the colour");
    assert!(rel < 0.02, "vignetting changed the albedo of lit pixels by {:.2} %", rel * 100.0);
    let _ = std::fs::remove_dir_all(&ws);
}

/// 表示する画像（8 bit）
fn display(color: &ImageF, cfg: &krbr_scene::ProjectConfig) -> Vec<u8> {
    tonemap_rgba8(color, cfg.exposure_ev, &glow_params(cfg))
}

/// 基準画像との比較：同梱の基板をいくつかの視点・方式で描き、tests/golden/<名前>.png と比べる。
/// 見た目を変えるつもりの無い変更で、描画が変わっていないこと（例：d69639a の前は、レジストの厚みを反対の面で求めて、
/// 寄った画像の 83 % の画素が変わっていたが、ほかのテストは通っていた）。
/// 同じ GPU・ドライバなら画素ごとに一致する。GPU やドライバが違うと計算の誤差で少し変わることがあるので、少しの差は許す。
/// 見た目を変える変更をしたら、変わった画像（失敗したときに一時フォルダに書く）を確かめてから、基準を作り直す：
///   set KRBR_UPDATE_GOLDEN=1 && cargo test --release -p krbr-app --test regressions golden -- --ignored
#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn golden_images() {
    let _g = serial();
    let ws = std::env::temp_dir().join(format!("krbr_rust_golden_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    common::use_config_dir(&ws.join("config"));
    let ctx = context();
    let golden_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("golden");
    let update = std::env::var_os("KRBR_UPDATE_GOLDEN").is_some();
    let base = RenderOptions { pcb: bench_dir().join("krbr_bench.kicad_pcb"), hdr: "builtin:studio".into(), spp: Some(16), width: Some(240), height: Some(135), ignore_remembered: true, no_kicad_cli: true, ..Default::default() };
    let close = RenderOptions { look_at: vec![150.0, 135.0], zoom: Some(4.0), elevation: Some(60.0), ..base.clone() };
    let views = [
        ("overview", base.clone()),
        ("closeup", close.clone()),
        ("bottom", RenderOptions { elevation: Some(-60.0), ..base.clone() }),
        ("closeup_bdpt_spectral_thin", RenderOptions { integrator: Some(krbr_scene::IntegratorKind::Bdpt), spectral: Some(true), lens: Some(LensModel::Thin), ..close.clone() }),
        ("closeup_vcm_glass", RenderOptions { integrator: Some(krbr_scene::IntegratorKind::Vcm), substrate: "glass_frosted".into(), ..close.clone() }),
    ];
    let mut failed = Vec::new();
    for (name, o) in &views {
        let (color, _, cfg) = render(&ctx, o);
        let px = display(&color, &cfg);
        let (w, h) = (color.width, color.height);
        let file = golden_dir.join(format!("{name}.png"));
        if update {
            std::fs::create_dir_all(&golden_dir).unwrap();
            image::save_buffer(&file, &px, w, h, image::ColorType::Rgba8).unwrap();
            println!("{name}: wrote {}", file.display());
            continue;
        }
        let reference = image::open(&file).unwrap_or_else(|e| panic!("{}: {e} (create the references with KRBR_UPDATE_GOLDEN=1)", file.display())).to_rgba8();
        assert_eq!(reference.dimensions(), (w, h), "{name}: size");
        let (mut sum, mut changed) = (0.0f64, 0usize);
        for (a, b) in px.chunks_exact(4).zip(reference.as_raw().chunks_exact(4)) {
            let d = (0..3).map(|c| (a[c] as i32 - b[c] as i32).abs()).max().unwrap();
            sum += (0..3).map(|c| (a[c] as i32 - b[c] as i32).abs() as f64).sum::<f64>() / 3.0;
            changed += usize::from(d > 8);
        }
        let n = (w * h) as f64;
        let (mean, frac) = (sum / n / 255.0, changed as f64 / n);
        println!("{name:<28} mean |diff| {mean:.5}, changed {:.2} %", frac * 100.0);
        // 許す差：平均で 8 bit の 1 段の半分、はっきり変わった画素が 0.5 %
        if mean > 0.5 / 255.0 || frac > 0.005 {
            let out = std::env::temp_dir().join(format!("krbr_golden_{name}.png"));
            image::save_buffer(&out, &px, w, h, image::ColorType::Rgba8).unwrap();
            failed.push(format!("{name} (mean {mean:.5}, changed {:.2} %; now: {}, reference: {})", frac * 100.0, out.display(), file.display()));
        }
    }
    let _ = std::fs::remove_dir_all(&ws);
    assert!(failed.is_empty(), "rendering changed:\n  {}", failed.join("\n  "));
}
