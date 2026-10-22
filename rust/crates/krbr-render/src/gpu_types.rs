//! GPU に送る構造体。shaders/common.glsl（Vertex, MeshInfo, Material, InstanceInfo, Frame）、lights.glsl（Emitter）、
//! bidir.glsl（LightVertex）と同じ並びにすること。大きさは const assert で確かめる（隙間が無いことの確認も兼ねる）。
//! シェーダと共有する定数（バインディング番号など）は shader_consts.rs

use glam::{UVec4, Vec3, Vec4};
use krbr_scene::material::{Material, MaterialKind};
use krbr_scene::spectrum::Rgb2SpecTable;

use crate::shader_consts::{LUMINANCE_WEIGHTS, MAX_LENS_SURFACES};
use crate::vk::GpuPod;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuVertex {
    pub pos: Vec3,
    pub pad0: f32,
    pub nrm: Vec3,
    pub pad1: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuMeshInfo {
    pub vertex_offset: u32,
    pub index_offset: u32,
    pub tri_offset: u32,
    pub pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuMaterial {
    pub base_color: Vec4,
    pub spec_color: Vec4,
    pub params0: Vec4,
    pub coat_color: Vec4,
    pub params1: Vec4,
    pub params2: Vec4,
    pub emission: Vec4,
    pub transmission: Vec4,
    pub transmission2: Vec4,
    pub subsurface: Vec4,
    pub params3: Vec4,
    pub spec_base: Vec4,
    pub spec_specular: Vec4,
    pub spec_coat: Vec4,
    pub spec_trans: Vec4,
    pub spec_subsurface: Vec4,
    pub spec_emission: Vec4,
}

/// 発光の単位：放射輝度 1.0 を 1000 cd/m² とみなす
pub const EMISSION_SCALE: f32 = 1.0 / 1000.0;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuInstanceInfo {
    pub mesh: u32,
    pub material_base: u32,
    pub pad0: u32,
    pub pad1: u32,
    /// ワールド → オブジェクトの 3x3（列）
    pub w2o: [Vec4; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuEmitter {
    /// w: 材質の番号（float に詰めたビット）
    pub p0: Vec4,
    /// w: 面積（mm²）
    pub p1: Vec4,
    pub p2: Vec4,
}

/// BDPT / VCM の光源側の頂点（bidir.glsl の LightVertex。GPU だけが読み書きし、Rust はバッファの大きさにだけ使う）
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuLightVertex {
    pub pos_inst: Vec4,
    pub p_obj_mat: Vec4,
    pub dirs: UVec4,
    pub throughput: Vec4,
    pub mis: Vec4,
    pub links: UVec4,
}

/// 輝度（リニア sRGB の Y。重みはシェーダの luminanceOf と共通）
pub fn luminance(c: Vec3) -> f32 {
    c.dot(Vec3::from_array(LUMINANCE_WEIGHTS))
}

/// 材質の発光の放射輝度（RGB）。GPU の材質（emission）と発光体の表（build_emitters）の両方がこれを使う。
/// 単位の換算（EMISSION_SCALE）と輝度の重み（シェーダと共有する LUMINANCE_WEIGHTS）は描画側の約束なので、
/// 材質を定義する krbr-scene ではなくここに置く（使うのも krbr-render の中だけ）
pub fn emission_radiance(m: &Material) -> Vec3 {
    m.emission_color * (m.emission_luminance * EMISSION_SCALE)
}

pub fn to_gpu(m: &Material, spec: Option<&Rgb2SpecTable>) -> GpuMaterial {
    let masked = m.kind == MaterialKind::Masked;
    // 分散：nF, nC から Cauchy の式の係数を求める
    let (nf, nc, nd) = (m.ior_at(486.1), m.ior_at(656.3), m.ior_at(587.6));
    let dispersive = (nf - nc).abs() > 1e-6;
    let b = if dispersive { (nf - nc) / (1.0 / (486.1f32 * 486.1) - 1.0 / (656.3f32 * 656.3)) } else { 0.0 };
    let a = nd - b / (587.6f32 * 587.6);
    let emission = emission_radiance(m);
    let mut g = GpuMaterial {
        base_color: m.base_color.extend(m.base_weight),
        spec_color: m.specular_color.extend(m.specular_weight),
        params0: Vec4::new(m.base_metalness, m.specular_roughness, m.specular_ior, m.coat_weight),
        coat_color: m.coat_color.extend(m.coat_roughness),
        params1: Vec4::new(m.coat_ior, m.kind as u32 as f32, if masked { m.masked_base as f32 } else { 0.0 }, if masked { m.masked_mask as f32 } else { 0.0 }),
        params2: Vec4::new(m.bump_pattern as u32 as f32, m.bump_strength, m.bump_scale, m.roughness_variation),
        emission: emission.extend(m.geometry_opacity),
        transmission: Vec4::new(m.transmission_weight, m.transmission_dispersion_abbe_number, m.transmission_dispersion_scale, if m.geometry_thin_walled { 1.0 } else { 0.0 }),
        transmission2: m.transmission_color.extend(m.transmission_depth),
        subsurface: m.subsurface_color.extend(m.subsurface_radius),
        params3: Vec4::new(m.subsurface_weight, a, b, if dispersive { 1.0 } else { 0.0 }),
        ..Default::default()
    };
    if let Some(t) = spec {
        let coef = |c: Vec3| t.lookup(c).extend(0.0);
        g.spec_base = coef(m.base_color);
        g.spec_specular = coef(m.specular_color);
        g.spec_coat = coef(m.coat_color);
        g.spec_trans = coef(m.transmission_color);
        g.spec_subsurface = coef(m.subsurface_color);
        g.spec_emission = t.unbounded(emission);
    }
    g
}

/// フレームの定数（common.glsl の uniform Frame。std140 で、どのフィールドも 16 バイト単位）
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameUbo {
    pub cam_pos: Vec4,
    pub cam_right: Vec4,
    pub cam_up: Vec4,
    pub cam_forward: Vec4,
    pub board_map: Vec4,
    pub env_params: Vec4,
    pub dims: UVec4,
    pub counts: UVec4,
    pub mask_params: Vec4,
    pub mask_params2: Vec4,
    pub spectral: Vec4,
    pub rgb_to_xyz: [Vec4; 3],
    pub light_params: Vec4,
    pub scene_center: Vec4,
    pub vcm_params: Vec4,
    pub lens0: Vec4,
    pub lens1: Vec4,
    pub lens2: Vec4,
    pub lens3: Vec4,
    pub lens4: Vec4,
    pub lens_surfaces: [Vec4; MAX_LENS_SURFACES],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PushConstants {
    pub sample_index: u32,
    pub seed: u32,
}

// 大きさ（GLSL の std430 / std140 での大きさと同じ。隙間があればここで分かる）
const _: () = assert!(std::mem::size_of::<GpuVertex>() == 32);
const _: () = assert!(std::mem::size_of::<GpuMeshInfo>() == 16);
const _: () = assert!(std::mem::size_of::<GpuMaterial>() == 17 * 16);
const _: () = assert!(std::mem::size_of::<GpuInstanceInfo>() == 64);
const _: () = assert!(std::mem::size_of::<GpuEmitter>() == 48);
const _: () = assert!(std::mem::size_of::<GpuLightVertex>() == 96);
const _: () = assert!(std::mem::size_of::<FrameUbo>() == (22 + MAX_LENS_SURFACES) * 16);
const _: () = assert!(std::mem::size_of::<PushConstants>() == 8);

// 安全性：どれも repr(C) で、上の assert により隙間が無い（パディングはフィールドとして明示してある）
unsafe impl GpuPod for GpuVertex {}
unsafe impl GpuPod for GpuMeshInfo {}
unsafe impl GpuPod for GpuMaterial {}
unsafe impl GpuPod for GpuInstanceInfo {}
unsafe impl GpuPod for GpuEmitter {}
unsafe impl GpuPod for GpuLightVertex {}
unsafe impl GpuPod for FrameUbo {}
unsafe impl GpuPod for PushConstants {}
