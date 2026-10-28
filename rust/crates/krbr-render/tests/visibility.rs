//! 部品の表示・非表示（GpuScene::set_visible_mask）：隠した種類のインスタンスは、最初から無いシーンと同じに描ける。
//! 光線が通り抜けるだけでなく、隠した部品の発光体も光らない（選ばない）ことを、PT・BDPT・VCM で確かめる。画像は out/vis_*.png
//!   cargo test --release -p krbr-render --test visibility -- --ignored --nocapture

mod common;

use common::*;
use glam::{Mat4, Vec3};
use krbr_core::mesh::Mesh;
use krbr_scene::camera::orbit_camera;
use krbr_scene::scene::{visibility, Instance, InstanceOwner, Scene, SceneMesh};

/// 床（基板）、光る箱（表面実装の部品）、灰色の箱（スルーホールの部品）。with_* が false ならその部品を入れない
fn scene(with_smd: bool, with_tht: bool) -> Scene {
    let materials = vec![diffuse("floor", Vec3::splat(0.6)), emitter("led", 4000.0), diffuse("block", Vec3::new(0.5, 0.5, 0.55))];
    let mut floor = Mesh::default();
    add_quad(&mut floor, Vec3::new(-100.0, 0.0, -100.0), Vec3::new(100.0, 0.0, -100.0), Vec3::new(100.0, 0.0, 100.0), Vec3::new(-100.0, 0.0, 100.0), Vec3::Y, 0);
    let mut led = Mesh::default();
    add_box(&mut led, Vec3::new(-30.0, 0.0, -5.0), Vec3::new(-20.0, 8.0, 5.0), 1);
    let mut block = Mesh::default();
    add_box(&mut block, Vec3::new(5.0, 0.0, -15.0), Vec3::new(35.0, 25.0, 15.0), 2);
    let mut s = Scene { materials, ..Default::default() };
    let add = |mesh: Mesh, class: u8, s: &mut Scene| {
        s.meshes.push(SceneMesh { name: "m".into(), mesh });
        let owner = if class == visibility::BOARD { InstanceOwner::Board } else { InstanceOwner::Placeholder { footprint: 0 } };
        s.instances.push(Instance { mesh: s.meshes.len() as u32 - 1, xform: Mat4::IDENTITY, material_base: 0, owner });
        s.instance_class.push(class);
    };
    add(floor, visibility::BOARD, &mut s);
    if with_smd {
        add(led, visibility::SMD, &mut s);
    }
    if with_tht {
        add(block, visibility::THROUGH_HOLE, &mut s);
    }
    s
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn hidden_parts_render_as_if_absent() {
    let _g = gpu_lock();
    let ctx = context();
    let env = uniform_env(0.02, 16, 8);
    let cam = orbit_camera(Vec3::new(0.0, 10.0, 0.0), 180.0, 35.0, 20.0, 40.0, 4.0 / 3.0);
    let full = scene(true, true);
    let (w, h, spp) = (160, 120, 1024);
    for (mask, name, smd, tht) in [
        (visibility::ALL & !visibility::SMD, "no_smd", false, true),
        (visibility::ALL & !visibility::THROUGH_HOLE, "no_tht", true, false),
        (visibility::BOARD | visibility::OTHER, "board_only", false, false),
    ] {
        let mut gpu = gpu_scene(&ctx, &full, &env);
        gpu.set_visible_mask(mask).expect("visible mask");
        let reference_scene = scene(smd, tht);
        let reference = gpu_scene(&ctx, &reference_scene, &env);
        for (integrator, seed) in [(IntegratorKind::Pt, 1), (IntegratorKind::Bdpt, 2), (IntegratorKind::Vcm, 3)] {
            let mut f = frame(cam, 8);
            f.seed = seed;
            let a = render(&ctx, &gpu, w, h, spp, integrator, f.clone());
            f.seed = seed + 10;
            let b = render(&ctx, &reference, w, h, spp, integrator, f.clone());
            // 同じシーンを乱数の種だけ変えて描いたときのノイズと比べる
            f.seed = seed + 20;
            let c = render(&ctx, &reference, w, h, spp, integrator, f);
            let (e, ma, mb) = relative_rmse(&a.color, &b.color, Window::ALL);
            let (noise, _, _) = relative_rmse(&b.color, &c.color, Window::ALL);
            println!("{name} {integrator:?}: mean hidden {ma:.5} vs absent {mb:.5}, rel. RMSE {e:.4} (noise {noise:.4})");
            save_png(&format!("vis_{name}_{integrator:?}.png"), &a.color, 0.0);
            assert!((ma / mb - 1.0).abs() < 0.02, "{name} {integrator:?}: mean {ma} vs {mb}");
            assert!(e < 1.5 * noise + 0.01, "{name} {integrator:?}: rel. RMSE {e} (noise {noise})");
        }
        // 全部を見せ直すと、元のシーンに戻る（CDF を送り直す）
        gpu.set_visible_mask(visibility::ALL).expect("visible mask");
        let back = render(&ctx, &gpu, w, h, 256, IntegratorKind::Pt, frame(cam, 8));
        let whole = render(&ctx, &gpu_scene(&ctx, &full, &env), w, h, 256, IntegratorKind::Pt, frame(cam, 8));
        assert_eq!(back.color.rgba, whole.color.rgba, "{name}: showing everything again");
    }
}
