//! .kicad_pcb の S 式から `Board` を組み立てる

use std::path::Path;

use krbr_core::{to_hex, xxh64};

use crate::board::*;
use crate::curves::{arc_points, rounded_rect_points, ARC_TOL};
use crate::sexpr::{self, Node};
use crate::stroke_text::needs_kicad_cli;

fn xy(n: Option<&Node>) -> Vec2 {
    match n {
        Some(n) => Vec2::new(n.num(1), n.num(2)),
        None => Vec2::ZERO,
    }
}

fn xyz(n: Option<&Node>) -> Vec3 {
    match n.and_then(|n| n.child("xyz")) {
        Some(x) => Vec3::new(x.num(1), x.num(2), x.num(3)),
        None => Vec3::ZERO,
    }
}

fn stroke_width(n: &Node) -> f64 {
    if let Some(w) = n.child("stroke").and_then(|s| s.child_num("width")) {
        return w;
    }
    n.child_num("width").unwrap_or(0.0)
}

fn is_filled(n: &Node) -> bool {
    match n.child("fill") {
        Some(f) => matches!(f.str(1), "yes" | "solid"),
        None => false,
    }
}

fn is_hidden(n: &Node) -> bool {
    if n.has_atom("hide") {
        return true;
    }
    if let Some(h) = n.child_bool("hide") {
        return h;
    }
    if let Some(e) = n.child("effects") {
        if e.has_atom("hide") {
            return true;
        }
        if let Some(h) = e.child_bool("hide") {
            return h;
        }
    }
    false
}

fn read_pts(pts: Option<&Node>) -> Vec<Vec2> {
    let mut out = Vec::new();
    let Some(pts) = pts else { return out };
    for it in &pts.items {
        if !it.is_list {
            continue;
        }
        match it.tag() {
            "xy" => out.push(xy(Some(it))),
            // 円弧は図形の Arc と同じ規則（curves::arc_points、許容誤差 ARC_TOL）で折れ線にする
            "arc" => out.extend(arc_points(xy(it.child("start")), xy(it.child("mid")), xy(it.child("end")), ARC_TOL)),
            _ => {}
        }
    }
    out
}

#[derive(Clone, Copy, Default)]
struct Xform {
    pos: Vec2,
    angle: f64,
}

impl Xform {
    fn apply(&self, p: Vec2) -> Vec2 {
        self.pos + rotate_point(p, self.angle)
    }
}

/// gr_* / fp_* を読む。prefix 抜きのタグ名（line, arc, ...）で判定
fn read_graphic(n: &Node, kind: &str, xf: &Xform, fp: Option<usize>) -> Option<Graphic> {
    let mut g = Graphic {
        layer: n.child_str("layer").unwrap_or("").to_string(),
        width: stroke_width(n),
        filled: is_filled(n),
        footprint: fp,
        ..Default::default()
    };
    match kind {
        "line" => {
            g.kind = GrKind::Line;
            g.start = xf.apply(xy(n.child("start")));
            g.end = xf.apply(xy(n.child("end")));
        }
        "arc" => {
            g.kind = GrKind::Arc;
            n.child("mid")?; // 旧形式（angle 指定）は非対応
            g.start = xf.apply(xy(n.child("start")));
            g.mid = xf.apply(xy(n.child("mid")));
            g.end = xf.apply(xy(n.child("end")));
        }
        "circle" => {
            g.kind = GrKind::Circle;
            g.start = xf.apply(xy(n.child("center")));
            g.end = xf.apply(xy(n.child("end")));
        }
        "rect" => {
            g.kind = GrKind::Poly;
            let (a, b) = (xy(n.child("start")), xy(n.child("end")));
            g.pts = rounded_rect_points(a.min(b), a.max(b), n.child_num("radius").unwrap_or(0.0), ARC_TOL).into_iter().map(|p| xf.apply(p)).collect();
        }
        "poly" => {
            g.kind = GrKind::Poly;
            g.pts = read_pts(n.child("pts")).into_iter().map(|p| xf.apply(p)).collect();
        }
        "curve" | "bezier" => {
            g.kind = GrKind::Curve;
            g.pts = read_pts(n.child("pts")).into_iter().map(|p| xf.apply(p)).collect();
            if g.pts.len() != 4 {
                return None;
            }
        }
        _ => return None,
    }
    Some(g)
}

/// knockout の文字の、字を囲む四角形（文字の向きにそろえ、KiCad と同じ余白 max(線幅 / 2, 高さ / 9) を足す）
fn knockout_box(glyphs: &[Vec<Vec2>], angle_deg: f64, margin: f64) -> Vec<Vec2> {
    let (mut lo, mut hi) = (Vec2::splat(f64::MAX), Vec2::splat(f64::MIN));
    for p in glyphs.iter().flatten() {
        let q = rotate_point(*p, -angle_deg);
        lo = lo.min(q);
        hi = hi.max(q);
    }
    let (lo, hi) = (lo - Vec2::splat(margin), hi + Vec2::splat(margin));
    [lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)].into_iter().map(|p| rotate_point(p, angle_deg)).collect()
}

/// 線フォントの 1 文字の送り幅の上限（文字の幅に対する比）。KiCad の線フォントは W や M などの広い字でも
/// 送りが文字の幅を少し超える程度なので、余裕を見て 1.2 倍とする（範囲を狭く見積もらないための上限）
const STROKE_ADVANCE_MAX: f64 = 1.2;
/// 複数行の文字の行の送り（文字の高さに対する比。KiCad の既定の行間）
const STROKE_LINE_PITCH: f64 = 1.62;
/// 線の太さの指定が無いときの、文字の高さに対する太さ（KiCad の既定）
const DEFAULT_THICKNESS_RATIO: f64 = 0.15;

/// 文字の (高さ, 幅, 線の太さ)。(effects (font (size 高さ 幅) (thickness t)))
fn font_metrics(n: &Node) -> (f64, f64, f64) {
    let font = n.child("effects").and_then(|e| e.child("font"));
    let size = font.and_then(|f| f.child("size"));
    let height = size.map(|s| s.num(1)).unwrap_or(1.0);
    let width = size.map(|s| s.num_or(2, height)).unwrap_or(height);
    let thickness = font.and_then(|f| f.child_num("thickness")).unwrap_or(height * DEFAULT_THICKNESS_RATIO);
    (height, width, thickness)
}

/// 線フォントの文字の範囲の見積もり：基準点からの距離は、文字の塊の幅＋高さ（＝対角線以上）を超えない。
/// 寄せ（justify）や回転によらず含むように、基準点を中心とする正方形で囲む
fn stroke_text_box(n: &Node, text: &str, pos: Vec2) -> Box2 {
    let (height, width, thickness) = font_metrics(n);
    let lines = text.split('\n').count().max(1) as f64;
    let longest = text.split('\n').map(|l| l.chars().count()).max().unwrap_or(0) as f64;
    Box2::around(pos, longest * width * STROKE_ADVANCE_MAX + lines * height * STROKE_LINE_PITCH + thickness)
}

/// 文字（property / fp_text / gr_text）。render_cache があれば多角形として取り込む。
/// xf は部品の位置と向き（部品の文字の位置は部品に対する値、角度は基板に対する値（読める向きにそろえた絶対の角度）で
/// 保存されている。基板直下の文字は恒等変換）
fn read_text(n: &Node, fp: Option<usize>, xf: &Xform, board: &mut Board) {
    if is_hidden(n) {
        return;
    }
    let layer = n.child_str("layer").unwrap_or("");
    if layer.is_empty() {
        return;
    }
    if let Some(cache) = n.child("render_cache") {
        // render_cache は基板座標。穴（e の中など）は切れ目でつないだ 1 本の輪郭になっている
        let glyphs: Vec<Vec<Vec2>> = cache.children("polygon").map(|poly| read_pts(poly.child("pts"))).filter(|pts| pts.len() >= 3).collect();
        let knockout = n.child("layer").is_some_and(|l| l.has_atom("knockout"));
        let text_graphic = |pts: Vec<Vec2>, cutouts: Vec<Vec<Vec2>>| Graphic { kind: GrKind::Poly, layer: layer.to_string(), filled: true, footprint: fp, from_text: true, pts, cutouts, ..Default::default() };
        if knockout && !glyphs.is_empty() {
            let (height, _, thickness) = font_metrics(n);
            // 部品の文字でも (at x y 角度) の角度は基板に対する値（部品の回転を足すと二重になる。180° の部品の文字は 0°）
            let angle = n.child("at").map(|a| a.num_or(3, 0.0)).unwrap_or(0.0);
            let hull = knockout_box(&glyphs, angle, (0.5 * thickness).max(height / 9.0));
            board.graphics.push(text_graphic(hull, glyphs));
        } else {
            board.graphics.extend(glyphs.into_iter().map(|pts| text_graphic(pts, Vec::new())));
        }
        return;
    }
    if !needs_kicad_cli(n) {
        return;
    }
    // property の場合 items[1] が名前、items[2] が値。fp_text は items[1] が種別、items[2] が文字
    let text = if n.tag() == "gr_text" { n.str(1) } else { n.str(2) };
    if text.is_empty() {
        return;
    }
    let pos = xf.apply(xy(n.child("at")));
    board.stroke_texts.push(StrokeText { text: text.to_string(), layer: layer.to_string(), pos, footprint: fp, bbox: stroke_text_box(n, text, pos) });
}

/// 文字の枠（gr_text_box / fp_text_box）。krbr は枠も中の文字も描かない（シルクのものは kicad-cli に描かせる）ので、
/// 線フォントの文字と同じ一覧に入れて、kicad-cli を呼ぶかの判定と差分の検出に含める。
/// 枠は (start)(end) か、回した枠なら (pts) の 4 隅で、部品内なら部品に対する座標
fn read_text_box(n: &Node, fp: Option<usize>, xf: &Xform, board: &mut Board) {
    let layer = n.child_str("layer").unwrap_or("");
    if layer.is_empty() || !needs_kicad_cli(n) {
        return;
    }
    let corners: Vec<Vec2> = if n.has("pts") { read_pts(n.child("pts")) } else { vec![xy(n.child("start")), xy(n.child("end"))] };
    // 枠の線の太さと、文字が枠からはみ出す分（1 行の高さと文字の線の太さ）を見込む
    let (height, _, thickness) = font_metrics(n);
    let margin = stroke_width(n) / 2.0 + height + thickness;
    let mut bbox = Box2::default();
    for p in corners {
        bbox.add_radius(xf.apply(p), margin);
    }
    if !bbox.valid() {
        return;
    }
    board.stroke_texts.push(StrokeText { text: n.str(1).to_string(), layer: layer.to_string(), pos: bbox.center(), footprint: fp, bbox });
}

fn parse_shape(s: &str) -> PadShape {
    match s {
        "circle" => PadShape::Circle,
        "rect" => PadShape::Rect,
        "oval" => PadShape::Oval,
        "trapezoid" => PadShape::Trapezoid,
        "roundrect" => PadShape::RoundRect,
        "custom" => PadShape::Custom,
        _ => PadShape::Rect,
    }
}

fn read_pad(n: &Node, xf: &Xform, fp: Option<usize>, board: &mut Board) {
    let at = n.child("at");
    let mut p = Pad {
        number: n.str(1).to_string(),
        kind: match n.str(2) {
            "smd" => PadType::Smd,
            "thru_hole" => PadType::ThruHole,
            "np_thru_hole" => PadType::NpThruHole,
            _ => PadType::Connect,
        },
        shape: parse_shape(n.str(3)),
        pos: xf.apply(xy(at)),
        angle_deg: at.map(|a| a.num_or(3, 0.0)).unwrap_or(0.0),
        size: xy(n.child("size")),
        rect_delta: xy(n.child("rect_delta")),
        round_ratio: n.child_num("roundrect_rratio").unwrap_or(0.25),
        chamfer_ratio: n.child_num("chamfer_ratio").unwrap_or(0.0),
        chamfer_corners: 0,
        drill: Drill::default(),
        layers: Vec::new(),
        mask_margin: n.child_num("solder_mask_margin"),
        custom_anchor: PadShape::Circle,
        primitives: Vec::new(),
        footprint: fp,
    };
    if let Some(ch) = n.child("chamfer") {
        for i in 1..ch.len() {
            p.chamfer_corners |= match ch.str(i) {
                "top_left" => 1,
                "top_right" => 2,
                "bottom_left" => 4,
                "bottom_right" => 8,
                _ => 0,
            };
        }
    }
    if let Some(d) = n.child("drill") {
        p.drill.present = true;
        let mut i = 1;
        if d.str(1) == "oval" {
            p.drill.oval = true;
            i = 2;
        }
        let w = d.num_or(i, 0.0);
        let h = match d.items.get(i + 1) {
            Some(next) if !next.is_list => d.num_or(i + 1, w),
            _ => w,
        };
        p.drill.size = Vec2::new(w, h);
        p.drill.offset = xy(d.child("offset"));
        if w <= 0.0 {
            p.drill.present = false;
        }
    }
    if let Some(l) = n.child("layers") {
        for i in 1..l.len() {
            p.layers.push(l.str(i).to_string());
        }
    }
    if p.shape == PadShape::Custom {
        if let Some(a) = n.child("options").and_then(|o| o.child_str("anchor")) {
            p.custom_anchor = parse_shape(a);
        }
        if let Some(prims) = n.child("primitives") {
            let ident = Xform::default();
            for it in &prims.items {
                if !it.is_list {
                    continue;
                }
                let Some(kind) = it.tag().strip_prefix("gr_") else { continue };
                if let Some(mut g) = read_graphic(it, kind, &ident, fp) {
                    if !it.has("fill") && g.kind == GrKind::Poly {
                        g.filled = true; // 旧形式の既定
                    }
                    p.primitives.push(g);
                }
            }
        }
    }
    if n.has("padstack") {
        krbr_core::debug!("pad {} uses padstack (per-layer shapes); using default layer shape", p.number);
    }
    board.pads.push(p);
}

fn read_model(n: &Node) -> Model3D {
    let mut offset = xyz(n.child("offset"));
    if !n.has("offset") && n.has("at") {
        offset = xyz(n.child("at")) * 25.4; // 旧形式（インチ）
    }
    Model3D {
        path: n.str(1).to_string(),
        offset,
        scale: if n.has("scale") { xyz(n.child("scale")) } else { Vec3::ONE },
        rotate: xyz(n.child("rotate")),
        hidden: n.has_atom("hide") || n.child_bool("hide").unwrap_or(false),
        opacity: n.child_num("opacity").unwrap_or(1.0),
    }
}

/// ゾーン（基板直下と部品内で共通）。塗りのデータを読み、塗るはずなのにデータが無いものを数える
/// （キープアウト・ルール領域は塗らないので数えない）
fn read_zone(n: &Node, board: &mut Board) {
    let has_fill = n.children("filled_polygon").next().is_some();
    let wants_fill = n.child("fill").map(|f| f.has_atom("yes")).unwrap_or(false);
    if !has_fill && wants_fill && !n.has("keepout") {
        board.unfilled_zones += 1;
    }
    for fpoly in n.children("filled_polygon") {
        let pts = read_pts(fpoly.child("pts"));
        if pts.len() >= 3 {
            board.zones.push(ZoneFill { layer: fpoly.child_str("layer").unwrap_or("").to_string(), pts });
        }
    }
}

fn read_footprint(n: &Node, board: &mut Board) {
    let at = n.child("at");
    let mut fp = Footprint {
        lib_id: n.str(1).to_string(),
        back: n.child_str("layer").unwrap_or("F.Cu") == "B.Cu",
        pos: xy(at),
        angle_deg: at.map(|a| a.num_or(3, 0.0)).unwrap_or(0.0),
        ..Default::default()
    };
    if let Some(a) = n.child("attr") {
        fp.smd = a.has_atom("smd");
        fp.through_hole = a.has_atom("through_hole");
        fp.dnp = a.has_atom("dnp");
        fp.board_only = a.has_atom("board_only");
    }
    if let Some(d) = n.child_bool("dnp") {
        fp.dnp = fp.dnp || d;
    }
    let idx = Some(board.footprints.len());
    let xf = Xform { pos: fp.pos, angle: fp.angle_deg };
    let first_pad = board.pads.len();

    for it in &n.items {
        if !it.is_list {
            continue;
        }
        let tag = it.tag();
        match tag {
            "property" => {
                match it.str(1) {
                    "Reference" => fp.reference = it.str(2).to_string(),
                    "Value" => fp.value = it.str(2).to_string(),
                    _ => {}
                }
                read_text(it, idx, &xf, board);
            }
            "fp_text" => {
                if it.str(1) == "reference" && fp.reference.is_empty() {
                    fp.reference = it.str(2).to_string();
                }
                if it.str(1) == "value" && fp.value.is_empty() {
                    fp.value = it.str(2).to_string();
                }
                read_text(it, idx, &xf, board);
            }
            "fp_text_box" => read_text_box(it, idx, &xf, board),
            "pad" => read_pad(it, &xf, idx, board),
            "model" => fp.models.push(read_model(it)),
            "zone" => read_zone(it, board),
            _ => {
                if let Some(kind) = tag.strip_prefix("fp_") {
                    if let Some(g) = read_graphic(it, kind, &xf, idx) {
                        board.graphics.push(g);
                    }
                }
            }
        }
    }
    // レジストの広がりは パッド → 部品 → 基板 の順に継ぐ（部品の指定が無いパッドに部品の値を入れる）
    if let Some(m) = n.child_num("solder_mask_margin") {
        for p in board.pads[first_pad..].iter_mut().filter(|p| p.mask_margin.is_none()) {
            p.mask_margin = Some(m);
        }
    }
    board.footprints.push(fp);
}

fn read_tenting(t: Option<&Node>) -> (Option<bool>, Option<bool>) {
    let Some(t) = t else { return (None, None) };
    // v10: (tenting (front yes) (back yes))  /  旧: (tenting front back) / (tenting none)
    if t.has("front") || t.has("back") {
        return (t.child_bool("front"), t.child_bool("back"));
    }
    (Some(t.has_atom("front")), Some(t.has_atom("back")))
}

fn read_setup(n: &Node, board: &mut Board) {
    if let Some(v) = n.child_num("pad_to_mask_clearance") {
        board.setup.pad_to_mask_clearance = v;
    }
    let (f, b) = read_tenting(n.child("tenting"));
    if let Some(f) = f {
        board.setup.tent_front = f;
    }
    if let Some(b) = b {
        board.setup.tent_back = b;
    }
    if let Some(st) = n.child("stackup") {
        board.stackup.copper_finish = st.child_str("copper_finish").unwrap_or("").to_string();
        for l in st.children("layer") {
            let color = l.child_str("color").unwrap_or("").to_string();
            match l.str(1) {
                "F.Mask" => board.stackup.mask_color_front = color,
                "B.Mask" => board.stackup.mask_color_back = color,
                "F.SilkS" => board.stackup.silk_color_front = color,
                "B.SilkS" => board.stackup.silk_color_back = color,
                "F.Cu" => {
                    if let Some(t) = l.child_num("thickness") {
                        board.stackup.copper_thickness = t;
                    }
                }
                _ => {}
            }
        }
    }
}

/// 項目を読む前の各一覧の長さ。読んだ後との差が、その項目が作ったもの
struct ItemMark {
    g: usize,
    p: usize,
    t: usize,
    v: usize,
    z: usize,
    s: usize,
}

fn mark_of(b: &Board) -> ItemMark {
    ItemMark { g: b.graphics.len(), p: b.pads.len(), t: b.tracks.len(), v: b.vias.len(), z: b.zones.len(), s: b.stroke_texts.len() }
}

fn add_item(n: &Node, hash: u64, m: &ItemMark, b: &mut Board) {
    let mut key = n.child_str("uuid").or_else(|| n.child_str("tstamp")).unwrap_or("").to_string();
    if key.is_empty() {
        key = format!("h:{}", to_hex(hash));
    }
    let mut item = BoardItem { key, hash, ..Default::default() };
    let bb = &mut item.bbox;
    // 範囲はタイルのカリング（krbr-geom の raster.rs）と同じ関数で求める（ずれると、変えた所のタイルが作り直されない）
    for g in &b.graphics[m.g..] {
        bb.add_box(&g.bbox());
        item.edge_cuts = item.edge_cuts || g.layer == "Edge.Cuts";
    }
    for p in &b.pads[m.p..] {
        bb.add_box(&p.extent(&b.setup));
        item.holes = item.holes || p.drill.present;
    }
    for t in &b.tracks[m.t..] {
        bb.add_box(&t.bbox());
    }
    for v in &b.vias[m.v..] {
        bb.add_box(&v.bbox());
        item.holes = true;
    }
    for z in &b.zones[m.z..] {
        bb.add_box(&z.bbox());
    }
    for s in &b.stroke_texts[m.s..] {
        if is_plotted_text_layer(&s.layer) {
            item.stroke_text = true;
            bb.add_box(&s.bbox);
        }
    }
    b.items.push(item);
}

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    Parse(sexpr::ParseError),
    NotBoard,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "cannot read board: {e}"),
            LoadError::Parse(e) => write!(f, "{e}"),
            LoadError::NotBoard => write!(f, "not a kicad_pcb file"),
        }
    }
}

impl std::error::Error for LoadError {}

pub fn load_board(file: &Path) -> Result<Board, LoadError> {
    let text = std::fs::read_to_string(file).map_err(LoadError::Io)?;
    let root = sexpr::parse(&text).map_err(LoadError::Parse)?;
    if root.tag() != "kicad_pcb" {
        return Err(LoadError::NotBoard);
    }
    let abs = std::path::absolute(file).unwrap_or_else(|_| file.to_path_buf());
    let mut board = Board {
        file: file.to_path_buf(),
        project_dir: abs.parent().map(|p| p.to_path_buf()).unwrap_or_default(),
        project_name: file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        version: root.child_num("version").unwrap_or(0.0) as i64,
        ..Default::default()
    };
    if let Some(t) = root.child("general").and_then(|g| g.child_num("thickness")) {
        board.stackup.thickness = t;
    }

    let ident = Xform::default();
    let mut setup_key = String::new();
    let hashes = sexpr::hash_items(&root.items);
    for (it, &hash) in root.items.iter().zip(&hashes) {
        if !it.is_list {
            continue;
        }
        let tag = it.tag();
        if matches!(tag, "general" | "layers" | "setup") {
            setup_key += &to_hex(hash);
        }
        let is_item = matches!(tag, "footprint" | "module" | "segment" | "arc" | "via" | "zone") || tag.starts_with("gr_");
        let mark = mark_of(&board);
        match tag {
            "setup" => read_setup(it, &mut board),
            "footprint" | "module" => read_footprint(it, &mut board),
            "segment" => board.tracks.push(Track {
                start: xy(it.child("start")),
                end: xy(it.child("end")),
                mid: None,
                width: it.child_num("width").unwrap_or(0.0),
                layer: it.child_str("layer").unwrap_or("").to_string(),
            }),
            "arc" => board.tracks.push(Track {
                start: xy(it.child("start")),
                end: xy(it.child("end")),
                mid: Some(xy(it.child("mid"))),
                width: it.child_num("width").unwrap_or(0.0),
                layer: it.child_str("layer").unwrap_or("").to_string(),
            }),
            "via" => {
                let (tent_front, tent_back) = read_tenting(it.child("tenting"));
                let layers = it.child("layers");
                board.vias.push(Via {
                    pos: xy(it.child("at")),
                    size: it.child_num("size").unwrap_or(0.0),
                    drill: it.child_num("drill").unwrap_or(0.0),
                    layer_from: layers.map(|l| l.str_or(1, "F.Cu")).unwrap_or("F.Cu").to_string(),
                    layer_to: layers.map(|l| l.str_or(2, "B.Cu")).unwrap_or("B.Cu").to_string(),
                    tent_front,
                    tent_back,
                });
            }
            "zone" => read_zone(it, &mut board),
            "gr_text" => read_text(it, None, &ident, &mut board),
            "gr_text_box" => read_text_box(it, None, &ident, &mut board),
            _ => {
                if let Some(kind) = tag.strip_prefix("gr_") {
                    if let Some(g) = read_graphic(it, kind, &ident, None) {
                        board.graphics.push(g);
                    }
                }
            }
        }
        if is_item {
            add_item(it, hash, &mark, &mut board);
        }
    }
    board.setup_hash = xxh64(setup_key.as_bytes());
    Ok(board)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"(kicad_pcb (version 20260206) (generator "pcbnew")
  (general (thickness 1.2))
  (setup (pad_to_mask_clearance 0.05) (tenting (front yes) (back no))
    (stackup (layer "F.Mask" (type "Top Solder Mask") (color "Purple")) (layer "F.Cu" (type "copper") (thickness 0.035)) (copper_finish "ENIG")))
  (footprint "Lib:Part" (layer "B.Cu") (uuid "u1") (at 10 20 90)
    (property "Reference" "R1" (at 0 0 0) (layer "B.SilkS") (effects (font (size 1 1))))
    (property "Value" "10k" (at 0 0 0) (layer "B.Fab") (hide yes))
    (attr smd dnp)
    (fp_line (start -1 0) (end 1 0) (stroke (width 0.1)) (layer "B.SilkS"))
    (pad "1" smd roundrect (at 0.8 0 90) (size 1 0.5) (layers "B.Cu" "B.Mask") (roundrect_rratio 0.2))
    (pad "2" thru_hole circle (at -0.8 0 90) (size 1 1) (drill 0.6 (offset 0.1 0)) (layers "*.Cu" "*.Mask"))
    (model "${KICAD10_3DMODEL_DIR}/R.step" (offset (xyz 0 0 0)) (scale (xyz 1 1 1)) (rotate (xyz 0 0 0))))
  (segment (start 0 0) (end 5 0) (width 0.25) (layer "F.Cu") (uuid "s1"))
  (via (at 3 3) (size 0.6) (drill 0.3) (layers "F.Cu" "B.Cu") (tenting (front no)) (uuid "v1"))
  (zone (layer "F.Cu") (uuid "z1") (fill yes) (filled_polygon (layer "F.Cu") (pts (xy 0 0) (xy 1 0) (xy 1 1))))
  (gr_text "hello" (at 1 1) (layer "F.SilkS") (uuid "t1"))
)"#;

    fn parse_sample() -> Board {
        load_text("sample.kicad_pcb", SAMPLE)
    }

    fn load_text(name: &str, text: &str) -> Board {
        // テストは並列に走るので、呼び出しごとに別のフォルダにする（同じファイルだと書きかけを読むことがある）。読んだら消す
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!("krbr_rust_parser_test_{}_{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(name);
        std::fs::write(&file, text).unwrap();
        let b = load_board(&file);
        let _ = std::fs::remove_dir_all(&dir);
        b.unwrap()
    }

    #[test]
    fn reads_setup_and_stackup() {
        let b = parse_sample();
        assert_eq!(b.version, 20260206);
        assert_eq!(b.stackup.thickness, 1.2);
        assert_eq!(b.stackup.copper_finish, "ENIG");
        assert_eq!(b.stackup.mask_color_front, "Purple");
        assert_eq!(b.setup.pad_to_mask_clearance, 0.05);
        assert!(b.setup.tent_front && !b.setup.tent_back);
        assert_eq!(b.project_name, "sample");
    }

    #[test]
    fn reads_footprint_pads_in_board_coordinates() {
        let b = parse_sample();
        assert_eq!(b.footprints.len(), 1);
        let fp = &b.footprints[0];
        assert_eq!(fp.reference, "R1");
        assert_eq!(fp.value, "10k");
        assert!(fp.back && fp.smd && fp.dnp);
        assert_eq!(fp.models.len(), 1);
        assert_eq!(b.pads.len(), 2);
        // (at 0.8 0) を部品の位置 (10,20)、回転 90° で変換：x 軸上の点は画面の上（-y）へ
        let p1 = &b.pads[0];
        assert!((p1.pos.x - 10.0).abs() < 1e-9 && (p1.pos.y - 19.2).abs() < 1e-9);
        assert_eq!(p1.shape, PadShape::RoundRect);
        assert_eq!(p1.round_ratio, 0.2);
        let p2 = &b.pads[1];
        assert_eq!(p2.kind, PadType::ThruHole);
        assert!(p2.drill.present && p2.drill.size == Vec2::new(0.6, 0.6));
        assert_eq!(p2.drill.offset, Vec2::new(0.1, 0.0));
        assert!(p2.on_layer("In1.Cu"));
    }

    #[test]
    fn reads_tracks_vias_zones_texts() {
        let b = parse_sample();
        assert_eq!(b.tracks.len(), 1);
        assert_eq!(b.vias.len(), 1);
        assert_eq!(b.vias[0].tent_front, Some(false));
        assert_eq!(b.vias[0].tent_back, None);
        assert_eq!(b.zones.len(), 1);
        assert_eq!(b.unfilled_zones, 0);
        // 隠した Value は数えない。R1（線フォント）と hello
        assert_eq!(b.stroke_texts.len(), 2);
        // fp_line は図形として 1 つ
        assert_eq!(b.graphics.len(), 1);
        assert_eq!(b.graphics[0].footprint, Some(0));
    }

    #[test]
    fn items_and_diff() {
        let b = parse_sample();
        // footprint, segment, via, zone, gr_text
        assert_eq!(b.items.len(), 5);
        assert_eq!(b.items[0].key, "u1");
        assert!(b.items[0].holes);
        assert!(b.items[2].holes); // via
        assert!(b.items[4].stroke_text);
        let d = diff_boards(&b, &b);
        assert_eq!((d.added, d.removed, d.changed), (0, 0, 0));
        assert!(!d.full && d.dirty.is_empty());

        let moved = SAMPLE.replace("(via (at 3 3)", "(via (at 4 3)");
        let b2 = load_text("moved.kicad_pcb", &moved);
        let d = diff_boards(&b, &b2);
        assert_eq!((d.added, d.removed, d.changed), (0, 0, 1));
        assert!(d.holes_changed && !d.full);
        assert_eq!(d.dirty.len(), 2); // 新旧の範囲
    }

    #[test]
    fn footprint_zones_and_custom_pad_extent() {
        let text = r#"(kicad_pcb (version 20260206) (generator "pcbnew")
  (setup (pad_to_mask_clearance 0.1))
  (footprint "Lib:Part" (layer "F.Cu") (uuid "fp1") (at 10 20 90)
    (pad "1" smd custom (at 0 0 90) (size 0.5 0.5) (layers "F.Cu" "F.Mask") (options (clearance outline) (anchor circle))
      (primitives (gr_poly (pts (xy 0 -0.5) (xy 3 -0.5) (xy 3 0.5) (xy 0 0.5)) (width 0) (fill yes))))
    (zone (layer "F.Cu") (fill yes) (polygon (pts (xy 0 0) (xy 1 0) (xy 1 1))))
    (zone (layer "F.Cu") (keepout (tracks not_allowed)) (fill yes) (polygon (pts (xy 0 0) (xy 1 0) (xy 1 1)))))
  (zone (layer "F.Cu") (uuid "z1") (fill yes) (polygon (pts (xy 0 0) (xy 1 0) (xy 1 1))))
)"#;
        let b = load_text("fpzone.kicad_pcb", text);
        // 部品内の塗られていないゾーンも数える（キープアウトは数えない）
        assert_eq!(b.unfilled_zones, 2);
        assert!(b.unfilled_zones_message().is_some());
        // カスタムパッドの範囲はプリミティブ（x = 3 まで）とレジストの広がり 0.1 を含む。部品の 90° で x は -y へ
        let e = b.pads[0].extent(&b.setup);
        assert!((e.lo.y - (20.0 - 3.1)).abs() < 1e-9 && (e.hi.y - (20.0 + 0.35)).abs() < 1e-9, "{e:?}");
        assert!((e.lo.x - (10.0 - 0.6)).abs() < 1e-9 && (e.hi.x - (10.0 + 0.6)).abs() < 1e-9, "{e:?}");
        // 差分の範囲も同じ
        let fp = &b.items[0];
        assert!(fp.bbox.lo.y <= e.lo.y && fp.bbox.hi.y >= e.hi.y);
    }

    #[test]
    fn rounded_rect_and_knockout_text() {
        let text = r#"(kicad_pcb (version 20260206) (generator "pcbnew")
  (gr_rect (start 2 1) (end 0 0) (radius 0.25) (stroke (width 0.1) (type solid)) (fill yes) (layer "F.Mask") (uuid "r1"))
  (gr_rect (start 0 0) (end 2 1) (stroke (width 0.1) (type solid)) (fill no) (layer "F.Mask") (uuid "r2"))
  (gr_text "I" (at 10 10 90) (layer "F.Mask" knockout) (uuid "t1")
    (effects (font (face "Roboto") (size 0.9 0.9) (thickness 0.2)))
    (render_cache "I" 0 (polygon (pts (xy 10 10) (xy 10.2 10) (xy 10.2 11) (xy 10 11)))))
  (gr_text "I" (at 10 10 0) (layer "F.Mask") (uuid "t2")
    (effects (font (face "Roboto") (size 0.9 0.9) (thickness 0.2)))
    (render_cache "I" 0 (polygon (pts (xy 10 10) (xy 10.2 10) (xy 10.2 11) (xy 10 11)))))
)"#;
        let b = load_text("rounded.kicad_pcb", text);
        assert_eq!(b.graphics.len(), 4);
        // 角の丸い四角形：外形は start/end の範囲のまま、角は半径 0.25 で削れる（角の点 (0,0) からの距離 (√2 − 1)·r）
        let r = &b.graphics[0];
        assert!(r.pts.len() > 8);
        let (lo, hi) = r.pts.iter().fold((Vec2::splat(9.0), Vec2::splat(-9.0)), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        assert!((lo - Vec2::ZERO).length() < 1e-9 && (hi - Vec2::new(2.0, 1.0)).length() < 1e-9);
        let nearest = r.pts.iter().map(|p| p.length()).fold(f64::MAX, f64::min);
        // 頂点が 45° ちょうどに来るとは限らない：45° から最大で半区間ずれた頂点の、角までの距離 r √(3 − 2√2 cos δ) 以下
        let half_step = std::f64::consts::FRAC_PI_2 / crate::curves::segments_for(0.25, std::f64::consts::FRAC_PI_2, ARC_TOL) as f64 / 2.0;
        let farthest = 0.25 * (3.0 - 2.0 * 2f64.sqrt() * half_step.cos()).sqrt();
        assert!(nearest >= (2f64.sqrt() - 1.0) * 0.25 - 1e-9 && nearest <= farthest + 1e-9, "{nearest}");
        assert_eq!(b.graphics[1].pts.len(), 4); // 半径なしは 4 点のまま
        // knockout：字を囲む四角形（余白 max(0.2 / 2, 0.9 / 9) = 0.1）から字を抜く。90° 回した文字でも、字の向きの四角形
        let k = &b.graphics[2];
        assert_eq!(k.cutouts.len(), 1);
        let (lo, hi) = k.pts.iter().fold((Vec2::splat(99.0), Vec2::splat(-99.0)), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        assert!((lo - Vec2::new(9.9, 9.9)).length() < 1e-9 && (hi - Vec2::new(10.3, 11.1)).length() < 1e-9, "{lo} {hi}");
        assert!(b.graphics[3].cutouts.is_empty() && b.graphics[3].pts.len() == 4);
    }

    /// レジストの広がりは パッド → 部品 → 基板 の順に継ぐ（以前は部品の指定を読まず、基板の既定に飛んでいた）
    #[test]
    fn footprint_solder_mask_margin_is_inherited() {
        let text = r#"(kicad_pcb (version 20260206) (generator "pcbnew")
  (setup (pad_to_mask_clearance 0.02))
  (footprint "Test:M" (layer "F.Cu") (uuid "f1") (at 10 10 0) (solder_mask_margin 0.05)
    (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu" "F.Mask") (uuid "p1"))
    (pad "2" smd rect (at 2 0) (size 1 1) (layers "F.Cu" "F.Mask") (solder_mask_margin 0.1) (uuid "p2")))
  (footprint "Test:N" (layer "F.Cu") (uuid "f2") (at 20 10 0)
    (pad "1" smd rect (at 0 0) (size 1 1) (layers "F.Cu" "F.Mask") (uuid "p3")))
)"#;
        let b = load_text("mask_margin.kicad_pcb", text);
        let m: Vec<f64> = b.pads.iter().map(|p| p.mask_expansion(&b.setup)).collect();
        assert_eq!(m, vec![0.05, 0.1, 0.02]);
    }

    /// 部品の中の knockout の文字：角度は基板に対する値なので、45° の部品の 0° の文字は、軸にそろった四角形で抜く
    /// （以前は部品の角度を足して、45° 傾いた大きな四角形になっていた）
    #[test]
    fn knockout_text_in_a_rotated_footprint() {
        let text = r#"(kicad_pcb (version 20260206) (generator "pcbnew")
  (footprint "Test:K" (layer "F.Cu") (uuid "f1") (at 10 10 45)
    (property "Reference" "I" (at 0 0 0) (layer "F.Mask" knockout) (uuid "p1")
      (effects (font (size 0.9 0.9) (thickness 0.2)))
      (render_cache "I" 0 (polygon (pts (xy 10 10) (xy 10.2 10) (xy 10.2 11) (xy 10 11))))))
)"#;
        let b = load_text("knockout_fp.kicad_pcb", text);
        let k = b.graphics.iter().find(|g| !g.cutouts.is_empty()).expect("knockout text");
        let (lo, hi) = k.pts.iter().fold((Vec2::splat(99.0), Vec2::splat(-99.0)), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        assert!((lo - Vec2::new(9.9, 9.9)).length() < 1e-9 && (hi - Vec2::new(10.3, 11.1)).length() < 1e-9, "{lo} {hi}");
    }
}
