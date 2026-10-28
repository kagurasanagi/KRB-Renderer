//! VRML 2.0（VRML97）の読み込み：.wrl と、gzip で圧縮した .wrz。KiCad の 3D ライブラリ（KiCad 8 まで）や
//! 部品メーカーのモデルの形式。
//!
//! ファイルを汎用のノードの木（ノードの種類と、フィールドの名前と値）に読み、形のあるノードだけをたどって三角形にする。
//! - 形：Shape の IndexedFaceSet（多角形は耳切り法で三角形に分割、法線は creaseAngle に従って滑らかにする。
//!   ファイルに法線があればそれを使う）。Box・Sphere などの基本形状、線・点、Inline（別ファイル）は読まない
//! - 配置：Transform（translation・rotation・scale・scaleOrientation・center）、Group などの children、Switch の選んだ子、LOD の最初の段
//! - 部位：Material ごとに 1 つ（DEF の名前、diffuseColor、transparency）。KiCad 標準モデルの色は STEP と同じ値なので、
//!   既定の材質の割り当て（"kicad:metal_grey_pins" など）がそのまま効く
//! - 単位：KiCad と同じく 1 単位 = 0.1 インチ（2.54 mm）。座標の向きは STEP と同じ（Z が基板の上）
//! - DEF / USE、コメント、PROTO / EXTERNPROTO / ROUTE（読み飛ばす）。VRML 1.0 は読まない（エラーにする）

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::Path;
use std::rc::Rc;

use glam::{DMat3, DMat4, DQuat, DVec2, DVec3, Vec3};

use crate::{ModelMesh, ModelPart};

/// 読み込み器の版。キャッシュのキーに入れる（形の作り方を変えたら上げる）
pub const VRML_LOADER_VERSION: u32 = 1;

/// VRML の 1 単位（mm）。KiCad の 3D ビューアと同じく 0.1 インチ
pub const VRML_UNIT_MM: f64 = 2.54;

/// 入れ子の深さの上限（壊れたファイルでスタックを使い切らない）
const MAX_DEPTH: usize = 256;

/// .wrl / .wrz を読む（中身が gzip なら、拡張子によらず展開する）
pub fn load_vrml(p: &Path) -> Result<ModelMesh, String> {
    let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
    parse_vrml(&decode(&bytes)?)
}

fn decode(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut out = Vec::new();
        flate2::read::MultiGzDecoder::new(bytes).read_to_end(&mut out).map_err(|e| format!("gzip: {e}"))?;
        return Ok(String::from_utf8_lossy(&out).into_owned());
    }
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

/// VRML 2.0 の文字列から、メッシュ（mm）と部位を作る
pub fn parse_vrml(text: &str) -> Result<ModelMesh, String> {
    let header = text.trim_start_matches('\u{feff}').lines().next().unwrap_or("").trim();
    if header.starts_with("#VRML V1.0") {
        return Err("VRML 1.0 is not supported (only VRML 2.0 / VRML97)".into());
    }
    if !header.starts_with("#VRML V2.0") {
        return Err("not a VRML 2.0 file (the first line is not \"#VRML V2.0 ...\")".into());
    }
    let mut p = Parser { toks: tokenize(text), pos: 0, defs: HashMap::new(), depth: 0, unknown_use: 0 };
    let roots = p.statements()?;
    let mut b = Builder::default();
    let root = DMat4::from_scale(DVec3::splat(VRML_UNIT_MM));
    for n in &roots {
        b.node(n, root, 0);
    }
    if p.unknown_use > 0 {
        krbr_core::debug!("VRML: {} USE of an undefined name ignored", p.unknown_use);
    }
    for (what, n) in &b.ignored {
        krbr_core::debug!("VRML: {n} {what} node(s) not read");
    }
    if b.mesh.indices.is_empty() {
        return Err("no faces (IndexedFaceSet) in the file".into());
    }
    Ok(ModelMesh { mesh: b.mesh, parts: b.parts })
}

// ---------------------------------------------------------------- 字句

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Open,
    Close,
    LBracket,
    RBracket,
    Word(String),
    Str(String),
}

/// コメント（# から行末）、空白とカンマは区切り。{ } [ ] は 1 文字で 1 つ、"..." は文字列（\" と \\ を戻す）
fn tokenize(s: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '#' => {
                for c in it.by_ref() {
                    if c == '\n' || c == '\r' {
                        break;
                    }
                }
            }
            c if c.is_whitespace() || c == ',' => {}
            '{' => out.push(Tok::Open),
            '}' => out.push(Tok::Close),
            '[' => out.push(Tok::LBracket),
            ']' => out.push(Tok::RBracket),
            '"' => {
                let mut v = String::new();
                while let Some(c) = it.next() {
                    match c {
                        '\\' => {
                            if let Some(e) = it.next() {
                                v.push(e);
                            }
                        }
                        '"' => break,
                        c => v.push(c),
                    }
                }
                out.push(Tok::Str(v));
            }
            c => {
                let mut w = String::from(c);
                while let Some(&n) = it.peek() {
                    if n.is_whitespace() || matches!(n, ',' | '#' | '{' | '}' | '[' | ']' | '"') {
                        break;
                    }
                    w.push(n);
                    it.next();
                }
                out.push(Tok::Word(w));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- ノードの木

#[derive(Debug, Default)]
struct Node {
    kind: String,
    /// DEF の名前（無ければ空）
    def: String,
    fields: Vec<(String, Field)>,
}

#[derive(Debug)]
enum Field {
    /// 数・TRUE / FALSE・文字列の並び（[ ] の中も外も）
    Values(Vec<String>),
    /// ノード（NULL・未定義の USE なら None）
    Node(Option<Rc<Node>>),
    Nodes(Vec<Rc<Node>>),
}

impl Node {
    fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|(n, _)| n == name).map(|(_, f)| f)
    }

    fn values(&self, name: &str) -> &[String] {
        match self.field(name) {
            Some(Field::Values(v)) => v,
            _ => &[],
        }
    }

    fn numbers(&self, name: &str) -> Vec<f64> {
        self.values(name).iter().filter_map(|v| v.parse::<f64>().ok()).collect()
    }

    fn number(&self, name: &str, default: f64) -> f64 {
        self.numbers(name).first().copied().unwrap_or(default)
    }

    fn vec3(&self, name: &str, default: DVec3) -> DVec3 {
        match self.numbers(name)[..] {
            [x, y, z, ..] => DVec3::new(x, y, z),
            _ => default,
        }
    }

    fn boolean(&self, name: &str, default: bool) -> bool {
        match self.values(name).first().map(String::as_str) {
            Some("TRUE") => true,
            Some("FALSE") => false,
            _ => default,
        }
    }

    fn child(&self, name: &str) -> Option<&Rc<Node>> {
        match self.field(name) {
            Some(Field::Node(n)) => n.as_ref(),
            Some(Field::Nodes(v)) => v.first(),
            _ => None,
        }
    }

    fn children(&self, name: &str) -> &[Rc<Node>] {
        match self.field(name) {
            Some(Field::Nodes(v)) => v,
            Some(Field::Node(Some(n))) => std::slice::from_ref(n),
            _ => &[],
        }
    }
}

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
    defs: HashMap<String, Rc<Node>>,
    depth: usize,
    unknown_use: usize,
}

fn is_scalar(w: &str) -> bool {
    w == "TRUE" || w == "FALSE" || w.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '+' || c == '.')
}

impl Parser {
    fn peek(&self, k: usize) -> Option<&Tok> {
        self.toks.get(self.pos + k)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn word(&mut self) -> Result<String, String> {
        match self.next() {
            Some(Tok::Word(w)) => Ok(w),
            t => Err(format!("expected a name, found {t:?}")),
        }
    }

    /// ファイルの最上位：ノードの並び（PROTO などは読み飛ばす）
    fn statements(&mut self) -> Result<Vec<Rc<Node>>, String> {
        let mut out = Vec::new();
        while self.peek(0).is_some() {
            if self.skip_proto_or_route()? {
                continue;
            }
            if let Some(n) = self.node()? {
                out.push(n);
            }
        }
        Ok(out)
    }

    /// PROTO / EXTERNPROTO / ROUTE なら読み飛ばして true
    fn skip_proto_or_route(&mut self) -> Result<bool, String> {
        match self.peek(0) {
            Some(Tok::Word(w)) if w == "PROTO" => {
                self.pos += 2; // PROTO 名前
                self.skip_balanced(Tok::LBracket, Tok::RBracket)?;
                self.skip_balanced(Tok::Open, Tok::Close)?;
                Ok(true)
            }
            Some(Tok::Word(w)) if w == "EXTERNPROTO" => {
                self.pos += 2;
                self.skip_balanced(Tok::LBracket, Tok::RBracket)?;
                // URL：文字列か、文字列の並び
                match self.peek(0) {
                    Some(Tok::LBracket) => self.skip_balanced(Tok::LBracket, Tok::RBracket)?,
                    _ => self.pos += 1,
                }
                Ok(true)
            }
            Some(Tok::Word(w)) if w == "ROUTE" => {
                self.pos += 4; // ROUTE a.b TO c.d
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn skip_balanced(&mut self, open: Tok, close: Tok) -> Result<(), String> {
        if self.next().as_ref() != Some(&open) {
            return Err(format!("expected {open:?}"));
        }
        let mut level = 1;
        while level > 0 {
            match self.next() {
                Some(t) if t == open => level += 1,
                Some(t) if t == close => level -= 1,
                Some(_) => {}
                None => return Err(format!("missing {close:?}")),
            }
        }
        Ok(())
    }

    /// DEF 名前 ノード / USE 名前 / NULL / 種類 { フィールド }
    fn node(&mut self) -> Result<Option<Rc<Node>>, String> {
        let w = self.word()?;
        match w.as_str() {
            "DEF" => {
                let name = self.word()?;
                let kind = self.word()?;
                let n = self.node_body(kind, name.clone())?;
                self.defs.insert(name, Rc::clone(&n));
                Ok(Some(n))
            }
            "USE" => {
                let name = self.word()?;
                let n = self.defs.get(&name).cloned();
                if n.is_none() {
                    self.unknown_use += 1;
                }
                Ok(n)
            }
            "NULL" => Ok(None),
            _ => self.node_body(w, String::new()).map(Some),
        }
    }

    fn node_body(&mut self, kind: String, def: String) -> Result<Rc<Node>, String> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err("nodes nested too deeply".into());
        }
        if self.next() != Some(Tok::Open) {
            return Err(format!("expected '{{' after {kind}"));
        }
        let mut n = Node { kind, def, fields: Vec::new() };
        loop {
            match self.peek(0) {
                Some(Tok::Close) => {
                    self.pos += 1;
                    break;
                }
                None => return Err(format!("missing '}}' of {}", n.kind)),
                _ => {}
            }
            if self.skip_proto_or_route()? {
                continue;
            }
            let name = self.word()?;
            let value = self.value()?;
            n.fields.push((name, value));
        }
        self.depth -= 1;
        Ok(Rc::new(n))
    }

    fn starts_node(&self) -> bool {
        match (self.peek(0), self.peek(1)) {
            (Some(Tok::Word(w)), _) if w == "DEF" || w == "USE" || w == "NULL" => true,
            (Some(Tok::Word(_)), Some(Tok::Open)) => true,
            _ => false,
        }
    }

    fn value(&mut self) -> Result<Field, String> {
        if self.peek(0) == Some(&Tok::LBracket) {
            self.pos += 1;
            if self.starts_node() {
                let mut nodes = Vec::new();
                loop {
                    match self.peek(0) {
                        Some(Tok::RBracket) => {
                            self.pos += 1;
                            break;
                        }
                        None => return Err("missing ']'".into()),
                        _ => {}
                    }
                    if self.skip_proto_or_route()? {
                        continue;
                    }
                    if let Some(n) = self.node()? {
                        nodes.push(n);
                    }
                }
                return Ok(Field::Nodes(nodes));
            }
            let mut v = Vec::new();
            loop {
                match self.next() {
                    Some(Tok::RBracket) => break,
                    Some(Tok::Word(w)) | Some(Tok::Str(w)) => v.push(w),
                    Some(t) => return Err(format!("unexpected {t:?} in a list")),
                    None => return Err("missing ']'".into()),
                }
            }
            return Ok(Field::Values(v));
        }
        if self.starts_node() {
            return Ok(Field::Node(self.node()?));
        }
        // 数・TRUE / FALSE・文字列を、次のフィールドの名前（数でない語）か '}' まで
        let mut v = Vec::new();
        loop {
            match self.peek(0) {
                Some(Tok::Str(s)) => v.push(s.clone()),
                Some(Tok::Word(w)) if is_scalar(w) => v.push(w.clone()),
                _ => break,
            }
            self.pos += 1;
        }
        Ok(Field::Values(v))
    }
}

// ---------------------------------------------------------------- 形状の組み立て

/// 部位の見分け：DEF の名前、色（sRGB）、透明度のビット
type PartKey = (String, Option<[u32; 3]>, u32);

#[derive(Default)]
struct Builder {
    mesh: krbr_core::Mesh,
    parts: Vec<ModelPart>,
    part_of: HashMap<PartKey, u32>,
    /// 読まなかったノードの種類と数（ログ用）
    ignored: BTreeMap<String, usize>,
}

/// Transform の行列：T · C · R · SR · S · SR⁻¹ · C⁻¹
fn transform_matrix(n: &Node) -> DMat4 {
    let rot = |name: &str| -> DQuat {
        match n.numbers(name)[..] {
            [x, y, z, a, ..] if DVec3::new(x, y, z).length_squared() > 0.0 => DQuat::from_axis_angle(DVec3::new(x, y, z).normalize(), a),
            _ => DQuat::IDENTITY,
        }
    };
    let t = n.vec3("translation", DVec3::ZERO);
    let c = n.vec3("center", DVec3::ZERO);
    let s = n.vec3("scale", DVec3::ONE);
    let (r, sr) = (rot("rotation"), rot("scaleOrientation"));
    DMat4::from_translation(t + c) * DMat4::from_quat(r) * DMat4::from_quat(sr) * DMat4::from_scale(s) * DMat4::from_quat(sr.inverse()) * DMat4::from_translation(-c)
}

impl Builder {
    fn ignore(&mut self, what: &str) {
        *self.ignored.entry(what.to_string()).or_default() += 1;
    }

    fn node(&mut self, n: &Node, m: DMat4, depth: usize) {
        if depth > MAX_DEPTH {
            return;
        }
        match n.kind.as_str() {
            "Transform" => {
                let m = m * transform_matrix(n);
                for c in n.children("children") {
                    self.node(c, m, depth + 1);
                }
            }
            "Group" | "Anchor" | "Billboard" | "Collision" => {
                for c in n.children("children") {
                    self.node(c, m, depth + 1);
                }
            }
            "Switch" => {
                let which = n.number("whichChoice", -1.0);
                if which >= 0.0 {
                    if let Some(c) = n.children("choice").get(which as usize) {
                        self.node(c, m, depth + 1);
                    }
                }
            }
            "LOD" => {
                if let Some(c) = n.children("level").first() {
                    self.node(c, m, depth + 1);
                }
            }
            "Shape" => self.shape(n, m),
            "Inline" => self.ignore("Inline"),
            // 見た目に関わらないもの（視点・光源・背景など）と、知らない種類（PROTO の実体など）は黙って飛ばす
            _ => {}
        }
    }

    /// Shape の材質から部位の番号
    fn part(&mut self, shape: &Node) -> u32 {
        let mat = shape.child("appearance").and_then(|a| a.child("material"));
        let (name, srgb, alpha) = match mat {
            Some(m) => {
                let c = m.vec3("diffuseColor", DVec3::splat(0.8)).clamp(DVec3::ZERO, DVec3::ONE).as_vec3();
                (m.def.clone(), Some(c), (1.0 - m.number("transparency", 0.0)).clamp(0.0, 1.0) as f32)
            }
            None => (String::new(), None, 1.0),
        };
        let key = (name.clone(), srgb.map(|c| c.to_array().map(f32::to_bits)), alpha.to_bits());
        if let Some(&i) = self.part_of.get(&key) {
            return i;
        }
        let i = self.parts.len() as u32;
        self.parts.push(ModelPart { name, srgb: srgb.unwrap_or(Vec3::ZERO), alpha, has_color: srgb.is_some() });
        self.part_of.insert(key, i);
        i
    }

    fn shape(&mut self, shape: &Node, m: DMat4) {
        let Some(g) = shape.child("geometry") else { return };
        match g.kind.as_str() {
            "IndexedFaceSet" => {
                let part = self.part(shape);
                self.face_set(g, m, part);
            }
            "IndexedLineSet" | "PointSet" => {}
            other => self.ignore(other),
        }
    }

    fn face_set(&mut self, g: &Node, m: DMat4, part: u32) {
        let points: Vec<DVec3> = g.child("coord").map(|c| c.numbers("point").chunks_exact(3).map(|p| m.transform_point3(DVec3::new(p[0], p[1], p[2]))).collect()).unwrap_or_default();
        if points.is_empty() {
            return;
        }
        let ccw = g.boolean("ccw", true);
        // 鏡に映す配置（行列式が負）では、表と裏が入れ替わる
        let flip = !ccw ^ (m.determinant() < 0.0);
        let index: Vec<i64> = g.numbers("coordIndex").iter().map(|&v| v as i64).collect();

        // 面（-1 で区切る）。範囲外の番号を含む面と、頂点が 3 つ未満の面は捨てる
        let mut faces: Vec<(usize, Vec<usize>)> = Vec::new(); // (coordIndex の中の開始位置, 頂点の番号)
        let mut start = 0;
        for (k, &v) in index.iter().chain(std::iter::once(&-1)).enumerate() {
            if v >= 0 {
                continue;
            }
            let f = &index[start..k.min(index.len())];
            if f.len() >= 3 && f.iter().all(|&i| (i as usize) < points.len()) {
                faces.push((start, f.iter().map(|&i| i as usize).collect()));
            }
            start = k + 1;
        }
        if faces.is_empty() {
            return;
        }
        // 面の法線（Newell の方法。面積の重み付き）
        let face_n: Vec<DVec3> = faces
            .iter()
            .map(|(_, f)| {
                let mut n = DVec3::ZERO;
                for k in 0..f.len() {
                    let (a, b) = (points[f[k]], points[f[(k + 1) % f.len()]]);
                    n += DVec3::new((a.y - b.y) * (a.z + b.z), (a.z - b.z) * (a.x + b.x), (a.x - b.x) * (a.y + b.y));
                }
                if flip {
                    -n
                } else {
                    n
                }
            })
            .collect();

        // ファイルの法線（あれば）
        let given: Vec<DVec3> = g.child("normal").map(|n| n.numbers("vector").chunks_exact(3).map(|v| DVec3::new(v[0], v[1], v[2])).collect()).unwrap_or_default();
        let normal_index: Vec<i64> = g.numbers("normalIndex").iter().map(|&v| v as i64).collect();
        let per_vertex = g.boolean("normalPerVertex", true);
        let normal_m = DMat3::from_mat4(m).inverse().transpose();
        let given_normal = |fi: usize, start: usize, corner: usize, coord: usize| -> Option<DVec3> {
            if given.is_empty() {
                return None;
            }
            let i = if per_vertex {
                if normal_index.is_empty() {
                    coord as i64
                } else {
                    *normal_index.get(start + corner)?
                }
            } else if normal_index.is_empty() {
                fi as i64
            } else {
                *normal_index.get(fi)?
            };
            let n = normal_m * *given.get(usize::try_from(i).ok()?)?;
            (n.length_squared() > 0.0).then(|| n.normalize())
        };

        // creaseAngle：同じ頂点を使う面のうち、法線の角度がこれ以下のものと平均する
        let crease = g.number("creaseAngle", 0.0).max(0.0);
        let mut faces_at: HashMap<usize, Vec<usize>> = HashMap::new();
        if crease > 0.0 && given.is_empty() {
            for (fi, (_, f)) in faces.iter().enumerate() {
                for &v in f {
                    faces_at.entry(v).or_default().push(fi);
                }
            }
        }
        let cos_crease = crease.min(std::f64::consts::PI).cos();
        let smooth_normal = |fi: usize, v: usize| -> DVec3 {
            let nf = face_n[fi].normalize_or_zero();
            let Some(adj) = faces_at.get(&v) else { return nf };
            let mut sum = DVec3::ZERO;
            for &g in adj {
                if face_n[g].normalize_or_zero().dot(nf) >= cos_crease - 1e-9 {
                    sum += face_n[g];
                }
            }
            sum.try_normalize().unwrap_or(nf)
        };

        // 頂点：同じ点・同じ法線の角は共有する
        let mut vert_of: HashMap<(usize, [u64; 3]), u32> = HashMap::new();
        for (fi, (start, f)) in faces.iter().enumerate() {
            if face_n[fi].length_squared() == 0.0 {
                continue; // 面積の無い面
            }
            let corner_vertex: Vec<u32> = f
                .iter()
                .enumerate()
                .map(|(k, &v)| {
                    let n = given_normal(fi, *start, k, v).unwrap_or_else(|| smooth_normal(fi, v));
                    let key = (v, n.to_array().map(f64::to_bits));
                    *vert_of.entry(key).or_insert_with(|| self.mesh.add_vertex(points[v].as_vec3(), n.as_vec3()))
                })
                .collect();
            for [a, b, c] in triangulate(&f.iter().map(|&v| points[v]).collect::<Vec<_>>(), face_n[fi]) {
                let (a, b, c) = (corner_vertex[a], corner_vertex[b], corner_vertex[c]);
                if flip {
                    self.mesh.add_triangle(a, c, b, part);
                } else {
                    self.mesh.add_triangle(a, b, c, part);
                }
            }
        }
    }
}

/// 多角形を三角形に分ける（耳切り法。角の番号の 3 つ組を、元の多角形と同じ回る向きで返す）。normal は面の法線
/// （2 次元に投影する面を選ぶのに使う）。耳が見つからない（自己交差など）ときは、残りを扇形に分ける
fn triangulate(poly: &[DVec3], normal: DVec3) -> Vec<[usize; 3]> {
    let n = poly.len();
    if n == 3 {
        return vec![[0, 1, 2]];
    }
    // 法線の一番大きい成分の軸を落として 2 次元にする。投影で回る向きが変わることがあるので、向きは面積の符号で見る
    let a = normal.abs();
    let to2 = |p: DVec3| -> DVec2 {
        if a.x >= a.y && a.x >= a.z {
            DVec2::new(p.y, p.z)
        } else if a.y >= a.z {
            DVec2::new(p.z, p.x)
        } else {
            DVec2::new(p.x, p.y)
        }
    };
    let pts: Vec<DVec2> = poly.iter().map(|&p| to2(p)).collect();
    let area: f64 = (0..n).map(|i| pts[i].perp_dot(pts[(i + 1) % n])).sum();
    let orient = if area >= 0.0 { 1.0 } else { -1.0 };
    let cross = |a: DVec2, b: DVec2, c: DVec2| (b - a).perp_dot(c - a) * orient;
    let mut idx: Vec<usize> = (0..n).collect();
    let mut out = Vec::with_capacity(n - 2);
    let mut guard = 0;
    while idx.len() > 3 && guard < n * n {
        guard += 1;
        let m = idx.len();
        let mut clipped = false;
        for i in 0..m {
            let (ip, ic, inx) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            let (p, c, q) = (pts[ip], pts[ic], pts[inx]);
            if cross(p, c, q) <= 0.0 {
                continue; // へこんだ角
            }
            // ほかの頂点が三角形の中に無い
            let inside = idx.iter().any(|&j| {
                if j == ip || j == ic || j == inx {
                    return false;
                }
                let x = pts[j];
                cross(p, c, x) >= 0.0 && cross(c, q, x) >= 0.0 && cross(q, p, x) >= 0.0
            });
            if inside {
                continue;
            }
            out.push([ip, ic, inx]);
            idx.remove(i);
            clipped = true;
            break;
        }
        if !clipped {
            break;
        }
    }
    // 残り（3 つ、または耳が見つからなかった分）は扇形に
    for k in 1..idx.len().saturating_sub(1) {
        out.push([idx[0], idx[k], idx[k + 1]]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "#VRML V2.0 utf8\n";

    fn parse(body: &str) -> ModelMesh {
        parse_vrml(&format!("{HEAD}{body}")).unwrap()
    }

    /// 三角形の面積の和と、面積で重みを付けた法線の和（向きの確認）
    fn area_and_normal(m: &ModelMesh) -> (f64, DVec3) {
        let (mut area, mut n) = (0.0, DVec3::ZERO);
        for t in m.mesh.indices.chunks_exact(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.mesh.positions[i as usize].as_dvec3());
            let cr = (b - a).cross(c - a);
            area += cr.length() / 2.0;
            n += cr / 2.0;
        }
        (area, n)
    }

    #[test]
    fn a_triangle_in_tenths_of_an_inch_with_a_named_material() {
        let m = parse(
            "Shape { appearance Appearance { material DEF PIN-01 Material { diffuseColor 0.824 0.82 0.781 transparency 0.25 } }\n\
             geometry IndexedFaceSet { coord Coordinate { point [0 0 0, 1 0 0, 0 1 0] } coordIndex [0, 1, 2, -1] } }",
        );
        assert_eq!(m.mesh.positions, vec![Vec3::ZERO, Vec3::new(2.54, 0.0, 0.0), Vec3::new(0.0, 2.54, 0.0)]);
        assert_eq!(m.mesh.indices, vec![0, 1, 2]);
        assert!(m.mesh.normals.iter().all(|n| (*n - Vec3::Z).length() < 1e-6));
        assert_eq!(m.parts.len(), 1);
        let p = &m.parts[0];
        assert_eq!((p.name.as_str(), p.has_color, p.alpha), ("PIN-01", true, 0.75));
        // KiCad 標準モデルの色なので、STEP と同じ割り当ての鍵になる
        assert_eq!(p.kicad_color_name(), Some("metal_grey_pins"));
    }

    #[test]
    fn def_use_shares_geometry_and_parts() {
        let m = parse(
            "Shape { appearance Appearance { material DEF M Material { diffuseColor 1 0 0 } }\n\
               geometry DEF TRI IndexedFaceSet { coord Coordinate { point [0 0 0 1 0 0 0 1 0] } coordIndex [0 1 2] } }\n\
             Transform { translation 10 0 0 children [ Shape { appearance Appearance { material USE M } geometry USE TRI } ] }\n\
             Shape { geometry USE TRI }",
        );
        assert_eq!(m.mesh.triangle_count(), 3);
        assert_eq!(m.mesh.tri_material, vec![0, 0, 1]); // 材質の無い Shape は別の部位（色なし）
        assert_eq!(m.parts.len(), 2);
        assert!(!m.parts[1].has_color && m.parts[1].name.is_empty());
        assert!((m.mesh.positions[3].x - 25.4).abs() < 1e-5, "{:?}", m.mesh.positions[3]);
    }

    #[test]
    fn nested_transforms_compose_like_vrml() {
        // 外：Z 軸まわりに 90°、内：(1, 0, 0) へ動かして 2 倍。点 (1, 0, 0) -> 内で (3, 0, 0) -> 外で (0, 3, 0)
        let m = parse(
            "Transform { rotation 0 0 1 1.5707963267948966 children Transform { translation 1 0 0 scale 2 2 2 children [\n\
               Shape { geometry IndexedFaceSet { coord Coordinate { point [0 0 0, 1 0 0, 0 1 0] } coordIndex [0 1 2 -1] } } ] } }",
        );
        let p = m.mesh.positions[1] / 2.54;
        assert!((p - Vec3::new(0.0, 3.0, 0.0)).length() < 1e-5, "{p:?}");
        // center のまわりの回転：(1, 0, 0) を中心に 180° 回すと、原点は (2, 0, 0) へ
        let m = parse(
            "Transform { center 1 0 0 rotation 0 0 1 3.141592653589793 children Shape { geometry IndexedFaceSet {\n\
               coord Coordinate { point [0 0 0, 0 -1 0, 1 1 0] } coordIndex [0 1 2] } } }",
        );
        let p = m.mesh.positions[0] / 2.54;
        assert!((p - Vec3::new(2.0, 0.0, 0.0)).length() < 1e-5, "{p:?}");
    }

    #[test]
    fn polygons_are_split_and_concave_ones_keep_their_area() {
        // 正方形（4 角）と L 字（6 角、へこみあり）。どちらも面積と向き（+Z）が保たれる
        let m = parse(
            "Shape { geometry IndexedFaceSet { convex FALSE coord Coordinate { point [\n\
               0 0 0, 2 0 0, 2 1 0, 1 1 0, 1 2 0, 0 2 0 ] } coordIndex [0 1 2 3 4 5 -1] } }",
        );
        assert_eq!(m.mesh.triangle_count(), 4);
        let (area, n) = area_and_normal(&m);
        assert!((area - 3.0 * 2.54 * 2.54).abs() < 1e-4, "L area {area}");
        assert!(n.z > 0.0 && (n.length() - area).abs() < 1e-4, "{n:?}");
        let sq = parse("Shape { geometry IndexedFaceSet { coord Coordinate { point [0 0 0 1 0 0 1 1 0 0 1 0] } coordIndex [0 1 2 3] } }");
        assert_eq!(sq.mesh.triangle_count(), 2);
        assert!((area_and_normal(&sq).0 - 2.54 * 2.54).abs() < 1e-4);
    }

    /// 立方体（外向きの 6 面）
    fn cube(extra: &str) -> String {
        format!(
            "Shape {{ geometry IndexedFaceSet {{ {extra} coord Coordinate {{ point [0 0 0, 1 0 0, 1 1 0, 0 1 0, 0 0 1, 1 0 1, 1 1 1, 0 1 1] }}\n\
             coordIndex [0 3 2 1 -1, 4 5 6 7 -1, 0 1 5 4 -1, 2 3 7 6 -1, 1 2 6 5 -1, 3 0 4 7 -1] }} }}"
        )
    }

    #[test]
    fn crease_angle_decides_flat_or_smooth_normals() {
        // creaseAngle 0：角ごとに面の法線（24 頂点、軸の向き）
        let flat = parse(&cube(""));
        assert_eq!(flat.mesh.positions.len(), 24);
        assert!(flat.mesh.normals.iter().all(|n| n.abs().max_element() > 0.999));
        // 90° より大きい：隣の面と平均して斜めの法線（8 頂点）
        let smooth = parse(&cube("creaseAngle 1.6"));
        assert_eq!(smooth.mesh.positions.len(), 8);
        assert!(smooth.mesh.normals.iter().all(|n| (n.abs() - Vec3::splat(1.0 / 3f32.sqrt())).length() < 1e-5));
        // 法線は外向き：角 (1, 1, 1) の法線は (1, 1, 1) の向き
        let i = smooth.mesh.positions.iter().position(|p| (*p - Vec3::splat(2.54)).length() < 1e-5).unwrap();
        assert!(smooth.mesh.normals[i].min_element() > 0.5);
    }

    #[test]
    fn winding_follows_ccw_and_mirroring() {
        let (_, n) = area_and_normal(&parse(&cube("")));
        assert!(n.length() < 1e-6); // 閉じた形
        // 外向きか：各三角形の法線と、中心から三角形への向きが同じ側
        let outward = |m: &ModelMesh| {
            m.mesh.indices.chunks_exact(3).all(|t| {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.mesh.positions[i as usize].as_dvec3());
                let center = m.mesh.positions.iter().map(|p| p.as_dvec3()).sum::<DVec3>() / m.mesh.positions.len() as f64;
                (b - a).cross(c - a).dot((a + b + c) / 3.0 - center) > 0.0
            })
        };
        assert!(outward(&parse(&cube(""))));
        // ccw FALSE：ファイルの巻き方が逆（時計回りが表）。逆にした立方体を ccw FALSE で読むと外向き
        let reversed = cube("ccw FALSE").replace("0 3 2 1", "1 2 3 0").replace("4 5 6 7", "7 6 5 4").replace("0 1 5 4", "4 5 1 0").replace("2 3 7 6", "6 7 3 2").replace("1 2 6 5", "5 6 2 1").replace("3 0 4 7", "7 4 0 3");
        let m = parse(&reversed);
        assert!(outward(&m));
        // 法線も外向き（同じ点・同じ法線の頂点が、普通に読んだ立方体にある）
        let base = parse(&cube(""));
        let has = |p: Vec3, n: Vec3| base.mesh.positions.iter().zip(&base.mesh.normals).any(|(q, k)| (*q - p).length() < 1e-6 && (*k - n).length() < 1e-6);
        assert!(m.mesh.positions.iter().zip(&m.mesh.normals).all(|(p, n)| has(*p, *n)));
        // 鏡に映す配置（scale -1）でも外向き
        let mirrored = parse(&format!("Transform {{ scale -1 1 1 children [ {} ] }}", &cube("")[..]));
        assert!(outward(&mirrored));
    }

    #[test]
    fn given_normals_are_used() {
        let m = parse(
            "Shape { geometry IndexedFaceSet { coord Coordinate { point [0 0 0 1 0 0 0 1 0] } coordIndex [0 1 2 -1]\n\
               normal Normal { vector [0 0 1, 0.6 0 0.8] } normalIndex [0 1 0 -1] } }",
        );
        assert!((m.mesh.normals[1] - Vec3::new(0.6, 0.0, 0.8)).length() < 1e-6, "{:?}", m.mesh.normals);
        assert!((m.mesh.normals[0] - Vec3::Z).length() < 1e-6);
    }

    #[test]
    fn comments_protos_routes_and_unknown_nodes_are_skipped() {
        let m = parse(
            "# comment { [\n\
             PROTO Foo [ field SFFloat x 1 ] { Group { children [] } }\n\
             EXTERNPROTO Bar [ field SFFloat y ] \"bar.wrl#Bar\"\n\
             WorldInfo { title \"a } tricky [ title\" info [\"x\", \"y\"] }\n\
             NavigationInfo { type [\"EXAMINE\"] headlight TRUE }\n\
             Foo { x 2 }\n\
             DEF G Group { children [ Shape { geometry IndexedFaceSet { solid FALSE coord Coordinate { point [0 0 0 1 0 0 0 1 0] } coordIndex [0 1 2] } } ] }\n\
             ROUTE G.children TO G.children\n\
             Shape { geometry Box { size 1 1 1 } }\n\
             Inline { url \"other.wrl\" }",
        );
        assert_eq!(m.mesh.triangle_count(), 1);
    }

    #[test]
    fn errors_for_vrml1_empty_and_broken_files() {
        assert!(parse_vrml("#VRML V1.0 ascii\nSeparator { }").unwrap_err().contains("VRML 1.0"));
        assert!(parse_vrml("solid x\nfacet").unwrap_err().contains("not a VRML 2.0"));
        assert!(parse_vrml(HEAD).unwrap_err().contains("no faces"));
        assert!(parse_vrml(&format!("{HEAD}Shape {{ geometry IndexedFaceSet {{ coordIndex [0 1 2]")).is_err());
        // 範囲外の頂点番号の面は捨てる
        let m = parse("Shape { geometry IndexedFaceSet { coord Coordinate { point [0 0 0 1 0 0 0 1 0] } coordIndex [0 1 2 -1 0 1 9 -1] } }");
        assert_eq!(m.mesh.triangle_count(), 1);
        // 深すぎる入れ子はエラー（スタックを使い切らない）
        let deep = format!("{HEAD}{}{}", "Group { children [ ".repeat(2000), "] } ".repeat(2000));
        assert!(parse_vrml(&deep).unwrap_err().contains("deeply"));
    }

    #[test]
    fn gzip_compressed_files_are_read() {
        use std::io::Write;
        let text = format!("{HEAD}Shape {{ geometry IndexedFaceSet {{ coord Coordinate {{ point [0 0 0 1 0 0 0 1 0] }} coordIndex [0 1 2] }} }}");
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(text.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        let dir = std::env::temp_dir().join(format!("krbr_rust_vrml_gz_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("part.wrz");
        std::fs::write(&p, gz).unwrap();
        let m = load_vrml(&p).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(m.mesh.triangle_count(), 1);
    }
}
