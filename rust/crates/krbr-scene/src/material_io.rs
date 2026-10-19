//! 材質の TOML 表現。パラメータの一覧（OpenPBR の名前）から読み書きする。色はリニア（OpenPBR の流儀）

use glam::Vec3;
use toml::Value;

use crate::material::{BumpPattern, Material, Sellmeier};

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

pub fn read_color(v: &Value) -> Option<Vec3> {
    match v {
        Value::Array(a) if a.len() == 3 => Some(Vec3::new(as_f32(&a[0])?, as_f32(&a[1])?, as_f32(&a[2])?)),
        other => as_f32(other).map(Vec3::splat), // 単一の値なら灰色
    }
}

/// t に書かれた項目だけを m に上書きする。知らない項目の名前を unknown に入れる
pub fn read_material(t: &toml::Table, m: &mut Material, unknown: &mut Vec<String>) {
    let ps = params();
    for (k, node) in t {
        if k == "krbr" {
            continue;
        }
        let Some((_, p)) = ps.iter().find(|(name, _)| name == k) else {
            unknown.push(k.clone());
            continue;
        };
        match p {
            Param::F(get) => {
                if let Some(v) = as_f32(node) {
                    *get(m) = v;
                }
            }
            Param::C(get) => {
                if let Some(c) = read_color(node) {
                    *get(m) = c;
                }
            }
            Param::B(get) => {
                if let Value::Boolean(v) = node {
                    *get(m) = *v;
                }
            }
        }
    }
    if let Some(Value::Table(k)) = t.get("krbr") {
        if let Some(Value::String(v)) = k.get("bump_pattern") {
            m.bump_pattern = match v.as_str() {
                "noise" => BumpPattern::Noise,
                "weave" => BumpPattern::Weave,
                _ => BumpPattern::None,
            };
        }
        if let Some(v) = k.get("bump_strength").and_then(as_f32) {
            m.bump_strength = v;
        }
        if let Some(v) = k.get("bump_scale").and_then(as_f32) {
            m.bump_scale = v;
        }
        if let Some(v) = k.get("roughness_variation").and_then(as_f32) {
            m.roughness_variation = v;
        }
        if let Some(Value::Boolean(v)) = k.get("base_color_from_model") {
            m.base_color_from_model = *v;
        }
        if let (Some(Value::Array(sb)), Some(Value::Array(sc))) = (k.get("sellmeier_b"), k.get("sellmeier_c")) {
            if sb.len() == 3 && sc.len() == 3 {
                let mut s = Sellmeier { b: [0.0; 3], c: [0.0; 3] };
                for i in 0..3 {
                    s.b[i] = as_f32(&sb[i]).unwrap_or(0.0);
                    s.c[i] = as_f32(&sc[i]).unwrap_or(0.0);
                }
                m.sellmeier = Some(s);
            }
        }
    }
}

fn color_array(c: Vec3) -> Value {
    Value::Array(vec![Value::Float(c.x as f64), Value::Float(c.y as f64), Value::Float(c.z as f64)])
}

/// 既定値と異なる項目だけを書き出す（[krbr] の拡張を含む）
pub fn write_material(m: &Material) -> toml::Table {
    let def = Material::default();
    let mut t = toml::Table::new();
    let mut mm = m.clone();
    let mut dd = def.clone();
    for (name, p) in params() {
        match p {
            Param::F(get) => {
                let (a, b) = (*get(&mut mm), *get(&mut dd));
                if a != b {
                    t.insert(name.into(), Value::Float(a as f64));
                }
            }
            Param::C(get) => {
                let (a, b) = (*get(&mut mm), *get(&mut dd));
                if a != b && !(name == "base_color" && m.base_color_from_model) {
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
        k.insert("bump_pattern".into(), Value::String(match m.bump_pattern { BumpPattern::Noise => "noise", BumpPattern::Weave => "weave", BumpPattern::None => "none" }.into()));
        k.insert("bump_strength".into(), Value::Float(m.bump_strength as f64));
        k.insert("bump_scale".into(), Value::Float(m.bump_scale as f64));
    }
    if m.roughness_variation != 0.0 {
        k.insert("roughness_variation".into(), Value::Float(m.roughness_variation as f64));
    }
    if let Some(s) = &m.sellmeier {
        k.insert("sellmeier_b".into(), Value::Array(s.b.iter().map(|v| Value::Float(*v as f64)).collect()));
        k.insert("sellmeier_c".into(), Value::Array(s.c.iter().map(|v| Value::Float(*v as f64)).collect()));
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
        let mut unknown = Vec::new();
        read_material(&back, &mut r, &mut unknown);
        assert!(unknown.is_empty());
        assert_eq!(r.base_color, m.base_color);
        assert_eq!(r.coat_weight, 1.0);
        assert_eq!(r.bump_pattern, BumpPattern::Weave);
        assert_eq!(r.sellmeier, m.sellmeier);
        // 未知の項目は報告する
        let t2: toml::Table = toml::from_str("foo = 1\nbase_metalness = 1").unwrap();
        read_material(&t2, &mut r, &mut unknown);
        assert_eq!(unknown, vec!["foo".to_string()]);
        assert_eq!(r.base_metalness, 1.0);
    }
}
