//! グロー：表示（GPU の shaders/glow.comp と tonemap.comp）と、画像の保存（CPU の image_io::tonemap_rgba8）が同じ画素になること
mod common;

use ash::vk;
use krbr_render::glow::GlowParams;
use krbr_render::image_io::tonemap_rgba8;
use krbr_render::tonemapper::{Background, DisplayMode, Tonemapper};
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
    gpu_tonemap_with(ctx, img, exposure_ev, glow, Background::Opaque)
}

/// background が Opaque 以外なら、img の A をアルファとして渡す（累積の形にする：色は A = 1 のサンプル 1 つ、
/// アルベドの A（数えたサンプル）は 1、最初に当たった点の A（当たったサンプル）はアルファ）
fn gpu_tonemap_with(ctx: &krbr_render::Context, img: &ImageF, exposure_ev: f32, glow: &GlowParams, background: Background) -> Vec<u8> {
    let (w, h) = (img.width, img.height);
    let upload = |rgba: &[f32]| {
        let im = ctx.create_image_2d(w, h, 1, vk::Format::R32G32B32A32_SFLOAT, vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_DST).expect("image");
        let bytes: Vec<u8> = rgba.iter().flat_map(|v| v.to_le_bytes()).collect();
        ctx.upload_image(&im, &bytes, vk::ImageLayout::GENERAL).expect("upload");
        im
    };
    let accum: Vec<f32> = img.rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2], 1.0]).collect();
    let ones = vec![1.0f32; (w * h * 4) as usize];
    let hits: Vec<f32> = img.rgba.chunks_exact(4).flat_map(|p| [0.0, 0.0, 0.0, p[3]]).collect();
    let (mut color, mut albedo, mut position) = (upload(&accum), upload(&ones), upload(&hits));
    let mut rb = ctx.create_buffer((w * h * 4) as u64, vk::BufferUsageFlags::TRANSFER_DST, true, true).expect("readback buffer");
    {
        let tm = Tonemapper::with_views(ctx, color.view, albedo.view, color.view, position.view, w, h).expect("tonemapper");
        let (out, buf) = (tm.output().image, rb.buffer);
        ctx.submit_and_wait(|dev, cmd| {
            tm.record(cmd, exposure_ev, DisplayMode::Beauty, false, glow, background);
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
    for im in [&mut color, &mut albedo, &mut position] {
        ctx.destroy_image(im);
    }
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

/// 背景を透明にした画像（色はアルファを掛けた値）：保存の PNG（CPU）と、表示の「アルファを付ける」（GPU）が同じ画素になる。
/// 透明な所はアルファ 0、グローは物の上にだけかかる。表示の市松模様は、不透明な所では元の色のまま
#[test]
#[ignore]
fn transparent_background_on_the_display_matches_the_saved_image() {
    let _g = common::gpu_lock();
    let ctx = common::context();
    let (w, h) = (96u32, 64u32);
    // 左の 1/3 は透明（背景）、真ん中の列は半透明（縁）、残りは不透明
    let mut img = test_image(w, h);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let a = if x < w / 3 { 0.0 } else if x == w / 3 { 0.4 } else { 1.0 };
            for c in 0..3 {
                img.rgba[i + c] *= a;
            }
            img.rgba[i + 3] = a;
        }
    }
    assert!(img.has_alpha());
    for glow in [GlowParams::OFF, GlowParams::default()] {
        let gpu = gpu_tonemap_with(&ctx, &img, 0.7, &glow, Background::Alpha);
        let cpu = tonemap_rgba8(&img, 0.7, &glow);
        let max_diff = gpu.iter().zip(&cpu).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        println!("transparent, {glow:?}: max difference {max_diff} / 255");
        assert!(max_diff <= 2, "{glow:?}: GPU and CPU differ by {max_diff}");
        let px = |x: u32, y: u32| &cpu[((y * w + x) * 4) as usize..((y * w + x) * 4 + 4) as usize];
        assert_eq!(px(3, 10)[3], 0, "background is transparent");
        assert_eq!(px(w / 3, 10)[3], 102, "edge is partly transparent");
        assert_eq!(px(70, 45)[3], 255, "the board is opaque");
    }
    // 表示：透明な所は市松模様（灰色）、不透明な所はアルファを付けたときと同じ色
    let shown = gpu_tonemap_with(&ctx, &img, 0.7, &GlowParams::OFF, Background::Checker);
    let saved = gpu_tonemap_with(&ctx, &img, 0.7, &GlowParams::OFF, Background::Alpha);
    let i = ((10 * w + 3) * 4) as usize;
    assert!(shown[i] > 60 && shown[i] == shown[i + 1] && shown[i + 3] == 255, "checker: {:?}", &shown[i..i + 4]);
    let j = ((45 * w + 70) * 4) as usize;
    assert_eq!(&shown[j..j + 4], &saved[j..j + 4]);
}
