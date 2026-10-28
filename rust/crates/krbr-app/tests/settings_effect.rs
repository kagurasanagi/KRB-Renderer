//! 設定の反映：コマンドラインと基板の設定ファイル（<名前>.krbr）の設定を 1 つずつ変えて描き、画像が変わることを確かめる
//! （「設定を変えても何も変わらない」バグを機械的に見つける）。あわせて、同じ条件なら同じ画像になること（決定性）と、
//! 画像に NaN・無限大（RGB モードでは負の値も）が無いことを確かめる。
//! 同梱のベンチマークの基板（bench\krbr_bench.kicad_pcb）を使い、全体設定はテスト用の空のフォルダにする。
//! Vulkan の GPU が要るので、明示したときだけ走らせる：
//!   cargo test --release -p krbr-app --test settings_effect -- --ignored --nocapture

mod common;

use std::path::{Path, PathBuf};

use krbr_app::prepare::{frame_params, glow_params, initial_camera, prepare_scene, RenderOptions};
use krbr_render::image_io::{tonemap_rgba8, ImageF};
use krbr_render::{render_offscreen, Context, ContextOptions, GpuScene, RenderSettings};
use krbr_scene::{IntegratorKind, LensModel};

const W: u32 = 320;
const H: u32 = 180;
const SPP: u32 = 16;

fn bench_board() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bench/krbr_bench.kicad_pcb")
}

/// 部品・配線・シルク・ビア・光る LED の集まったところを寄って見る（凹凸やレジストの違いが見える大きさ）
fn base_options() -> RenderOptions {
    RenderOptions {
        pcb: bench_board(),
        hdr: "builtin:studio".into(),
        spp: Some(SPP),
        width: Some(W),
        height: Some(H),
        look_at: vec![150.0, 135.0],
        zoom: Some(4.0),
        elevation: Some(60.0),
        ignore_remembered: true,
        no_kicad_cli: true,
        ..Default::default()
    }
}

/// ビアと部品の端子に寄って見る（ビアのくぼみ・表面の細かい凹凸の模様は、画面で数画素より細かいと描かないので、寄らないと見えない）
fn close_options() -> RenderOptions {
    RenderOptions { look_at: vec![151.3, 150.0], zoom: Some(12.0), elevation: Some(70.0), ..base_options() }
}

/// 描いた結果：表示する画像（露出・グロー・トーンマッピング済み、0..1）と、リニアの色・アルベド
struct Shot {
    display: Vec<f32>,
    color: ImageF,
    albedo: ImageF,
}

/// krbr render と同じ流れ（設定の解決 → 描画 → トーンマッピング）。krbr_text は基板の設定ファイルの中身（空なら無し）
fn shoot(ctx: &Context, o: &RenderOptions, krbr_text: &str) -> Shot {
    let file = krbr_scene::board_file::resolve(&o.pcb);
    if krbr_text.is_empty() {
        let _ = std::fs::remove_file(&file);
    } else {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, krbr_text).unwrap();
    }
    let prep = prepare_scene(o).expect("scene");
    let cfg = &prep.cfg;
    let cam = initial_camera(&prep, o, cfg.width, cfg.height);
    let mut gpu = GpuScene::new(ctx);
    gpu.build(&prep.scene, &prep.env, &prep.dist, cfg.scene.bump_intensity).unwrap();
    let rs = RenderSettings { width: cfg.width, height: cfg.height, spp: cfg.spp, integrator: cfg.integrator, frame: frame_params(cfg, cam, o.seed), denoise: None };
    let out = render_offscreen(ctx, &gpu, &rs).unwrap();
    let display = tonemap_rgba8(&out.color, cfg.exposure_ev, &glow_params(cfg)).iter().map(|&v| v as f32 / 255.0).collect();
    Shot { display, color: out.color, albedo: out.albedo }
}

/// 表示する画像の差：(RGB の平均の絶対差（0..1）, はっきり（8 bit で 3 段以上）変わった画素の割合)
fn diff(a: &Shot, b: &Shot) -> (f64, f64) {
    let (mut sum, mut n, mut changed, mut pixels) = (0.0f64, 0usize, 0usize, 0usize);
    for (pa, pb) in a.display.chunks_exact(4).zip(b.display.chunks_exact(4)) {
        let mut m = 0.0f32;
        for c in 0..3 {
            let d = (pa[c] - pb[c]).abs();
            sum += d as f64;
            m = m.max(d);
            n += 1;
        }
        changed += usize::from(m >= 3.0 / 255.0);
        pixels += 1;
    }
    (sum / n as f64, changed as f64 / pixels as f64)
}

/// NaN・無限大と、負の値の数。分光モードの結果（XYZ）はリニア sRGB に直すと sRGB の色域の外の色が負になり、
/// 色収差があると代表の 1 波長だけを追うので、少ないサンプルでは単色に近い色（大きく負）にもなる。
/// 分光モードでは NaN・無限大だけを数える
fn bad_values(img: &ImageF, spectral: bool) -> usize {
    let vals = || img.rgba.chunks_exact(4).flat_map(|p| p[..3].iter().copied());
    let (nonfinite, negative) = (vals().filter(|v| !v.is_finite()).count(), if spectral { 0 } else { vals().filter(|v| *v < 0.0).count() });
    if nonfinite + negative > 0 {
        let min = vals().filter(|v| v.is_finite()).fold(f32::MAX, f32::min);
        println!("  non-finite {nonfinite}, negative {negative}, min {min}");
    }
    nonfinite + negative
}

/// 変えたと分かる差の下限：平均の差が 1/1000 以上（8 bit の 1 段の 1/4）か、はっきり変わった画素が 0.1 % 以上
/// （ビアのくぼみのように、小さな所だけが変わる設定もある）。同じ設定なら画像は画素ごとに一致する（決定性を先に確かめる）ので、
/// この差は設定が効いた分
const MIN_MEAN: f64 = 1e-3;
const MIN_CHANGED: f64 = 1e-3;

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn every_setting_changes_the_image() {
    let ws = std::env::temp_dir().join(format!("krbr_rust_settings_effect_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    common::use_config_dir(&ws.join("config")); // 基板の設定ファイルはテスト用の全体設定のフォルダに置く（同梱の基板の横に書かない）
    let ctx = Context::new(&ContextOptions { low_queue_priority: true, ..Default::default() }).unwrap();

    let base = base_options();
    let reference = shoot(&ctx, &base, "");
    assert_eq!(bad_values(&reference.color, false), 0, "NaN / inf / negative values in the colour");
    assert_eq!(bad_values(&reference.albedo, false), 0, "NaN / inf / negative values in the albedo");
    // 決定性：同じ条件なら同じ画像
    let again = shoot(&ctx, &base, "");
    assert_eq!(diff(&reference, &again), (0.0, 0.0), "the same settings must give the same image");
    let close = close_options();
    let close_ref = shoot(&ctx, &close, "");

    // 薄レンズ・分光・VCM の細かい設定は、それを使っているときに比べる
    let thin = RenderOptions { lens: Some(LensModel::Thin), ..base.clone() };
    let thin_ref = shoot(&ctx, &thin, "");
    let thin_blades = RenderOptions { blades: Some(6), f_number: Some(1.4), ..thin.clone() };
    let blades_ref = shoot(&ctx, &thin_blades, "");
    let spectral_thin = RenderOptions { spectral: Some(true), ..thin.clone() };
    let spectral_thin_ref = shoot(&ctx, &spectral_thin, "");
    let vcm = RenderOptions { integrator: Some(IntegratorKind::Vcm), ..base.clone() };
    let vcm_ref = shoot(&ctx, &vcm, "");
    // 背景を透明にする設定は、背景の見える引いた構図で比べる
    let wide = RenderOptions { look_at: Vec::new(), zoom: None, elevation: Some(40.0), ..base.clone() };
    let wide_ref = shoot(&ctx, &wide, "");

    // (名前, 基準の設定, 基準の画像, 変えた設定, 基板の設定ファイル)
    let cases: Vec<(&str, &RenderOptions, &Shot, RenderOptions, &str)> = vec![
        // コマンドライン
        ("--exposure", &base, &reference, RenderOptions { exposure: Some(2.0), ..base.clone() }, ""),
        ("--glow", &base, &reference, RenderOptions { glow: Some(0.0), ..base.clone() }, ""),
        ("--glow-threshold", &base, &reference, RenderOptions { glow_threshold: Some(0.5), ..base.clone() }, ""),
        ("--glow-spread", &base, &reference, RenderOptions { glow_spread: Some(0.1), ..base.clone() }, ""),
        ("--env-intensity", &base, &reference, RenderOptions { env_intensity: Some(2.0), ..base.clone() }, ""),
        ("--env-rotation", &base, &reference, RenderOptions { env_rotation: Some(90.0), ..base.clone() }, ""),
        ("--clamp", &base, &reference, RenderOptions { radiance_clamp: Some(0.2), ..base.clone() }, ""),
        ("--spectral", &base, &reference, RenderOptions { spectral: Some(true), ..base.clone() }, ""),
        ("--transparent", &wide, &wide_ref, RenderOptions { transparent_background: Some(true), ..wide.clone() }, ""),
        ("--no-fillet", &close, &close_ref, RenderOptions { solder_fillet: Some(false), ..close.clone() }, ""),
        ("--max-depth", &base, &reference, RenderOptions { max_depth: Some(1), ..base.clone() }, ""),
        ("--fov", &base, &reference, RenderOptions { fov: Some(45.0), ..base.clone() }, ""),
        ("--elevation", &base, &reference, RenderOptions { elevation: Some(40.0), ..base.clone() }, ""),
        ("--azimuth", &base, &reference, RenderOptions { azimuth: Some(120.0), ..base.clone() }, ""),
        ("--zoom", &base, &reference, RenderOptions { zoom: Some(6.0), ..base.clone() }, ""),
        ("--look-at", &base, &reference, RenderOptions { look_at: vec![160.0, 140.0], ..base.clone() }, ""),
        ("--seed", &base, &reference, RenderOptions { seed: 7, ..base.clone() }, ""),
        ("--spp", &base, &reference, RenderOptions { spp: Some(SPP * 2), ..base.clone() }, ""),
        ("--integrator bdpt", &base, &reference, RenderOptions { integrator: Some(IntegratorKind::Bdpt), ..base.clone() }, ""),
        ("--integrator vcm", &base, &reference, vcm.clone(), ""),
        ("--lens thin", &base, &reference, thin.clone(), ""),
        ("--lens realistic", &base, &reference, RenderOptions { lens: Some(LensModel::Realistic), ..base.clone() }, ""),
        ("--mask-color", &base, &reference, RenderOptions { mask_color: "jlcpcb-purple".into(), ..base.clone() }, ""),
        ("--substrate", &base, &reference, RenderOptions { substrate: "glass_frosted".into(), ..base.clone() }, ""),
        ("--f-number", &thin, &thin_ref, RenderOptions { f_number: Some(1.0), ..thin.clone() }, ""),
        ("--focal-length", &thin, &thin_ref, RenderOptions { focal_length: Some(150.0), ..thin.clone() }, ""),
        ("--focus-distance", &thin, &thin_ref, RenderOptions { focus_distance: Some(40.0), ..thin.clone() }, ""),
        ("--blades", &thin, &thin_ref, RenderOptions { blades: Some(5), ..thin.clone() }, ""),
        // 基板の設定ファイル（コマンドラインに無いもの）
        ("[board] finish", &base, &reference, base.clone(), "[board]\nfinish = \"HASL\"\n"),
        ("[board] mask_roughness", &base, &reference, base.clone(), "[board]\nmask_roughness = 0.8\n"),
        ("[board] mask_scatter", &base, &reference, base.clone(), "[board]\nmask_scatter = 0.95\n"),
        ("[board] mask_color preset", &base, &reference, base.clone(), "[board]\nmask_color = \"jlcpcb-white\"\n"),
        ("[board] silk_color", &base, &reference, base.clone(), "[board]\nsilk_color = [0.9, 0.1, 0.1]\n"),
        ("[board] fr4_color", &base, &reference, base.clone(), "[board]\nfr4_color = [0.1, 0.1, 0.9]\n"),
        ("[relief] copper_um", &base, &reference, base.clone(), "[relief]\ncopper_um = 200\n"),
        ("[relief] mask_on_copper_um", &base, &reference, base.clone(), "[relief]\nmask_on_copper_um = 80\n"),
        ("[relief] mask_on_substrate_um", &base, &reference, base.clone(), "[relief]\nmask_on_substrate_um = 5\n"),
        ("[relief] silk_um", &base, &reference, base.clone(), "[relief]\nsilk_um = 120\n"),
        ("[relief] via_dimple_um", &close, &close_ref, close.clone(), "[relief]\nvia_dimple_um = 0\n"),
        ("[surface] bump_intensity", &close, &close_ref, close.clone(), "[surface]\nbump_intensity = 2.0\n"),
        ("[solder] fillet", &close, &close_ref, close.clone(), "[solder]
fillet = false
"),
        ("[solder] material", &close, &close_ref, close.clone(), "[solder]
material = \"metal_gold\"
"),
        ("[camera] margin", &base, &reference, base.clone(), "[camera]\nmargin = 0.3\n"),
        ("[render] exposure_ev", &base, &reference, base.clone(), "[render]\nexposure_ev = -1.0\n"),
        ("[render] glow", &base, &reference, base.clone(), "[render]\nglow = 0.0\n"),
        ("[render] transparent_background", &wide, &wide_ref, wide.clone(), "[render]
transparent_background = true
"),
        ("[render] radiance_clamp", &base, &reference, base.clone(), "[render]\nradiance_clamp = 0.2\n"),
        ("[environment] intensity", &base, &reference, base.clone(), "[environment]\nintensity = 3.0\n"),
        ("[environment] rotation_deg", &base, &reference, base.clone(), "[environment]\nrotation_deg = 120.0\n"),
        ("[lens] blade_rotation_deg", &thin_blades, &blades_ref, thin_blades.clone(), "[lens]\nblade_rotation_deg = 30.0\n"),
        ("[lens] blade_roundness", &thin_blades, &blades_ref, thin_blades.clone(), "[lens]\nblade_roundness = 1.0\n"),
        ("[lens] natural_vignetting", &thin, &thin_ref, thin.clone(), "[lens]\nnatural_vignetting = true\n"),
        ("[lens] optical_vignetting", &thin_blades, &blades_ref, thin_blades.clone(), "[lens]\noptical_vignetting = 1.0\n"),
        ("[lens] abbe_number", &spectral_thin, &spectral_thin_ref, spectral_thin.clone(), "[lens]\nabbe_number = 20.0\n"),
        ("[lens] lateral_ca", &spectral_thin, &spectral_thin_ref, spectral_thin.clone(), "[lens]\nlateral_ca = 0.02\n"),
        ("[render] vcm_radius", &vcm, &vcm_ref, vcm.clone(), "[render]\nvcm_radius = 2.0\n"),
    ];

    let mut no_effect = Vec::new();
    for (name, _, before, o, krbr) in &cases {
        let after = shoot(&ctx, o, krbr);
        let (mean, changed) = diff(before, &after);
        println!("{name:<30} mean {mean:.5}, changed {:.2} %", changed * 100.0);
        let spectral = o.spectral == Some(true);
        assert_eq!(bad_values(&after.color, spectral), 0, "{name}: NaN / inf / negative values in the colour");
        assert_eq!(bad_values(&after.albedo, spectral), 0, "{name}: NaN / inf / negative values in the albedo");
        if mean < MIN_MEAN && changed < MIN_CHANGED {
            no_effect.push(format!("{name} (mean {mean:.5}, changed {:.3} %)", changed * 100.0));
        }
    }
    let _ = std::fs::remove_dir_all(&ws);
    assert!(no_effect.is_empty(), "settings with no visible effect: {}", no_effect.join(", "));
}
