//! 白い炉テスト（white furnace test）：一様な白い環境光（放射輝度 1）の中に球を置き、オフスクリーンで描く。
//! 吸収のない材質なら、球の画素の値は 1 になる。1 を超えたらエネルギーが増えている（誤り）。
//! 単一散乱のマイクロファセット（多重散乱の補償なし）や、層の重ね方の近似による損失は、許容範囲として記録する。
//!   cargo test --release -p krbr-render --test furnace -- --ignored --nocapture

mod common;

use common::*;
use glam::{DVec3, Vec3};
use krbr_core::mesh::Mesh;
use krbr_render::Context;
use krbr_scene::camera::orbit_camera;
use krbr_scene::material::Material;

struct FurnaceResult {
    mean: f64,
    mean_rgb: DVec3,
}

fn furnace(ctx: &Context, mat: &Material, spp: u32, spectral: bool, integrator: IntegratorKind) -> FurnaceResult {
    let mut mesh = Mesh::default();
    add_sphere(&mut mesh, Vec3::ZERO, 10.0, 0, 96, 48);
    let s = scene_of(vec![mat.clone()], mesh);
    let gpu = gpu_scene(ctx, &s, &uniform_env(1.0, 64, 32));
    let mut f = frame(orbit_camera(Vec3::ZERO, 60.0, 20.0, 30.0, 25.0, 1.0), 64);
    f.spectral = spectral;
    let out = render(ctx, &gpu, 96, 96, spp, integrator, f);

    let (mut sum, mut sum_rgb, mut n) = (0.0f64, DVec3::ZERO, 0usize);
    for (c, nrm) in out.color.rgba.chunks_exact(4).zip(out.normal.rgba.chunks_exact(4)) {
        if nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2] < 0.25 {
            continue; // 背景（環境マップが直接見えている画素）
        }
        let rgb = DVec3::new(c[0] as f64, c[1] as f64, c[2] as f64);
        sum += rgb.element_sum() / 3.0;
        sum_rgb += rgb;
        n += 1;
    }
    let n = n.max(1) as f64;
    FurnaceResult { mean: sum / n, mean_rgb: sum_rgb / n }
}

struct Case {
    name: &'static str,
    m: Material,
    /// 平均の許容範囲（1 を超えないこと。下側は既知の損失）
    lo: f64,
    hi: f64,
    spectral: bool,
}

fn case(name: &'static str, lo: f64, hi: f64, spectral: bool, setup: impl FnOnce(&mut Material)) -> Case {
    let mut m = Material { base_color: Vec3::ONE, ..Default::default() };
    setup(&mut m);
    Case { name, m, lo, hi, spectral }
}

fn lambert(m: &mut Material) {
    m.specular_weight = 0.0;
}
fn metal(r: f32) -> impl FnOnce(&mut Material) {
    move |m| {
        m.base_metalness = 1.0;
        m.specular_roughness = r;
    }
}
fn translucent(m: &mut Material) {
    m.specular_weight = 0.0;
    m.subsurface_weight = 0.3;
    m.subsurface_color = Vec3::ONE;
    m.subsurface_radius = 1000.0;
}
fn glass(r: f32) -> impl FnOnce(&mut Material) {
    move |m| {
        m.specular_roughness = r;
        m.transmission_weight = 1.0;
    }
}
fn coated_lambert(m: &mut Material) {
    m.specular_weight = 0.0;
    m.coat_weight = 1.0;
    m.coat_roughness = 0.1;
}
fn acrylic(m: &mut Material) {
    m.specular_roughness = 0.0;
    m.specular_ior = 1.491;
    m.transmission_weight = 1.0;
    m.transmission_dispersion_scale = 1.0;
    m.transmission_dispersion_abbe_number = 57.0;
}

fn check(cases: &[Case], spp: u32, integrator: IntegratorKind, ctx: &Context) -> Vec<String> {
    let mut failures = Vec::new();
    for c in cases {
        let r = furnace(ctx, &c.m, spp, c.spectral, integrator);
        println!("  {:<5} {:<16} mean {:.4}  (allowed {:.3} .. {:.3})", integrator.as_str(), c.name, r.mean, c.lo, c.hi);
        if r.mean < c.lo || r.mean > c.hi {
            failures.push(format!("{} {}: mean {:.4}", integrator.as_str(), c.name, r.mean));
        }
    }
    failures
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn white_furnace() {
    let _g = gpu_lock();
    let ctx = context();
    let cases = [
        // 鏡面なしの白い拡散：ちょうど 1
        case("lambert", 0.985, 1.015, false, lambert),
        // 完全な鏡（F0 = 1）：ちょうど 1
        case("mirror", 0.985, 1.015, false, metal(0.0)),
        // 粗い金属：多重散乱の補償がないので 1 より小さい
        case("metal_r0.3", 0.90, 1.015, false, metal(0.3)),
        // 粗さ 1（α = 1）の単一散乱 GGX（F = 1）の半球平均アルベドは数値積分で 0.409。それと一致すること
        case("metal_r1.0", 0.39, 0.43, false, metal(1.0)),
        // 拡散の透過（基材の透光性の仕組み）：吸収のない白なら、入って出ても 1
        case("translucent", 0.985, 1.015, false, translucent),
        // 透明な誘電体（ガラス）：反射と屈折を合わせて 1
        case("glass", 0.985, 1.015, false, glass(0.0)),
        // 粗いガラス：単一散乱のマイクロファセットなので少し損失がある
        case("glass_r0.3", 0.85, 1.015, false, glass(0.3)),
        // 白い誘電体（鏡面＋拡散）：層の重ね方の近似で少し損失がある
        case("dielectric_r0.3", 0.80, 1.015, false, |_| {}),
        case("coated_lambert", 0.75, 1.015, false, coated_lambert),
        // 分光モード：白い拡散と、分散のあるガラス（代表波長以外を打ち切っても、偏りなく 1）
        case("lambert (spec)", 0.985, 1.015, true, lambert),
        case("acrylic (spec)", 0.98, 1.02, true, acrylic),
        case("coated (spec)", 0.75, 1.015, true, coated_lambert),
    ];
    println!("white furnace test (mean radiance on the sphere, ideal = 1):");
    let failures = check(&cases, 256, IntegratorKind::Pt, &ctx);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// 分光モードでも、色のついた拡散は RGB モードと同じ色になる（RGB → スペクトル → XYZ → RGB の往復）
#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn spectral_color_constancy() {
    let _g = gpu_lock();
    let ctx = context();
    for c in [Vec3::new(0.8, 0.3, 0.1), Vec3::new(0.1, 0.4, 0.8), Vec3::new(0.2, 0.7, 0.25)] {
        let m = Material { specular_weight: 0.0, base_color: c, ..Default::default() };
        let rgb = furnace(&ctx, &m, 128, false, IntegratorKind::Pt).mean_rgb;
        let spec = furnace(&ctx, &m, 128, true, IntegratorKind::Pt).mean_rgb;
        println!("  base {:.2} {:.2} {:.2}: RGB {:.4} {:.4} {:.4}, spectral {:.4} {:.4} {:.4}", c.x, c.y, c.z, rgb.x, rgb.y, rgb.z, spec.x, spec.y, spec.z);
        for k in 0..3 {
            assert!((spec[k] - rgb[k]).abs() < 0.02 + 0.03 * rgb[k], "channel {k}: spectral {} vs RGB {}", spec[k], rgb[k]);
        }
    }
}

/// 測定値の金属：分光モード（波長ごとの n, k から厳密なフレネル）と RGB モード（同じ表から求めた F0 と F82-tint）で
/// 同じ色になる。BDPT でも分光モードの金属の表を引ける。F82-tint（specular_color < 1）は Schlick より暗くなる
#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn measured_metals_rgb_matches_spectral() {
    let _g = gpu_lock();
    let ctx = context();
    for id in ["Au", "Cu", "Cr", "Os", "CuZn30"] {
        let mut m = Material { specular_roughness: 0.3, ..Default::default() };
        m.set_conductor(krbr_scene::conductor::find(id).unwrap());
        let rgb = furnace(&ctx, &m, 128, false, IntegratorKind::Pt).mean_rgb;
        let spec = furnace(&ctx, &m, 128, true, IntegratorKind::Pt).mean_rgb;
        let bdpt = furnace(&ctx, &m, 64, true, IntegratorKind::Bdpt).mean_rgb;
        println!("  {id:<7} RGB {:.4} {:.4} {:.4}, spectral {:.4} {:.4} {:.4}, spectral BDPT {:.4} {:.4} {:.4}", rgb.x, rgb.y, rgb.z, spec.x, spec.y, spec.z, bdpt.x, bdpt.y, bdpt.z);
        for k in 0..3 {
            assert!((spec[k] - rgb[k]).abs() < 0.01 + 0.03 * rgb[k], "{id} channel {k}: spectral {} vs RGB {}", spec[k], rgb[k]);
            assert!((bdpt[k] - spec[k]).abs() < 0.01 + 0.03 * spec[k], "{id} channel {k}: BDPT {} vs PT {}", bdpt[k], spec[k]);
        }
    }
    let schlick = Material { base_color: Vec3::splat(0.6), base_metalness: 1.0, specular_roughness: 0.3, ..Default::default() };
    let tinted = Material { specular_color: Vec3::splat(0.7), ..schlick.clone() };
    let (a, b) = (furnace(&ctx, &schlick, 128, false, IntegratorKind::Pt).mean, furnace(&ctx, &tinted, 128, false, IntegratorKind::Pt).mean);
    println!("  F82-tint 1.0: {a:.4}, 0.7: {b:.4}");
    assert!(b < a - 0.01 && b > 0.5 * a, "F82-tint: {a} -> {b}");
}

/// 双方向の積分器（BDPT / VCM）でも、白い炉の中で 1 になる（MIS の重みの和が 1 であることの確かめにもなる）
#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn white_furnace_bidirectional() {
    let _g = gpu_lock();
    let ctx = context();
    let cases = [
        case("lambert", 0.98, 1.02, false, lambert),
        case("metal_r0.3", 0.90, 1.02, false, metal(0.3)),
        case("dielectric_r0.3", 0.80, 1.02, false, |_| {}),
        case("translucent", 0.98, 1.02, false, translucent),
        case("glass", 0.98, 1.02, false, glass(0.0)),
        // すりガラス：透過する材質は粗さによらず鏡面とみなして通り抜ける（isDeltaLike）。MIS の重みの和が 1 のままで
        // 偏らないこと（PT の白い炉と同じく、単一散乱のマイクロファセットの損失の分だけ 1 より小さい）
        case("glass_r0.3", 0.85, 1.02, false, glass(0.3)),
        case("acrylic (spec)", 0.97, 1.03, true, acrylic),
    ];
    println!("white furnace test, bidirectional (mean radiance on the sphere, ideal = 1):");
    let mut failures = check(&cases, 128, IntegratorKind::Bdpt, &ctx);
    failures.extend(check(&cases, 128, IntegratorKind::Vcm, &ctx));
    assert!(failures.is_empty(), "{failures:#?}");
}

/// 鏡面とみなす頂点の近似（逆向きの確率密度 = 順向き）を両側から同じに数えるので、粗い面でも BDPT / VCM は PT と同じ値になる。
/// 単一散乱の損失がある材質（すりガラス・粗い金属）で、白い炉の平均が PT と一致することを確かめる。
/// （PT の側も、直接サンプリングで試さない透過の向きを MIS で割り引いていると、すりガラスで数 % 暗くなる。その確かめも兼ねる）
#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn rough_surfaces_bidirectional_match_pt() {
    let _g = gpu_lock();
    let ctx = context();
    let mut failures = Vec::new();
    let materials = [case("glass_r0.3", 0.0, 1.0, false, glass(0.3)), case("glass_r0.6", 0.0, 1.0, false, glass(0.6)), case("metal_r0.3", 0.0, 1.0, false, metal(0.3))];
    for Case { name, m, .. } in &materials {
        let pt = furnace(&ctx, m, 512, false, IntegratorKind::Pt).mean;
        for integ in [IntegratorKind::Bdpt, IntegratorKind::Vcm] {
            let b = furnace(&ctx, m, 256, false, integ).mean;
            println!("  {name:<12} pt {pt:.4}, {integ} {b:.4}");
            if (b / pt - 1.0).abs() > 0.015 {
                failures.push(format!("{name}: pt {pt:.4} vs {integ} {b:.4}"));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
