//! 部品の 3D モデルへの材質の割り当て。
//!   全体設定  model_map.toml：3D モデルごとの設定（他のプロジェクトと共有）と、色・名前の規則
//!   プロジェクト（基板の設定ファイル <名前>.krbr の [ref.*] / [[model]]）：部品番号ごと・3D モデルごとの上書き
//! 解決順序（優先度の高い順）：
//!   1. 部品番号ごとの上書き（プロジェクト）  2. 3D モデルごとの上書き（プロジェクト）
//!   3. 3D モデルごとの設定（全体）           4. 色・名前の規則（全体 → 組み込み）  5. 既定値

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use glam::Vec3;
use krbr_model::ModelPart;
use toml::Value;

use crate::material::Material;
use crate::material_library::{variant_error, with_variant, MaterialLibrary};

const BUILTIN_RULES: &str = r#"
[[rule]]   # アルミ電解コンデンサ（KiCad 標準のモデル）：缶と上面（金属色の部位）はアルミ
model = "CP_Radial_D*"
part = "kicad:metal_grey_pins"
material = "aluminum"
[[rule]]   # その帯（スリーブへの印刷）は樹脂。色はモデルのまま
model = "CP_Radial_D*"
part = "kicad:light_brown_body"
material = "plastic"
model_color = true
[[rule]]   # 面実装のアルミ電解コンデンサの缶
model = "CP_Elec_*"
part = "kicad:metal_grey_pins"
material = "aluminum"
[[rule]]
part = "kicad:metal_grey_pins"
material = "metal_tin"
model_color = true
[[rule]]
part = "kicad:gold_pins"
material = "metal_gold"
[[rule]]
part = "kicad:black_body"
material = "epoxy_black"
[[rule]]
part = "kicad:light_brown_body"
material = "ceramic"
model_color = true
[[rule]]   # わずかに黄みのある灰色（端子の金属）
sat_max = 0.12
val_min = 0.55
val_max = 0.88
material = "metal_tin"
model_color = true
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
model_color = true
[[rule]]
material = "plastic"
model_color = true
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
    /// 3D モデルの面の色を、部位の色の調整（材質名の +color=）にする
    pub model_color: bool,
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

/// 文字列の項目（無ければ空）。文字列でなければ problems に書いて空
fn str_of(t: &toml::Table, key: &str, at: &str, problems: &mut Vec<String>) -> String {
    match t.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(_) => {
            problems.push(format!("{at}: '{key}' must be a string; ignored"));
            String::new()
        }
        None => String::new(),
    }
}

/// 知らない鍵を problems に書く（材質ライブラリが未知のパラメータを警告するのと同じ方針）
fn check_keys(t: &toml::Table, known: &[&str], at: &str, problems: &mut Vec<String>) {
    for k in t.keys() {
        if !known.contains(&k.as_str()) {
            problems.push(format!("{at}: unknown key '{k}' (expected one of {}); ignored", known.join(", ")));
        }
    }
}

fn read_assignment(t: &toml::Table, at: &str, problems: &mut Vec<String>) -> PartAssignment {
    let mut a = PartAssignment { material: str_of(t, "material", at, problems), parts: BTreeMap::new() };
    match t.get("parts") {
        Some(Value::Table(parts)) => {
            for (k, v) in parts {
                match v.as_str() {
                    Some(s) => {
                        a.parts.insert(k.clone(), s.to_string());
                    }
                    None => problems.push(format!("{at}: parts.\"{k}\" must be a material name; ignored")),
                }
            }
        }
        Some(_) => problems.push(format!("{at}: 'parts' must be a table {{ \"<part key>\" = \"<material>\" }}; ignored")),
        None => {}
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

fn read_models(root: &toml::Table, problems: &mut Vec<String>) -> Vec<ModelEntry> {
    let mut out = Vec::new();
    match root.get("model") {
        Some(Value::Array(arr)) => {
            for (i, n) in arr.iter().enumerate() {
                let at = format!("[[model]] #{}", i + 1);
                let Value::Table(t) = n else {
                    problems.push(format!("{at}: must be a table; ignored"));
                    continue;
                };
                check_keys(t, &["path", "hash", "material", "parts"], &at, problems);
                let e = ModelEntry { path: str_of(t, "path", &at, problems), hash: str_of(t, "hash", &at, problems), assign: read_assignment(t, &at, problems) };
                if e.path.is_empty() && e.hash.is_empty() {
                    problems.push(format!("{at}: has neither 'path' nor 'hash'; ignored"));
                    continue;
                }
                out.push(e);
            }
        }
        Some(_) => problems.push("'model' must be written as [[model]] (an array of tables); ignored".into()),
        None => {}
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

const RULE_KEYS: &[&str] = &["part", "model", "hue_min", "hue_max", "sat_min", "sat_max", "val_min", "val_max", "material", "model_color"];

fn read_rules(root: &toml::Table, problems: &mut Vec<String>) -> Vec<MaterialRule> {
    let mut out = Vec::new();
    match root.get("rule") {
        Some(Value::Array(arr)) => {
            for (i, n) in arr.iter().enumerate() {
                let at = format!("[[rule]] #{}", i + 1);
                let Value::Table(t) = n else {
                    problems.push(format!("{at}: must be a table; ignored"));
                    continue;
                };
                check_keys(t, RULE_KEYS, &at, problems);
                let mut opt = |k: &str| match t.get(k) {
                    Some(v) => {
                        let x = as_f32(v);
                        if x.is_none() {
                            problems.push(format!("{at}: '{k}' must be a number; ignored"));
                        }
                        x
                    }
                    None => None,
                };
                let (hue_min, hue_max, sat_min, sat_max, val_min, val_max) = (opt("hue_min"), opt("hue_max"), opt("sat_min"), opt("sat_max"), opt("val_min"), opt("val_max"));
                let model_color = match t.get("model_color") {
                    Some(Value::Boolean(b)) => *b,
                    Some(_) => {
                        problems.push(format!("{at}: 'model_color' must be true/false; ignored"));
                        false
                    }
                    None => false,
                };
                let r = MaterialRule {
                    model_color,
                    part: str_of(t, "part", &at, problems),
                    model: str_of(t, "model", &at, problems),
                    material: str_of(t, "material", &at, problems),
                    hue_min,
                    hue_max,
                    sat_min,
                    sat_max,
                    val_min,
                    val_max,
                };
                if r.material.is_empty() {
                    problems.push(format!("{at}: has no 'material'; ignored"));
                    continue;
                }
                out.push(r);
            }
        }
        Some(_) => problems.push("'rule' must be written as [[rule]] (an array of tables); ignored".into()),
        None => {}
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
                if r.model_color {
                    t.insert("model_color".into(), Value::Boolean(true));
                }
                Value::Table(t)
            })
            .collect(),
    )
}

fn parse_file(p: &Path) -> Result<toml::Table, String> {
    let text = std::fs::read_to_string(p).map_err(|_| "cannot read".to_string())?;
    toml::from_str(&text).map_err(|e| e.to_string())
}

fn warn_problems(p: &Path, problems: Vec<String>) {
    for e in problems {
        krbr_core::warn!("{}: {}", p.display(), e);
    }
}

/// model_map.toml を新しく作るときの中身
const GLOBAL_MAP_HEADER: &str = "# krbr: 3D モデルごとの材質の設定（全体設定。他のプロジェクトと共有）\n\
# [[model]] path（展開前のパス）か hash（ファイル内容の XXH64）が一致したモデルに使う。\n\
#   material: 全部位, parts: 部位ごと（キーは XDE の名前 / \"kicad:<色名>\" / \"#rrggbb\" / \"*\"）\n\
# [[rule]] 色・名前の規則（上から順に評価。組み込みの規則より先）。材質そのものの定義は materials/*.toml の [material.<名前>]\n";

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
    /// 読む。知らない表・鍵、型の誤り、material の無い規則は、ファイル名つきで警告して読み飛ばす
    pub fn load(p: &Path) -> Result<Self, String> {
        let root = parse_file(p)?;
        let mut problems = Vec::new();
        for k in root.keys() {
            match k.as_str() {
                "model" | "rule" => {}
                "rules" => problems.push("unknown table 'rules' (the rules are [[rule]]); ignored".into()),
                "presets" | "material" => problems.push(format!("unknown table '{k}' (materials are defined in materials/*.toml as [material.<name>]); ignored")),
                _ => problems.push(format!("unknown table '{k}' (expected [[model]] / [[rule]]); ignored")),
            }
        }
        let g = Self { models: read_models(&root, &mut problems), rules: read_rules(&root, &mut problems) };
        warn_problems(p, problems);
        Ok(g)
    }

    /// 書く。ProjectOverrides::save と同じく、一時ファイルに書いてから置き換え、手で書いたコメントと並びを残す。
    /// [[rule]] は手で書くもので、プログラムは変えないので、読んだときと同じなら触らない（規則の間のコメントも残る）
    pub fn save(&self, p: &Path) -> Result<(), String> {
        let mut root = toml::Table::new();
        if !self.models.is_empty() {
            root.insert("model".into(), write_models(&self.models));
        }
        if !self.rules.is_empty() {
            root.insert("rule".into(), write_rules(&self.rules));
        }
        let rules_on_disk = parse_file(p).ok().map(|t| read_rules(&t, &mut Vec::new()));
        let keys: &[&str] = if rules_on_disk.as_ref() == Some(&self.rules) { &["model"] } else { &["model", "rule"] };
        crate::board_file::try_edit_toml(p, GLOBAL_MAP_HEADER, |doc| crate::board_file::try_replace_tables(doc, &root, keys))
    }
    pub fn upsert(&mut self, path: &str, hash: &str) -> &mut ModelEntry {
        upsert_model(&mut self.models, path, hash)
    }
}

/// プロジェクトの上書き（基板の設定ファイル <名前>.krbr の [ref.<部品番号>] と [[model]]）
#[derive(Debug, Clone, Default)]
pub struct ProjectOverrides {
    pub refs: BTreeMap<String, PartAssignment>,
    pub models: Vec<ModelEntry>,
}

impl ProjectOverrides {
    /// 基板の設定ファイルから [ref.*] / [[model]] を読む（ほかの表は crate::config が読む）。
    /// 知らない鍵・型の誤りは、ファイル名つきで警告して読み飛ばす
    pub fn load(p: &Path) -> Result<Self, String> {
        let root = parse_file(p)?;
        let mut problems = Vec::new();
        let mut o = Self { refs: BTreeMap::new(), models: read_models(&root, &mut problems) };
        match root.get("ref") {
            Some(Value::Table(r)) => {
                for (k, v) in r {
                    let at = format!("[ref.{k}]");
                    match v {
                        Value::Table(t) => {
                            check_keys(t, &["material", "parts"], &at, &mut problems);
                            o.refs.insert(k.clone(), read_assignment(t, &at, &mut problems));
                        }
                        _ => problems.push(format!("{at}: must be a table {{ material = \"...\" }}; ignored")),
                    }
                }
            }
            Some(_) => problems.push("'ref' must be a table ([ref.<reference>]); ignored".into()),
            None => {}
        }
        warn_problems(p, problems);
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
        // 基板の設定ファイル <名前>.krbr の [ref.*] / [[model]] だけを差し替える（ほかの表とコメントは残す）。
        // 新しい内容を作れなければファイルを書かない（上書きが消えない）
        crate::board_file::try_edit_toml(p, &crate::board_file::template(), |doc| crate::board_file::try_replace_tables(doc, &root, &["ref", "model"]))
    }
    pub fn upsert(&mut self, path: &str, hash: &str) -> &mut ModelEntry {
        upsert_model(&mut self.models, path, hash)
    }
}

pub fn builtin_rules() -> &'static Vec<MaterialRule> {
    static RULES: OnceLock<Vec<MaterialRule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let root: toml::Table = toml::from_str(BUILTIN_RULES).expect("builtin rules");
        let mut problems = Vec::new();
        let rules = read_rules(&root, &mut problems);
        debug_assert!(problems.is_empty(), "builtin rules: {problems:?}");
        rules
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

/// 材質がどの段階で決まったか（モジュールの先頭の解決順序の 1〜5）。表示用の文字列（ResolvedMaterial::source）はこれから作る
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MaterialStage {
    /// 1. 部品番号ごとの上書き（プロジェクト）
    ProjectRef,
    /// 2. 3D モデルごとの上書き（プロジェクト）
    ProjectModel,
    /// 3. 3D モデルごとの設定（全体）
    GlobalModel,
    /// 4. 色・名前の規則（全体）。規則の番号（1 から）
    GlobalRule(usize),
    /// 4. 色・名前の規則（組み込み）。規則の番号（1 から）
    BuiltinRule(usize),
    /// 5. 既定値
    #[default]
    Fallback,
}

impl MaterialStage {
    /// 表示用の文字列（krbr material show、ビューアの説明）。reference は部品番号（ProjectRef のときだけ使う）
    pub fn label(&self, reference: &str) -> String {
        match self {
            MaterialStage::ProjectRef => format!("ref {reference} (project)"),
            MaterialStage::ProjectModel => "model (project)".into(),
            MaterialStage::GlobalModel => "model (global)".into(),
            MaterialStage::GlobalRule(i) => format!("rule {i} (global)"),
            MaterialStage::BuiltinRule(i) => format!("rule {i} (builtin)"),
            MaterialStage::Fallback => "default".into(),
        }
    }

    /// label の逆。表示用の文字列しか持っていないとき（MaterialAssignment::source）に段階を知る。label 以外の文字列なら None
    pub fn from_label(s: &str) -> Option<Self> {
        let rule = |suffix: &str| s.strip_prefix("rule ").and_then(|r| r.strip_suffix(suffix)).and_then(|n| n.parse::<usize>().ok());
        Some(match s {
            "model (project)" => MaterialStage::ProjectModel,
            "model (global)" => MaterialStage::GlobalModel,
            "default" => MaterialStage::Fallback,
            _ if s.starts_with("ref ") && s.ends_with(" (project)") => MaterialStage::ProjectRef,
            _ => {
                if let Some(i) = rule(" (global)") {
                    MaterialStage::GlobalRule(i)
                } else {
                    MaterialStage::BuiltinRule(rule(" (builtin)")?)
                }
            }
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedMaterial {
    pub material: Material,
    /// 材質ライブラリの名前
    pub name: String,
    /// どの段階で決まったか（表示用。stage.label() と同じ "ref U2 (project)" など）
    pub source: String,
    /// どの段階で決まったか。段階で処理を分けるときは source の文字列ではなくこちらを見る
    pub stage: MaterialStage,
}

/// 規則の材質名。model_color なら、3D モデルの面の色を部位の色の調整（+color=）にする（材質名に色の指定があればそちら）
fn rule_material(rule: &MaterialRule, part: Option<&ModelPart>) -> String {
    match part.filter(|p| rule.model_color && p.has_color) {
        Some(p) => with_variant(&rule.material, |v| {
            v.color.get_or_insert(p.srgb.to_array().map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8));
        }),
        None => rule.material.clone(),
    }
}

pub struct MaterialResolver<'a> {
    lib: &'a MaterialLibrary,
    project: &'a ProjectOverrides,
    global: &'a GlobalModelMap,
    /// 警告した材質名（同じ名前を部位ごとに何度も警告しない）。シーンを組み立てるたびに作り直すので、
    /// ビューアで設定を直しては試すと、組み立て直すたびにまた知らせる
    warned: Mutex<BTreeSet<String>>,
}

impl<'a> MaterialResolver<'a> {
    pub fn new(lib: &'a MaterialLibrary, project: &'a ProjectOverrides, global: &'a GlobalModelMap) -> Self {
        Self { lib, project, global, warned: Mutex::new(BTreeSet::new()) }
    }

    pub fn library(&self) -> &'a MaterialLibrary {
        self.lib
    }

    /// 名前が材質ライブラリにあれば材質を作る。無ければ警告して None（次の段階へ）。
    /// "+emit=" の書き方の誤りは、ライブラリに無いのとは別に知らせる
    fn make(&self, name: &str, stage: MaterialStage, q: &MaterialQuery) -> Option<ResolvedMaterial> {
        if name.is_empty() {
            return None;
        }
        let source = stage.label(q.reference);
        let Some(m) = self.lib.find_variant(name) else {
            if self.warned.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string()) {
                match variant_error(name) {
                    Some(e) => krbr_core::warn!("material '{}' ({}): {}; skipped", name, source, e),
                    None => krbr_core::warn!("material '{}' ({}) is not in the material library; skipped", name, source),
                }
            }
            return None;
        };
        let mut r = ResolvedMaterial { material: m, name: name.to_string(), source, stage };
        r.material.name = name.to_string();
        Some(r)
    }

    pub fn resolve(&self, q: &MaterialQuery) -> ResolvedMaterial {
        // 1. 部品番号ごとの上書き（プロジェクト）
        if !q.reference.is_empty() {
            if let Some(a) = self.project.refs.get(q.reference) {
                if let Some(r) = self.make(&a.lookup(q.part), MaterialStage::ProjectRef, q) {
                    return r;
                }
            }
        }
        // 2. 3D モデルごとの上書き（プロジェクト）
        for e in &self.project.models {
            if e.matches(q.model_spec, q.model_hash) {
                if let Some(r) = self.make(&e.assign.lookup(q.part), MaterialStage::ProjectModel, q) {
                    return r;
                }
            }
        }
        // 3. 3D モデルごとの設定（全体）
        for e in &self.global.models {
            if e.matches(q.model_spec, q.model_hash) {
                if let Some(r) = self.make(&e.assign.lookup(q.part), MaterialStage::GlobalModel, q) {
                    return r;
                }
            }
        }
        // 4. 色・名前の規則（全体 → 組み込み）
        for (i, rule) in self.global.rules.iter().enumerate() {
            if rule.matches(q.model_file, q.part) {
                if let Some(r) = self.make(&rule_material(rule, q.part), MaterialStage::GlobalRule(i + 1), q) {
                    return r;
                }
            }
        }
        for (i, rule) in builtin_rules().iter().enumerate() {
            if rule.matches(q.model_file, q.part) {
                if let Some(r) = self.make(&rule_material(rule, q.part), MaterialStage::BuiltinRule(i + 1), q) {
                    return r;
                }
            }
        }
        // 5. 既定値
        let stage = MaterialStage::Fallback;
        let mut r = ResolvedMaterial { name: "default".into(), source: stage.label(q.reference), stage, ..Default::default() };
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
        assert_eq!(r.resolve(&q(&pins)).name, "metal_tin+color=#d2d1c7"); // 錫の規則は model_color
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
        // 上書きで決めた材質は、その材質の色（モデルの色は使わない）
        assert_eq!(r.resolve(&q(&body)).material.base_color, lib.find("plastic").unwrap().base_color);
        // 規則の model_color：モデルの色が部位の色の調整になる
        let grey = ModelPart { name: String::new(), srgb: Vec3::new(0.5, 0.25, 1.0), alpha: 1.0, has_color: true };
        let none = ProjectOverrides::default();
        let res = MaterialResolver::new(&lib, &none, &global).resolve(&q(&grey));
        assert_eq!(res.name, "plastic+color=#8040ff");
        assert!((res.material.base_color.x - 0.2158).abs() < 0.001);
    }

    // ref > プロジェクトのモデル > 全体のモデル > 規則 > 既定 の順に、段階を 1 つずつ足して確かめる
    #[test]
    fn resolution_order_full_chain() {
        let mut lib = MaterialLibrary::builtin();
        lib.load_text("[material.a]
[material.b]
[material.c]
[material.d]
", "test").unwrap();
        let pins = ModelPart { name: String::new(), srgb: Vec3::new(0.824, 0.820, 0.781), alpha: 1.0, has_color: true };
        let q = MaterialQuery { reference: "U3", model_spec: "${KICAD10_3DMODEL_DIR}/X.step", model_hash: "abcd", model_file: "X.step", part: Some(&pins) };
        let mut project = ProjectOverrides::default();
        let mut global = GlobalModelMap::default();
        let resolve = |p: &ProjectOverrides, g: &GlobalModelMap| {
            let r = MaterialResolver::new(&lib, p, g).resolve(&q);
            (r.name, r.source)
        };

        // 組み込みの規則（kicad:metal_grey_pins）。先頭の 3 つは電解コンデンサのモデル用
        assert_eq!(resolve(&project, &global), ("metal_tin+color=#d2d1c7".into(), "rule 4 (builtin)".into()));
        global.rules.push(MaterialRule { part: "kicad:*".into(), material: "d".into(), ..Default::default() });
        assert_eq!(resolve(&project, &global).0, "d");
        global.upsert("${KICAD10_3DMODEL_DIR}/X.step", "").assign.material = "c".into();
        assert_eq!(resolve(&project, &global), ("c".into(), "model (global)".into()));
        project.upsert("", "abcd").assign.material = "b".into(); // ハッシュだけで照合
        assert_eq!(resolve(&project, &global), ("b".into(), "model (project)".into()));
        project.refs.entry("U3".into()).or_default().material = "a".into();
        assert_eq!(resolve(&project, &global), ("a".into(), "ref U3 (project)".into()));
        assert_eq!(MaterialResolver::new(&lib, &project, &global).resolve(&q).stage, MaterialStage::ProjectRef);

        // 部位ごとの指定は、その部位にだけ効く
        project.refs.insert("U3".into(), PartAssignment::default());
        project.refs.get_mut("U3").unwrap().parts.insert("#ffffff".into(), "a".into());
        assert_eq!(resolve(&project, &global).0, "b");
        project.refs.get_mut("U3").unwrap().parts.insert("kicad:metal_grey_pins".into(), "a".into());
        assert_eq!(resolve(&project, &global).0, "a");
        // 材質ライブラリにない名前は飛ばして次の段階へ
        project.refs.get_mut("U3").unwrap().parts.insert("kicad:metal_grey_pins".into(), "does_not_exist".into());
        assert_eq!(resolve(&project, &global).0, "b");
    }

    #[test]
    fn stage_labels_round_trip() {
        for st in [MaterialStage::ProjectRef, MaterialStage::ProjectModel, MaterialStage::GlobalModel, MaterialStage::GlobalRule(3), MaterialStage::BuiltinRule(12), MaterialStage::Fallback] {
            assert_eq!(MaterialStage::from_label(&st.label("U 7")), Some(st), "{st:?}");
        }
        assert_eq!(MaterialStage::from_label("rule x (global)"), None);
        assert_eq!(MaterialStage::from_label("something"), None);
    }

    #[test]
    fn invalid_entries_are_reported() {
        let root: toml::Table = toml::from_str(
            "[[rule]]\npart = \"kicad:*\"\n[[rule]]\nmaterial = \"plastic\"\nval_mn = 0.5\nsat_max = \"0.1\"\n[[model]]\nmaterial = \"x\"\n[[model]]\npath = \"a.step\"\nparts = { \"*\" = 1 }\n",
        )
        .unwrap();
        let mut problems = Vec::new();
        let rules = read_rules(&root, &mut problems);
        assert_eq!(rules.len(), 1); // material の無い規則は使わない（警告する）
        assert_eq!(rules[0].sat_max, None);
        let models = read_models(&root, &mut problems);
        assert_eq!(models.len(), 1);
        assert!(models[0].assign.parts.is_empty());
        assert_eq!(problems.len(), 5, "{problems:#?}");
        assert!(problems.iter().any(|p| p.contains("val_mn")));
    }

    #[test]
    fn global_model_map_save_keeps_comments_and_rules() {
        let dir = std::env::temp_dir().join(format!("krbr_rust_globalmap_keep_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("model_map.toml");
        std::fs::write(&p, "# my rules\n[[rule]]   # LEDs\nmodel = \"*LED*\"\nmaterial = \"glossy_plastic\"\n").unwrap();
        let mut g = GlobalModelMap::load(&p).unwrap();
        g.upsert("a.step", "").assign.material = "ceramic".into();
        g.save(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# my rules") && text.contains("[[rule]]   # LEDs"), "{text}");
        let h = GlobalModelMap::load(&p).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!((h.rules, h.models[0].assign.material.as_str()), (g.rules, "ceramic"));
    }

    #[test]
    fn model_entries_match_by_unexpanded_path_or_content_hash() {
        let e = ModelEntry { path: "${KIPRJMOD}/a.step".into(), hash: "1234".into(), ..Default::default() };
        assert!(e.matches("${KIPRJMOD}/a.step", ""));
        assert!(e.matches("C:/elsewhere/renamed.step", "1234"));
        assert!(!e.matches("C:/elsewhere/renamed.step", "9999"));
    }

    #[test]
    fn global_model_map_round_trip() {
        let dir = std::env::temp_dir().join(format!("krbr_rust_globalmap_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut g = GlobalModelMap::default();
        g.upsert("a.step", "").assign.parts.insert("*".into(), "ceramic".into());
        g.rules.push(MaterialRule { model: "*LED*".into(), val_min: Some(0.5), material: "glossy_plastic".into(), ..Default::default() });
        let p = dir.join("model_map.toml");
        g.save(&p).unwrap();
        let h = GlobalModelMap::load(&p).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(h.rules.len(), 1);
        assert_eq!(h.rules[0].model, "*LED*");
        assert_eq!(h.rules[0].val_min, Some(0.5));
        assert_eq!(h.rules[0].material, "glossy_plastic");
        assert_eq!(h.models[0].assign.parts["*"], "ceramic");
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
