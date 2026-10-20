//! 名前付きの材質の一覧。組み込みの材質に、全体設定の materials/*.toml を重ねる（同じ名前は書いた項目だけ上書き）

use std::collections::BTreeMap;
use std::path::Path;

use crate::material::Material;
use crate::material_io::read_material;

/// 材質名の後ろに付けると、表面の模様（凹凸と粗さのむら）を無くした版になる（例 "plastic+smooth"）。Rust 版だけの書き方
pub const SMOOTH_SUFFIX: &str = "+smooth";

/// 名前を（ライブラリの材質名, 模様なしか）に分ける
pub fn split_variant(name: &str) -> (&str, bool) {
    match name.strip_suffix(SMOOTH_SUFFIX) {
        Some(base) => (base, true),
        None => (name, false),
    }
}

/// 模様のあり・なしを切り替えた名前
pub fn with_smooth(name: &str, smooth: bool) -> String {
    let (base, _) = split_variant(name);
    if smooth { format!("{base}{SMOOTH_SUFFIX}") } else { base.to_string() }
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

    /// 名前の材質を作る。名前の後ろの「+smooth」は、表面の模様（凹凸と粗さのむら）を無くした版
    pub fn find_variant(&self, name: &str) -> Option<Material> {
        let (base, smooth) = split_variant(name);
        let mut m = self.materials.get(base)?.clone();
        if smooth {
            m.bump_pattern = crate::material::BumpPattern::None;
            m.bump_strength = 0.0;
            m.roughness_variation = 0.0;
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
    fn builtin_and_overlay() {
        let mut lib = MaterialLibrary::builtin();
        assert!(lib.find("metal_tin").unwrap().base_metalness == 1.0);
        assert!(lib.find("plastic").unwrap().base_color_from_model);
        assert!(!lib.find("epoxy_black").unwrap().base_color_from_model);
        lib.load_text("[material.plastic]\nspecular_roughness = 0.9\n[material.mine]\nbase_color = [1, 0, 0]\n", "test").unwrap();
        let p = lib.find("plastic").unwrap();
        assert_eq!(p.specular_roughness, 0.9);
        assert!(p.base_color_from_model); // 書いた項目だけ上書き
        assert_eq!(lib.source_of("mine"), "test");
        assert_eq!(lib.find("mine").unwrap().base_color.x, 1.0);
    }
}
