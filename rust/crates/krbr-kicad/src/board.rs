//! .kicad_pcb から読み取った基板の内容。座標はすべて基板座標（mm、x 右・y 下）

use std::collections::HashMap;
use std::path::PathBuf;

pub use glam::{DVec2 as Vec2, DVec3 as Vec3};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrKind {
    Line,
    Arc,
    Circle,
    Poly,
    Curve,
}

/// 基板座標の矩形
#[derive(Debug, Clone, Copy)]
pub struct Box2 {
    pub lo: Vec2,
    pub hi: Vec2,
}

impl Default for Box2 {
    fn default() -> Self {
        Self { lo: Vec2::splat(1e30), hi: Vec2::splat(-1e30) }
    }
}

impl Box2 {
    pub fn add(&mut self, p: Vec2) {
        self.lo = self.lo.min(p);
        self.hi = self.hi.max(p);
    }
    pub fn add_radius(&mut self, p: Vec2, r: f64) {
        self.add(p - Vec2::splat(r));
        self.add(p + Vec2::splat(r));
    }
    pub fn add_box(&mut self, b: &Box2) {
        if b.valid() {
            self.add(b.lo);
            self.add(b.hi);
        }
    }
    pub fn valid(&self) -> bool {
        self.lo.x <= self.hi.x && self.lo.y <= self.hi.y
    }
    pub fn intersects(&self, b: &Box2) -> bool {
        self.valid() && b.valid() && self.lo.x <= b.hi.x && b.lo.x <= self.hi.x && self.lo.y <= b.hi.y && b.lo.y <= self.hi.y
    }
    pub fn expanded(&self, m: f64) -> Box2 {
        if self.valid() {
            Box2 { lo: self.lo - Vec2::splat(m), hi: self.hi + Vec2::splat(m) }
        } else {
            *self
        }
    }
    pub fn center(&self) -> Vec2 {
        (self.lo + self.hi) * 0.5
    }
}

/// 差分の検出用：ファイルの最上位の項目（部品・配線・ビア・ベタ・図形）ごとのキーと内容のハッシュと範囲
#[derive(Debug, Clone, Default)]
pub struct BoardItem {
    /// UUID（無ければ内容のハッシュ）
    pub key: String,
    /// 項目の S 式全体のハッシュ
    pub hash: u64,
    /// 銅箔・レジスト・シルクなどの範囲（基板座標）
    pub bbox: Box2,
    pub edge_cuts: bool,
    pub holes: bool,
    /// 線フォントのシルクの文字を含む（kicad-cli で描き直しが要る）
    pub stroke_silk: bool,
}

/// 図形（基板直下の gr_* と、部品内の fp_*、文字の render_cache）
#[derive(Debug, Clone)]
pub struct Graphic {
    pub kind: GrKind,
    pub layer: String,
    /// 線幅（0 なら輪郭なし）
    pub width: f64,
    pub filled: bool,
    /// Line: start-end / Arc: start-mid-end / Circle: start=中心, end=円周上
    pub start: Vec2,
    pub mid: Vec2,
    pub end: Vec2,
    /// Poly: 頂点 / Curve: ベジェ制御点 4 つ
    pub pts: Vec<Vec2>,
    /// 所属部品（None は基板）
    pub footprint: Option<usize>,
    /// 文字（render_cache）由来
    pub from_text: bool,
    /// Poly の塗りから抜く多角形（knockout の文字：文字を囲む四角形から字の形を抜く）
    pub cutouts: Vec<Vec<Vec2>>,
}

impl Default for Graphic {
    fn default() -> Self {
        Self {
            kind: GrKind::Line,
            layer: String::new(),
            width: 0.0,
            filled: false,
            start: Vec2::ZERO,
            mid: Vec2::ZERO,
            end: Vec2::ZERO,
            pts: Vec::new(),
            footprint: None,
            from_text: false,
            cutouts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadType {
    Smd,
    ThruHole,
    NpThruHole,
    Connect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PadShape {
    Circle,
    Rect,
    Oval,
    Trapezoid,
    RoundRect,
    Custom,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Drill {
    pub present: bool,
    pub oval: bool,
    /// 円なら x = y = 直径
    pub size: Vec2,
    /// パッド座標系でのずれ
    pub offset: Vec2,
}

#[derive(Debug, Clone)]
pub struct Pad {
    pub number: String,
    pub kind: PadType,
    pub shape: PadShape,
    /// 基板座標
    pub pos: Vec2,
    /// パッドの絶対角度（KiCad のファイルは部品回転込み）
    pub angle_deg: f64,
    pub size: Vec2,
    pub rect_delta: Vec2,
    pub round_ratio: f64,
    pub chamfer_ratio: f64,
    /// bit0: top_left, 1: top_right, 2: bottom_left, 3: bottom_right
    pub chamfer_corners: u32,
    pub drill: Drill,
    pub layers: Vec<String>,
    pub mask_margin: Option<f64>,
    pub custom_anchor: PadShape,
    /// カスタムパッド（パッド座標系、未回転）
    pub primitives: Vec<Graphic>,
    pub footprint: Option<usize>,
}

impl Pad {
    pub fn on_layer(&self, layer: &str) -> bool {
        for l in &self.layers {
            if l == layer {
                return true;
            }
            if l.len() > 2 && l.starts_with("*.") {
                // "*.Cu" -> 任意の ".Cu"
                let suffix = &l[1..];
                if layer.len() > suffix.len() && layer.ends_with(suffix) {
                    return true;
                }
            }
            if let Some(suffix) = l.strip_prefix("F&B") {
                if layer == format!("F{suffix}") || layer == format!("B{suffix}") {
                    return true;
                }
            }
        }
        false
    }
}

#[derive(Debug, Clone)]
pub struct Track {
    pub start: Vec2,
    pub end: Vec2,
    /// 円弧配線
    pub mid: Option<Vec2>,
    pub width: f64,
    pub layer: String,
}

#[derive(Debug, Clone)]
pub struct Via {
    pub pos: Vec2,
    pub size: f64,
    pub drill: f64,
    pub layer_from: String,
    pub layer_to: String,
    pub tent_front: Option<bool>,
    pub tent_back: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ZoneFill {
    pub layer: String,
    /// KiCad の fractured polygon（穴は切れ目でつながった単一輪郭）
    pub pts: Vec<Vec2>,
}

/// 線フォントの文字（render_cache が無いので自前では形を作れない）
#[derive(Debug, Clone)]
pub struct StrokeText {
    pub text: String,
    pub layer: String,
    pub pos: Vec2,
    pub footprint: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct Model3D {
    /// ファイルに書かれたままの指定
    pub path: String,
    pub offset: Vec3,
    pub scale: Vec3,
    pub rotate: Vec3,
    pub hidden: bool,
    pub opacity: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Footprint {
    pub lib_id: String,
    pub reference: String,
    pub value: String,
    pub pos: Vec2,
    pub angle_deg: f64,
    /// 裏面実装
    pub back: bool,
    pub smd: bool,
    pub through_hole: bool,
    pub dnp: bool,
    pub board_only: bool,
    pub models: Vec<Model3D>,
}

#[derive(Debug, Clone)]
pub struct Stackup {
    pub thickness: f64,
    /// "ENIG" / "HAL SnPb" / "None" など
    pub copper_finish: String,
    pub mask_color_front: String,
    pub mask_color_back: String,
    pub silk_color_front: String,
    pub silk_color_back: String,
    pub copper_thickness: f64,
}

impl Default for Stackup {
    fn default() -> Self {
        Self {
            thickness: 1.6,
            copper_finish: String::new(),
            mask_color_front: String::new(),
            mask_color_back: String::new(),
            silk_color_front: String::new(),
            silk_color_back: String::new(),
            copper_thickness: 0.035,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BoardSetup {
    pub pad_to_mask_clearance: f64,
    pub tent_front: bool,
    pub tent_back: bool,
}

impl Default for BoardSetup {
    fn default() -> Self {
        Self { pad_to_mask_clearance: 0.0, tent_front: true, tent_back: true }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Board {
    pub file: PathBuf,
    pub project_dir: PathBuf,
    pub project_name: String,
    pub version: i64,
    pub stackup: Stackup,
    pub setup: BoardSetup,
    /// 基板直下・部品内の図形すべて
    pub graphics: Vec<Graphic>,
    pub pads: Vec<Pad>,
    pub tracks: Vec<Track>,
    pub vias: Vec<Via>,
    pub zones: Vec<ZoneFill>,
    pub stroke_texts: Vec<StrokeText>,
    pub footprints: Vec<Footprint>,
    pub unfilled_zones: usize,
    /// 差分の検出用
    pub items: Vec<BoardItem>,
    /// general / setup / layers のハッシュ（変われば全体を作り直す）
    pub setup_hash: u64,
}

/// 前回の読み込みとの差分
#[derive(Debug, Clone, Default)]
pub struct BoardDiff {
    /// 外形・層構成などが変わった（全体を作り直す）
    pub full: bool,
    /// 穴が変わった（基材の側面・穴の内壁を作り直す）
    pub holes_changed: bool,
    pub stroke_silk_changed: bool,
    /// 作り直しが要る範囲
    pub dirty: Vec<Box2>,
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
}

pub fn diff_boards(before: &Board, after: &Board) -> BoardDiff {
    let mut d = BoardDiff::default();
    if before.setup_hash != after.setup_hash {
        d.full = true;
        return d;
    }
    let mut old: HashMap<&str, &BoardItem> = before.items.iter().map(|it| (it.key.as_str(), it)).collect();
    fn touch(d: &mut BoardDiff, it: &BoardItem) {
        if it.edge_cuts {
            d.full = true;
        }
        if it.holes {
            d.holes_changed = true;
        }
        if it.stroke_silk {
            d.stroke_silk_changed = true;
        }
        if it.bbox.valid() {
            d.dirty.push(it.bbox);
        }
    }
    for it in &after.items {
        match old.remove(it.key.as_str()) {
            None => {
                d.added += 1;
                touch(&mut d, it);
            }
            Some(o) if o.hash != it.hash => {
                d.changed += 1;
                touch(&mut d, o);
                touch(&mut d, it);
            }
            Some(_) => {}
        }
    }
    for (_, it) in old {
        d.removed += 1;
        touch(&mut d, it);
    }
    d
}

/// 基板座標系での回転（KiCad 準拠：y 下向き座標で正の角度は画面上で反時計回り）
pub fn rotate_point(p: Vec2, angle_deg: f64) -> Vec2 {
    if angle_deg == 0.0 {
        return p;
    }
    let a = angle_deg.to_radians();
    let (s, c) = a.sin_cos();
    Vec2::new(p.x * c + p.y * s, -p.x * s + p.y * c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_is_counterclockwise_on_screen() {
        // y 下向き座標で +90°：x 軸上の点は画面の上（-y）へ
        let p = rotate_point(Vec2::new(1.0, 0.0), 90.0);
        assert!((p.x).abs() < 1e-12 && (p.y + 1.0).abs() < 1e-12);
    }

    #[test]
    fn pad_layer_wildcards() {
        let mut p = Pad {
            number: "1".into(),
            kind: PadType::ThruHole,
            shape: PadShape::Circle,
            pos: Vec2::ZERO,
            angle_deg: 0.0,
            size: Vec2::ZERO,
            rect_delta: Vec2::ZERO,
            round_ratio: 0.0,
            chamfer_ratio: 0.0,
            chamfer_corners: 0,
            drill: Drill::default(),
            layers: vec!["*.Cu".into(), "F&B.Mask".into()],
            mask_margin: None,
            custom_anchor: PadShape::Circle,
            primitives: Vec::new(),
            footprint: None,
        };
        assert!(p.on_layer("F.Cu") && p.on_layer("In1.Cu") && p.on_layer("B.Mask"));
        assert!(!p.on_layer("F.SilkS"));
        p.layers = vec!["F.Cu".into()];
        assert!(p.on_layer("F.Cu") && !p.on_layer("B.Cu"));
    }
}
