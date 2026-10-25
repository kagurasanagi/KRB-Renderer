//! 名前付きの材質の一覧。組み込みの材質に、全体設定の materials/*.toml を重ねる（同じ名前は書いた項目だけ上書き）

use std::collections::BTreeMap;
use std::path::Path;

use glam::Vec3;

use crate::material::{parse_hex_rgb, srgb_to_linear, Material};
use crate::material_io::read_material;

/// 材質名の後ろに付けると、表面の模様（凹凸と粗さのむら）を無くした版になる（例 "plastic+smooth"）。Rust 版だけの書き方
pub const SMOOTH_SUFFIX: &str = "+smooth";
/// 材質名の後ろに付けると、発光する版になる（例 "plastic+emit=#ff2010,5000"：sRGB の色と輝度 cd/m²）。LED の点灯など
pub const EMIT_PREFIX: &str = "+emit=";
/// 材質名の後ろに付けると、色を変えた版になる（例 "plastic_abs+color=#c03020"：sRGB）。誘電体は地の色、透明な材質は
/// 透過の色、金属は F0 の色（測定値の金属なら、測定値をやめて指定の色の金属にする）
pub const COLOR_PREFIX: &str = "+color=";
/// 材質名の後ろに付けると、粗さ（specular_roughness、0..1）を変えた版になる（例 "metal_nickel+rough=0.4"）
pub const ROUGH_PREFIX: &str = "+rough=";

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
        let srgb = parse_hex_rgb(color.trim().trim_start_matches('#'))?;
        let luminance: f32 = lum.trim().parse().ok().filter(|v: &f32| v.is_finite() && *v >= 0.0)?;
        Some(Self { srgb, luminance })
    }

    pub fn suffix(&self) -> String {
        format!("{EMIT_PREFIX}{},{}", hex_rgb(self.srgb), self.luminance)
    }

    /// 発光の色（リニア）
    pub fn linear_color(&self) -> Vec3 {
        srgb8_to_linear(self.srgb)
    }
}

fn hex_rgb([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn srgb8_to_linear(c: [u8; 3]) -> Vec3 {
    srgb_to_linear(Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32) / 255.0)
}

fn parse_color(s: &str) -> Option<[u8; 3]> {
    parse_hex_rgb(s.trim().trim_start_matches('#'))
}

fn parse_roughness(s: &str) -> Option<f32> {
    s.trim().parse().ok().filter(|v: &f32| (0.0..=1.0).contains(v))
}

/// 材質名の後ろに付ける変種（部位ごとの調整）。名前は "<材質>[+smooth][+color=..][+rough=..][+emit=..]" の順に組み立てる
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Variant {
    /// 表面の模様を無くす
    pub smooth: bool,
    /// 色（sRGB）
    pub color: Option<[u8; 3]>,
    /// 粗さ（0..1）
    pub roughness: Option<f32>,
    pub emission: Option<Emission>,
}

impl Variant {
    /// 材質名を（ライブラリの材質名, 変種）に分ける。書き方の誤った指定は名前の一部とみなす（ライブラリに無い名前になる）
    pub fn parse(name: &str) -> (&str, Variant) {
        let mut v = Variant::default();
        let mut base = name;
        loop {
            if let Some(b) = base.strip_suffix(SMOOTH_SUFFIX) {
                (base, v.smooth) = (b, true);
                continue;
            }
            // 後ろから外すので、同じ指定が 2 つあれば後ろのものを使う
            if let Some((b, e)) = base.rsplit_once(EMIT_PREFIX).and_then(|(b, s)| Emission::parse(s).map(|e| (b, e))) {
                (base, v.emission) = (b, v.emission.or(Some(e)));
            } else if let Some((b, c)) = base.rsplit_once(COLOR_PREFIX).and_then(|(b, s)| parse_color(s).map(|c| (b, c))) {
                (base, v.color) = (b, v.color.or(Some(c)));
            } else if let Some((b, r)) = base.rsplit_once(ROUGH_PREFIX).and_then(|(b, s)| parse_roughness(s).map(|r| (b, r))) {
                (base, v.roughness) = (b, v.roughness.or(Some(r)));
            } else {
                return (base, v);
            }
        }
    }

    /// 材質名を組み立てる
    pub fn name(&self, base: &str) -> String {
        let mut s = base.to_string();
        if self.smooth {
            s += SMOOTH_SUFFIX;
        }
        if let Some(c) = self.color {
            s += COLOR_PREFIX;
            s += &hex_rgb(c);
        }
        if let Some(r) = self.roughness {
            s += &format!("{ROUGH_PREFIX}{}", (r * 1000.0).round() / 1000.0);
        }
        if let Some(e) = self.emission {
            s += &e.suffix();
        }
        s
    }

    /// 材質に当てはめる
    pub fn apply(&self, m: &mut Material) {
        if self.smooth {
            m.bump_pattern = crate::material::BumpPattern::None;
            m.bump_strength = 0.0;
            m.roughness_variation = 0.0;
        }
        if let Some(c) = self.color {
            let c = srgb8_to_linear(c);
            if m.transmission_weight > 0.0 && m.base_metalness < 0.5 {
                m.transmission_color = c;
            } else {
                m.base_color = c;
                if m.conductor.take().is_some() {
                    m.specular_color = Vec3::ONE; // 色を決めた金属は Schlick（F82-tint なし）
                }
            }
        }
        if let Some(r) = self.roughness {
            m.specular_roughness = r;
        }
        if let Some(e) = self.emission {
            m.emission_color = e.linear_color();
            m.emission_luminance = e.luminance;
            // 光っている部位の地の色は発光の色にする（色のついた拡散レンズ）。白い地のままだと、周りの光を白く返して発光の色を薄める
            let c = m.emission_color;
            m.base_color = c / c.max_element().max(1e-6) * EMISSIVE_BASE_ALBEDO;
        }
    }
}

/// 材質名の変種の書き方の誤り（"+emit=" の後ろが "#rrggbb,<cd/m²>" になっていない、など）の説明。誤りが無ければ None。
/// Variant::parse は誤った指定を名前の一部とみなす（ライブラリに無い名前になる）ので、見つからなかったときの警告に使う
pub fn variant_error(name: &str) -> Option<String> {
    let (base, _) = Variant::parse(name);
    for (prefix, expected) in [(EMIT_PREFIX, "#rrggbb,<cd/m²>"), (COLOR_PREFIX, "#rrggbb"), (ROUGH_PREFIX, "a number in 0..1")] {
        if let Some((_, spec)) = base.rsplit_once(prefix) {
            return Some(format!("malformed '{prefix}{spec}' (expected {prefix}{expected})"));
        }
    }
    None
}

/// 名前を（ライブラリの材質名, 模様なしか）に分ける
pub fn split_variant(name: &str) -> (&str, bool) {
    let (base, v) = Variant::parse(name);
    (base, v.smooth)
}

/// 変種の一部を変えた名前（ほかの指定は保つ）
pub fn with_variant(name: &str, f: impl FnOnce(&mut Variant)) -> String {
    let (base, mut v) = Variant::parse(name);
    f(&mut v);
    v.name(base)
}

/// 模様のあり・なしを切り替えた名前（ほかの指定は保つ）
pub fn with_smooth(name: &str, smooth: bool) -> String {
    with_variant(name, |v| v.smooth = smooth)
}

/// 発光を付け替えた名前（None なら外す。ほかの指定は保つ）
pub fn with_emission(name: &str, emission: Option<Emission>) -> String {
    with_variant(name, |v| v.emission = emission)
}

/// 材質を差し替えた名前（変種の指定は保つ）
pub fn with_base(name: &str, base: &str) -> String {
    Variant::parse(name).1.name(base)
}

/// krbr 組み込みの材質（OpenPBR Surface）。色はリニア
pub const BUILTIN_MATERIALS: &str = include_str!("builtin_materials.toml");

/// ビューアの材質エディタで作った材質を書くファイル（全体設定の materials/ の中）
pub const USER_MATERIALS_FILE: &str = "user.toml";

/// 組み込みの材質の名前（ビューアの一覧で、編集できない材質の印に使う）
pub fn builtin_names() -> &'static std::collections::BTreeSet<String> {
    static N: std::sync::OnceLock<std::collections::BTreeSet<String>> = std::sync::OnceLock::new();
    N.get_or_init(|| match toml::from_str::<toml::Table>(BUILTIN_MATERIALS) {
        Ok(t) => t.get("material").and_then(|m| m.as_table()).map(|m| m.keys().cloned().collect()).unwrap_or_default(),
        Err(_) => Default::default(),
    })
}

/// 組み込みの材質の名前の、ファイル（builtin_materials.toml）に書いた順（ビューアの一覧の順）
pub fn builtin_order() -> &'static Vec<String> {
    static O: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();
    O.get_or_init(|| {
        let doc: Result<toml_edit::DocumentMut, _> = BUILTIN_MATERIALS.parse();
        doc.ok().and_then(|d| d.get("material").and_then(|m| m.as_table()).map(|t| t.iter().map(|(k, _)| k.to_string()).collect())).unwrap_or_default()
    })
}

/// 自分の材質の ID の頭（ID は "user_" と 16 桁の 16 進。表示名は [krbr] label）
pub const USER_ID_PREFIX: &str = "user_";

/// 新しい自分の材質の ID（ライブラリのどの材質とも重ならないもの）。割り当て（設定ファイル）は ID で書くので、
/// 表示名は後から変えられる
pub fn new_user_material_id(lib: &MaterialLibrary) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    loop {
        let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let id = format!("{USER_ID_PREFIX}{}", krbr_core::to_hex(krbr_core::xxh64(format!("{now} {} {seq}", std::process::id()).as_bytes())));
        if lib.find(&id).is_none() {
            return id;
        }
    }
}

/// 材質の表示名として使えるか（空でなく、前後に空白が無く、改行などの制御文字を含まない）
pub fn valid_material_label(label: &str) -> bool {
    !label.trim().is_empty() && label.trim() == label && !label.contains(char::is_control)
}

/// materials/user.toml に材質を書く（同じ ID があれば置き換え、ほかの材質とコメントは残す）。label は表示名
/// （[krbr] label）。None なら消す
pub fn save_user_material(config_dir: &Path, name: &str, m: Option<(&Material, &str)>) -> Result<std::path::PathBuf, String> {
    let dir = config_dir.join("materials");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join(USER_MATERIALS_FILE);
    // 先頭の説明は、材質を置き換えると最初の表の飾り（コメント）と一緒に消えるので、外して書くたびに付け直す
    const HEADER: &str = "# krbr のビューアの材質エディタで作った材質（OpenPBR Surface。色はリニア）

";
    let text = if file.exists() { std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))? } else { String::new() };
    let text = text.strip_prefix(HEADER).unwrap_or(&text);
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{}: {e}", file.display()))?;
    if !doc.contains_key("material") {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        doc.insert("material", toml_edit::Item::Table(t));
    }
    let mats = doc["material"].as_table_mut().ok_or_else(|| format!("{}: 'material' is not a table", file.display()))?;
    match m {
        Some((m, label)) => {
            let mut t = crate::material_io::write_material(m);
            if let toml::Value::Table(k) = t.entry("krbr").or_insert_with(|| toml::Value::Table(Default::default())) {
                k.insert(LABEL_KEY.into(), toml::Value::String(label.to_string()));
            }
            let text = toml::to_string(&toml::Value::Table(t)).map_err(|e| e.to_string())?;
            let parsed: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
            let mut t = parsed.as_table().clone();
            t.set_implicit(false);
            mats.insert(name, toml_edit::Item::Table(t));
        }
        None => {
            mats.remove(name);
        }
    }
    std::fs::write(&file, format!("{HEADER}{}", doc.to_string().trim_start())).map_err(|e| format!("{}: {e}", file.display()))?;
    Ok(file)
}

/// 材質の表示名を書く [krbr] の項目（材質の値ではないので Material には入れず、ライブラリが持つ）
const LABEL_KEY: &str = "label";

/// 材質の一覧。キーは材質の ID（組み込みは "plastic_abs" などの名前、材質エディタで作った材質は "user_…"）
#[derive(Debug, Clone, Default)]
pub struct MaterialLibrary {
    materials: BTreeMap<String, Material>,
    sources: BTreeMap<String, String>,
    /// 表示名（[krbr] label。無ければ、組み込みは訳、それ以外は ID を表示する）
    labels: BTreeMap<String, String>,
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
            // 表示名は材質の値ではないので外してから読む
            let mut t = t.clone();
            if let Some(toml::Value::Table(k)) = t.get_mut("krbr") {
                match k.remove(LABEL_KEY) {
                    Some(toml::Value::String(l)) if valid_material_label(l.trim()) => {
                        self.labels.insert(name.clone(), l.trim().to_string());
                    }
                    Some(_) => krbr_core::warn!("{}: material '{}': [krbr] label must be a non-empty string", source, name),
                    None => {}
                }
            }
            let t = &t;
            let mut m = match self.materials.get(name) {
                Some(existing) => existing.clone(),
                None => Material { name: name.clone(), ..Default::default() },
            };
            let mut problems = Vec::new();
            read_material(t, &mut m, &mut problems);
            for p in problems {
                krbr_core::warn!("{}: material '{}': {}", source, name, p);
            }
            self.materials.insert(name.clone(), m);
            self.sources.insert(name.clone(), source.to_string());
        }
        Ok(())
    }

    pub fn find(&self, name: &str) -> Option<&Material> {
        self.materials.get(name)
    }

    /// 名前の材質を作る。名前の後ろの変種（Variant：「+smooth」は表面の模様を無くした版、「+color=#rrggbb」は色、
    /// 「+rough=<0..1>」は粗さを変えた版、「+emit=#rrggbb,<cd/m²>」はその色と輝度で光る版）を当てはめる
    pub fn find_variant(&self, name: &str) -> Option<Material> {
        let (base, v) = Variant::parse(name);
        let mut m = self.materials.get(base)?.clone();
        v.apply(&mut m);
        Some(m)
    }

    /// 材質を足す・置き換える（ビューアの材質エディタ）。source は表示用
    pub fn insert(&mut self, name: &str, mut m: Material, source: &str) {
        m.name = name.to_string();
        self.materials.insert(name.to_string(), m);
        self.sources.insert(name.to_string(), source.to_string());
    }

    pub fn remove(&mut self, name: &str) {
        self.materials.remove(name);
        self.sources.remove(name);
        self.labels.remove(name);
    }

    /// 材質の表示名（[krbr] label に書いたもの。無ければ None）
    pub fn label(&self, name: &str) -> Option<&str> {
        self.labels.get(name).map(|s| s.as_str())
    }

    pub fn set_label(&mut self, name: &str, label: &str) {
        self.labels.insert(name.to_string(), label.to_string());
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
        // 地の色は発光の色
        assert!((m.base_color.x - EMISSIVE_BASE_ALBEDO).abs() < 1e-6 && m.base_color.z == 0.0);
        // # は省略可。模様なしと組み合わせられる（どちらの順でも）
        for name in ["plastic+smooth+emit=ff8000,5000", "plastic+emit=#FF8000,5000+smooth"] {
            let (base, v) = Variant::parse(name);
            assert_eq!((base, v.smooth, v.emission), ("plastic", true, Some(Emission { srgb: [255, 128, 0], luminance: 5000.0 })));
        }
        // 書き方の誤りは材質名の一部とみなす（ライブラリに無いので見つからない）
        for bad in ["plastic+emit=#ff80,5000", "plastic+emit=#ff8000", "plastic+emit=#ff8000,-1", "plastic+emit=#gg8000,1", "plastic+emit=#ffあ000,1"] {
            assert!(lib.find_variant(bad).is_none(), "{bad}");
            assert!(variant_error(bad).is_some(), "{bad}"); // 「ライブラリに無い」ではなく書き方の誤りとして知らせる
        }
        assert_eq!(variant_error("plastic+smooth+emit=#ff8000,5000"), None);
        assert_eq!(variant_error("no_such"), None);
        // 組み立てと付け替え
        let e = Emission { srgb: [0x20, 0xff, 0x40], luminance: 2500.0 };
        assert_eq!(Variant { smooth: true, emission: Some(e), ..Default::default() }.name("plastic"), "plastic+smooth+emit=#20ff40,2500");
        assert_eq!(with_emission("plastic+smooth", Some(e)), "plastic+smooth+emit=#20ff40,2500");
        assert_eq!(with_emission("plastic+smooth+emit=#20ff40,2500", None), "plastic+smooth");
        assert_eq!(with_smooth("plastic+emit=#20ff40,2500", false), "plastic+emit=#20ff40,2500");
        assert_eq!(split_variant("plastic+smooth+emit=#20ff40,2500"), ("plastic", true));
        assert_eq!(Emission::parse("#20ff40,0.5").unwrap().suffix(), "+emit=#20ff40,0.5");
    }

    #[test]
    fn color_and_roughness_variants() {
        let lib = MaterialLibrary::builtin();
        // 誘電体：地の色と粗さ
        let m = lib.find_variant("plastic_abs+color=#ff0000+rough=0.2").unwrap();
        assert_eq!((m.base_color, m.specular_roughness), (Vec3::new(1.0, 0.0, 0.0), 0.2));
        // 透明な材質：透過の色
        let g = lib.find_variant("acrylic+color=#00ff00").unwrap();
        assert_eq!((g.transmission_color, g.base_color), (Vec3::new(0.0, 1.0, 0.0), Vec3::ONE));
        // 測定値の金属に色を付けると、測定値をやめてその色の金属（Schlick）になる。粗さだけなら測定値のまま
        let au = lib.find_variant("metal_gold+color=#808080").unwrap();
        assert_eq!((au.conductor, au.specular_color), (None, Vec3::ONE));
        assert!(lib.find_variant("metal_gold+rough=0.5").unwrap().conductor.is_some());
        // 組み立ての順は決まっていて、読み直すと同じ。後ろの指定が勝つ
        let v = Variant { smooth: true, color: Some([1, 2, 3]), roughness: Some(0.25), emission: Some(Emission { srgb: [4, 5, 6], luminance: 10.0 }) };
        let name = v.name("plastic");
        assert_eq!(name, "plastic+smooth+color=#010203+rough=0.25+emit=#040506,10");
        assert_eq!(Variant::parse(&name), ("plastic", v));
        assert_eq!(Variant::parse("plastic+rough=0.1+rough=0.3").1.roughness, Some(0.3));
        assert_eq!(with_base(&name, "plastic_abs"), name.replacen("plastic", "plastic_abs", 1));
        // 書き方の誤り
        for bad in ["plastic+color=#ff00", "plastic+rough=1.5", "plastic+rough=x"] {
            assert!(lib.find_variant(bad).is_none(), "{bad}");
            assert!(variant_error(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn builtin_presets() {
        let lib = MaterialLibrary::builtin();
        // どの組み込みの材質にも分類があり、金属は（錫を除いて）測定値。透明な材質の屈折率は分散式の nd と合っている
        for name in builtin_names() {
            let m = lib.find(name).unwrap();
            assert!(m.category.is_some(), "{name}");
            if m.category() == crate::material::Category::Metal && name != "metal_tin" {
                assert!(m.conductor.is_some(), "{name}");
            }
            if m.sellmeier.is_some() {
                assert!((m.ior_at(587.6) - m.specular_ior).abs() < 2e-3, "{name}: {} vs {}", m.ior_at(587.6), m.specular_ior);
            }
        }
        assert!(builtin_names().len() >= 40);
        assert!(valid_material_label("my red ABS") && valid_material_label("赤い樹脂") && valid_material_label("a+b.c \"#1\""));
        for bad in ["", " x", "x ", "a\nb"] {
            assert!(!valid_material_label(bad), "{bad}");
        }
    }

    #[test]
    fn user_materials_file() {
        let dir = std::env::temp_dir().join(format!("krbr_user_mat_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut lib = MaterialLibrary::builtin();
        let mut m = lib.find_variant("plastic_abs+color=#c03020").unwrap();
        m.category = Some(crate::material::Category::Plastic);
        let id = new_user_material_id(&lib);
        assert!(id.starts_with(USER_ID_PREFIX) && id != new_user_material_id(&lib));
        let file = save_user_material(&dir, &id, Some((&m, "赤い ABS"))).unwrap();
        save_user_material(&dir, "other", Some((&lib.find("metal_nickel").unwrap().clone(), "ニッケル"))).unwrap();
        save_user_material(&dir, &id, Some((&m, "赤い ABS+つや"))).unwrap(); // 置き換え（表示名を変える）
        let loaded = MaterialLibrary::load(&dir);
        let r = loaded.find(&id).unwrap();
        assert_eq!((r.base_color, r.specular_ior, r.category), (m.base_color, m.specular_ior, m.category));
        assert_eq!(loaded.label(&id), Some("赤い ABS+つや"));
        assert_eq!(loaded.find("other").unwrap().conductor, lib.find("metal_nickel").unwrap().conductor);
        assert_eq!(loaded.label("plastic"), None);
        save_user_material(&dir, "other", None).unwrap();
        assert!(MaterialLibrary::load(&dir).find("other").is_none());
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("# krbr") && text.contains("specular_ior = 1.53
") && text.contains("label = \"赤い ABS+つや\""), "{text}");
        lib.insert("x", m.clone(), "test");
        assert_eq!(lib.find("x").unwrap().name, "x");
        lib.remove("x");
        assert!(lib.find("x").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn builtin_and_overlay() {
        let mut lib = MaterialLibrary::builtin();
        assert!(lib.find("metal_tin").unwrap().base_metalness == 1.0);
        let bump = lib.find("plastic").unwrap().bump_strength;
        let color = lib.find("plastic").unwrap().base_color;
        lib.load_text("[material.plastic]\nspecular_roughness = 0.9\n[material.mine]\nbase_color = [1, 0, 0]\n", "test").unwrap();
        let p = lib.find("plastic").unwrap();
        assert_eq!(p.specular_roughness, 0.9);
        assert_eq!(p.base_color, color); // 書いた項目だけ上書き
        assert_eq!(p.bump_strength, bump);
        assert_eq!(lib.source_of("mine"), "test");
        assert_eq!(lib.find("mine").unwrap().base_color.x, 1.0);
    }
}
