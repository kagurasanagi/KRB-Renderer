//! ノイズ除去（OIDN）。少ないサンプルの画像をノイズ除去すると、多いサンプルの参照画像に近づくこと。
//! Vulkan の GPU と OIDN の DLL（<repo>\third_party\oidn\bin または exe の横）が要るので、明示したときだけ走らせる：
//!   cargo test --release -p krbr-app --test denoise -- --ignored --nocapture

mod common;

use krbr_app::prepare::{frame_params, initial_camera, prepare_scene, RenderOptions};
use krbr_render::image_io::ImageF;
use krbr_render::{render_offscreen, Context, ContextOptions, DenoiseQuality, GpuScene, RenderOutput, RenderSettings};

/// 画素の値（RGB）の平均の差
fn mean_abs_diff(a: &ImageF, b: &ImageF) -> f64 {
    let mut sum = 0.0f64;
    let mut n = 0usize;
    for (pa, pb) in a.rgba.chunks_exact(4).zip(b.rgba.chunks_exact(4)) {
        for c in 0..3 {
            sum += (pa[c] - pb[c]).abs().min(10.0) as f64; // ホタル 1 つで平均が振れないよう上限を付ける
            n += 1;
        }
    }
    sum / n as f64
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing and the OIDN DLLs"]
fn denoising_a_4spp_image_approaches_the_reference() {
    let ws = std::env::temp_dir().join(format!("krbr_rust_denoise_{}", std::process::id()));
    common::use_config_dir(&ws.join("config")); // 環境マップの記憶を、テストの基板の横に書かない
    assert!(krbr_render::denoiser_available(), "OIDN DLLs not found");
    let pcb = common::test_board();
    // 既定の基板では部品の多いところ（95, 95）を寄って見る。別の基板では自動の構図の中央
    let look_at = if common::custom_board() { Vec::new() } else { vec![95.0, 95.0] };
    let ctx = Context::new(&ContextOptions { low_queue_priority: true, ..Default::default() }).unwrap();
    for spectral in [false, true] {
        let o = RenderOptions { pcb: pcb.clone(), hdr: "builtin:studio".into(), width: Some(480), height: Some(270), look_at: look_at.clone(), zoom: 2.0, spectral: Some(spectral), ..Default::default() };
        let prep = prepare_scene(&o).unwrap();
        let cam = initial_camera(&prep, &o, 480, 270);
        let mut gpu = GpuScene::new(&ctx);
        gpu.build(&prep.scene, &prep.env, &prep.dist, prep.cfg.scene.bump_intensity);
        let render = |spp: u32, denoise: Option<(DenoiseQuality, f32)>| -> RenderOutput {
            let rs = RenderSettings { width: 480, height: 270, spp, integrator: "pt".into(), frame: frame_params(&prep.cfg, cam, 1), denoise };
            render_offscreen(&ctx, &gpu, &rs).unwrap()
        };
        let reference = render(1024, None).color;
        let noisy = render(4, Some((DenoiseQuality::High, 1.0)));
        let denoised = noisy.denoised.as_ref().expect("denoiser did not run");
        let (e_noisy, e_denoised) = (mean_abs_diff(&noisy.color, &reference), mean_abs_diff(denoised, &reference));
        println!("{}: 4 spp {e_noisy:.4}, denoised {e_denoised:.4}", if spectral { "spectral" } else { "RGB" });
        // 既定の基板（部品の多いところを寄って見る）では 40% 未満。別の基板の全体の構図は細かい模様が多く、減り方が小さい（kachkojir で 40.2%）
        let limit = if common::custom_board() { 0.5 } else { 0.4 };
        assert!(e_denoised < limit * e_noisy, "{e_denoised} vs {e_noisy}");

        if !spectral {
            // 強さ 0 は元の画像そのもの、0.5 はその間
            let zero = render(4, Some((DenoiseQuality::High, 0.0)));
            assert!(mean_abs_diff(zero.denoised.as_ref().unwrap(), &zero.color) < 1e-4);
            let half = render(4, Some((DenoiseQuality::High, 0.5)));
            let e_half = mean_abs_diff(half.denoised.as_ref().unwrap(), &reference);
            assert!(e_half < e_noisy && e_half > e_denoised, "{e_half}");
            // CPU デバイス（GPU で共有できない PC と同じ道）でも同じくらい近づく
            std::env::set_var("KRBR_DENOISE_DEVICE", "cpu");
            let cpu = render(4, Some((DenoiseQuality::High, 1.0)));
            std::env::remove_var("KRBR_DENOISE_DEVICE");
            let e_cpu = mean_abs_diff(cpu.denoised.as_ref().unwrap(), &reference);
            println!("CPU device: denoised {e_cpu:.4}");
            assert!(e_cpu < limit * e_noisy, "{e_cpu}");
        }
    }
    let _ = std::fs::remove_dir_all(&ws);
}
