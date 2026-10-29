//! 描画結果が変わっていないことを、累積画像（float）のビット単位で確かめる（BDPT / VCM は丸めの誤差まで）（速度改善の前後の比較。perf_design.md の
//! 「検証の共通手順」）。同梱の基板を、決まった条件（種・サンプル数・構図）で PT・PT 分光・BDPT・VCM で描く。
//! 基準は変更前の版で作り、<repo>/out/bitexact/ に置く（git には入れない）：
//!   KRBR_BITEXACT=save cargo test --release -p krbr-app --test bitexact -- --ignored --nocapture   # 基準を作る
//!   cargo test --release -p krbr-app --test bitexact -- --ignored --nocapture                      # 基準と比べる

mod common;

use std::path::{Path, PathBuf};

use krbr_app::prepare::{frame_params, initial_camera, prepare_scene, RenderOptions};
use krbr_render::image_io::ImageF;
use krbr_render::{render_offscreen, Context, ContextOptions, GpuScene, RenderSettings};
use krbr_scene::IntegratorKind;

struct Case {
    name: &'static str,
    integrator: IntegratorKind,
    spectral: bool,
    /// 自動の構図からの拡大（1 で基板全体。大きいと近くの面が多く、凹凸の模様が細かく見える）
    zoom: f64,
}

const CASES: [Case; 6] = [
    Case { name: "pt_overview", integrator: IntegratorKind::Pt, spectral: false, zoom: 1.0 },
    Case { name: "pt_closeup", integrator: IntegratorKind::Pt, spectral: false, zoom: 8.0 },
    Case { name: "pt_spectral", integrator: IntegratorKind::Pt, spectral: true, zoom: 1.0 },
    Case { name: "bdpt", integrator: IntegratorKind::Bdpt, spectral: false, zoom: 1.0 },
    Case { name: "bdpt_closeup", integrator: IntegratorKind::Bdpt, spectral: false, zoom: 8.0 },
    Case { name: "vcm", integrator: IntegratorKind::Vcm, spectral: false, zoom: 1.0 },
];

const SPP: u32 = 16;
/// BDPT / VCM で同じとみなす相対的な差の上限（atomic の足し算の順による丸め。同じ版を 2 回描いた差より十分大きく、
/// 描き方が変わった差より十分小さい）
const TOLERANCE: f32 = 1e-5;
const SEED: u32 = 3;

fn out_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../out/bitexact")
}

/// 色・アルベド・法線を、幅・高さ（u32 ずつ）に続けて float のまま並べる
fn to_bytes(images: &[&ImageF]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&images[0].width.to_le_bytes());
    b.extend_from_slice(&images[0].height.to_le_bytes());
    for img in images {
        for v in &img.rgba {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    b
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing and the reference images in out/bitexact"]
fn renders_match_the_reference_bit_for_bit() {
    let save = std::env::var("KRBR_BITEXACT").is_ok_and(|v| v == "save");
    let ws = std::env::temp_dir().join(format!("krbr_rust_bitexact_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    common::use_config_dir(&ws.join("config"));
    std::fs::create_dir_all(out_dir()).unwrap();
    let ctx = Context::new(&ContextOptions { low_queue_priority: true, ..Default::default() }).unwrap();
    let pcb = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bench/krbr_bench.kicad_pcb");
    let mut failed = Vec::new();
    for c in &CASES {
        let o = RenderOptions {
            pcb: pcb.clone(),
            hdr: "builtin:studio".into(),
            width: Some(320),
            height: Some(180),
            spp: Some(SPP),
            seed: SEED,
            integrator: Some(c.integrator),
            spectral: Some(c.spectral),
            zoom: Some(c.zoom),
            ignore_remembered: true,
            builtin_settings_only: true,
            no_kicad_cli: true,
            ..Default::default()
        };
        let prep = prepare_scene(&o).expect("scene");
        let cfg = &prep.cfg;
        let cam = initial_camera(&prep, &o, cfg.width, cfg.height);
        let mut gpu = GpuScene::new(&ctx);
        gpu.build(&prep.scene, &prep.env, &prep.dist, cfg.scene.bump_intensity).unwrap();
        let rs = RenderSettings { width: cfg.width, height: cfg.height, spp: SPP, integrator: c.integrator, frame: frame_params(cfg, cam, SEED), denoise: None };
        let out = render_offscreen(&ctx, &gpu, &rs).unwrap();
        let bytes = to_bytes(&[&out.color, &out.albedo, &out.normal]);
        let file = out_dir().join(format!("{}.f32", c.name));
        if save {
            std::fs::write(&file, &bytes).unwrap();
            println!("{}: saved {}", c.name, file.display());
            continue;
        }
        let reference = std::fs::read(&file).unwrap_or_else(|e| panic!("{}: {e} (make the references first with KRBR_BITEXACT=save)", file.display()));
        if reference == bytes {
            println!("{}: identical", c.name);
            continue;
        }
        let f = |b: &[u8]| f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        let (mut differing, mut max_rel) = (0usize, 0.0f32);
        for (a, b) in reference.chunks_exact(4).zip(bytes.chunks_exact(4)).skip(2) {
            let (a, b) = (f(a), f(b));
            if a.to_bits() != b.to_bits() {
                differing += 1;
                max_rel = max_rel.max((a - b).abs() / a.abs().max(b.abs()).max(1e-6));
            }
        }
        let n = bytes.len() / 4 - 2;
        // BDPT / VCM は光源側の経路を画面へ atomic の float の足し算で書くので、足す順で丸めが変わり、同じ版でも毎回少し違う。
        // その大きさ（丸めの誤差）までは同じとみなす
        let rounding = c.integrator != IntegratorKind::Pt && max_rel <= TOLERANCE && differing * 100 <= n;
        println!("{}: {differing} of {n} values differ, largest relative difference {max_rel:.2e}{}", c.name, if rounding { " (rounding of the atomic adds; accepted)" } else { "" });
        if !rounding {
            failed.push(c.name);
        }
    }
    let _ = std::fs::remove_dir_all(&ws);
    assert!(failed.is_empty(), "not bit-identical: {failed:?}");
}
