//! グロー：表示（GPU の shaders/glow.comp と tonemap.comp）と、画像の保存（CPU の image_io::tonemap_rgba8）が同じ画素になること
mod common;

use ash::vk;
use krbr_render::glow::GlowParams;
use krbr_render::image_io::tonemap_rgba8;
use krbr_render::tonemapper::{DisplayMode, Tonemapper};
use krbr_render::vk::memory_barrier;
use krbr_render::ImageF;

/// 暗い地に、明るい赤い点と白い点、明るさの段差（しきい値の前後）
fn test_image(w: u32, h: u32) -> ImageF {
    let mut img = ImageF::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let v = 0.02 + 0.6 * x as f32 / w as f32;
            img.rgba[i..i + 4].copy_from_slice(&[v, v * 0.9, v * 0.8, 1.0]);
        }
    }
    let mut put = |x: u32, y: u32, c: [f32; 3]| {
        let i = ((y * w + x) * 4) as usize;
        img.rgba[i..i + 3].copy_from_slice(&c);
    };
    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        put(30 + dx, 20 + dy, [40.0, 0.5, 0.2]);
        put(70 + dx, 45 + dy, [8.0, 8.0, 8.0]);
    }
    put(5, 60, [1.2, 0.1, 0.05]);
    img
}

fn gpu_tonemap(ctx: &krbr_render::Context, img: &ImageF, exposure_ev: f32, glow: &GlowParams) -> Vec<u8> {
    let (w, h) = (img.width, img.height);
    let mut color = ctx.create_image_2d(w, h, 1, vk::Format::R32G32B32A32_SFLOAT, vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_DST).expect("image");
    let bytes: Vec<u8> = img.rgba.iter().flat_map(|v| v.to_le_bytes()).collect();
    ctx.upload_image(&color, &bytes, vk::ImageLayout::GENERAL).expect("upload");
    let mut rb = ctx.create_buffer((w * h * 4) as u64, vk::BufferUsageFlags::TRANSFER_DST, true, true).expect("readback buffer");
    {
        let tm = Tonemapper::with_views(ctx, color.view, color.view, color.view, w, h).expect("tonemapper");
        let (out, buf) = (tm.output().image, rb.buffer);
        ctx.submit_and_wait(|dev, cmd| {
            tm.record(cmd, exposure_ev, DisplayMode::Beauty, false, glow);
            memory_barrier(dev, cmd, vk::PipelineStageFlags2::COMPUTE_SHADER, vk::AccessFlags2::SHADER_STORAGE_WRITE, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_READ);
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 })
                .image_extent(vk::Extent3D { width: w, height: h, depth: 1 });
            unsafe { dev.cmd_copy_image_to_buffer(cmd, out, vk::ImageLayout::GENERAL, buf, &[region]) };
            memory_barrier(dev, cmd, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE, vk::PipelineStageFlags2::HOST, vk::AccessFlags2::HOST_READ);
        })
        .expect("submit");
    }
    let px = rb.mapped_bytes().unwrap().to_vec();
    assert_eq!(px.len(), (w * h * 4) as usize);
    ctx.destroy_buffer(&mut rb);
    ctx.destroy_image(&mut color);
    px
}

#[test]
#[ignore]
fn glow_on_the_display_matches_the_saved_image() {
    let _g = common::gpu_lock();
    let ctx = common::context();
    let img = test_image(96, 64);
    let cases = [
        GlowParams::OFF,
        GlowParams::default(),
        GlowParams { strength: 3.0, threshold: 1.0, spread: 1.0 },
        GlowParams { strength: 1.0, threshold: 8.0, spread: 0.2 },
    ];
    for glow in &cases {
        let gpu = gpu_tonemap(&ctx, &img, 0.7, glow);
        let cpu = tonemap_rgba8(&img, 0.7, glow);
        let max_diff = gpu.iter().zip(&cpu).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        println!("{glow:?}: max difference {max_diff} / 255");
        assert!(max_diff <= 2, "{glow:?}: GPU and CPU differ by {max_diff}");
    }
    // グローは赤い点の周りを赤くする（緑・青より赤が増える）
    let (off, on) = (tonemap_rgba8(&img, 0.7, &GlowParams::OFF), tonemap_rgba8(&img, 0.7, &GlowParams::default()));
    let i = ((20 * 96 + 36) * 4) as usize;
    let gain = |k: usize| on[i + k] as i32 - off[i + k] as i32;
    assert!(gain(0) > 10 && gain(0) > 2 * gain(1).max(gain(2)), "red glow: {:?} -> {:?}", &off[i..i + 3], &on[i..i + 3]);
}
