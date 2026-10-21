//! GPU の検証テストの共通部分：テスト用の簡単な形状、1 メッシュだけのシーン、一様な環境マップ、描画、画像の比べ方。
//! テストは Vulkan の GPU（ray tracing）が要るので #[ignore] にしてあり、明示したときだけ走らせる：
//!   cargo test --release -p krbr-render -- --ignored --nocapture
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use glam::{Mat4, Vec2, Vec3};
use krbr_core::mesh::Mesh;
use krbr_render::{build_env_distribution, render_offscreen, Context, ContextOptions, FrameParams, GpuScene, ImageF, RenderOutput, RenderSettings};
use krbr_scene::camera::Camera;
use krbr_scene::material::Material;
use krbr_scene::scene::{Instance, InstanceOwner, Scene, SceneMesh};

/// GPU のテストは 1 つずつ走らせる（同じ実行ファイルの中のテストは並列に走るため）
pub fn gpu_lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn context() -> Context {
    krbr_core::log::set_level(krbr_core::log::Level::Warn);
    Context::new(&ContextOptions { low_queue_priority: true, ..Default::default() }).expect("Vulkan context")
}

/// 画像の出力先（<repo>/out）
pub fn out_dir() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../out");
    std::fs::create_dir_all(&p).ok();
    p
}

pub fn save_png(name: &str, img: &ImageF, exposure_ev: f32) {
    krbr_render::image_io::save_png_tonemapped(&out_dir().join(name), img, exposure_ev, &krbr_render::glow::GlowParams::OFF).expect("save png");
}

// ---------------------------------------------------------------- 形状

/// 平面の四角形（p0→p1→p2→p3 の順、法線は n の向き）
pub fn add_quad(m: &mut Mesh, p0: Vec3, mut p1: Vec3, p2: Vec3, mut p3: Vec3, n: Vec3, mat: u32) {
    if (p1 - p0).cross(p2 - p0).dot(n) < 0.0 {
        std::mem::swap(&mut p1, &mut p3);
    }
    let (a, b, c, d) = (m.add_vertex(p0, n), m.add_vertex(p1, n), m.add_vertex(p2, n), m.add_vertex(p3, n));
    m.add_triangle(a, b, c, mat);
    m.add_triangle(a, c, d, mat);
}

pub fn add_box(m: &mut Mesh, lo: Vec3, hi: Vec3, mat: u32) {
    let c = [
        Vec3::new(lo.x, lo.y, lo.z), Vec3::new(hi.x, lo.y, lo.z), Vec3::new(hi.x, hi.y, lo.z), Vec3::new(lo.x, hi.y, lo.z),
        Vec3::new(lo.x, lo.y, hi.z), Vec3::new(hi.x, lo.y, hi.z), Vec3::new(hi.x, hi.y, hi.z), Vec3::new(lo.x, hi.y, hi.z),
    ];
    add_quad(m, c[0], c[1], c[2], c[3], Vec3::NEG_Z, mat);
    add_quad(m, c[4], c[5], c[6], c[7], Vec3::Z, mat);
    add_quad(m, c[0], c[1], c[5], c[4], Vec3::NEG_Y, mat);
    add_quad(m, c[3], c[2], c[6], c[7], Vec3::Y, mat);
    add_quad(m, c[0], c[3], c[7], c[4], Vec3::NEG_X, mat);
    add_quad(m, c[1], c[2], c[6], c[5], Vec3::X, mat);
}

pub fn add_sphere(m: &mut Mesh, center: Vec3, r: f32, mat: u32, seg_u: u32, seg_v: u32) {
    use std::f64::consts::PI;
    let base = m.positions.len() as u32;
    for j in 0..=seg_v {
        let th = PI * j as f64 / seg_v as f64;
        for i in 0..=seg_u {
            let ph = 2.0 * PI * i as f64 / seg_u as f64;
            let n = Vec3::new((th.sin() * ph.cos()) as f32, th.cos() as f32, (th.sin() * ph.sin()) as f32);
            m.add_vertex(center + n * r, n);
        }
    }
    for j in 0..seg_v {
        for i in 0..seg_u {
            let a = base + j * (seg_u + 1) + i;
            let (b, c) = (a + 1, a + seg_u + 1);
            m.add_triangle(a, b, c, mat);
            m.add_triangle(b, c + 1, c, mat);
        }
    }
}

/// 凸な立体の三角形を、外向きの法線（重心から離れる向き）になるように足す
pub fn add_convex_tri(m: &mut Mesh, a: Vec3, mut b: Vec3, mut c: Vec3, centroid: Vec3, mat: u32) {
    let mut n = (b - a).cross(c - a).normalize();
    if n.dot((a + b + c) / 3.0 - centroid) < 0.0 {
        std::mem::swap(&mut b, &mut c);
        n = -n;
    }
    let (i, j, k) = (m.add_vertex(a, n), m.add_vertex(b, n), m.add_vertex(c, n));
    m.add_triangle(i, j, k, mat);
}

/// XZ 面の凸多角形を Y 方向に押し出した立体
pub fn add_prism(m: &mut Mesh, xz: &[Vec2], y0: f32, y1: f32, mat: u32) {
    let centroid = xz.iter().map(|p| Vec3::new(p.x, 0.5 * (y0 + y1), p.y)).sum::<Vec3>() / xz.len() as f32;
    let n = xz.len();
    for i in 0..n {
        let (p, q) = (xz[i], xz[(i + 1) % n]);
        let (a, b, c, d) = (Vec3::new(p.x, y0, p.y), Vec3::new(q.x, y0, q.y), Vec3::new(q.x, y1, q.y), Vec3::new(p.x, y1, p.y));
        add_convex_tri(m, a, b, c, centroid, mat);
        add_convex_tri(m, a, c, d, centroid, mat);
    }
    for i in 1..n - 1 {
        for y in [y0, y1] {
            add_convex_tri(m, Vec3::new(xz[0].x, y, xz[0].y), Vec3::new(xz[i].x, y, xz[i].y), Vec3::new(xz[i + 1].x, y, xz[i + 1].y), centroid, mat);
        }
    }
}

// ---------------------------------------------------------------- シーンと描画

/// 1 つのメッシュ・1 つの配置だけのシーン（基板面の距離場は使わない）
pub fn scene_of(materials: Vec<Material>, mesh: Mesh) -> Scene {
    Scene {
        materials,
        meshes: vec![SceneMesh { name: "test_scene".into(), mesh }],
        instances: vec![Instance { mesh: 0, xform: Mat4::IDENTITY, material_base: 0, owner: InstanceOwner::Board }],
        ..Default::default()
    }
}

/// 一様な色の環境マップ
pub fn uniform_env(v: f32, w: u32, h: u32) -> ImageF {
    let mut env = ImageF::new(w, h);
    for p in env.rgba.chunks_exact_mut(4) {
        p.copy_from_slice(&[v, v, v, 1.0]);
    }
    env
}

pub fn gpu_scene<'a>(ctx: &'a Context, s: &Scene, env: &ImageF) -> GpuScene<'a> {
    let mut gpu = GpuScene::new(ctx);
    gpu.build(s, env, &build_env_distribution(env, 2048), 1.0);
    gpu
}

/// 偏りのない（radiance_clamp = 0）設定
pub fn frame(camera: Camera, max_depth: u32) -> FrameParams {
    FrameParams { camera, max_depth, radiance_clamp: 0.0, ..Default::default() }
}

pub fn render(ctx: &Context, gpu: &GpuScene<'_>, w: u32, h: u32, spp: u32, integrator: &str, frame: FrameParams) -> RenderOutput {
    let rs = RenderSettings { width: w, height: h, spp, integrator: integrator.into(), frame, denoise: None };
    render_offscreen(ctx, gpu, &rs).expect("render")
}

pub fn diffuse(name: &str, c: Vec3) -> Material {
    Material { name: name.into(), base_color: c, specular_weight: 0.0, ..Default::default() }
}

pub fn emitter(name: &str, luminance: f32) -> Material {
    Material { emission_luminance: luminance, ..diffuse(name, Vec3::ZERO) }
}

// ---------------------------------------------------------------- 画像の比べ方

/// 比べる範囲（画素。[x0, x1) x [y0, y1)）
#[derive(Clone, Copy)]
pub struct Window {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Window {
    pub const ALL: Window = Window { x0: 0, y0: 0, x1: i32::MAX, y1: i32::MAX };
}

/// 4x4 画素ごとの平均を取った画像の、相対的な差（RMS / 平均）と、それぞれの平均
pub fn relative_rmse(a: &ImageF, b: &ImageF, win: Window) -> (f64, f64, f64) {
    const BS: i32 = 4;
    let (mut se, mut sa, mut sb, mut n) = (0.0f64, 0.0f64, 0.0f64, 0usize);
    let mut by = win.y0.max(0);
    while by + BS <= (a.height as i32).min(win.y1) {
        let mut bx = win.x0.max(0);
        while bx + BS <= (a.width as i32).min(win.x1) {
            let (mut va, mut vb) = (0.0f64, 0.0f64);
            for y in by..by + BS {
                for x in bx..bx + BS {
                    for c in 0..3 {
                        let i = (y as usize * a.width as usize + x as usize) * 4 + c;
                        va += a.rgba[i] as f64;
                        vb += b.rgba[i] as f64;
                    }
                }
            }
            va /= (BS * BS * 3) as f64;
            vb /= (BS * BS * 3) as f64;
            se += (va - vb) * (va - vb);
            sa += va;
            sb += vb;
            n += 1;
            bx += BS;
        }
        by += BS;
    }
    let n = n as f64;
    let mean = 0.5 * (sa + sb) / n;
    ((se / n).sqrt() / mean.max(1e-9), sa / n, sb / n)
}

/// 画素の RGB の平均
pub fn mean_rgb(img: &ImageF) -> f64 {
    let sum: f64 = img.rgba.chunks_exact(4).map(|p| (p[0] + p[1] + p[2]) as f64).sum();
    sum / (img.rgba.len() / 4 * 3) as f64
}
