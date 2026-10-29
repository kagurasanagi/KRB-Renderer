//! 分散の検証シーン：アクリル（nd 1.491、アッベ数 57）とポリカーボネート（nd 1.585、アッベ数 30）の
//! くさび形のプリズムを上下に並べ、その奥の縦縞の光源を透かして見る。分光モードでは、縞の縁が波長ごとにずれて色が分かれる。
//! 赤と青の成分の横方向のずれ（画素）を相互相関で測り、ポリカーボネートはアクリルの約 2.3 倍（(nF−nC) の比）になることを確かめる。
//! RGB モードでは分散がないので、ずれは 0。画像は out/p5_prisms_{spectral,rgb}.png
//!   cargo test --release -p krbr-render --test prism -- --ignored --nocapture

mod common;

use common::*;
use glam::{Vec2, Vec3};
use krbr_core::mesh::Mesh;
use krbr_render::ImageF;
use krbr_pbr::camera::Camera;
use krbr_scene::material_library::MaterialLibrary;

const W: u32 = 960;
const H: u32 = 600;

/// 行ごとに、赤と青の横方向のずれ（画素、小数まで）を正規化相互相関で求めて平均する
fn channel_shift(img: &ImageF, row0: i32, row1: i32, col0: i32, col1: i32) -> f64 {
    const MAX_SHIFT: i32 = 45;
    let (mut sum, mut rows) = (0.0f64, 0);
    for y in row0..row1 {
        let px = |x: i32, c: usize| img.rgba[(y as usize * img.width as usize + x as usize) * 4 + c] as f64;
        // 平均を引いてから相関をとる
        let mr = (col0..col1).map(|x| px(x, 0)).sum::<f64>() / (col1 - col0) as f64;
        let mb = (col0..col1).map(|x| px(x, 2)).sum::<f64>() / (col1 - col0) as f64;
        let corr: Vec<f64> = (-MAX_SHIFT..=MAX_SHIFT)
            .map(|s| {
                let (mut c, mut er, mut eb) = (0.0, 0.0, 0.0);
                for x in col0 + MAX_SHIFT..col1 - MAX_SHIFT {
                    let (r, b) = (px(x, 0) - mr, px(x + s, 2) - mb);
                    c += r * b;
                    er += r * r;
                    eb += b * b;
                }
                c / (er * eb).max(1e-30).sqrt()
            })
            .collect();
        let mut best = 0;
        for i in 1..corr.len() {
            if corr[i] > corr[best] {
                best = i;
            }
        }
        let mut sub = best as f64 - MAX_SHIFT as f64;
        if best > 0 && best + 1 < corr.len() {
            let (a, b, c) = (corr[best - 1], corr[best], corr[best + 1]);
            let d = a - 2.0 * b + c;
            if d < 0.0 {
                sub += 0.5 * (a - c) / d;
            }
        }
        sum += sub;
        rows += 1;
    }
    if rows > 0 { sum / rows as f64 } else { 0.0 }
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn prism_dispersion_acrylic_vs_polycarbonate() {
    let _g = gpu_lock();
    let ctx = context();
    let lib = MaterialLibrary::builtin();
    let materials = vec![emitter("slit_light", 1000.0), lib.find("acrylic").unwrap().clone(), lib.find("polycarbonate").unwrap().clone()]; // 縞の放射輝度は 1

    let mut mesh = Mesh::default();
    // 遠くの縦縞の光源（幅 18 mm、間隔 72 mm。画面上で約 100 画素ごと）
    let z_bars = -4000.0;
    let mut x = -1500.0f32;
    while x < 1500.0 {
        let n = Vec3::Z;
        let (i, j) = (mesh.add_vertex(Vec3::new(x, -600.0, z_bars), n), mesh.add_vertex(Vec3::new(x + 18.0, -600.0, z_bars), n));
        let (k, l) = (mesh.add_vertex(Vec3::new(x + 18.0, 600.0, z_bars), n), mesh.add_vertex(Vec3::new(x, 600.0, z_bars), n));
        mesh.add_triangle(i, j, k, 0);
        mesh.add_triangle(i, k, l, 0);
        x += 72.0;
    }
    // くさび形のプリズム（頂角 12°、手前の面は z = 0）。上がアクリル、下がポリカーボネート。
    // 頂角が小さい（薄いプリズム）ので、色の分かれ方は (nF − nC) にほぼ比例し、形の違いの影響が小さい
    let (w, t0, tan_a) = (30.0f32, 1.0f32, 12.0f32.to_radians().tan());
    let wedge = [Vec2::new(-w / 2.0, 0.0), Vec2::new(w / 2.0, 0.0), Vec2::new(w / 2.0, -(t0 + w * tan_a)), Vec2::new(-w / 2.0, -t0)];
    add_prism(&mut mesh, &wedge, 2.0, 14.0, 1);
    add_prism(&mut mesh, &wedge, -14.0, -2.0, 2);
    let s = scene_of(materials, mesh);
    let gpu = gpu_scene(&ctx, &s, &uniform_env(0.0, 16, 8)); // 真っ暗な環境（光源は縞だけ）

    // z = 0 で上下 ±20 mm（狭い画角で角度の分解能を上げる）
    let cam = Camera { position: Vec3::new(0.0, 0.0, 400.0), forward: Vec3::NEG_Z, right: Vec3::X, up: Vec3::Y, tan_half_fov_y: 20.0 / 400.0, aspect: 1.6, ..Default::default() };
    // 画面上のプリズムの範囲（z = 0 の面で）：列は x = ±11 mm、行は各プリズムの中央 ±4 mm
    let col = |x: f32| ((x / (20.0 * cam.aspect) + 1.0) * 0.5 * W as f32) as i32;
    let row = |y: f32| ((-y / 20.0 + 1.0) * 0.5 * H as f32) as i32;
    let (c0, c1) = (col(-11.0), col(11.0));

    let mut result = [(0.0f64, 0.0f64); 2]; // (アクリル, ポリカーボネート)
    for (mode, r) in result.iter_mut().enumerate() {
        let mut f = frame(cam, 16);
        f.spectral = mode == 1;
        let out = render(&ctx, &gpu, W, H, 256, IntegratorKind::Pt, f);
        save_png(if mode == 1 { "p5_prisms_spectral.png" } else { "p5_prisms_rgb.png" }, &out.color, 0.0);
        *r = (channel_shift(&out.color, row(12.0), row(4.0), c0, c1), channel_shift(&out.color, row(-4.0), row(-12.0), c0, c1));
    }
    let (ra, rp) = (result[1].0.abs(), result[1].1.abs());
    println!(
        "prism red/blue shift (px): RGB acrylic {:.2}, polycarbonate {:.2}; spectral acrylic {:.2}, polycarbonate {:.2} (ratio {:.2}, expected ~{:.2})",
        result[0].0, result[0].1, result[1].0, result[1].1, rp / ra.max(1e-6), (0.585 / 30.0) / (0.491 / 57.0)
    );
    // RGB モードでは分散がない
    assert!(result[0].0.abs() < 0.3 && result[0].1.abs() < 0.3, "{result:?}");
    // 分光モードでは色が分かれ、ポリカーボネートの方が大きく分かれる
    assert!(ra > 3.0, "{ra}");
    assert!(rp < 40.0, "{rp}"); // 相関の探索範囲（45）に張り付いていない
    assert!(rp / ra > 1.9 && rp / ra < 2.6, "{}", rp / ra);
}
