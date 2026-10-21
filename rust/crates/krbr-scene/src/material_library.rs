//! 名前付きの材質の一覧。組み込みの材質に、全体設定の materials/*.toml を重ねる（同じ名前は書いた項目だけ上書き）

use std::collections::BTreeMap;
use std::path::Path;

use glam::Vec3;

use crate::material::{srgb_to_linear, Material};
use crate::material_io::read_material;

/// 材質名の後ろに付けると、表面の模様（凹凸と粗さのむら）を無くした版になる（例 "plastic+smooth"）。Rust 版だけの書き方
pub const SMOOTH_SUFFIX: &str = "+smooth";
/// 材質名の後ろに付けると、発光する版になる（例 "plastic+emit=#ff2010,5000"：sRGB の色と輝度 cd/m²）。LED の点灯など
pub const EMIT_PREFIX: &str = "+emit=";

/// 発光する部位の地の色（拡散の反射率）：発光の色を最も明るい成分がこの値になるようにしたもの
pub const EMISSIVE_BASE_ALBEDO: f32 = 0.7;

/// 発光の指定（材質名の "+emit=#rrggbb,<cd/m²>"）
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Emission {
    /// 表示の色（sRGB）
    pub srgb: [u8; 3],
    /// 輝度（cd/m²）
    pub luminance: f32,
}

impl Emission {
    /// "#rrggbb,<cd/m²>"（# は省略可）
    pub fn parse(s: &str) -> Option<Self> {
        let (color, lum) = s.split_once(',')?;
        let hex = color.trim().trim_start_matches('#');
        if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        let luminance: f32 = lum.trim().parse().ok().filter(|v: &f32| v.is_finite() && *v >= 0.0)?;
        Some(Self { srgb: [byte(0)?, byte(2)?, byte(4)?], luminance })
    }

    pub fn suffix(&self) -> String {
        let [r, g, b] = self.srgb;
        format!("{EMIT_PREFIX}#{r:02x}{g:02x}{b:02x},{}", self.luminance)
    }

    /// 発光の色（リニア）
    pub fn linear_color(&self) -> Vec3 {
        srgb_to_linear(Vec3::new(self.srgb[0] as f32, self.srgb[1] as f32, self.srgb[2] as f32) / 255.0)
    }
}

/// 材質名を（ライブラリの材質名, 模様なしか, 発光）に分ける。後ろに付いた "+smooth" と "+emit=..." を外す
pub fn parse_variant(name: &str) -> (&str, bool, Option<Emission>) {
    let (mut base, mut smooth, mut emission) = (name, false, None);
    loop {
        if let Some(b) = base.strip_suffix(SMOOTH_SUFFIX) {
            (base, smooth) = (b, true);
        } else if let Some((b, e)) = base.rsplit_once(EMIT_PREFIX).and_then(|(b, s)| Emission::parse(s).map(|e| (b, e))) {
            (base, emission) = (b, emission.or(Some(e)));
        } else {
            return (base, smooth, emission);
        }
    }
}

/// 材質名を組み立てる（"<材質>[+smooth][+emit=...]"）
pub fn variant_name(base: &str, smooth: bool, emission: Option<Emission>) -> String {
    let mut s = base.to_string();
    if smooth {
        s += SMOOTH_SUFFIX;
    }
    if let Some(e) = emission {
        s += &e.suffix();
    }
    s
}

/// 名前を（ライブラリの材質名, 模様なしか）に分ける（発光の指定は外す）
pub fn split_variant(name: &str) -> (&str, bool) {
    let (base, smooth, _) = parse_variant(name);
    (base, smooth)
}

/// 模様のあり・なしを切り替えた名前（発光の指定は保つ）
pub fn with_smooth(name: &str, smooth: bool) -> String {
    let (base, _, emission) = parse_variant(name);
    variant_name(base, smooth, emission)
}

/// 発光を付け替えた名前（None なら外す。模様のあり・なしは保つ）
pub fn with_emission(name: &str, emission: Option<Emission>) -> String {
    let (base, smooth, _) = parse_variant(name);
    variant_name(base, smooth, emission)
}

/// krbr 組み込みの材質（OpenPBR Surface）。色はリニア。base_color を書かなければ 3D モデルの面の色を使う
pub const BUILTIN_MATERIALS: &str = r#"# krbr 組み込みの材質（OpenPBR Surface）。色はリニア
# 全体設定の materials/*.toml に同じ名前を書くと、書いた項目だけ上書きされる
# roughness_variation（粗さのむら）は 0.1〜0.8 mm 周期の模様。引きの画では画素と同じくらいの細かさになり
# 砂粒のようなノイズに見えるので、部品ではごく弱くしている（部品の質感は specular_roughness で表す）

[material.metal_tin]           # 錫めっきのリード・端子
base_metalness = 1.0
specular_roughness = 0.28
[material.metal_tin.krbr]
bump_pattern = "noise"
bump_strength = 0.012
bump_scale = 0.08
roughness_variation = 0.03

[material.metal_gold]          # 金めっきの端子
base_metalness = 1.0
specular_roughness = 0.22
[material.metal_gold.krbr]
bump_pattern = "noise"
bump_strength = 0.01
bump_scale = 0.08
roughness_variation = 0.03

[material.plastic]             # 樹脂のボディ
specular_roughness = 0.45
specular_ior = 1.5
[material.plastic.krbr]
bump_pattern = "noise"
bump_strength = 0.012
bump_scale = 0.3
roughness_variation = 0.02

[material.glossy_plastic]      # 白い樹脂など、つやのあるもの
specular_roughness = 0.3
specular_ior = 1.5
[material.glossy_plastic.krbr]
bump_pattern = "noise"
bump_strength = 0.01
bump_scale = 0.3
roughness_variation = 0.02

[material.epoxy_black]         # IC のモールド樹脂（つや消しの黒）
base_color = [0.018, 0.018, 0.018]
specular_roughness = 0.55
specular_ior = 1.55
[material.epoxy_black.krbr]
bump_pattern = "noise"
bump_strength = 0.015
bump_scale = 0.1
roughness_variation = 0.03

[material.ceramic]             # チップ部品のセラミック
specular_roughness = 0.4
specular_ior = 1.6
[material.ceramic.krbr]
bump_pattern = "noise"
bump_strength = 0.015
bump_scale = 0.05
roughness_variation = 0.03

[material.aluminum]            # 電解コンデンサの缶など
base_color = [0.91, 0.92, 0.92]
base_metalness = 1.0
specular_roughness = 0.35
[material.aluminum.krbr]
bump_pattern = "noise"
bump_strength = 0.01
bump_scale = 0.2
roughness_variation = 0.03

[material.acrylic]             # アクリル（PMMA）
base_color = [1.0, 1.0, 1.0]
specular_roughness = 0.0
specular_ior = 1.491
transmission_weight = 1.0
transmission_dispersion_scale = 1.0
transmission_dispersion_abbe_number = 57.0

[material.polycarbonate]       # ポリカーボネート
base_color = [1.0, 1.0, 1.0]
specular_roughness = 0.0
specular_ior = 1.585
transmission_weight = 1.0
transmission_dispersion_scale = 1.0
transmission_dispersion_abbe_number = 30.0
"#;

#[derive(Debug, Clone, Default)]
pub struct MaterialLibrary {
    materials: BTreeMap<String, Material>,
    sources: BTreeMap<String, String>,
}

impl MaterialLibrary {
    pub fn builtin() -> Self {
        let mut lib = Self::default();
        if let Err(e) = lib.load_text(BUILTIN_MATERIALS, "builtin") {
            krbr_core::error!("builtin materials: {}", e);
        }
        lib
    }

    /// 組み込みの材質と、config_dir/materials/*.toml を読む
    pub fn load(config_dir: &Path) -> Self {
        let mut lib = Self::builtin();
        let dir = config_dir.join("materials");
        let Ok(entries) = std::fs::read_dir(&dir) else { return lib };
        let mut files: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.is_file() && p.extension().map(|e| e == "toml").unwrap_or(false)).collect();
        files.sort();
        for f in files {
            match lib.load_file(&f) {
                Ok(()) => krbr_core::info!("materials: {}", f.display()),
                Err(e) => krbr_core::warn!("materials {}: {} - ignored", f.display(), e),
            }
        }
        lib
    }

    pub fn load_file(&mut self, p: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(p).map_err(|_| "cannot read".to_string())?;
        self.load_text(&text, &p.to_string_lossy())
    }

    pub fn load_text(&mut self, text: &str, source: &str) -> Result<(), String> {
        let root: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
        let Some(toml::Value::Table(mats)) = root.get("material") else { return Ok(()) };
        for (name, node) in mats {
            let toml::Value::Table(t) = node else { continue };
            let mut m = match self.materials.get(name) {
                Some(existing) => existing.clone(),
                None => Material { name: name.clone(), base_color_from_model: true, ..Default::default() }, // base_color が無ければモデルの色
            };
            let mut unknown = Vec::new();
            read_material(t, &mut m, &mut unknown);
            if t.contains_key("base_color") {
                m.base_color_from_model = false;
            }
            if let Some(toml::Value::Table(k)) = t.get("krbr") {
                if let Some(toml::Value::Boolean(v)) = k.get("base_color_from_model") {
                    m.base_color_from_model = *v;
                }
            }
            for u in unknown {
                krbr_core::warn!("{}: material '{}': unknown parameter '{}'", source, name, u);
            }
            self.materials.insert(name.clone(), m);
            self.sources.insert(name.clone(), source.to_string());
        }
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<&Material> {
        self.materials.get(name)
    }

    /// 名前の材質を作る。名前の後ろの「+smooth」は、表面の模様（凹凸と粗さのむら）を無くした版、
    /// 「+emit=#rrggbb,<cd/m²>」は、その色と輝度で光る版
    pub fn find_variant(&self, name: &str) -> Option<Material> {
        let (base, smooth, emission) = parse_variant(name);
        let mut m = self.materials.get(base)?.clone();
        if smooth {
            m.bump_pattern = crate::material::BumpPattern::None;
            m.bump_strength = 0.0;
            m.roughness_variation = 0.0;
        }
        if let Some(e) = emission {
            m.emission_color = e.linear_color();
            m.emission_luminance = e.luminance;
            // 光っている部位の地の色は発光の色にする（色のついた拡散レンズ）。白い地のままだと、周りの光を白く返して発光の色を薄める
            let c = m.emission_color;
            m.base_color = c / c.max_element().max(1e-6) * EMISSIVE_BASE_ALBEDO;
            m.base_color_from_model = false;
        }
        Some(m)
    }

    pub fn names(&self) -> Vec<String> {
        self.materials.keys().cloned().collect()
    }

    pub fn source_of(&self, name: &str) -> &str {
        self.sources.get(name).map(|s| s.as_str()).unwrap_or("")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smooth_variant_drops_the_surface_pattern() {
        let lib = MaterialLibrary::builtin();
        let base = lib.find("plastic").unwrap();
        assert_ne!(base.bump_pattern, crate::material::BumpPattern::None);
        let s = lib.find_variant("plastic+smooth").unwrap();
        assert_eq!(s.bump_pattern, crate::material::BumpPattern::None);
        assert_eq!((s.bump_strength, s.roughness_variation), (0.0, 0.0));
        assert_eq!(s.specular_roughness, base.specular_roughness); // 模様以外はそのまま
        assert!(lib.find_variant("no_such+smooth").is_none());
        assert_eq!(with_smooth("plastic", true), "plastic+smooth");
        assert_eq!(with_smooth("plastic+smooth", false), "plastic");
        assert_eq!(split_variant("metal_tin+smooth"), ("metal_tin", true));
    }

    #[test]
    fn emission_variant() {
        let lib = MaterialLibrary::builtin();
        let m = lib.find_variant("plastic+emit=#ff8000,5000").unwrap();
        assert_eq!(m.emission_luminance, 5000.0);
        assert!((m.emission_color - Vec3::new(1.0, srgb_to_linear(Vec3::splat(128.0 / 255.0)).x, 0.0)).abs().max_element() < 1e-6);
        assert_eq!(m.specular_roughness, lib.find("plastic").unwrap().specular_roughness); // 発光以外はそのまま
        assert_eq!(lib.find("plastic").unwrap().emission_luminance, 0.0);
        // 地の色は発光の色（モデルの色は使わない）
        assert!(!m.base_color_from_model);
        assert!((m.base_color.x - EMISSIVE_BASE_ALBEDO).abs() < 1e-6 && m.base_color.z == 0.0);
        // # は省略可。模様なしと組み合わせられる（どちらの順でも）
        for name in ["plastic+smooth+emit=ff8000,5000", "plastic+emit=#FF8000,5000+smooth"] {
            let (base, smooth, e) = parse_variant(name);
            assert_eq!((base, smooth, e), ("plastic", true, Some(Emission { srgb: [255, 128, 0], luminance: 5000.0 })));
        }
        // 書き方の誤りは材質名の一部とみなす（ライブラリに無いので見つからない）
        for bad in ["plastic+emit=#ff80,5000", "plastic+emit=#ff8000", "plastic+emit=#ff8000,-1", "plastic+emit=#gg8000,1"] {
            assert!(lib.find_variant(bad).is_none(), "{bad}");
        }
        // 組み立てと付け替え
        let e = Emission { srgb: [0x20, 0xff, 0x40], luminance: 2500.0 };
        assert_eq!(variant_name("plastic", true, Some(e)), "plastic+smooth+emit=#20ff40,2500");
        assert_eq!(with_emission("plastic+smooth", Some(e)), "plastic+smooth+emit=#20ff40,2500");
        assert_eq!(with_emission("plastic+smooth+emit=#20ff40,2500", None), "plastic+smooth");
        assert_eq!(with_smooth("plastic+emit=#20ff40,2500", false), "plastic+emit=#20ff40,2500");
        assert_eq!(split_variant("plastic+smooth+emit=#20ff40,2500"), ("plastic", true));
        assert_eq!(Emission::parse("#20ff40,0.5").unwrap().suffix(), "+emit=#20ff40,0.5");
    }

    #[test]
    fn builtin_and_overlay() {
        let mut lib = MaterialLibrary::builtin();
        assert!(lib.find("metal_tin").unwrap().base_metalness == 1.0);
        let bump = lib.find("plastic").unwrap().bump_strength;
        assert!(lib.find("plastic").unwrap().base_color_from_model);
        assert!(!lib.find("epoxy_black").unwrap().base_color_from_model);
        lib.load_text("[material.plastic]\nspecular_roughness = 0.9\n[material.mine]\nbase_color = [1, 0, 0]\n", "test").unwrap();
        let p = lib.find("plastic").unwrap();
        assert_eq!(p.specular_roughness, 0.9);
        assert!(p.base_color_from_model); // 書いた項目だけ上書き
        assert_eq!(p.bump_strength, bump);
        assert_eq!(lib.source_of("mine"), "test");
        assert_eq!(lib.find("mine").unwrap().base_color.x, 1.0);
    }
}
