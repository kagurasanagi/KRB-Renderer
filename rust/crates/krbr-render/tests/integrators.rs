//! 積分器の一致テストと、集光模様の収束
//!  - 一致：面光源・相互反射・光沢のあるシーンを PT / BDPT / VCM で描き、平均と画素ごとの差が小さいこと
//!  - 集光模様：透明なふた（アクリル板）越しに、ボールレンズが LED の光を床に集める様子を見る。
//!    カメラ → 板（鏡面）→ 床（拡散）→ レンズ（鏡面）→ LED の経路は、PT・BDPT では極めて収束しにくく、VCM では収束する。
//!    同じ条件で 2 回（乱数の種を変えて）描き、その差（ノイズ）を比べる。画像は out/p6_*.png
//!   cargo test --release -p krbr-render --test integrators -- --ignored --nocapture

mod common;

use common::*;
use glam::Vec3;
use krbr_core::mesh::Mesh;
use krbr_render::{Context, GpuScene, RenderOutput};
use krbr_scene::camera::{orbit_camera, Camera};
use krbr_scene::material::Material;
use krbr_scene::material_library::MaterialLibrary;

fn render_with(ctx: &Context, gpu: &GpuScene<'_>, cam: Camera, w: u32, h: u32, spp: u32, integrator: IntegratorKind, seed: u32) -> RenderOutput {
    let mut f = frame(cam, 12);
    f.rr_start_depth = 12;
    f.seed = seed;
    render(ctx, gpu, w, h, spp, integrator, f)
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn integrators_agree() {
    let _g = gpu_lock();
    let ctx = context();
    let metal = Material { name: "metal".into(), base_color: Vec3::new(0.9, 0.8, 0.6), base_metalness: 1.0, specular_roughness: 0.3, ..Default::default() };
    let materials = vec![diffuse("floor", Vec3::splat(0.6)), diffuse("wall", Vec3::new(0.5, 0.55, 0.6)), diffuse("box", Vec3::new(0.7, 0.3, 0.2)), emitter("light", 20000.0), metal];
    let mut m = Mesh::default();
    add_quad(&mut m, Vec3::new(-100.0, 0.0, -100.0), Vec3::new(100.0, 0.0, -100.0), Vec3::new(100.0, 0.0, 100.0), Vec3::new(-100.0, 0.0, 100.0), Vec3::Y, 0);
    add_quad(&mut m, Vec3::new(-100.0, 0.0, -40.0), Vec3::new(100.0, 0.0, -40.0), Vec3::new(100.0, 120.0, -40.0), Vec3::new(-100.0, 120.0, -40.0), Vec3::Z, 1);
    add_box(&mut m, Vec3::new(-35.0, 0.0, -20.0), Vec3::new(-5.0, 30.0, 10.0), 2);
    add_sphere(&mut m, Vec3::new(25.0, 15.0, 0.0), 15.0, 4, 96, 48);
    add_quad(&mut m, Vec3::new(-10.0, 70.0, -10.0), Vec3::new(10.0, 70.0, -10.0), Vec3::new(10.0, 70.0, 10.0), Vec3::new(-10.0, 70.0, 10.0), Vec3::NEG_Y, 3);
    let s = scene_of(materials, m);
    let gpu = gpu_scene(&ctx, &s, &uniform_env(0.05, 16, 8));
    let cam = orbit_camera(Vec3::new(0.0, 25.0, 0.0), 220.0, 25.0, 15.0, 40.0, 4.0 / 3.0);

    let pt = render_with(&ctx, &gpu, cam, 160, 120, 2048, IntegratorKind::Pt, 1);
    let bdpt = render_with(&ctx, &gpu, cam, 160, 120, 1024, IntegratorKind::Bdpt, 2);
    let vcm = render_with(&ctx, &gpu, cam, 160, 120, 1024, IntegratorKind::Vcm, 3);
    let pt2 = render_with(&ctx, &gpu, cam, 160, 120, 2048, IntegratorKind::Pt, 4);

    let (e_pt, m_pt, _) = relative_rmse(&pt.color, &pt2.color, Window::ALL);
    let (e_b, _, m_b) = relative_rmse(&pt.color, &bdpt.color, Window::ALL);
    let (e_v, _, m_v) = relative_rmse(&pt.color, &vcm.color, Window::ALL);
    println!("integrator agreement: mean pt {m_pt:.5}, bdpt {m_b:.5}, vcm {m_v:.5}; rel. RMSE vs pt: pt(seed 4) {e_pt:.4}, bdpt {e_b:.4}, vcm {e_v:.4}");
    save_png("p6_agree_pt.png", &pt.color, 0.0);
    save_png("p6_agree_bdpt.png", &bdpt.color, 0.0);
    save_png("p6_agree_vcm.png", &vcm.color, 0.0);
    assert!((m_b / m_pt - 1.0).abs() < 0.01, "bdpt mean {m_b} vs pt {m_pt}");
    assert!((m_v / m_pt - 1.0).abs() < 0.02, "vcm mean {m_v} vs pt {m_pt}");
    // 画素ごとの差は、PT どうし（乱数の種だけ違う）のノイズの大きさと同程度
    assert!(e_b < 2.0 * e_pt + 0.01, "bdpt rel. RMSE {e_b}");
    assert!(e_v < 2.0 * e_pt + 0.02, "vcm rel. RMSE {e_v}");
}

const CW: u32 = 320;
const CH: u32 = 240;
/// 集光点（床の原点）のまわりの 48 x 48 画素で比べる（原点は画面の中央に写る）
const CAUSTIC_WIN: Window = Window { x0: CW as i32 / 2 - 24, y0: CH as i32 / 2 - 24, x1: CW as i32 / 2 + 24, y1: CH as i32 / 2 + 24 };

fn caustic_scene(ctx: &Context) -> (GpuScene<'_>, Camera) {
    let lib = MaterialLibrary::builtin();
    let acrylic = lib.find("acrylic").expect("acrylic").clone();
    let materials = vec![diffuse("board", Vec3::new(0.15, 0.45, 0.2)), emitter("led", 2.0e6), acrylic.clone(), acrylic];
    // ボールレンズ（半径 12、中心の高さ 30、焦点距離 nR / 2(n−1) ≈ 18 mm）が、高さ 76 の LED の像を床に結ぶ
    let mut m = Mesh::default();
    add_quad(&mut m, Vec3::new(-150.0, 0.0, -150.0), Vec3::new(150.0, 0.0, -150.0), Vec3::new(150.0, 0.0, 150.0), Vec3::new(-150.0, 0.0, 150.0), Vec3::Y, 0);
    add_quad(&mut m, Vec3::new(-1.5, 76.0, -1.5), Vec3::new(1.5, 76.0, -1.5), Vec3::new(1.5, 76.0, 1.5), Vec3::new(-1.5, 76.0, 1.5), Vec3::NEG_Y, 1);
    add_sphere(&mut m, Vec3::new(0.0, 30.0, 0.0), 12.0, 2, 96, 48); // ボールレンズ
    add_box(&mut m, Vec3::new(-160.0, 90.0, -160.0), Vec3::new(160.0, 94.0, 160.0), 3); // 筐体のふた（アクリル板）
    let s = scene_of(materials, m);
    let gpu = gpu_scene(ctx, &s, &uniform_env(0.0, 16, 8));
    (gpu, orbit_camera(Vec3::ZERO, 230.0, 50.0, 0.0, 40.0, CW as f32 / CH as f32))
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn caustics_through_a_cover_converge_with_vcm() {
    let _g = gpu_lock();
    let ctx = context();
    let (gpu, cam) = caustic_scene(&ctx);
    let mut noise = Vec::new();
    for integ in [IntegratorKind::Pt, IntegratorKind::Bdpt, IntegratorKind::Vcm] {
        let a = render_with(&ctx, &gpu, cam, CW, CH, 256, integ, 11);
        let b = render_with(&ctx, &gpu, cam, CW, CH, 256, integ, 12);
        let (e, ma, mb) = relative_rmse(&a.color, &b.color, CAUSTIC_WIN);
        println!("caustic scene, {integ:<4}: noise around the caustic (rel. RMSE between two seeds) {e:.3}, mean {:.4}, {:.2} s for 256 spp", 0.5 * (ma + mb), 0.5 * (a.seconds + b.seconds));
        save_png(&format!("p6_caustic_{integ}.png"), &a.color, 0.0);
        noise.push(e);
    }
    // VCM は PT・BDPT よりはっきりノイズが少ない（収束する）
    assert!(noise[2] < 0.5 * noise[0], "{noise:?}");
    assert!(noise[2] < 0.5 * noise[1], "{noise:?}");
    assert!(noise[2] < 0.15, "{noise:?}");
}

/// 正しい値に収束していること：十分なサンプル数の PT と、集光点のまわりの平均を比べる（VCM の偏りは半径の分だけ）。
/// PT 16384 spp で時間がかかるので分けてある
#[test]
#[ignore = "needs a Vulkan GPU with ray tracing (slow: 16384 spp reference)"]
fn caustics_vcm_matches_a_pt_reference() {
    let _g = gpu_lock();
    let ctx = context();
    let (gpu, cam) = caustic_scene(&ctx);
    let reference = render_with(&ctx, &gpu, cam, CW, CH, 16384, IntegratorKind::Pt, 21);
    let vcm = render_with(&ctx, &gpu, cam, CW, CH, 1024, IntegratorKind::Vcm, 22);
    let (_, m_ref, m_vcm) = relative_rmse(&reference.color, &vcm.color, CAUSTIC_WIN);
    save_png("p6_caustic_vcm_1024.png", &vcm.color, 0.0);
    save_png("p6_caustic_pt_16384.png", &reference.color, 0.0);
    println!("caustic scene: mean around the caustic, pt 16384 spp {m_ref:.4}, vcm 1024 spp {m_vcm:.4}");
    assert!((m_vcm / m_ref - 1.0).abs() < 0.05);
}
