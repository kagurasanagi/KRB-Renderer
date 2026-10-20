//! 部品の 3D モデルへの材質の割り当て。
//!   全体設定  model_map.toml：3D モデルごとの設定（他のプロジェクトと共有）と、色・名前の規則
//!   プロジェクト overrides.toml：部品番号ごと・3D モデルごとの上書き
//! 解決順序（優先度の高い順）：
//!   1. 部品番号ごとの上書き（プロジェクト）  2. 3D モデルごとの上書き（プロジェクト）
//!   3. 3D モデルごとの設定（全体）           4. 色・名前の規則（全体 → 組み込み）  5. 既定値

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use glam::Vec3;
use krbr_model::ModelPart;
use toml::Value;

use crate::material::Material;
use crate::material_library::MaterialLibrary;

const BUILTIN_RULES: &str = r#"
[[rule]]   # アルミ電解コンデンサ（KiCad 標準のモデル）：缶と上面（金属色の部位）はアルミ
model = "CP_Radial_D*"
part = "kicad:metal_grey_pins"
material = "aluminum"
[[rule]]   # その帯（スリーブへの印刷）は樹脂。色はモデルのまま
model = "CP_Radial_D*"
part = "kicad:light_brown_body"
material = "plastic"
[[rule]]   # 面実装のアルミ電解コンデンサの缶
model = "CP_Elec_*"
part = "kicad:metal_grey_pins"
material = "aluminum"
[[rule]]
part = "kicad:metal_grey_pins"
material = "metal_tin"
[[rule]]
part = "kicad:gold_pins"
material = "metal_gold"
[[rule]]
part = "kicad:black_body"
material = "epoxy_black"
[[rule]]
part = "kicad:light_brown_body"
material = "ceramic"
[[rule]]   # わずかに黄みのある灰色（端子の金属）
sat_max = 0.12
val_min = 0.55
val_max = 0.88
material = "metal_tin"
[[rule]]   # 金色
hue_min = 30.0
hue_max = 55.0
sat_min = 0.3
val_min = 0.6
material = "metal_gold"
[[rule]]   # ほぼ白
sat_max = 0.05
val_min = 0.88
material = "glossy_plastic"
[[rule]]
material = "plastic"
"#;

/// 材質の割り当て：部位ごとの指定（parts）と、それ以外の部位すべて（material）
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PartAssignment {
    pub material: String,
    pub parts: BTreeMap<String, String>,
}

impl PartAssignment {
    pub fn is_empty(&self) -> bool {
        self.material.is_empty() && self.parts.is_empty()
    }
    /// 部位に一致する材質名（無ければ空）
    pub fn lookup(&self, part: Option<&ModelPart>) -> String {
        if let Some(p) = part {
            for k in p.keys() {
                if let Some(m) = self.parts.get(&k) {
                    return m.clone();
                }
            }
        }
        if let Some(m) = self.parts.get("*") {
            return m.clone();
        }
        self.material.clone()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelEntry {
    /// 展開前のパス（.kicad_pcb の (model "...") に書かれたとおり）
    pub path: String,
    /// ファイル内容の XXH64（16 進）
    pub hash: String,
    pub assign: PartAssignment,
}

impl ModelEntry {
    pub fn matches(&self, spec: &str, file_hash: &str) -> bool {
        (!self.path.is_empty() && self.path == spec) || (!self.hash.is_empty() && !file_hash.is_empty() && self.hash == file_hash)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaterialRule {
    /// 部位のキーに対するワイルドカード（空なら全て）
    pub part: String,
    /// モデルのファイル名に対するワイルドカード（空なら全て）
    pub model: String,
    pub hue_min: Option<f32>,
    pub hue_max: Option<f32>,
    pub sat_min: Option<f32>,
    pub sat_max: Option<f32>,
    pub val_min: Option<f32>,
    pub val_max: Option<f32>,
    pub material: String,
}

fn rgb_to_hsv(c: Vec3) -> (f32, f32, f32) {
    let mx = c.x.max(c.y).max(c.z);
    let mn = c.x.min(c.y).min(c.z);
    let v = mx;
    let s = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
    if mx == mn {
        return (0.0, s, v);
    }
    let d = mx - mn;
    let mut h = if mx == c.x {
        60.0 * ((c.y - c.z) / d % 6.0)
    } else if mx == c.y {
        60.0 * ((c.z - c.x) / d + 2.0)
    } else {
        60.0 * ((c.x - c.y) / d + 4.0)
    };
    if h < 0.0 {
        h += 360.0;
    }
    (h, s, v)
}

impl MaterialRule {
    pub fn matches(&self, model_file: &str, p: Option<&ModelPart>) -> bool {
        if !self.model.is_empty() && !glob_match(&self.model, model_file) {
            return false;
        }
        if !self.part.is_empty() {
            let Some(p) = p else { return false };
            if !p.keys().iter().any(|k| glob_match(&self.part, k)) {
                return false;
            }
        }
        let needs_color = self.hue_min.is_some() || self.hue_max.is_some() || self.sat_min.is_some() || self.sat_max.is_some() || self.val_min.is_some() || self.val_max.is_some();
        if needs_color {
            let Some(p) = p else { return false };
            if !p.has_color {
                return false;
            }
            let (h, s, v) = rgb_to_hsv(p.srgb);
            if self.hue_min.map(|m| h < m).unwrap_or(false)
                || self.hue_max.map(|m| h > m).unwrap_or(false)
                || self.sat_min.map(|m| s < m).unwrap_or(false)
                || self.sat_max.map(|m| s > m).unwrap_or(false)
                || self.val_min.map(|m| v < m).unwrap_or(false)
                || self.val_max.map(|m| v > m).unwrap_or(false)
            {
                return false;
            }
        }
        true
    }
}

/// ワイルドカード（`*`, `?`）の照合。大小は無視
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = text.chars().collect();
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    let eq = |a: char, b: char| a.to_lowercase().eq(b.to_lowercase());
    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || eq(p[pi], s[si])) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            pi += 1;
            mark = si;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn as_f32(v: &Value) -> Option<f32> {
    match v {
        Value::Float(f) => Some(*f as f32),
        Value::Integer(i) => Some(*i as f32),
        _ => None,
    }
}

fn str_of(t: &toml::Table, key: &str) -> String {
    t.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn read_assignment(t: &toml::Table) -> PartAssignment {
    let mut a = PartAssignment { material: str_of(t, "material"), parts: BTreeMap::new() };
    if let Some(Value::Table(parts)) = t.get("parts") {
        for (k, v) in parts {
            if let Some(s) = v.as_str() {
                a.parts.insert(k.clone(), s.to_string());
            }
        }
    }
    a
}

fn write_assignment(t: &mut toml::Table, a: &PartAssignment) {
    if !a.material.is_empty() {
        t.insert("material".into(), Value::String(a.material.clone()));
    }
    if !a.parts.is_empty() {
        let mut parts = toml::Table::new();
        for (k, v) in &a.parts {
            parts.insert(k.clone(), Value::String(v.clone()));
        }
        t.insert("parts".into(), Value::Table(parts));
    }
}

fn read_models(root: &toml::Table) -> Vec<ModelEntry> {
    let mut out = Vec::new();
    if let Some(Value::Array(arr)) = root.get("model") {
        for n in arr {
            let Value::Table(t) = n else { continue };
            let e = ModelEntry { path: str_of(t, "path"), hash: str_of(t, "hash"), assign: read_assignment(t) };
            if !e.path.is_empty() || !e.hash.is_empty() {
                out.push(e);
            }
        }
    }
    out
}

fn write_models(models: &[ModelEntry]) -> Value {
    Value::Array(
        models
            .iter()
            .map(|e| {
                let mut t = toml::Table::new();
                if !e.path.is_empty() {
                    t.insert("path".into(), Value::String(e.path.clone()));
                }
                if !e.hash.is_empty() {
                    t.insert("hash".into(), Value::String(e.hash.clone()));
                }
                write_assignment(&mut t, &e.assign);
                Value::Table(t)
            })
            .collect(),
    )
}

fn read_rules(root: &toml::Table) -> Vec<MaterialRule> {
    let mut out = Vec::new();
    if let Some(Value::Array(arr)) = root.get("rule") {
        for n in arr {
            let Value::Table(t) = n else { continue };
            let opt = |k: &str| t.get(k).and_then(as_f32);
            let r = MaterialRule {
                part: str_of(t, "part"),
                model: str_of(t, "model"),
                material: str_of(t, "material"),
                hue_min: opt("hue_min"),
                hue_max: opt("hue_max"),
                sat_min: opt("sat_min"),
                sat_max: opt("sat_max"),
                val_min: opt("val_min"),
                val_max: opt("val_max"),
            };
            if !r.material.is_empty() {
                out.push(r);
            }
        }
    }
    out
}

fn write_rules(rules: &[MaterialRule]) -> Value {
    Value::Array(
        rules
            .iter()
            .map(|r| {
                let mut t = toml::Table::new();
                if !r.part.is_empty() {
                    t.insert("part".into(), Value::String(r.part.clone()));
                }
                if !r.model.is_empty() {
                    t.insert("model".into(), Value::String(r.model.clone()));
                }
                for (k, v) in [("hue_min", r.hue_min), ("hue_max", r.hue_max), ("sat_min", r.sat_min), ("sat_max", r.sat_max), ("val_min", r.val_min), ("val_max", r.val_max)] {
                    if let Some(v) = v {
                        t.insert(k.into(), Value::Float(v as f64));
                    }
                }
                t.insert("material".into(), Value::String(r.material.clone()));
                Value::Table(t)
            })
            .collect(),
    )
}

fn parse_file(p: &Path) -> Result<toml::Table, String> {
    let text = std::fs::read_to_string(p).map_err(|_| "cannot read".to_string())?;
    toml::from_str(&text).map_err(|e| e.to_string())
}

fn write_file(p: &Path, header: &str, t: &toml::Table) -> Result<(), String> {
    let body = toml::to_string(&Value::Table(t.clone())).map_err(|e| e.to_string())?;
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(p, format!("{header}\n\n{body}")).map_err(|_| format!("cannot write {}", p.display()))
}

fn upsert_model<'a>(models: &'a mut Vec<ModelEntry>, path: &str, hash: &str) -> &'a mut ModelEntry {
    let idx = models.iter().position(|e| (!path.is_empty() && e.path == path) || (!hash.is_empty() && e.hash == hash));
    match idx {
        Some(i) => {
            if models[i].path.is_empty() {
                models[i].path = path.to_string();
            }
            if !hash.is_empty() {
                models[i].hash = hash.to_string();
            }
            &mut models[i]
        }
        None => {
            models.push(ModelEntry { path: path.to_string(), hash: hash.to_string(), assign: PartAssignment::default() });
            models.last_mut().unwrap()
        }
    }
}

/// 全体設定 model_map.toml
#[derive(Debug, Clone, Default)]
pub struct GlobalModelMap {
    pub models: Vec<ModelEntry>,
    pub rules: Vec<MaterialRule>,
}

impl GlobalModelMap {
    pub fn load(p: &Path) -> Result<Self, String> {
        let root = parse_file(p)?;
        Ok(Self { models: read_models(&root), rules: read_rules(&root) })
    }
    pub fn save(&self, p: &Path) -> Result<(), String> {
        let mut root = toml::Table::new();
        if !self.models.is_empty() {
            root.insert("model".into(), write_models(&self.models));
        }
        if !self.rules.is_empty() {
            root.insert("rule".into(), write_rules(&self.rules));
        }
        write_file(
            p,
            "# krbr: 3D モデルごとの材質の設定（全体設定。他のプロジェクトと共有）\n\
             # [[model]] path（展開前のパス）か hash（ファイル内容の XXH64）が一致したモデルに使う。\n\
             #   material: 全部位, parts: 部位ごと（キーは XDE の名前 / \"kicad:<色名>\" / \"#rrggbb\" / \"*\"）\n\
             # [[rule]] 色・名前の規則（上から順に評価。組み込みの規則より先）",
            &root,
        )
    }
    pub fn upsert(&mut self, path: &str, hash: &str) -> &mut ModelEntry {
        upsert_model(&mut self.models, path, hash)
    }
}

/// プロジェクト overrides.toml
#[derive(Debug, Clone, Default)]
pub struct ProjectOverrides {
    pub refs: BTreeMap<String, PartAssignment>,
    pub models: Vec<ModelEntry>,
}

impl ProjectOverrides {
    pub fn load(p: &Path) -> Result<Self, String> {
        let root = parse_file(p)?;
        let mut o = Self { refs: BTreeMap::new(), models: read_models(&root) };
        if let Some(Value::Table(r)) = root.get("ref") {
            for (k, v) in r {
                if let Value::Table(t) = v {
                    o.refs.insert(k.clone(), read_assignment(t));
                }
            }
        }
        Ok(o)
    }
    pub fn save(&self, p: &Path) -> Result<(), String> {
        let mut root = toml::Table::new();
        if !self.refs.is_empty() {
            let mut r = toml::Table::new();
            for (k, v) in &self.refs {
                let mut t = toml::Table::new();
                write_assignment(&mut t, v);
                r.insert(k.clone(), Value::Table(t));
            }
            root.insert("ref".into(), Value::Table(r));
        }
        if !self.models.is_empty() {
            root.insert("model".into(), write_models(&self.models));
        }
        write_file(
            p,
            "# krbr: このプロジェクトの材質の上書き（全体設定より優先）\n\
             # [ref.<部品番号>] 部品ごと / [[model]] 3D モデルごと（path か hash で照合）\n\
             #   material: 全部位, parts: 部位ごと（キーは XDE の名前 / \"kicad:<色名>\" / \"#rrggbb\" / \"*\"）",
            &root,
        )
    }
    pub fn upsert(&mut self, path: &str, hash: &str) -> &mut ModelEntry {
        upsert_model(&mut self.models, path, hash)
    }
}

pub fn builtin_rules() -> &'static Vec<MaterialRule> {
    static RULES: OnceLock<Vec<MaterialRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let root: toml::Table = toml::from_str(BUILTIN_RULES).expect("builtin rules");
        read_rules(&root)
    })
}

pub struct MaterialQuery<'a> {
    /// 部品番号
    pub reference: &'a str,
    /// 展開前のパス
    pub model_spec: &'a str,
    /// ファイル内容のハッシュ
    pub model_hash: &'a str,
    /// ファイル名（規則の照合用）
    pub model_file: &'a str,
    pub part: Option<&'a ModelPart>,
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedMaterial {
    pub material: Material,
    /// 材質ライブラリの名前
    pub name: String,
    /// どの段階で決まったか（"ref U2 (project)" など）
    pub source: String,
}

pub struct MaterialResolver<'a> {
    lib: &'a MaterialLibrary,
    project: &'a ProjectOverrides,
    global: &'a GlobalModelMap,
}

impl<'a> MaterialResolver<'a> {
    pub fn new(lib: &'a MaterialLibrary, project: &'a ProjectOverrides, global: &'a GlobalModelMap) -> Self {
        Self { lib, project, global }
    }

    /// 名前が材質ライブラリにあれば材質を作る（base_color をモデルの色にする処理も行う）
    fn make(&self, name: &str, source: String, q: &MaterialQuery) -> Option<ResolvedMaterial> {
        if name.is_empty() {
            return None;
        }
        let Some(m) = self.lib.find_variant(name) else {
            static WARNED: OnceLock<Mutex<std::collections::BTreeSet<String>>> = OnceLock::new();
            let mut w = WARNED.get_or_init(|| Mutex::new(Default::default())).lock().unwrap();
            if w.insert(name.to_string()) {
                krbr_core::warn!("material '{}' ({}) is not in the material library; skipped", name, source);
            }
            return None;
        };
        let from_model = m.base_color_from_model;
        let mut r = ResolvedMaterial { material: m, name: name.to_string(), source };
        if let Some(p) = q.part {
            if from_model && p.has_color {
                r.material.base_color = p.linear_color();
            }
        }
        r.material.name = name.to_string();
        Some(r)
    }

    pub fn resolve(&self, q: &MaterialQuery) -> ResolvedMaterial {
        // 1. 部品番号ごとの上書き（プロジェクト）
        if !q.reference.is_empty() {
            if let Some(a) = self.project.refs.get(q.reference) {
                if let Some(r) = self.make(&a.lookup(q.part), format!("ref {} (project)", q.reference), q) {
                    return r;
                }
            }
        }
        // 2. 3D モデルごとの上書き（プロジェクト）
        for e in &self.project.models {
            if e.matches(q.model_spec, q.model_hash) {
                if let Some(r) = self.make(&e.assign.lookup(q.part), "model (project)".into(), q) {
                    return r;
                }
            }
        }
        // 3. 3D モデルごとの設定（全体）
        for e in &self.global.models {
            if e.matches(q.model_spec, q.model_hash) {
                if let Some(r) = self.make(&e.assign.lookup(q.part), "model (global)".into(), q) {
                    return r;
                }
            }
        }
        // 4. 色・名前の規則（全体 → 組み込み）
        for (i, rule) in self.global.rules.iter().enumerate() {
            if rule.matches(q.model_file, q.part) {
                if let Some(r) = self.make(&rule.material, format!("rule {} (global)", i + 1), q) {
                    return r;
                }
            }
        }
        for (i, rule) in builtin_rules().iter().enumerate() {
            if rule.matches(q.model_file, q.part) {
                if let Some(r) = self.make(&rule.material, format!("rule {} (builtin)", i + 1), q) {
                    return r;
                }
            }
        }
        // 5. 既定値
        let mut r = ResolvedMaterial { name: "default".into(), source: "default".into(), ..Default::default() };
        r.material.name = "default".into();
        if let Some(p) = q.part {
            if p.has_color {
                r.material.base_color = p.linear_color();
            }
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob() {
        assert!(glob_match("*.step", "Part.STEP"));
        assert!(glob_match("kicad:*pins", "kicad:metal_grey_pins"));
        assert!(!glob_match("kicad:*pins", "kicad:black_body"));
        assert!(glob_match("?_0603*", "C_0603_1608Metric.step"));
    }

    #[test]
    fn resolution_order() {
        let lib = MaterialLibrary::builtin();
        let pins = ModelPart { name: "Pins".into(), srgb: Vec3::new(0.824, 0.820, 0.781), alpha: 1.0, has_color: true };
        let body = ModelPart { name: String::new(), srgb: Vec3::new(0.148, 0.145, 0.145), alpha: 1.0, has_color: true };
        let mut project = ProjectOverrides::default();
        let global = GlobalModelMap::default();
        let r = MaterialResolver::new(&lib, &project, &global);
        fn q(part: &ModelPart) -> MaterialQuery<'_> {
            MaterialQuery { reference: "U1", model_spec: "${X}/a.step", model_hash: "h", model_file: "a.step", part: Some(part) }
        }
        // 組み込みの規則：KiCad の色名
        assert_eq!(r.resolve(&q(&pins)).name, "metal_tin");
        assert_eq!(r.resolve(&q(&body)).name, "epoxy_black");
        // 部品番号の上書きが勝つ（部位ごとの指定）
        let mut a = PartAssignment { material: "plastic".into(), parts: BTreeMap::new() };
        a.parts.insert("kicad:metal_grey_pins".into(), "metal_gold".into());
        project.refs.insert("U1".into(), a);
        let r = MaterialResolver::new(&lib, &project, &global);
        let res = r.resolve(&q(&pins));
        assert_eq!(res.name, "metal_gold");
        assert_eq!(res.source, "ref U1 (project)");
        assert_eq!(r.resolve(&q(&body)).name, "plastic");
        // plastic は base_color_from_model：モデルの色が入る
        assert!((r.resolve(&q(&body)).material.base_color.x - 0.0189).abs() < 0.002);
    }

    #[test]
    fn overrides_round_trip() {
        let dir = std::env::temp_dir().join("krbr_rust_modelmap_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut o = ProjectOverrides::default();
        o.refs.insert("U3".into(), PartAssignment { material: "ceramic".into(), parts: BTreeMap::new() });
        o.upsert("${K}/x.step", "abcd").assign.material = "plastic".into();
        let p = dir.join("overrides.toml");
        o.save(&p).unwrap();
        let back = ProjectOverrides::load(&p).unwrap();
        assert_eq!(back.refs["U3"].material, "ceramic");
        assert_eq!(back.models.len(), 1);
        assert!(back.models[0].matches("zzz", "abcd"));
    }
}
