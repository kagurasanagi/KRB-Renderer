//! 材質の TOML 表現。パラメータの一覧（OpenPBR の名前）から読み書きする。色はリニア（OpenPBR の流儀）

use glam::Vec3;
use toml::Value;

use crate::conductor;
use crate::material::{BumpPattern, Category, Material, Sellmeier};

enum Param {
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

/// OpenPBR Surface のパラメータの一覧
fn params() -> Vec<(&'static str, Param)> {
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

fn as_f32(v: &Value) -> Option<f32> {
    match v {
        Value::Float(f) => Some(*f as f32),
        Value::Integer(i) => Some(*i as f32),
        _ => None,
    }
}

/// 材質の色（リニア）：[r, g, b]、または数 1 つ（灰色）。材質は物理パラメータ（OpenPBR）なのでリニアで書く。
/// 基板の設定ファイル（<名前>.krbr）の色は画面で見る色なので sRGB の [r, g, b] だけ（crate::config）
pub fn read_color(v: &Value) -> Option<Vec3> {
    match v {
        Value::Array(a) if a.len() == 3 => Some(Vec3::new(as_f32(&a[0])?, as_f32(&a[1])?, as_f32(&a[2])?)),
        other => as_f32(other).map(Vec3::splat), // 単一の値なら灰色
    }
}

/// [krbr] の下に書ける項目
const KRBR_KEYS: &[&str] = &["bump_pattern", "bump_strength", "bump_scale", "roughness_variation", "base_color_from_model", "sellmeier_b", "sellmeier_c", "conductor", "category"];

/// 測定値の金属をやめる（[krbr] conductor に書く名前）
pub const NO_CONDUCTOR: &str = "none";

/// 測定値の金属の色を決める項目（書くと測定値の金属ではなくなる。conductor も書けばそちらが勝つ）
const CONDUCTOR_PARAMS: &[&str] = &["base_color", "specular_color", "base_metalness"];

fn sellmeier_coefficients(v: &Value) -> Option<[f32; 3]> {
    match v {
        Value::Array(a) if a.len() == 3 => Some([as_f32(&a[0])?, as_f32(&a[1])?, as_f32(&a[2])?]),
        _ => None,
    }
}

/// t に書かれた項目だけを m に上書きする。使えなかった項目（知らない名前・型の誤り・知らない模様の名前）は、
/// その説明（"unknown parameter 'foo'" など）を problems に入れて読み飛ばす（呼び出し側がファイル名つきで警告する）。
/// base_color_from_model は「base_color を書けば false、[krbr] に明示があればそれ」をここで 1 回だけ決める
pub fn read_material(t: &toml::Table, m: &mut Material, problems: &mut Vec<String>) {
    let ps = params();
    for (k, node) in t {
        if k == "krbr" {
            continue;
        }
        let Some((_, p)) = ps.iter().find(|(name, _)| name == k) else {
            problems.push(format!("unknown parameter '{k}'"));
            continue;
        };
        let ok = match p {
            Param::F(get) => as_f32(node).map(|v| *get(m) = v).is_some(),
            Param::C(get) => read_color(node).map(|c| *get(m) = c).is_some(),
            Param::B(get) => match node {
                Value::Boolean(v) => {
                    *get(m) = *v;
                    true
                }
                _ => false,
            },
        };
        if !ok {
            let expected = match p {
                Param::F(_) => "a number",
                Param::C(_) => "[r, g, b] or a number (linear)",
                Param::B(_) => "true/false",
            };
            problems.push(format!("'{k}': expected {expected}; ignored"));
        }
    }
    // base_color を書いたら、モデルの面の色ではなくその色を使う
    if t.contains_key("base_color") {
        m.base_color_from_model = false;
    }
    // 金属の色を自分で書いたら、測定値の金属ではなくなる（[krbr] conductor を書けばそちらが勝つ）
    if CONDUCTOR_PARAMS.iter().any(|k| t.contains_key(*k)) {
        m.conductor = None;
    }
    match t.get("krbr") {
        Some(Value::Table(k)) => read_krbr(k, m, problems),
        Some(_) => problems.push("'krbr': expected a table; ignored".into()),
        None => {}
    }
}

/// [krbr] の拡張
fn read_krbr(k: &toml::Table, m: &mut Material, problems: &mut Vec<String>) {
    let mut wrong = |key: &str, expected: &str| problems.push(format!("'krbr.{key}': expected {expected}; ignored"));
    match k.get("bump_pattern") {
        Some(Value::String(v)) => match v.parse::<BumpPattern>() {
            Ok(p) => m.bump_pattern = p,
            Err(e) => wrong("bump_pattern", &format!("one of none / noise / weave ({e})")),
        },
        Some(_) => wrong("bump_pattern", "a string"),
        None => {}
    }
    for (key, dst) in [("bump_strength", &mut m.bump_strength), ("bump_scale", &mut m.bump_scale), ("roughness_variation", &mut m.roughness_variation)] {
        if let Some(v) = k.get(key) {
            match as_f32(v) {
                Some(x) => *dst = x,
                None => wrong(key, "a number"),
            }
        }
    }
    // 明示があれば、base_color を書いたかどうかより優先する
    match k.get("base_color_from_model") {
        Some(Value::Boolean(v)) => m.base_color_from_model = *v,
        Some(_) => wrong("base_color_from_model", "true/false"),
        None => {}
    }
    match (k.get("sellmeier_b").map(sellmeier_coefficients), k.get("sellmeier_c").map(sellmeier_coefficients)) {
        (Some(Some(b)), Some(Some(c))) => m.sellmeier = Some(Sellmeier { b, c }),
        (None, None) => {}
        _ => wrong("sellmeier_b / sellmeier_c", "both, as [B1, B2, B3] and [C1, C2, C3]"),
    }
    match k.get("conductor") {
        Some(Value::String(v)) if v == NO_CONDUCTOR => m.conductor = None,
        Some(Value::String(v)) => match conductor::find(v) {
            Some(c) => m.set_conductor(c),
            None => wrong("conductor", &format!("one of {} or {NO_CONDUCTOR}", conductor::all().map(|c| c.id).collect::<Vec<_>>().join(" / "))),
        },
        Some(_) => wrong("conductor", "a string"),
        None => {}
    }
    match k.get("category") {
        Some(Value::String(v)) => match v.parse::<Category>() {
            Ok(c) => m.category = Some(c),
            Err(e) => wrong("category", &e),
        },
        Some(_) => wrong("category", "a string"),
        None => {}
    }
    for key in k.keys() {
        if !KRBR_KEYS.contains(&key.as_str()) {
            problems.push(format!("unknown parameter 'krbr.{key}'"));
        }
    }
}

/// f32 を、読み直すと同じ値になる最も短い 10 進の表記の f64 にする（0.3 が 0.30000001192092896 と書かれないように）
fn float(v: f32) -> Value {
    Value::Float(v.to_string().parse().unwrap_or(v as f64))
}

fn color_array(c: Vec3) -> Value {
    Value::Array(vec![float(c.x), float(c.y), float(c.z)])
}

/// 既定値と異なる項目だけを書き出す（[krbr] の拡張を含む）
pub fn write_material(m: &Material) -> toml::Table {
    let def = Material::default();
    let mut t = toml::Table::new();
    let mut mm = m.clone();
    let mut dd = def.clone();
    for (name, p) in params() {
        // 測定値の金属の色は [krbr] conductor から決まる
        if m.conductor.is_some() && CONDUCTOR_PARAMS.contains(&name) {
            continue;
        }
        match p {
            Param::F(get) => {
                let (a, b) = (*get(&mut mm), *get(&mut dd));
                if a != b {
                    t.insert(name.into(), float(a));
                }
            }
            Param::C(get) => {
                let (a, b) = (*get(&mut mm), *get(&mut dd));
                // base_color は、書いたかどうかで base_color_from_model が決まる（read_material）ので、
                // モデルの色を使うなら書かず、使わないなら既定値と同じでも書く
                let write = if name == "base_color" { !m.base_color_from_model } else { a != b };
                if write {
                    t.insert(name.into(), color_array(a));
                }
            }
            Param::B(get) => {
                let (a, b) = (*get(&mut mm), *get(&mut dd));
                if a != b {
                    t.insert(name.into(), Value::Boolean(a));
                }
            }
        }
    }
    let mut k = toml::Table::new();
    if m.base_color_from_model {
        k.insert("base_color_from_model".into(), Value::Boolean(true));
    }
    if m.bump_pattern != BumpPattern::None {
        k.insert("bump_pattern".into(), Value::String(m.bump_pattern.as_str().into()));
        k.insert("bump_strength".into(), float(m.bump_strength));
        k.insert("bump_scale".into(), float(m.bump_scale));
    }
    if m.roughness_variation != 0.0 {
        k.insert("roughness_variation".into(), float(m.roughness_variation));
    }
    if let Some(s) = &m.sellmeier {
        k.insert("sellmeier_b".into(), Value::Array(s.b.iter().map(|v| float(*v)).collect()));
        k.insert("sellmeier_c".into(), Value::Array(s.c.iter().map(|v| float(*v)).collect()));
    }
    if let Some(c) = m.conductor.and_then(conductor::get) {
        k.insert("conductor".into(), Value::String(c.id.into()));
    }
    if let Some(c) = m.category {
        k.insert("category".into(), Value::String(c.as_str().into()));
    }
    if !k.is_empty() {
        t.insert("krbr".into(), Value::Table(k));
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut m = Material { name: "x".into(), base_color: Vec3::new(0.1, 0.2, 0.3), coat_weight: 1.0, bump_pattern: BumpPattern::Weave, bump_strength: 0.1, bump_scale: 0.4, ..Default::default() };
        m.sellmeier = Some(Sellmeier { b: [1.0, 0.2, 1.0], c: [0.006, 0.02, 103.0] });
        let t = write_material(&m);
        let text = toml::to_string(&Value::Table(t.clone())).unwrap();
        let back: toml::Table = toml::from_str(&text).unwrap();
        let mut r = Material::default();
        let mut problems = Vec::new();
        read_material(&back, &mut r, &mut problems);
        assert!(problems.is_empty());
        assert_eq!(r.base_color, m.base_color);
        assert_eq!(r.coat_weight, 1.0);
        assert_eq!(r.bump_pattern, BumpPattern::Weave);
        assert_eq!(r.sellmeier, m.sellmeier);
        // 未知の項目・型の誤り・知らない模様は報告して読み飛ばす
        let t2: toml::Table = toml::from_str("foo = 1\nbase_metalness = 1\nspecular_roughness = \"0.5\"\n[krbr]\nbump_pattern = \"Noise\"\nbump_sclae = 1").unwrap();
        read_material(&t2, &mut r, &mut problems);
        assert_eq!(problems.len(), 4, "{problems:?}");
        assert!(problems[0].contains("'foo'") && problems.iter().any(|p| p.contains("krbr.bump_sclae")));
        assert_eq!((r.base_metalness, r.specular_roughness, r.bump_pattern), (1.0, Material::default().specular_roughness, BumpPattern::Weave));
    }

    #[test]
    fn conductor_and_category() {
        let read = |text: &str, m: &mut Material| {
            let mut problems = Vec::new();
            read_material(&toml::from_str(text).unwrap(), m, &mut problems);
            problems
        };
        let mut m = Material { base_color_from_model: true, ..Default::default() };
        assert!(read("specular_roughness = 0.1
[krbr]
conductor = \"Au\"
category = \"metal\"", &mut m).is_empty());
        let au = conductor::find("Au").unwrap();
        assert_eq!((m.conductor, m.category, m.base_metalness, m.base_color_from_model), (Some(au.index), Some(Category::Metal), 1.0, false));
        assert_eq!(m.base_color, au.f0_and_f82_tint().0);
        // 書き出すと conductor だけで、色は書かない。読み直すと同じ
        let t = write_material(&m);
        assert!(!t.contains_key("base_color") && !t.contains_key("specular_color"));
        let mut r = Material { base_color_from_model: true, ..Default::default() };
        read_material(&t, &mut r, &mut Vec::new());
        assert_eq!(r, m);
        // 色を書けば測定値の金属ではなくなる。"none" でも外れる。知らない名前は報告する
        let mut c = m.clone();
        read("base_color = [1, 0, 0]", &mut c);
        assert_eq!(c.conductor, None);
        let mut c = m.clone();
        read("[krbr]
conductor = \"none\"", &mut c);
        assert_eq!(c.conductor, None);
        let mut c = m.clone();
        assert_eq!(read("[krbr]
conductor = \"Unobtainium\"
category = \"wood\"", &mut c).len(), 2);
        assert_eq!(c.conductor, m.conductor);
        // 分類を書かなければパラメータから
        assert_eq!(Material::default().category(), Category::Other);
        assert_eq!(Material { transmission_weight: 1.0, ..Default::default() }.category(), Category::Glass);
    }

    #[test]
    fn base_color_from_model_rule() {
        let read = |text: &str, from_model: bool| {
            let mut m = Material { base_color_from_model: from_model, ..Default::default() };
            read_material(&toml::from_str(text).unwrap(), &mut m, &mut Vec::new());
            m.base_color_from_model
        };
        assert!(read("specular_roughness = 0.5", true)); // 書かなければそのまま
        assert!(!read("base_color = [1, 0, 0]", true)); // base_color を書けばその色
        assert!(read("base_color = [1, 0, 0]\n[krbr]\nbase_color_from_model = true", false)); // 明示が勝つ
        // 書き出しと読み直しで変わらない（既定値と同じ base_color でも、モデルの色を使わないなら書く）
        for from_model in [false, true] {
            let m = Material { base_color_from_model: from_model, ..Default::default() };
            let mut r = Material { base_color_from_model: !from_model, ..Default::default() };
            read_material(&write_material(&m), &mut r, &mut Vec::new());
            assert_eq!(r.base_color_from_model, from_model);
        }
    }
}
