//! 材質。パラメータは OpenPBR Surface（v1.1）に準拠する。krbr 独自の拡張は TOML では [material.<名前>.krbr] の下に書く。

use glam::Vec3;

/// 材質の種別。値は shaders/common.glsl の MAT_* と同じにすること
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum MaterialKind {
    Generic = 0,
    /// レジストの下に別の材質（masked_base）がある面。レジストは masked_mask
    Masked = 1,
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

impl BumpPattern {
    pub const ALL: [BumpPattern; 3] = [BumpPattern::None, BumpPattern::Noise, BumpPattern::Weave];

    /// 材質の TOML（[krbr] bump_pattern）での名前
    pub fn as_str(self) -> &'static str {
        match self {
            BumpPattern::None => "none",
            BumpPattern::Noise => "noise",
            BumpPattern::Weave => "weave",
        }
    }
}

impl std::str::FromStr for BumpPattern {
    type Err = String;
    /// 名前（none / noise / weave）から。知らない名前は Err（黙って none にしない）
    fn from_str(s: &str) -> Result<Self, String> {
        Self::ALL.into_iter().find(|p| p.as_str() == s).ok_or_else(|| format!("unknown bump_pattern '{s}' (none / noise / weave)"))
    }
}

/// 材質の分類（材質の一覧でのまとまり。描画には使わない）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Metal,
    Plastic,
    Rubber,
    Ceramic,
    Glass,
    Gem,
    Other,
}

impl Category {
    /// 一覧に出す順
    pub const ALL: [Category; 7] = [Category::Metal, Category::Plastic, Category::Rubber, Category::Ceramic, Category::Glass, Category::Gem, Category::Other];

    /// 材質の TOML（[krbr] category）での名前
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Metal => "metal",
            Category::Plastic => "plastic",
            Category::Rubber => "rubber",
            Category::Ceramic => "ceramic",
            Category::Glass => "glass",
            Category::Gem => "gem",
            Category::Other => "other",
        }
    }
}

impl std::str::FromStr for Category {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        Self::ALL.into_iter().find(|c| c.as_str() == s).ok_or_else(|| format!("unknown category '{s}' (metal / plastic / rubber / ceramic / glass / gem / other)"))
    }
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
    pub masked_base: u32,
    pub masked_mask: u32,

    // ---- OpenPBR Surface（既定値は仕様のとおり）。色はリニア
    /// 例外：レジストの材質（scene.rs の make_mask_material）では、顔料が光を散らす確率に流用する
    /// （shaders/scene.glsl のレジストの処理と対応。材質ライブラリの材質では仕様どおりの意味）
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
    pub bump_pattern: BumpPattern,
    /// 傾きの大きさ（0.05 で約 3°）
    pub bump_strength: f32,
    /// 凹凸の大きさ（mm）
    pub bump_scale: f32,
    /// specular_roughness の相対的なむら（0..1）
    pub roughness_variation: f32,
    /// 指定があればアッベ数より優先
    pub sellmeier: Option<Sellmeier>,
    /// 測定値の複素屈折率を使う金属（crate::conductor の表の番号）。base_color と specular_color はその表から求めた値
    pub conductor: Option<usize>,
    /// 一覧での分類。書かなければパラメータから決める（category()）
    pub category: Option<Category>,
}

impl Default for Material {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: MaterialKind::Generic,
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
            bump_pattern: BumpPattern::None,
            bump_strength: 0.0,
            bump_scale: 0.2,
            roughness_variation: 0.0,
            sellmeier: None,
            conductor: None,
            category: None,
        }
    }
}

impl Material {
    /// 測定値の金属にする：金属にし、base_color（F0）と specular_color（F82-tint）を表から求める
    pub fn set_conductor(&mut self, c: crate::conductor::Conductor) {
        let (f0, tint) = c.f0_and_f82_tint();
        self.conductor = Some(c.index);
        self.base_metalness = 1.0;
        self.base_color = f0;
        self.specular_color = tint;
    }

    /// 一覧での分類（書いていなければ、金属・透明・その他をパラメータから決める）
    pub fn category(&self) -> Category {
        self.category.unwrap_or(if self.base_metalness > 0.5 {
            Category::Metal
        } else if self.transmission_weight > 0.0 {
            Category::Glass
        } else {
            Category::Other
        })
    }

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

/// OpenPBR のパラメータの型と、材質の中の場所
pub enum Param {
    F(fn(&mut Material) -> &mut f32),
    C(fn(&mut Material) -> &mut Vec3),
    B(fn(&mut Material) -> &mut bool),
}

macro_rules! f {
    ($name:literal, $field:ident) => {
        ($name, Param::F(|m: &mut Material| &mut m.$field))
    };
}
macro_rules! c {
    ($name:literal, $field:ident) => {
        ($name, Param::C(|m: &mut Material| &mut m.$field))
    };
}
macro_rules! b {
    ($name:literal, $field:ident) => {
        ($name, Param::B(|m: &mut Material| &mut m.$field))
    };
}

/// OpenPBR Surface のパラメータの一覧（名前は仕様と同じ。材質の TOML の読み書きと、名前で値を設定する set_* が使う）
pub fn params() -> Vec<(&'static str, Param)> {
    vec![
        f!("base_weight", base_weight),
        c!("base_color", base_color),
        f!("base_diffuse_roughness", base_diffuse_roughness),
        f!("base_metalness", base_metalness),
        f!("specular_weight", specular_weight),
        c!("specular_color", specular_color),
        f!("specular_roughness", specular_roughness),
        f!("specular_roughness_anisotropy", specular_roughness_anisotropy),
        f!("specular_ior", specular_ior),
        f!("transmission_weight", transmission_weight),
        c!("transmission_color", transmission_color),
        f!("transmission_depth", transmission_depth),
        c!("transmission_scatter", transmission_scatter),
        f!("transmission_scatter_anisotropy", transmission_scatter_anisotropy),
        f!("transmission_dispersion_scale", transmission_dispersion_scale),
        f!("transmission_dispersion_abbe_number", transmission_dispersion_abbe_number),
        f!("subsurface_weight", subsurface_weight),
        c!("subsurface_color", subsurface_color),
        f!("subsurface_radius", subsurface_radius),
        c!("subsurface_radius_scale", subsurface_radius_scale),
        f!("subsurface_scatter_anisotropy", subsurface_scatter_anisotropy),
        f!("fuzz_weight", fuzz_weight),
        c!("fuzz_color", fuzz_color),
        f!("fuzz_roughness", fuzz_roughness),
        f!("coat_weight", coat_weight),
        c!("coat_color", coat_color),
        f!("coat_roughness", coat_roughness),
        f!("coat_roughness_anisotropy", coat_roughness_anisotropy),
        f!("coat_ior", coat_ior),
        f!("coat_darkening", coat_darkening),
        f!("thin_film_weight", thin_film_weight),
        f!("thin_film_thickness", thin_film_thickness),
        f!("thin_film_ior", thin_film_ior),
        f!("emission_luminance", emission_luminance),
        c!("emission_color", emission_color),
        f!("geometry_opacity", geometry_opacity),
        b!("geometry_thin_walled", geometry_thin_walled),
    ]
}


/// [krbr] の拡張のうち数の項目
const KRBR_FLOATS: &[&str] = &["bump_strength", "bump_scale", "roughness_variation"];

/// 測定値の金属の色を決める項目（書くと測定値の金属ではなくなる）
pub const CONDUCTOR_PARAMS: &[&str] = &["base_color", "specular_color", "base_metalness"];

impl Material {
    /// 数のパラメータを名前で設定する（OpenPBR の名前、真偽値は 0 以外を真、krbr の拡張の bump_strength / bump_scale /
    /// roughness_variation）。色のパラメータに数を渡すと灰色。測定値の金属の色（CONDUCTOR_PARAMS）を書くと測定値の金属
    /// ではなくなる。知らない名前は Err
    pub fn set_float(&mut self, name: &str, v: f32) -> Result<(), String> {
        if let Some((_, p)) = params().into_iter().find(|(n, _)| *n == name) {
            match p {
                Param::F(get) => *get(self) = v,
                Param::C(get) => *get(self) = Vec3::splat(v),
                Param::B(get) => *get(self) = v != 0.0,
            }
            if CONDUCTOR_PARAMS.contains(&name) {
                self.conductor = None;
            }
            return Ok(());
        }
        match name {
            "bump_strength" => self.bump_strength = v,
            "bump_scale" => self.bump_scale = v,
            "roughness_variation" => self.roughness_variation = v,
            _ => return Err(format!("unknown number parameter '{name}'")),
        }
        Ok(())
    }

    /// 色のパラメータ（リニア）を名前で設定する。測定値の金属の色（CONDUCTOR_PARAMS）を書くと測定値の金属ではなくなる
    pub fn set_color(&mut self, name: &str, c: Vec3) -> Result<(), String> {
        match params().into_iter().find(|(n, _)| *n == name) {
            Some((_, Param::C(get))) => *get(self) = c,
            _ => return Err(format!("unknown color parameter '{name}'")),
        }
        if CONDUCTOR_PARAMS.contains(&name) {
            self.conductor = None;
        }
        Ok(())
    }

    /// 文字列の項目（krbr の拡張）を名前で設定する：bump_pattern（none / noise / weave）、conductor（測定値の金属の名前。
    /// "none" でやめる）、category。知らない名前・値は Err
    pub fn set_string(&mut self, name: &str, v: &str) -> Result<(), String> {
        match name {
            "bump_pattern" => self.bump_pattern = v.parse()?,
            "category" => self.category = Some(v.parse()?),
            "conductor" if v == "none" => self.conductor = None,
            "conductor" => self.set_conductor(crate::conductor::find(v).ok_or_else(|| format!("unknown conductor '{v}' ({})", crate::conductor::all().map(|c| c.id).collect::<Vec<_>>().join(" / ")))?),
            _ if KRBR_FLOATS.contains(&name) || params().iter().any(|(n, _)| *n == name) => return Err(format!("'{name}' is not a string parameter")),
            _ => return Err(format!("unknown parameter '{name}'")),
        }
        Ok(())
    }
}

// sRGB とリニアの変換・16 進の色の解析は krbr-core（3D モデルの面の色と共有）に 1 つだけ置く。0..1 に収めてから変換する
pub use krbr_core::color::{linear_to_srgb, linear_to_srgb_f, parse_hex_rgb, srgb_to_linear, srgb_to_linear_f};

#[cfg(test)]
mod tests {
    use super::*;

    /// 名前で設定する：OpenPBR の数・色・真偽値と krbr の拡張。測定値の金属の色を書くと測定値の金属ではなくなる
    #[test]
    fn set_by_name() {
        let mut m = Material::default();
        m.set_float("specular_roughness", 0.1).unwrap();
        m.set_color("base_color", Vec3::new(0.1, 0.2, 0.3)).unwrap();
        m.set_float("coat_color", 0.5).unwrap();
        m.set_float("geometry_thin_walled", 1.0).unwrap();
        m.set_float("bump_strength", 0.05).unwrap();
        m.set_string("bump_pattern", "noise").unwrap();
        assert_eq!((m.specular_roughness, m.base_color, m.coat_color), (0.1, Vec3::new(0.1, 0.2, 0.3), Vec3::splat(0.5)));
        assert!(m.geometry_thin_walled && m.bump_strength == 0.05 && m.bump_pattern == BumpPattern::Noise);
        assert!(m.set_float("no_such_thing", 1.0).is_err());
        assert!(m.set_color("specular_roughness", Vec3::ONE).is_err());
        assert!(m.set_string("bump_pattern", "zigzag").is_err());
        assert!(m.set_string("base_weight", "1").is_err());

        m.set_string("conductor", "Au").unwrap();
        assert!(m.conductor.is_some() && m.base_metalness == 1.0);
        m.set_float("base_metalness", 1.0).unwrap();
        assert_eq!(m.conductor, None);
        m.set_string("conductor", "Au").unwrap();
        m.set_string("conductor", "none").unwrap();
        assert_eq!(m.conductor, None);
    }

    #[test]
    fn abbe_number_dispersion() {
        let m = Material { specular_ior: 1.491, transmission_dispersion_scale: 1.0, transmission_dispersion_abbe_number: 57.0, ..Default::default() };
        let (nf, nc, nd) = (m.ior_at(486.1), m.ior_at(656.3), m.ior_at(587.6));
        assert!((nd - 1.491).abs() < 1e-4);
        assert!(((nd - 1.0) / (nf - nc) - 57.0).abs() < 0.05);
        let no_dispersion = Material { transmission_dispersion_scale: 0.0, ..m.clone() };
        assert_eq!(no_dispersion.ior_at(400.0), 1.491);
        let plain = Material::default();
        assert_eq!(plain.ior_at(400.0), 1.5);
    }

    #[test]
    fn bump_pattern_names() {
        for p in BumpPattern::ALL {
            assert_eq!(p.as_str().parse::<BumpPattern>(), Ok(p));
        }
        assert!("Noise".parse::<BumpPattern>().is_err());
    }

    #[test]
    fn srgb_round_trip() {
        for v in [0.0f32, 0.1, 0.5, 0.9, 1.0] {
            assert!((linear_to_srgb_f(srgb_to_linear_f(v)) - v).abs() < 1e-5);
        }
    }
}
