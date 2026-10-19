//! シェーダ（shaders/common.glsl）と一致させる GPU 用の構造体（C++ 版 render/gpu_types.hpp と同じ並び）

use glam::{UVec4, Vec3, Vec4};
use krbr_scene::material::{Material, MaterialKind};
use krbr_scene::spectrum::Rgb2SpecTable;

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

const _: () = assert!(std::mem::size_of::<GpuMaterial>() == 272);
const _: () = assert!(std::mem::size_of::<GpuVertex>() == 32);

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

pub fn to_gpu(m: &Material, spec: Option<&Rgb2SpecTable>) -> GpuMaterial {
    let masked = m.kind == MaterialKind::Masked;
    // 分散：nF, nC から Cauchy の式の係数を求める
    let (nf, nc, nd) = (m.ior_at(486.1), m.ior_at(656.3), m.ior_at(587.6));
    let dispersive = (nf - nc).abs() > 1e-6;
    let b = if dispersive { (nf - nc) / (1.0 / (486.1f32 * 486.1) - 1.0 / (656.3f32 * 656.3)) } else { 0.0 };
    let a = nd - b / (587.6f32 * 587.6);
    let emission = m.emission_color * (m.emission_luminance * EMISSION_SCALE);
    let mut g = GpuMaterial {
        base_color: m.base_color.extend(m.base_weight),
        spec_color: m.specular_color.extend(m.specular_weight),
        params0: Vec4::new(m.base_metalness, m.specular_roughness, m.specular_ior, m.coat_weight),
        coat_color: m.coat_color.extend(m.coat_roughness),
        params1: Vec4::new(m.coat_ior, m.kind as u32 as f32, if masked { m.masked_base as f32 } else { m.board_side as f32 }, if masked { m.masked_mask as f32 } else { 0.0 }),
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
    pub board_mats: UVec4,
    pub board_mats2: UVec4,
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
    pub lens_surfaces: [Vec4; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PushConstants {
    pub sample_index: u32,
    pub seed: u32,
}
