//! オフスクリーンでのレンダリングの進行（サンプルの投入・待機・読み戻し）

use std::time::Instant;

use crate::gpu_scene::GpuScene;
use crate::image_io::ImageF;
use crate::integrator::{FrameParams, Progressive};
use crate::vk::Context;

#[derive(Debug, Clone)]
pub struct RenderSettings {
    pub width: u32,
    pub height: u32,
    pub spp: u32,
    pub integrator: String,
    pub frame: FrameParams,
}

pub struct RenderOutput {
    pub color: ImageF,
    pub albedo: ImageF,
    pub normal: ImageF,
    pub seconds: f64,
}

pub fn render_offscreen(ctx: &Context, scene: &GpuScene<'_>, s: &RenderSettings) -> Result<RenderOutput, String> {
    let mut prog = Progressive::new(ctx, scene, &s.integrator, s.width, s.height, s.frame.clone())?;
    krbr_core::info!("rendering {}x{}, {} spp, integrator '{}', max depth {}, {}", s.width, s.height, s.spp, prog.integrator_name(), s.frame.max_depth, if s.frame.spectral { "spectral" } else { "RGB" });

    // 1 サンプルずつ投入して完了を待つ。GPU を長時間占有しないので、作業中の画面描画が止まりにくい
    let t0 = Instant::now();
    let mut last_log = t0;
    for i in 0..s.spp {
        ctx.submit_and_wait(|_, cmd| prog.record(cmd, 1));
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
    let aov = prog.readback();
    krbr_core::info!("render finished in {:.2} s ({:.1} ms/spp)", seconds, seconds * 1000.0 / s.spp.max(1) as f64);
    Ok(RenderOutput { color: aov.color, albedo: aov.albedo, normal: aov.normal, seconds })
}
