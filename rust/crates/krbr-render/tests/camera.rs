//! カメラ：点光源をぼかしたときのボケの形、画面の端のキャッツアイ、色収差による縁の色のにじみ、
//! 周辺減光、薄レンズでの積分器の一致、実レンズの明るさとピント。画像は out/p7_*.png
//!   cargo test --release -p krbr-render --test camera -- --ignored --nocapture

mod common;

use common::*;
use glam::{Vec2, Vec3};
use krbr_core::mesh::Mesh;
use krbr_render::{Context, ImageF, RenderOutput};
use krbr_pbr::camera::Camera;
use krbr_pbr::lens::{LensModel, LensParams};
use krbr_pbr::material::Material;

const W: u32 = 640;
const H: u32 = 400;
const TAN_HALF: f32 = 0.25;
const ASPECT: f32 = 1.6;
const LIGHT_Z: f32 = -1000.0;

fn test_camera() -> Camera {
    Camera { position: Vec3::ZERO, forward: Vec3::NEG_Z, right: Vec3::X, up: Vec3::Y, tan_half_fov_y: TAN_HALF, aspect: ASPECT, distance: 250.0, ..Default::default() }
}

/// 画面の正規化座標（-1..1、y は下向き）に写る、z = LIGHT_Z の面の点
fn point_at(ndc: Vec2) -> Vec3 {
    Vec3::new(ndc.x * -LIGHT_Z * TAN_HALF * ASPECT, -ndc.y * -LIGHT_Z * TAN_HALF, LIGHT_Z)
}
fn pixel_of(ndc_x: f32, ndc_y: f32) -> Vec2 {
    Vec2::new((ndc_x + 1.0) * 0.5 * W as f32, (ndc_y + 1.0) * 0.5 * H as f32)
}

fn thin_lens(f_number: f32, focus: f32) -> LensParams {
    LensParams { model: LensModel::Thin, focal_length_mm: 50.0, f_number, focus_distance_mm: focus, ..Default::default() }
}

/// 小さな正方形の光源（放射輝度 1）を、画面の正規化座標 ndc の位置に置いて描く
fn render_lights(ctx: &Context, ndc: &[Vec2], size_mm: f32, lens: &LensParams, spectral: bool, spp: u32, integrator: IntegratorKind) -> RenderOutput {
    let mut m = Mesh::default();
    let h = 0.5 * size_mm;
    for &n in ndc {
        let c = point_at(n);
        add_quad(&mut m, c + Vec3::new(-h, -h, 0.0), c + Vec3::new(h, -h, 0.0), c + Vec3::new(h, h, 0.0), c + Vec3::new(-h, h, 0.0), Vec3::Z, 0);
    }
    let s = scene_of(vec![emitter("light", 1000.0)], m);
    let gpu = gpu_scene(ctx, &s, &uniform_env(0.0, 16, 8));
    let mut f = frame(test_camera(), 4);
    f.lens = lens.clone();
    f.spectral = spectral;
    render(ctx, &gpu, W, H, spp, integrator, f)
}

/// 一様な環境光（放射輝度 1）だけを見る。後ろに写らない小さな板を置く
fn render_uniform_env(ctx: &Context, lens: &LensParams, spp: u32) -> RenderOutput {
    let mut m = Mesh::default();
    add_quad(&mut m, Vec3::new(-1.0, -1.0, 5000.0), Vec3::new(1.0, -1.0, 5000.0), Vec3::new(1.0, 1.0, 5000.0), Vec3::new(-1.0, 1.0, 5000.0), Vec3::NEG_Z, 0);
    let s = scene_of(vec![Material::default()], m);
    let gpu = gpu_scene(ctx, &s, &uniform_env(1.0, 16, 8));
    let f = krbr_render::FrameParams { camera: test_camera(), lens: lens.clone(), ..Default::default() };
    render(ctx, &gpu, W, H, spp, IntegratorKind::Pt, f)
}

/// チャンネル k（3 なら輝度）の値（3x3 の平均でならす）
fn value_at(img: &ImageF, x: i32, y: i32, k: usize) -> f32 {
    let (mut s, mut n) = (0.0f32, 0);
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (xx, yy) = (x + dx, y + dy);
            if xx < 0 || yy < 0 || xx >= img.width as i32 || yy >= img.height as i32 {
                continue;
            }
            let p = &img.rgba[(yy as usize * img.width as usize + xx as usize) * 4..];
            s += if k < 3 { p[k] } else { (p[0] + p[1] + p[2]) / 3.0 };
            n += 1;
        }
    }
    if n > 0 { s / n as f32 } else { 0.0 }
}

struct Bokeh {
    centroid: Vec2,
    /// 1° ごとの、重心から縁までの距離
    radius: Vec<f32>,
    /// 画面中心から重心への向きと、それに直交する向きの分散
    var_along: f64,
    var_across: f64,
    mean_radius: f64,
}

/// center のまわり win 画素の範囲で、明るさの半分を境にボケの形を測る
fn measure_bokeh(img: &ImageF, center: Vec2, win: i32, k: usize) -> Bokeh {
    let (cx, cy) = (center.x as i32, center.y as i32);
    let pixels = || (cy - win..=cy + win).flat_map(move |y| (cx - win..=cx + win).map(move |x| (x, y)));
    let peak = pixels().map(|(x, y)| value_at(img, x, y, k)).fold(0.0f32, f32::max);
    // 内側の平均（最大値の 30% を超える画素）を基準にして、その半分を境とする
    let (mut sum_in, mut n_in) = (0.0f64, 0);
    for (x, y) in pixels() {
        let v = value_at(img, x, y, k);
        if v > 0.3 * peak {
            sum_in += v as f64;
            n_in += 1;
        }
    }
    let thr = (0.5 * sum_in / n_in.max(1) as f64) as f32;
    let pts: Vec<Vec2> = pixels().filter(|&(x, y)| value_at(img, x, y, k) > thr).map(|(x, y)| Vec2::new(x as f32 + 0.5, y as f32 + 0.5)).collect();
    let centroid = if pts.is_empty() {
        center
    } else {
        let (sx, sy) = pts.iter().fold((0.0f64, 0.0f64), |(sx, sy), p| (sx + p.x as f64, sy + p.y as f64));
        Vec2::new((sx / pts.len() as f64) as f32, (sy / pts.len() as f64) as f32)
    };
    let mut along = centroid - Vec2::new(W as f32 * 0.5, H as f32 * 0.5);
    along = if along.length() > 1.0 { along.normalize() } else { Vec2::X };
    let across = Vec2::new(-along.y, along.x);
    let (mut var_along, mut var_across) = (0.0f64, 0.0f64);
    for p in &pts {
        let d = *p - centroid;
        var_along += (d.dot(along) * d.dot(along)) as f64;
        var_across += (d.dot(across) * d.dot(across)) as f64;
    }
    let mut radius = Vec::with_capacity(360);
    let mut mean_radius = 0.0f64;
    for a in 0..360 {
        let th = (a as f32).to_radians();
        let dir = Vec2::new(th.cos(), -th.sin()); // 画面の y は下向きなので、角度は上向きを正にする
        let (mut r, mut miss, mut t) = (0.0f32, 0, 0.0f32);
        while t < win as f32 {
            let p = centroid + dir * t;
            if value_at(img, p.x as i32, p.y as i32, k) > thr {
                r = t;
                miss = 0;
            } else {
                miss += 1;
                if miss > 4 {
                    break;
                }
            }
            t += 0.5;
        }
        radius.push(r);
        mean_radius += r as f64 / 360.0;
    }
    Bokeh { centroid, radius, var_along, var_across, mean_radius }
}

/// 角度 a0 から step ごとの方向の、半径の平均（±3° の平均）
fn radius_at(b: &Bokeh, a0: f64, step: f64, count: i32) -> f64 {
    let (mut s, mut n) = (0.0f64, 0);
    for i in 0..count {
        for d in -3..=3 {
            let a = ((a0 + step * i as f64).round() as i32 + d).rem_euclid(360);
            s += b.radius[a as usize] as f64;
            n += 1;
        }
    }
    s / n as f64
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn thin_lens_bokeh_aperture_blades() {
    let _g = gpu_lock();
    let ctx = context();
    // 絞り F2（半径 12.5 mm）、ピント 250 mm、光源 1000 mm：ボケの直径は約 2a(1/250 − 1/1000) = 0.075（tan）≈ 60 画素
    for (blades, rotation, file) in [(0, 0.0f32, "p7_bokeh_circle.png"), (6, 0.0, "p7_bokeh_hex.png"), (6, 30.0, "p7_bokeh_hex_rot30.png")] {
        let lens = LensParams { blades, blade_rotation_deg: rotation, ..thin_lens(2.0, 250.0) };
        let out = render_lights(&ctx, &[Vec2::ZERO], 4.0, &lens, false, 2048, IntegratorKind::Pt);
        save_png(file, &out.color, 5.0);
        let b = measure_bokeh(&out.color, pixel_of(0.0, 0.0), 60, 3);
        let r_max = b.radius.iter().copied().fold(f32::MIN, f32::max) as f64;
        let r_min = b.radius.iter().copied().fold(f32::MAX, f32::min) as f64;
        // 頂点の方向（回転の角度から 60° ごと）と、辺の中央の方向（さらに 30° ずれる）の半径
        let (r_vertex, r_edge) = (radius_at(&b, rotation as f64, 60.0, 6), radius_at(&b, rotation as f64 + 30.0, 60.0, 6));
        println!("bokeh blades {blades} rot {rotation:.0}: mean radius {:.1} px, max/min {:.3}, vertex/edge {:.3} (hexagon 1.155)", b.mean_radius, r_max / r_min, r_vertex / r_edge);
        assert!(b.mean_radius > 22.0 && b.mean_radius < 40.0, "{}", b.mean_radius);
        let ratio = r_vertex / r_edge;
        if blades == 0 {
            assert!((ratio - 1.0).abs() < 0.03, "{ratio}");
        } else {
            assert!(ratio > 1.08 && ratio < 1.20, "{ratio}");
        }
    }
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn thin_lens_cats_eye_and_natural_vignetting() {
    let _g = gpu_lock();
    let ctx = context();
    let mut ratio = [0.0f64; 2];
    for (i, r) in ratio.iter_mut().enumerate() {
        let lens = LensParams { optical_vignetting: if i == 0 { 0.0 } else { 0.6 }, ..thin_lens(2.0, 250.0) };
        let out = render_lights(&ctx, &[Vec2::new(0.8, 0.75)], 4.0, &lens, false, 2048, IntegratorKind::Pt);
        if i == 1 {
            save_png("p7_cats_eye.png", &out.color, 5.0);
        }
        let b = measure_bokeh(&out.color, pixel_of(0.8, 0.75), 60, 3);
        *r = (b.var_along / b.var_across).sqrt();
        println!("corner bokeh, optical vignetting {:.1}: radial / tangential extent {:.3}", lens.optical_vignetting, *r);
    }
    assert!((ratio[0] - 1.0).abs() < 0.05, "{ratio:?}");
    assert!(ratio[1] < 0.8, "{ratio:?}"); // 画面中心の向きにつぶれたレモン形

    // 自然な周辺減光：一様な環境光を見ると、隅は cos⁴θ だけ暗くなる
    let out = render_uniform_env(&ctx, &LensParams { natural_vignetting: true, ..Default::default() }, 16);
    let (center, corner) = (value_at(&out.color, W as i32 / 2, H as i32 / 2, 3), value_at(&out.color, 1, 1, 3));
    let tan_corner = TAN_HALF as f64 * (1.0 + (ASPECT * ASPECT) as f64).sqrt() * (1.0 - 2.0 / H as f64);
    let expected = (1.0 / (1.0 + tan_corner * tan_corner)).powi(2);
    let got = (corner / center) as f64;
    println!("natural vignetting: corner / center {got:.4} (cos^4 {expected:.4})");
    assert!((got / expected - 1.0).abs() < 0.02, "{got} vs {expected}");
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn thin_lens_chromatic_aberration_in_spectral_mode() {
    let _g = gpu_lock();
    let ctx = context();
    // 軸上色収差：ぼかした点光源のボケの大きさが、青（短い波長でピントが手前）で大きく、赤で小さい
    let mut ratio = [0.0f64; 2];
    for (i, r) in ratio.iter_mut().enumerate() {
        let lens = LensParams { abbe_number: if i == 0 { 0.0 } else { 20.0 }, ..thin_lens(2.0, 250.0) };
        let out = render_lights(&ctx, &[Vec2::ZERO], 4.0, &lens, true, 4096, IntegratorKind::Pt);
        if i == 1 {
            save_png("p7_axial_ca.png", &out.color, 5.0);
        }
        let (red, blue) = (measure_bokeh(&out.color, pixel_of(0.0, 0.0), 70, 0), measure_bokeh(&out.color, pixel_of(0.0, 0.0), 70, 2));
        *r = blue.mean_radius / red.mean_radius;
        println!("axial CA (Abbe {:.0}): bokeh radius blue {:.1} px, red {:.1} px, ratio {:.3}", lens.abbe_number, blue.mean_radius, red.mean_radius, *r);
    }
    assert!((ratio[0] - 1.0).abs() < 0.03, "{ratio:?}");
    assert!(ratio[1] > 1.05, "{ratio:?}");

    // 倍率色収差：ピントの合った点光源（画面の端）で、青の像が赤より外側にずれる
    let mut shift = [0.0f64; 2];
    for (i, s) in shift.iter_mut().enumerate() {
        let lens = LensParams { lateral_ca: if i == 0 { 0.0 } else { 0.01 }, ..thin_lens(16.0, 1000.0) };
        let out = render_lights(&ctx, &[Vec2::new(0.85, 0.0)], 2.0, &lens, true, 1024, IntegratorKind::Pt);
        if i == 1 {
            save_png("p7_lateral_ca.png", &out.color, 5.0);
        }
        let (red, blue) = (measure_bokeh(&out.color, pixel_of(0.85, 0.0), 20, 0), measure_bokeh(&out.color, pixel_of(0.85, 0.0), 20, 2));
        *s = (blue.centroid.x - red.centroid.x) as f64;
        println!("lateral CA {:.3}: blue - red image position {:.2} px (expected ~{:.2})", lens.lateral_ca, *s, lens.lateral_ca as f64 * 0.85 * W as f64 / 2.0);
    }
    assert!(shift[0].abs() < 0.3, "{shift:?}");
    assert!(shift[1] > 1.0, "{shift:?}");
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn thin_lens_integrators_agree_and_realistic_lens() {
    let _g = gpu_lock();
    let ctx = context();
    // 薄レンズ（周辺減光・口径食あり）で、光源側からカメラへ直接つなぐ戦略の確率密度が合っていること
    {
        let floor = Material { base_color: Vec3::splat(0.6), specular_weight: 0.0, ..Default::default() };
        let light = Material { base_color: Vec3::ZERO, emission_luminance: 20000.0, ..floor.clone() };
        let mut m = Mesh::default();
        add_quad(&mut m, Vec3::new(-200.0, -60.0, -600.0), Vec3::new(200.0, -60.0, -600.0), Vec3::new(200.0, -60.0, 0.0), Vec3::new(-200.0, -60.0, 0.0), Vec3::Y, 0);
        add_box(&mut m, Vec3::new(-40.0, -60.0, -320.0), Vec3::new(0.0, -20.0, -280.0), 0);
        add_quad(&mut m, Vec3::new(-15.0, 40.0, -330.0), Vec3::new(15.0, 40.0, -330.0), Vec3::new(15.0, 40.0, -300.0), Vec3::new(-15.0, 40.0, -300.0), Vec3::NEG_Y, 1);
        let s = scene_of(vec![floor, light], m);
        let gpu = gpu_scene(&ctx, &s, &uniform_env(0.02, 16, 8));
        let lens = LensParams { blades: 6, natural_vignetting: true, optical_vignetting: 0.5, ..thin_lens(4.0, 260.0) };
        let mean: Vec<f64> = [(IntegratorKind::Pt, 2048), (IntegratorKind::Bdpt, 1024), (IntegratorKind::Vcm, 1024)]
            .iter()
            .map(|&(integ, spp)| {
                let mut f = frame(test_camera(), 8);
                f.lens = lens.clone();
                mean_rgb(&render(&ctx, &gpu, 160, 100, spp, integ, f).color)
            })
            .collect();
        println!("thin lens agreement: mean pt {:.5}, bdpt {:.5}, vcm {:.5}", mean[0], mean[1], mean[2]);
        assert!((mean[1] / mean[0] - 1.0).abs() < 0.015, "{mean:?}");
        assert!((mean[2] / mean[0] - 1.0).abs() < 0.025, "{mean:?}");
    }
    // 実レンズ（内蔵の double Gauss 50mm f/2）：一様な環境光で、画面の中央の明るさが 1（ピンホールと同じ）
    {
        let out = render_uniform_env(&ctx, &LensParams { model: LensModel::Realistic, focus_distance_mm: 1000.0, ..Default::default() }, 256);
        let mut c = 0.0f32;
        for dy in -4..=4 {
            for dx in -4..=4 {
                c += value_at(&out.color, W as i32 / 2 + dx, H as i32 / 2 + dy, 3) / 81.0;
            }
        }
        let corner = value_at(&out.color, 2, 2, 3);
        println!("realistic lens, uniform environment: center {c:.4}, corner {corner:.4}");
        assert!((c - 1.0).abs() < 0.05, "{c}");
        assert!(corner < c, "{corner} vs {c}"); // 周辺減光
    }
    // 実レンズのピント：ピントの合った点光源は小さく明るく、外すと広がる
    {
        let mut peak = [0.0f32; 2];
        for (i, p) in peak.iter_mut().enumerate() {
            let lens = LensParams { model: LensModel::Realistic, focus_distance_mm: if i == 0 { 1000.0 } else { 300.0 }, ..Default::default() };
            let out = render_lights(&ctx, &[Vec2::ZERO], 2.0, &lens, false, 1024, IntegratorKind::Pt);
            save_png(if i == 0 { "p7_real_focused.png" } else { "p7_real_defocused.png" }, &out.color, 5.0);
            for y in H as i32 / 2 - 30..H as i32 / 2 + 30 {
                for x in W as i32 / 2 - 30..W as i32 / 2 + 30 {
                    *p = p.max(value_at(&out.color, x, y, 3));
                }
            }
        }
        println!("realistic lens: peak in focus {:.4}, out of focus {:.4}", peak[0], peak[1]);
        assert!(peak[0] > 4.0 * peak[1], "{peak:?}");
    }
}

/// 実レンズを絞ったとき（F 22）：射出瞳から選ぶ方法と、後ろの面全体から選ぶ方法（前の方法）で、画面のどこでも同じ明るさ
/// （射出瞳の矩形が、通る光線をすべて含んでいる）。中央はピンホールと同じ 1
#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn realistic_lens_exit_pupil_is_unbiased() {
    let _g = gpu_lock();
    let ctx = context();
    let lens = LensParams { model: LensModel::Realistic, f_number: 22.0, focus_distance_mm: 300.0, natural_vignetting: true, ..Default::default() };
    let pupil = render_uniform_env(&ctx, &lens, 256);
    std::env::set_var("KRBR_LENS_FULL_DISK", "1");
    let full = render_uniform_env(&ctx, &lens, 8192);
    std::env::remove_var("KRBR_LENS_FULL_DISK");
    // 画面を 4 x 4 の区画に分けて、区画の平均を比べる
    let block = |img: &ImageF, bx: u32, by: u32| {
        let (w, h) = (img.width / 4, img.height / 4);
        let mut s = 0.0f64;
        for y in by * h..(by + 1) * h {
            for x in bx * w..(bx + 1) * w {
                s += img.rgba[((y * img.width + x) * 4 + 1) as usize] as f64;
            }
        }
        s / (w * h) as f64
    };
    for by in 0..4 {
        for bx in 0..4 {
            let (a, b) = (block(&pupil.color, bx, by), block(&full.color, bx, by));
            println!("block {bx},{by}: exit pupil {a:.4}, full disk {b:.4}");
            assert!((a / b - 1.0).abs() < 0.04, "block {bx},{by}: {a} vs {b}");
        }
    }
    let c = value_at(&pupil.color, W as i32 / 2, H as i32 / 2, 3);
    assert!((c - 1.0).abs() < 0.05, "center {c}");
}

/// 実レンズ：ピントを変えても画角が変わらない（画面の端の光源の像の位置が動かない）。以前はフィルムの大きさを
/// 薄いレンズの近似で決めていたので、ピントを合わせ直すたびに基板の見かけの大きさが変わった
#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn realistic_lens_keeps_the_angle_of_view_when_refocusing() {
    let _g = gpu_lock();
    let ctx = context();
    let at = Vec2::new(0.7, 0.0);
    let mut xs = Vec::new();
    for focus in [1000.0f32, 400.0, 150.0] {
        let lens = LensParams { model: LensModel::Realistic, f_number: 8.0, focus_distance_mm: focus, ..Default::default() };
        let out = render_lights(&ctx, &[at], 4.0, &lens, false, 512, IntegratorKind::Pt);
        let b = measure_bokeh(&out.color, pixel_of(at.x, at.y), 60, 1);
        println!("realistic lens, focus {focus} mm: image of the light at x = {:.2} px (expected {:.2})", b.centroid.x, pixel_of(at.x, at.y).x);
        xs.push(b.centroid.x as f64);
    }
    for x in &xs {
        assert!((x - xs[0]).abs() < 2.0, "{xs:?}");
    }
}

/// 画像の R, G, B それぞれの平均
fn mean_channels(img: &ImageF) -> [f64; 3] {
    let n = (img.rgba.len() / 4) as f64;
    let mut m = [0.0f64; 3];
    for p in img.rgba.chunks_exact(4) {
        for k in 0..3 {
            m[k] += p[k] as f64 / n;
        }
    }
    m
}

/// 分光モード・薄レンズの色収差（倍率・軸上）でも、PT・BDPT・VCM が同じ色に収束する。
/// 色収差では代表波長以外を打ち切るので、VCM（繰り返しごとに全画素で同じ波長）では 1 回ごとの画像が単色になるが、
/// 積めば同じ色になること
#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn chromatic_thin_lens_integrators_agree_in_color() {
    let _g = gpu_lock();
    let ctx = context();
    let floor = Material { base_color: Vec3::new(0.2, 0.6, 0.3), specular_weight: 0.0, ..Default::default() };
    let red = Material { base_color: Vec3::new(0.8, 0.15, 0.1), specular_weight: 0.0, ..Default::default() };
    let light = Material { base_color: Vec3::ZERO, emission_luminance: 20000.0, specular_weight: 0.0, ..Default::default() };
    let mut m = Mesh::default();
    add_quad(&mut m, Vec3::new(-200.0, -60.0, -600.0), Vec3::new(200.0, -60.0, -600.0), Vec3::new(200.0, -60.0, 0.0), Vec3::new(-200.0, -60.0, 0.0), Vec3::Y, 0);
    add_box(&mut m, Vec3::new(-40.0, -60.0, -320.0), Vec3::new(0.0, -20.0, -280.0), 1);
    add_quad(&mut m, Vec3::new(-15.0, 40.0, -330.0), Vec3::new(15.0, 40.0, -330.0), Vec3::new(15.0, 40.0, -300.0), Vec3::new(-15.0, 40.0, -300.0), Vec3::NEG_Y, 2);
    let s = scene_of(vec![floor, red, light], m);
    let gpu = gpu_scene(&ctx, &s, &uniform_env(0.02, 16, 8));
    for (name, lens) in [
        ("no CA", thin_lens(4.0, 260.0)),
        ("lateral CA", LensParams { lateral_ca: 0.03, ..thin_lens(4.0, 260.0) }),
        ("axial CA", LensParams { abbe_number: 20.0, ..thin_lens(4.0, 260.0) }),
    ] {
        let means: Vec<[f64; 3]> = [(IntegratorKind::Pt, 4096), (IntegratorKind::Bdpt, 2048), (IntegratorKind::Vcm, 2048)]
            .iter()
            .map(|&(integ, spp)| {
                let mut f = frame(test_camera(), 8);
                f.lens = lens.clone();
                f.spectral = true;
                let out = render(&ctx, &gpu, 160, 100, spp, integ, f);
                save_png(&format!("p7_ca_agree_{}_{integ:?}.png", name.replace(' ', "_")), &out.color, 0.0);
                mean_channels(&out.color)
            })
            .collect();
        println!("{name}: pt {:.5?}, bdpt {:.5?}, vcm {:.5?}", means[0], means[1], means[2]);
        for k in 0..3 {
            // VCM の波長は繰り返しごとに全画素で共通なので、独立な乱数だと色収差では全体の色が数 % ずれた（列にして 1 % 未満）
            assert!((means[1][k] / means[0][k] - 1.0).abs() < 0.015, "{name} bdpt channel {k}: {means:?}");
            assert!((means[2][k] / means[0][k] - 1.0).abs() < 0.015, "{name} vcm channel {k}: {means:?}");
        }
    }
}

/// 確認用の画像：多角形のボケ、画面の端のキャッツアイ、縁の色のにじみ（分光モード）。ボケは光が広がって暗いので +4 EV で保存。
/// 32768 spp で時間がかかるので分けてある
#[test]
#[ignore = "needs a Vulkan GPU with ray tracing (slow: 32768 spp)"]
fn camera_showcase_image() {
    let _g = gpu_lock();
    let ctx = context();
    let ndc: Vec<Vec2> = (0..3).flat_map(|j| (0..5).map(move |i| Vec2::new(-0.8 + 0.4 * i as f32, -0.7 + 0.7 * j as f32))).collect();
    let lens = LensParams {
        blades: 7,
        blade_rotation_deg: 10.0,
        blade_roundness: 0.2,
        natural_vignetting: true,
        optical_vignetting: 0.6,
        abbe_number: 25.0,
        lateral_ca: 0.006,
        ..thin_lens(2.0, 300.0)
    };
    let out = render_lights(&ctx, &ndc, 6.0, &lens, true, 32768, IntegratorKind::Pt);
    save_png("p7_showcase.png", &out.color, 4.0);
}
