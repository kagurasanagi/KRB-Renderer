//! 材質の TOML 表現。パラメータの一覧（OpenPBR の名前）から読み書きする。色はリニア（OpenPBR の流儀）

use glam::Vec3;
use toml::Value;

use crate::conductor;
use crate::material::{params, BumpPattern, Category, Material, Param, Sellmeier, CONDUCTOR_PARAMS};

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
const KRBR_KEYS: &[&str] = &["bump_pattern", "bump_strength", "bump_scale", "roughness_variation", "sellmeier_b", "sellmeier_c", "conductor", "category"];

/// 測定値の金属をやめる（[krbr] conductor に書く名前）
pub const NO_CONDUCTOR: &str = "none";


fn sellmeier_coefficients(v: &Value) -> Option<[f32; 3]> {
    match v {
        Value::Array(a) if a.len() == 3 => Some([as_f32(&a[0])?, as_f32(&a[1])?, as_f32(&a[2])?]),
        _ => None,
    }
}

/// t に書かれた項目だけを m に上書きする。使えなかった項目（知らない名前・型の誤り・知らない模様の名前）は、
/// その説明（"unknown parameter 'foo'" など）を problems に入れて読み飛ばす（呼び出し側がファイル名つきで警告する）
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
                if a != b {
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
        let mut m = Material::default();
        assert!(read("specular_roughness = 0.1
[krbr]
conductor = \"Au\"
category = \"metal\"", &mut m).is_empty());
        let au = conductor::find("Au").unwrap();
        assert_eq!((m.conductor, m.category, m.base_metalness), (Some(au.index), Some(Category::Metal), 1.0));
        assert_eq!(m.base_color, au.f0_and_f82_tint().0);
        // 書き出すと conductor だけで、色は書かない。読み直すと同じ
        let t = write_material(&m);
        assert!(!t.contains_key("base_color") && !t.contains_key("specular_color"));
        let mut r = Material::default();
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
}
