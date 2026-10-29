//! 背景を透明にする（FrameParams::transparent_background）：カメラの光線が何にも当たらない画素は色 0・アルファ 0、
//! 物の上はアルファ 1 で、色は背景を描いたときと同じ（環境光は照明と映り込みにはそのまま使う）。縁は半透明。PT・BDPT・VCM。
//! 画像は out/bg_*.png（透明な PNG）
//!   cargo test --release -p krbr-render --test background -- --ignored --nocapture

mod common;

use common::*;
use glam::Vec3;
use krbr_core::mesh::Mesh;
use krbr_render::image_io::tonemap_rgba8;
use krbr_render::ImageF;
use krbr_pbr::camera::orbit_camera;

/// 真ん中に浮かべた灰色の板と箱（画面の四隅には何も無い）
fn scene() -> krbr_pbr::RenderScene {
    let mut m = Mesh::default();
    add_quad(&mut m, Vec3::new(-30.0, 0.0, -20.0), Vec3::new(30.0, 0.0, -20.0), Vec3::new(30.0, 0.0, 20.0), Vec3::new(-30.0, 0.0, 20.0), Vec3::Y, 0);
    add_box(&mut m, Vec3::new(-8.0, 0.0, -8.0), Vec3::new(8.0, 10.0, 8.0), 1);
    scene_of(vec![diffuse("board", Vec3::splat(0.6)), diffuse("part", Vec3::new(0.2, 0.3, 0.6))], m)
}

fn alpha(img: &ImageF, x: u32, y: u32) -> f32 {
    img.rgba[((y * img.width + x) * 4 + 3) as usize]
}

fn rgb(img: &ImageF, x: u32, y: u32) -> [f32; 3] {
    let i = ((y * img.width + x) * 4) as usize;
    [img.rgba[i], img.rgba[i + 1], img.rgba[i + 2]]
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn transparent_background_keeps_the_object_and_drops_the_environment() {
    let _g = gpu_lock();
    let ctx = context();
    let env = uniform_env(0.8, 16, 8);
    let gpu = gpu_scene(&ctx, &scene(), &env);
    let cam = orbit_camera(Vec3::new(0.0, 3.0, 0.0), 90.0, 40.0, 20.0, 40.0, 4.0 / 3.0);
    let (w, h, spp) = (160, 120, 512);
    for (integrator, seed) in [(IntegratorKind::Pt, 1), (IntegratorKind::Bdpt, 2), (IntegratorKind::Vcm, 3)] {
        let mut f = frame(cam, 8);
        f.seed = seed;
        let opaque = render(&ctx, &gpu, w, h, spp, integrator, f.clone()).color;
        f.transparent_background = true;
        let clear = render(&ctx, &gpu, w, h, spp, integrator, f.clone()).color;
        f.seed = seed + 10;
        let clear2 = render(&ctx, &gpu, w, h, spp, integrator, f).color;
        save_png(&format!("bg_{integrator:?}.png"), &clear, 0.0);

        // 背景を描くと、どこもアルファ 1 で、四隅は環境光の色
        assert!(!opaque.has_alpha(), "{integrator:?}: opaque background has alpha");
        assert!((rgb(&opaque, 0, 0)[0] - 0.8).abs() < 1e-3, "{integrator:?}: corner {:?}", rgb(&opaque, 0, 0));
        // 透明：四隅は色もアルファも 0、物の真ん中はアルファ 1、縁に半透明の画素がある
        for (x, y) in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
            assert_eq!(alpha(&clear, x, y), 0.0, "{integrator:?}: corner ({x}, {y})");
            assert_eq!(rgb(&clear, x, y), [0.0; 3], "{integrator:?}: corner ({x}, {y})");
        }
        assert_eq!(alpha(&clear, w / 2, h / 2), 1.0, "{integrator:?}: center");
        let partial = clear.rgba.chunks_exact(4).filter(|p| p[3] > 0.0 && p[3] < 1.0).count();
        assert!(partial > 20, "{integrator:?}: {partial} anti-aliased edge pixels");

        // 物の上（アルファ 1）の色は、背景を描いたときと同じ（同じ乱数の種なので、違いはアルファ 1 の画素で 0）。
        // 種を変えた透明の画像との差（ノイズ）が、相手の明るさに比べて小さいことも見る
        let (mut same, mut n, mut noise, mut sum) = (0usize, 0usize, 0f64, 0f64);
        for i in 0..(w * h) as usize {
            if clear.rgba[i * 4 + 3] < 1.0 {
                continue;
            }
            n += 1;
            if (0..3).all(|c| (clear.rgba[i * 4 + c] - opaque.rgba[i * 4 + c]).abs() <= 1e-5 * opaque.rgba[i * 4 + c].abs().max(1.0)) {
                same += 1;
            }
            noise += (clear.rgba[i * 4] - clear2.rgba[i * 4]).abs() as f64;
            sum += clear.rgba[i * 4] as f64;
        }
        println!("{integrator:?}: {n} opaque pixels, {same} identical to the opaque render, {partial} edge pixels, relative noise {:.4}", noise / sum);
        assert!(n > (w * h / 5) as usize, "{integrator:?}: {n} opaque pixels");
        assert_eq!(same, n, "{integrator:?}: the object looks different with a transparent background");

        // 保存の PNG：アルファを付け、四隅は 0、真ん中は 255
        let px = tonemap_rgba8(&clear, 0.0, &krbr_render::glow::GlowParams::default());
        assert_eq!(px[3], 0);
        assert_eq!(px[(((h / 2) * w + w / 2) * 4 + 3) as usize], 255);
    }
}
