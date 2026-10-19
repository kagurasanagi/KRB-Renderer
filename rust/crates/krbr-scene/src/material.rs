//! 材質。パラメータは OpenPBR Surface（v1.1）に準拠する。krbr 独自の拡張は TOML では [material.<名前>.krbr] の下に書く。

use glam::Vec3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MaterialKind {
    Generic = 0,
    /// 層テクスチャで材質を決める基板面（凹凸を形状にしない場合）
    BoardSurface = 1,
    /// レジストの下に別の材質（masked_base）がある面。レジストは masked_mask
    Masked = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BumpPattern {
    None = 0,
    /// 等方的なノイズ（塗膜のゆず肌・粒状感・樹脂のむら）
    Noise = 1,
    /// ガラス繊維の織り目（FR4）。bump_scale が織りのピッチ
    Weave = 2,
}

/// Sellmeier の分散式 n²(λ) = 1 + Σ B_i λ² / (λ² − C_i)（λ は µm、C_i は µm²）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sellmeier {
    pub b: [f32; 3],
    pub c: [f32; 3],
}

impl Sellmeier {
    pub fn ior(&self, lambda_um: f32) -> f32 {
        let l2 = lambda_um * lambda_um;
        let mut n2 = 1.0;
        for i in 0..3 {
            n2 += self.b[i] * l2 / (l2 - self.c[i]);
        }
        n2.max(1.0).sqrt()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Material {
    pub name: String,
    pub kind: MaterialKind,
    /// BoardSurface 用（0=表, 1=裏）
    pub board_side: u32,
    pub masked_base: u32,
    pub masked_mask: u32,

    // ---- OpenPBR Surface（既定値は仕様のとおり）。色はリニア
    pub base_weight: f32,
    pub base_color: Vec3,
    pub base_diffuse_roughness: f32,
    pub base_metalness: f32,

    pub specular_weight: f32,
    pub specular_color: Vec3,
    pub specular_roughness: f32,
    pub specular_roughness_anisotropy: f32,
    /// 分散がある場合は nd（587.6 nm）として扱う
    pub specular_ior: f32,

    pub transmission_weight: f32,
    pub transmission_color: Vec3,
    pub transmission_depth: f32,
    pub transmission_scatter: Vec3,
    pub transmission_scatter_anisotropy: f32,
    pub transmission_dispersion_scale: f32,
    pub transmission_dispersion_abbe_number: f32,

    pub subsurface_weight: f32,
    pub subsurface_color: Vec3,
    pub subsurface_radius: f32,
    pub subsurface_radius_scale: Vec3,
    pub subsurface_scatter_anisotropy: f32,

    pub fuzz_weight: f32,
    pub fuzz_color: Vec3,
    pub fuzz_roughness: f32,

    pub coat_weight: f32,
    pub coat_color: Vec3,
    pub coat_roughness: f32,
    pub coat_roughness_anisotropy: f32,
    pub coat_ior: f32,
    pub coat_darkening: f32,

    pub thin_film_weight: f32,
    /// µm
    pub thin_film_thickness: f32,
    pub thin_film_ior: f32,

    /// cd/m²
    pub emission_luminance: f32,
    pub emission_color: Vec3,

    pub geometry_opacity: f32,
    pub geometry_thin_walled: bool,

    // ---- krbr 独自の拡張（TOML では [krbr] の下）
    /// base_color を 3D モデルの面の色にする（材質ライブラリで base_color 省略時）
    pub base_color_from_model: bool,
    pub bump_pattern: BumpPattern,
    /// 傾きの大きさ（0.05 で約 3°）
    pub bump_strength: f32,
    /// 凹凸の大きさ（mm）
    pub bump_scale: f32,
    /// specular_roughness の相対的なむら（0..1）
    pub roughness_variation: f32,
    /// 指定があればアッベ数より優先
    pub sellmeier: Option<Sellmeier>,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: MaterialKind::Generic,
            board_side: 0,
            masked_base: 0,
            masked_mask: 0,
            base_weight: 1.0,
            base_color: Vec3::splat(0.8),
            base_diffuse_roughness: 0.0,
            base_metalness: 0.0,
            specular_weight: 1.0,
            specular_color: Vec3::ONE,
            specular_roughness: 0.3,
            specular_roughness_anisotropy: 0.0,
            specular_ior: 1.5,
            transmission_weight: 0.0,
            transmission_color: Vec3::ONE,
            transmission_depth: 0.0,
            transmission_scatter: Vec3::ZERO,
            transmission_scatter_anisotropy: 0.0,
            transmission_dispersion_scale: 0.0,
            transmission_dispersion_abbe_number: 20.0,
            subsurface_weight: 0.0,
            subsurface_color: Vec3::splat(0.8),
            subsurface_radius: 1.0,
            subsurface_radius_scale: Vec3::new(1.0, 0.5, 0.25),
            subsurface_scatter_anisotropy: 0.0,
            fuzz_weight: 0.0,
            fuzz_color: Vec3::ONE,
            fuzz_roughness: 0.5,
            coat_weight: 0.0,
            coat_color: Vec3::ONE,
            coat_roughness: 0.0,
            coat_roughness_anisotropy: 0.0,
            coat_ior: 1.6,
            coat_darkening: 1.0,
            thin_film_weight: 0.0,
            thin_film_thickness: 0.5,
            thin_film_ior: 1.4,
            emission_luminance: 0.0,
            emission_color: Vec3::ONE,
            geometry_opacity: 1.0,
            geometry_thin_walled: false,
            base_color_from_model: false,
            bump_pattern: BumpPattern::None,
            bump_strength: 0.0,
            bump_scale: 0.2,
            roughness_variation: 0.0,
            sellmeier: None,
        }
    }
}

impl Material {
    /// 波長 λ（nm）での屈折率。分散がなければ specular_ior
    pub fn ior_at(&self, lambda_nm: f32) -> f32 {
        if let Some(s) = &self.sellmeier {
            return s.ior(lambda_nm * 1e-3);
        }
        if self.transmission_dispersion_scale <= 0.0 || self.transmission_dispersion_abbe_number <= 0.0 {
            return self.specular_ior;
        }
        // OpenPBR：アッベ数 V = (nd − 1) / (nF − nC) と Cauchy の式 n(λ) = A + B/λ² から
        const LF: f32 = 486.1;
        const LC: f32 = 656.3;
        const LD: f32 = 587.6;
        let v = self.transmission_dispersion_abbe_number / self.transmission_dispersion_scale;
        let b = (self.specular_ior - 1.0) / (v * (1.0 / (LF * LF) - 1.0 / (LC * LC)));
        let a = self.specular_ior - b / (LD * LD);
        a + b / (lambda_nm * lambda_nm)
    }
}

/// sRGB（表示値）からリニアへ
pub fn srgb_to_linear_f(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn srgb_to_linear(c: Vec3) -> Vec3 {
    Vec3::new(srgb_to_linear_f(c.x), srgb_to_linear_f(c.y), srgb_to_linear_f(c.z))
}

pub fn linear_to_srgb_f(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

pub fn linear_to_srgb(c: Vec3) -> Vec3 {
    Vec3::new(linear_to_srgb_f(c.x), linear_to_srgb_f(c.y), linear_to_srgb_f(c.z))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abbe_number_dispersion() {
        let m = Material { specular_ior: 1.491, transmission_dispersion_scale: 1.0, transmission_dispersion_abbe_number: 57.0, ..Default::default() };
        let (nf, nc, nd) = (m.ior_at(486.1), m.ior_at(656.3), m.ior_at(587.6));
        assert!((nd - 1.491).abs() < 1e-4);
        assert!(((nd - 1.0) / (nf - nc) - 57.0).abs() < 0.05);
        let plain = Material::default();
        assert_eq!(plain.ior_at(400.0), 1.5);
    }

    #[test]
    fn srgb_round_trip() {
        for v in [0.0f32, 0.1, 0.5, 0.9, 1.0] {
            assert!((linear_to_srgb_f(srgb_to_linear_f(v)) - v).abs() < 1e-5);
        }
    }
}
