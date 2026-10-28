//! フロー（krbr_render::flow）：前のコマの画像をフローでずらすと、今のコマの画像になる（物は動かず、カメラだけが動く）
mod common;

use common::*;
use glam::Vec3;
use krbr_core::mesh::Mesh;
use krbr_render::{flow, Progressive};
use krbr_scene::material::Material;
use krbr_scene::{orbit_camera, IntegratorKind};

const W: u32 = 192;
const H: u32 = 128;

/// 画像の (x, y)（画素の中心が +0.5）の値を、まわりの 4 画素から補間する（チャンネル c）
fn bilinear(img: &[f32], x: f32, y: f32, c: usize) -> Option<f32> {
    let (fx, fy) = (x - 0.5, y - 0.5);
    let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
    if x0 < 0 || y0 < 0 || x0 + 1 >= W as i32 || y0 + 1 >= H as i32 {
        return None;
    }
    let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
    let v = |xx: i32, yy: i32| img[((yy as u32 * W + xx as u32) * 4) as usize + c];
    Some((v(x0, y0) * (1.0 - tx) + v(x0 + 1, y0) * tx) * (1.0 - ty) + (v(x0, y0 + 1) * (1.0 - tx) + v(x0 + 1, y0 + 1) * tx) * ty)
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn previous_frame_warped_by_the_flow_matches_the_current_frame() {
    let _g = gpu_lock();
    let ctx = context();
    // 市松模様の床と、箱と球（模様が無いと、ずらしても差が出ない）
    let mut m = Mesh::default();
    for j in -6i32..6 {
        for i in -8i32..8 {
            let (x0, z0) = (i as f32 * 10.0, j as f32 * 10.0);
            add_quad(&mut m, Vec3::new(x0, 0.0, z0 + 10.0), Vec3::new(x0 + 10.0, 0.0, z0 + 10.0), Vec3::new(x0 + 10.0, 0.0, z0), Vec3::new(x0, 0.0, z0), Vec3::Y, ((i + j) & 1) as u32);
        }
    }
    add_box(&mut m, Vec3::new(-15.0, 0.0, -10.0), Vec3::new(5.0, 12.0, 8.0), 2);
    add_sphere(&mut m, Vec3::new(20.0, 8.0, 5.0), 8.0, 0, 32, 16);
    let mats: Vec<Material> = vec![diffuse("white", Vec3::splat(0.8)), diffuse("dark", Vec3::splat(0.1)), diffuse("red", Vec3::new(0.7, 0.1, 0.1))];
    let s = scene_of(mats, m);
    let gpu = gpu_scene(&ctx, &s, &uniform_env(1.0, 16, 8));
    let aspect = W as f32 / H as f32;
    let prev_cam = orbit_camera(Vec3::new(0.0, 4.0, 0.0), 140.0, 35.0, 20.0, 35.0, aspect);
    let cur_cam = orbit_camera(Vec3::new(0.0, 4.0, 0.0), 140.0, 35.0, 22.0, 35.0, aspect);
    let shoot = |cam| {
        let mut p = Progressive::new(&ctx, &gpu, IntegratorKind::Pt, W, H, frame(cam, 4)).unwrap();
        for _ in 0..256 {
            ctx.submit_and_wait(|_, cmd| p.record(cmd, 1)).unwrap();
        }
        let color = p.readback().unwrap().color;
        let pos = p.readback_position().unwrap();
        (color, pos)
    };
    let (prev, _) = shoot(prev_cam);
    let (cur, pos) = shoot(cur_cam);
    let fl = flow::backward_flow(&pos, W, H, &flow::FlowCamera::pinhole(cur_cam), &flow::FlowCamera::pinhole(prev_cam));
    // 今のコマの各画素を、前のコマの画像のフローの先の値と比べる（ずらさない場合と比べて、ずっと小さい）
    let (mut warped, mut plain, mut n) = (0.0f64, 0.0f64, 0.0f64);
    let mut moved = 0.0f64;
    for y in 2..H - 2 {
        for x in 2..W - 2 {
            let i = (y * W + x) as usize;
            let (px, py) = (x as f32 + 0.5 + fl[i][0], y as f32 + 0.5 + fl[i][1]);
            let Some(a) = bilinear(&prev.rgba, px, py, 1) else { continue };
            let c = cur.rgba[i * 4 + 1];
            let b = prev.rgba[i * 4 + 1];
            warped += ((a - c) as f64).powi(2);
            plain += ((b - c) as f64).powi(2);
            moved += ((fl[i][0] * fl[i][0] + fl[i][1] * fl[i][1]) as f64).sqrt();
            n += 1.0;
        }
    }
    let (warped, plain, moved) = ((warped / n).sqrt(), (plain / n).sqrt(), moved / n);
    println!("flow: mean motion {moved:.2} px; RMSE warped {warped:.4}, not warped {plain:.4}");
    assert!(moved > 1.0, "the camera should move the image ({moved})");
    assert!(warped < 0.35 * plain, "warped {warped} vs {plain}");
}
