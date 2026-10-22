//! 材質エディタのプレビュー：材質の見本（市松模様の床の上の球）と、選んだ部品だけを置いたシーンを、
//! 小さな画像に描く（オフスクリーン。ビューアと同じ GPU・同じ積分器）。照明は内蔵のスタジオ（基板の環境マップによらない）

use std::sync::Arc;

use glam::{Mat4, Vec3};
use krbr_core::mesh::Mesh;
use krbr_render::{build_env_distribution, Context, EnvDistribution, FrameParams, GpuScene, ImageF, Progressive};
use krbr_scene::camera::Camera;
use krbr_scene::scene::{Instance, InstanceOwner, Scene, SceneMesh};
use krbr_scene::{orbit_camera, IntegratorKind, Material};

/// 見本・部品のプレビューの照明（内蔵のスタジオ）と、その重点的サンプリングの分布。一度だけ作って共有する
pub struct PreviewLight {
    env: ImageF,
    dist: EnvDistribution,
}

impl PreviewLight {
    pub fn new() -> Arc<Self> {
        let env = krbr_render::studio_env::studio_environment(1024);
        let dist = build_env_distribution(&env, 1024);
        Arc::new(Self { env, dist })
    }
}

/// 小さなオフスクリーンの描画 1 つ（シーン・累積・積分器）
pub struct Preview {
    // 積分器は GpuScene を参照するので、先に捨てる（フィールドは宣言の順に捨てられる）
    prog: Progressive<'static>,
    gpu: Box<GpuScene<'static>>,
    /// 発光が変わったときに形状ごと送り直すための、シーンの写し
    scene: Scene,
}

/// プレビューの描き方（偏りを気にせず、見やすさと速さを優先する）
fn preview_frame(camera: Camera, spectral: bool) -> FrameParams {
    FrameParams { camera, radiance_clamp: 20.0, max_depth: 8, spectral, ..Default::default() }
}

impl Preview {
    pub fn new(ctx: &'static Context, scene: Scene, light: &PreviewLight, width: u32, height: u32, camera: Camera, spectral: bool) -> Result<Self, String> {
        let mut gpu = Box::new(GpuScene::new(ctx));
        gpu.build(&scene, &light.env, &light.dist, 1.0)?;
        // 安全性：Box の中身はムーブされず、prog は gpu より先に捨てる（フィールドの順）
        let gpu_ref: &'static GpuScene<'static> = unsafe { &*(&*gpu as *const GpuScene<'static>) };
        let prog = Progressive::new(ctx, gpu_ref, IntegratorKind::Pt, width, height, preview_frame(camera, spectral))?;
        Ok(Self { prog, gpu, scene })
    }

    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// 材質を差し替える（累積は 0 から）。発光が変わったら形状ごと送り直す。GPU が待機中に呼ぶこと
    pub fn set_materials(&mut self, materials: Vec<Material>) -> Result<(), String> {
        if materials == self.scene.materials {
            return Ok(());
        }
        let emission_changed = materials.len() != self.scene.materials.len() || materials.iter().zip(&self.scene.materials).any(|(a, b)| a.emission_luminance != b.emission_luminance || a.emission_color != b.emission_color);
        self.scene.materials = materials;
        if emission_changed {
            self.gpu.rebuild_geometry(&self.scene, 1.0)?;
            self.prog.rebuild_integrator()?;
        } else {
            self.gpu.update_materials(&self.scene.materials, 1.0)?;
            self.prog.reset();
        }
        Ok(())
    }

    /// カメラ・色の計算を変える（変わったときだけ累積を 0 から）
    pub fn set_view(&mut self, camera: Camera, spectral: bool) {
        let f = self.prog.frame();
        if f.camera.position != camera.position || f.camera.forward != camera.forward || f.camera.tan_half_fov_y != camera.tan_half_fov_y || f.spectral != spectral {
            self.prog.set_frame(preview_frame(camera, spectral));
        }
    }

    pub fn samples(&self) -> u32 {
        self.prog.samples()
    }

    /// サンプルを count 積んで完了を待つ。GPU が待機中に呼ぶこと
    pub fn add_samples(&mut self, ctx: &Context, count: u32) -> Result<(), String> {
        let prog = &mut self.prog;
        ctx.submit_and_wait(|_, cmd| prog.record(cmd, count))
    }

    /// 今の累積を、表示と同じトーンマッピングで egui の画像にする。GPU が待機中に呼ぶこと
    pub fn image(&self, exposure_ev: f32) -> Result<egui::ColorImage, String> {
        let aov = self.prog.readback()?;
        let rgba = krbr_render::image_io::tonemap_rgba8(&aov.color, exposure_ev, &krbr_render::glow::GlowParams::OFF);
        Ok(egui::ColorImage::from_rgba_unmultiplied([aov.color.width as usize, aov.color.height as usize], &rgba))
    }
}

// ---------------------------------------------------------------- 形状

fn add_quad(m: &mut Mesh, p: [Vec3; 4], n: Vec3, mat: u32) {
    let v: Vec<u32> = p.iter().map(|q| m.add_vertex(*q, n)).collect();
    m.add_triangle(v[0], v[1], v[2], mat);
    m.add_triangle(v[0], v[2], v[3], mat);
}

fn add_sphere(m: &mut Mesh, center: Vec3, r: f32, mat: u32, seg_u: u32, seg_v: u32) {
    use std::f32::consts::PI;
    let base = m.positions.len() as u32;
    for j in 0..=seg_v {
        let th = PI * j as f32 / seg_v as f32;
        for i in 0..=seg_u {
            let ph = 2.0 * PI * i as f32 / seg_u as f32;
            let n = Vec3::new(th.sin() * ph.cos(), th.cos(), th.sin() * ph.sin());
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

/// 床（y = y0 の市松模様。材質 light と dark）。half は半分の大きさ、cell は升目の大きさ
fn add_checker_floor(m: &mut Mesh, center: Vec3, y0: f32, half: f32, cell: f32, light: u32, dark: u32) {
    let n = (half / cell).ceil() as i32;
    for iz in -n..n {
        for ix in -n..n {
            let (x0, z0) = (center.x + ix as f32 * cell, center.z + iz as f32 * cell);
            let mat = if (ix + iz).rem_euclid(2) == 0 { light } else { dark };
            add_quad(m, [Vec3::new(x0, y0, z0), Vec3::new(x0, y0, z0 + cell), Vec3::new(x0 + cell, y0, z0 + cell), Vec3::new(x0 + cell, y0, z0)], Vec3::Y, mat);
        }
    }
}

fn floor_materials() -> [Material; 2] {
    let diffuse = |name: &str, v: f32| Material { name: name.into(), base_color: Vec3::splat(v), specular_roughness: 0.6, ..Default::default() };
    [diffuse("preview_floor_light", 0.45), diffuse("preview_floor_dark", 0.12)]
}

// ---------------------------------------------------------------- 材質の見本

/// 見本の球の半径（mm）
const SWATCH_R: f32 = 10.0;

/// 見本の大きさ（画素）
pub const SWATCH_PX: u32 = 96;

/// 見本のシーン：材質 0 が球、1・2 が床
pub fn swatch_scene(m: &Material) -> Scene {
    let mut mesh = Mesh::default();
    add_sphere(&mut mesh, Vec3::new(0.0, SWATCH_R, 0.0), SWATCH_R, 0, 64, 32);
    add_checker_floor(&mut mesh, Vec3::ZERO, 0.0, 80.0, 5.0, 1, 2);
    let [a, b] = floor_materials();
    Scene {
        materials: vec![swatch_material(m), a, b],
        meshes: vec![SceneMesh { name: "swatch".into(), mesh }],
        instances: vec![Instance { mesh: 0, xform: Mat4::IDENTITY, material_base: 0, owner: InstanceOwner::Board }],
        ..Default::default()
    }
}

/// 見本の球が表す大きさ（mm）：中で吸収される材質（宝石・色ガラス）の色は厚みで変わるので、見本は小さな部品・宝石の
/// 大きさの球の見え方にする（描く球は SWATCH_R の大きさなので、吸収の深さをその比で伸ばす）
const SWATCH_REPRESENTS_MM: f32 = 4.0;

/// 見本に描く材質：3D モデルの面の色を使う材質は、明るめの灰色で見せる
pub fn swatch_material(m: &Material) -> Material {
    let mut m = m.clone();
    if m.base_color_from_model {
        m.base_color = Vec3::splat(0.5);
    }
    let scale = 2.0 * SWATCH_R / SWATCH_REPRESENTS_MM;
    m.transmission_depth *= scale;
    m.subsurface_radius *= scale;
    m
}

/// 見本の材質の一覧（球 + 床）
pub fn swatch_materials(m: &Material) -> Vec<Material> {
    let [a, b] = floor_materials();
    vec![swatch_material(m), a, b]
}

pub fn swatch_camera() -> Camera {
    // スタジオの照明は既定のカメラ（方位 45°・仰角 35°）から見てよく見えるように置いてある
    orbit_camera(Vec3::new(0.0, SWATCH_R * 0.9, 0.0), 62.0, 22.0, 45.0, 26.0, 1.0)
}

// ---------------------------------------------------------------- 選んだ部品

/// 選んだ部品だけのシーン：そのインスタンスの形状と材質（番号は詰め直す）と、その下の床。
/// 戻り値の 2 つめは、元の材質の番号 → このシーンの材質の番号（材質の編集をすぐ見せるのに使う）
pub fn part_scene(main: &Scene, instances: &[usize]) -> Option<(Scene, Vec<(u32, u32)>)> {
    let mut s = Scene::default();
    let mut map: Vec<(u32, u32)> = Vec::new();
    let mut lo = Vec3::splat(f32::MAX);
    let mut hi = Vec3::splat(f32::MIN);
    for &i in instances {
        let inst = main.instances.get(i)?;
        let src = &main.meshes.get(inst.mesh as usize)?.mesh;
        let parts = src.tri_material.iter().copied().max().map(|m| m + 1).unwrap_or(0);
        let base = s.materials.len() as u32;
        for k in 0..parts {
            let id = inst.material_base + k;
            s.materials.push(main.materials.get(id as usize)?.clone());
            map.push((id, base + k));
        }
        for p in &src.positions {
            let w = inst.xform.transform_point3(*p);
            lo = lo.min(w);
            hi = hi.max(w);
        }
        s.meshes.push(SceneMesh { name: main.meshes[inst.mesh as usize].name.clone(), mesh: src.clone() });
        s.instances.push(Instance { mesh: s.meshes.len() as u32 - 1, xform: inst.xform, material_base: base, owner: InstanceOwner::Board });
    }
    if s.instances.is_empty() || lo.x > hi.x {
        return None;
    }
    // 床：部品の下面に、部品の大きさに合わせた市松模様
    let size = (hi - lo).max_element().max(0.5);
    let mut floor = Mesh::default();
    let center = (lo + hi) * 0.5;
    let base = s.materials.len() as u32;
    add_checker_floor(&mut floor, Vec3::new(center.x, 0.0, center.z), lo.y - size * 0.002, size * 4.0, size * 0.25, 0, 1);
    s.materials.extend(floor_materials());
    s.meshes.push(SceneMesh { name: "preview_floor".into(), mesh: floor });
    s.instances.push(Instance { mesh: s.meshes.len() as u32 - 1, xform: Mat4::IDENTITY, material_base: base, owner: InstanceOwner::Board });
    s.bounds.add(lo);
    s.bounds.add(hi);
    Some((s, map))
}

/// 部品のプレビューの視点（部品の境界を囲む球が画面に収まる距離）。zoom は 1 で収まる大きさ
pub fn part_camera(scene: &Scene, elevation_deg: f64, azimuth_deg: f64, zoom: f32, aspect: f32) -> Camera {
    const FOV: f64 = 30.0;
    let center = scene.bounds.center();
    let radius = (scene.bounds.extent().length() * 0.5).max(0.3);
    let fit = radius / (FOV.to_radians() as f32 * 0.5).sin() * if aspect < 1.0 { 1.0 / aspect } else { 1.0 };
    orbit_camera(center, fit * 1.05 / zoom.max(0.05), elevation_deg, azimuth_deg, FOV, aspect)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 見本を何枚か描いて、横に並べた画像を <repo>/out/material_swatches.png に書く（見た目の確かめ用）
    ///   cargo test --release -p krbr-app --lib preview -- --ignored --nocapture
    #[test]
    #[ignore = "needs a Vulkan GPU with ray tracing"]
    fn render_swatches() {
        krbr_core::log::set_level(krbr_core::log::Level::Warn);
        let ctx: &'static Context = Box::leak(Box::new(Context::new(&krbr_render::ContextOptions { low_queue_priority: true, ..Default::default() }).unwrap()));
        let lib = krbr_scene::MaterialLibrary::builtin();
        let names = ["metal_gold", "metal_osmium", "plastic_abs+color=#c03020", "glass_frosted", "pet_clear", "gem_diamond", "gem_ruby", "gem_sapphire", "gem_emerald", "gem_cubic_zirconia"];
        let light = PreviewLight::new();
        let mut p = Preview::new(ctx, swatch_scene(&lib.find_variant(names[0]).unwrap()), &light, SWATCH_PX, SWATCH_PX, swatch_camera(), false).unwrap();
        let n = SWATCH_PX as usize;
        let mut strip = vec![0u8; n * n * 4 * names.len()];
        for (k, name) in names.iter().enumerate() {
            p.set_materials(swatch_materials(&lib.find_variant(name).unwrap())).unwrap();
            for _ in 0..4 {
                p.add_samples(ctx, 32).unwrap();
            }
            let img = p.image(0.0).unwrap();
            for y in 0..n {
                for x in 0..n {
                    let c = img.pixels[y * n + x];
                    let i = (y * n * names.len() + k * n + x) * 4;
                    strip[i..i + 4].copy_from_slice(&[c.r(), c.g(), c.b(), 255]);
                }
            }
        }
        let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../out");
        std::fs::create_dir_all(&out).unwrap();
        krbr_render::image_io::save_png_rgba8(&out.join("material_swatches.png"), (n * names.len()) as u32, n as u32, &strip).unwrap();
    }
}
