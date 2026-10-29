//! Vulkan の検証レイヤー（同期の検証を含む）を有効にして、主な GPU の処理を一通り流し、エラーが 1 つも出ないこと。
//! 積分器（PT / BDPT / VCM）× 色の計算（RGB / 分光）× レンズについて、1 回の描画、ノイズ除去とその読み戻し、
//! ビューアと同じ 1 つのコマンドに複数のサンプルを積む累積、トーンマッピング（背景を透明にしたときの市松模様の表示も）を通す。
//! Vulkan の GPU と検証レイヤー（Vulkan SDK）が要るので、明示したときだけ走らせる：
//!   cargo test --release -p krbr-app --test validation -- --ignored --nocapture

mod common;

use std::path::Path;

use krbr_app::prepare::{frame_params, glow_params, initial_camera, prepare_scene, RenderOptions};
use krbr_render::tonemapper::{Background, DisplayMode, Tonemapper};
use krbr_render::vk::validation_error_count;
use krbr_render::{render_offscreen, Context, ContextOptions, DenoiseQuality, GpuScene, Progressive, RenderSettings};
use krbr_scene::{IntegratorKind, LensModel};

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing and the Vulkan validation layer"]
fn gpu_work_passes_the_validation_layer() {
    let ws = std::env::temp_dir().join(format!("krbr_rust_validation_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    common::use_config_dir(&ws.join("config"));
    let ctx = Context::new(&ContextOptions { validation: true, sync_validation: true, low_queue_priority: true, ..Default::default() }).unwrap();
    assert!(ctx.validating(), "the Vulkan validation layer (VK_LAYER_KHRONOS_validation) is not installed");
    let denoise = krbr_render::denoiser_available();
    let (w, h) = (160, 90);
    let before = validation_error_count();
    for lens in [LensModel::Pinhole, LensModel::Thin, LensModel::Realistic] {
        for spectral in [false, true] {
            let o = RenderOptions {
                pcb: Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bench/krbr_bench.kicad_pcb"),
                hdr: "builtin:studio".into(),
                width: Some(w),
                height: Some(h),
                spectral: Some(spectral),
                // 背景を透明にする経路（アルファの読み戻し、市松模様の表示）も通す。時間を増やさないよう分光のときに
                transparent_background: Some(spectral),
                lens: Some(lens),
                ignore_remembered: true,
                no_kicad_cli: true,
                ..Default::default()
            };
            let prep = prepare_scene(&o).expect("scene");
            let cfg = &prep.cfg;
            let cam = initial_camera(&prep, &o, w, h);
            let mut gpu = GpuScene::new(&ctx);
            gpu.build(&prep.scene, &prep.env, &prep.dist, cfg.scene.bump_intensity).unwrap();
            for kind in IntegratorKind::ALL {
                let what = format!("{kind} {lens:?} {}", if spectral { "spectral, transparent background" } else { "rgb" });
                // 1 回の描画（krbr render）。ノイズ除去があれば、その結果も読み戻す
                let rs = RenderSettings { width: w, height: h, spp: 2, integrator: kind, frame: frame_params(cfg, cam, 0), denoise: denoise.then_some((DenoiseQuality::Fast, 1.0)), gpu_limit: Default::default() };
                render_offscreen(&ctx, &gpu, &rs).unwrap_or_else(|e| panic!("{what}: {e}"));
                // ビューアと同じ累積：1 つのコマンドに 3 サンプルを 2 回積み、トーンマッピングして読み戻す
                let mut prog = Progressive::new(&ctx, &gpu, kind, w, h, frame_params(cfg, cam, 0)).unwrap_or_else(|e| panic!("{what}: {e}"));
                let tone = Tonemapper::new(&ctx, prog.targets()).unwrap();
                for _ in 0..2 {
                    ctx.submit_and_wait(|_, cmd| {
                        prog.record(cmd, 3);
                        tone.record(cmd, cfg.exposure_ev, DisplayMode::Beauty, prog.xyz_output(), &glow_params(cfg), Background::for_display(prog.frame().transparent_background));
                    })
                    .unwrap();
                }
                prog.readback().unwrap_or_else(|e| panic!("{what}: {e}"));
                assert_eq!(validation_error_count(), before, "{what}: the validation layer reported errors (see the log)");
            }
        }
    }
    let _ = std::fs::remove_dir_all(&ws);
}
