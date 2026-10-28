//! オフスクリーンでのレンダリングの進行（サンプルの投入・待機・読み戻し）

use std::time::Instant;

use krbr_scene::IntegratorKind;

use crate::gpu_scene::GpuScene;
use crate::image_io::ImageF;
use crate::denoiser::{DenoiseQuality, Denoiser};
use crate::integrator::{xyz_to_linear_srgb, FrameParams, Progressive};
use crate::vk::Context;

#[derive(Debug, Clone)]
pub struct RenderSettings {
    pub width: u32,
    pub height: u32,
    pub spp: u32,
    pub integrator: IntegratorKind,
    pub frame: FrameParams,
    /// ノイズ除去（OIDN）もする：品質と、元の画像と混ぜる強さ（0..1）
    pub denoise: Option<(DenoiseQuality, f32)>,
}

pub struct RenderOutput {
    pub color: ImageF,
    pub albedo: ImageF,
    pub normal: ImageF,
    pub seconds: f64,
    /// ノイズ除去した画像（リニア sRGB）。使えなければ None
    pub denoised: Option<ImageF>,
}

pub fn render_offscreen(ctx: &Context, scene: &GpuScene<'_>, s: &RenderSettings) -> Result<RenderOutput, String> {
    let mut prog = Progressive::new(ctx, scene, s.integrator, s.width, s.height, s.frame.clone())?;
    krbr_core::info!("rendering {}x{}, {} spp, integrator '{}', max depth {}, {}", s.width, s.height, s.spp, prog.integrator_name(), s.frame.max_depth, if s.frame.spectral { "spectral" } else { "RGB" });

    // 1 サンプルずつ投入して完了を待つ。GPU を長時間占有しないので、作業中の画面描画が止まりにくい
    let t0 = Instant::now();
    let mut last_log = t0;
    for i in 0..s.spp {
        ctx.submit_and_wait(|_, cmd| prog.record(cmd, 1))?;
        std::thread::yield_now();
        let now = Instant::now();
        if now.duration_since(last_log).as_secs_f64() > 2.0 || i + 1 == s.spp {
            let el = now.duration_since(t0).as_secs_f64();
            let eta = el / (i + 1) as f64 * (s.spp - i - 1) as f64;
            krbr_core::info!("  {}/{} spp, {:.1} s elapsed, ~{:.1} s left", i + 1, s.spp, el, eta);
            last_log = now;
        }
    }
    let seconds = t0.elapsed().as_secs_f64();
    let aov = prog.readback()?;
    krbr_core::info!("render finished in {:.2} s ({:.1} ms/spp)", seconds, seconds * 1000.0 / s.spp.max(1) as f64);
    let xyz = prog.xyz_output();
    let denoised = s.denoise.and_then(|(q, strength)| match Denoiser::new(ctx, prog.targets(), q) {
        Ok(d) => {
            let run = || -> Result<(ImageF, f64), String> {
                ctx.submit_and_wait(|_, cmd| d.record_prepare(cmd, xyz))?;
                let ms = d.execute()?;
                ctx.submit_and_wait(|_, cmd| d.record_output(cmd, strength, xyz))?;
                Ok((d.read_output()?, ms))
            };
            match run() {
                Ok((mut img, ms)) => {
                    if xyz {
                        xyz_to_linear_srgb(&mut img);
                    }
                    // 背景が透明なら、ノイズ除去していない画像のアルファを付ける（アルファはノイズが少ない）
                    img.copy_alpha_from(&aov.color);
                    krbr_core::info!("denoised in {:.0} ms ({}, quality {}, strength {:.2})", ms, d.device_name(), q.name(), strength);
                    Some(img)
                }
                Err(e) => {
                    krbr_core::warn!("{e}; writing the noisy image only");
                    None
                }
            }
        }
        Err(e) => {
            krbr_core::warn!("denoiser not available ({e}); writing the noisy image only");
            None
        }
    });
    Ok(RenderOutput { color: aov.color, albedo: aov.albedo, normal: aov.normal, seconds, denoised })
}
