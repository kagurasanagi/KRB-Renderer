//! .kicad_pcb の S 式から `Board` を組み立てる（C++ 版 `board_parser.cpp` の移植）

use std::path::Path;

use krbr_core::{to_hex, xxh64};

use crate::board::*;
use crate::sexpr::{self, Node};

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

/// 3 点を通る円弧を折れ線にする（ゾーンの pts 内の arc 用）
fn append_arc(out: &mut Vec<Vec2>, a: Vec2, m: Vec2, b: Vec2, tol: f64) {
    use std::f64::consts::PI;
    let d = 2.0 * (a.x * (m.y - b.y) + m.x * (b.y - a.y) + b.x * (a.y - m.y));
    if d.abs() < 1e-12 {
        out.push(a);
        out.push(b);
        return;
    }
    let (a2, m2, b2) = (a.dot(a), m.dot(m), b.dot(b));
    let c = Vec2::new(
        (a2 * (m.y - b.y) + m2 * (b.y - a.y) + b2 * (a.y - m.y)) / d,
        (a2 * (b.x - m.x) + m2 * (a.x - b.x) + b2 * (m.x - a.x)) / d,
    );
    let r = (a - c).length();
    let t0 = (a.y - c.y).atan2(a.x - c.x);
    let tm = (m.y - c.y).atan2(m.x - c.x);
    let t1 = (b.y - c.y).atan2(b.x - c.x);
    let norm = |mut t: f64| {
        while t < 0.0 {
            t += 2.0 * PI;
        }
        while t >= 2.0 * PI {
            t -= 2.0 * PI;
        }
        t
    };
    let mut sweep = norm(t1 - t0);
    if norm(tm - t0) > sweep {
        sweep -= 2.0 * PI; // 逆回り
    }
    let step = 2.0 * (1.0 - tol / r.max(1e-9)).max(-1.0).acos();
    let n = ((sweep.abs() / step.max(1e-4)).ceil() as i64).max(2);
    for i in 0..=n {
        let t = t0 + sweep * i as f64 / n as f64;
        out.push(c + r * Vec2::new(t.cos(), t.sin()));
    }
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
            "arc" => append_arc(&mut out, xy(it.child("start")), xy(it.child("mid")), xy(it.child("end")), 0.001),
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
            g.pts = rounded_rect_pts(a.min(b), a.max(b), n.child_num("radius").unwrap_or(0.0)).into_iter().map(|p| xf.apply(p)).collect();
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

/// 角の丸い四角形（lo-hi、角の半径 r）の頂点。r = 0 なら 4 点
fn rounded_rect_pts(lo: Vec2, hi: Vec2, r: f64) -> Vec<Vec2> {
    let r = r.min(0.5 * (hi.x - lo.x)).min(0.5 * (hi.y - lo.y));
    if r <= 0.0 {
        return vec![lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)];
    }
    // 弦と円弧の差が 1 µm 以下になる分割数（1/4 周あたり）
    const TOL: f64 = 0.001;
    let step = 2.0 * (1.0 - TOL / r).max(-1.0).acos();
    let n = ((std::f64::consts::FRAC_PI_2 / step).ceil() as usize).clamp(2, 64);
    // 角の中心と、その角の円弧の始まりの角度（y 下向きの座標で時計回りに一周）
    let corners = [(Vec2::new(hi.x - r, lo.y + r), -90.0f64), (Vec2::new(hi.x - r, hi.y - r), 0.0), (Vec2::new(lo.x + r, hi.y - r), 90.0), (Vec2::new(lo.x + r, lo.y + r), 180.0)];
    let mut pts = Vec::with_capacity(4 * (n + 1));
    for (c, a0) in corners {
        for i in 0..=n {
            let a = (a0 + 90.0 * i as f64 / n as f64).to_radians();
            pts.push(c + Vec2::new(a.cos(), a.sin()) * r);
        }
    }
    pts
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

/// 文字（property / fp_text / gr_text）。render_cache があれば多角形として取り込む。
/// parent_angle は部品の向き（部品の文字の角度は、部品に対する角度で保存されている）
fn read_text(n: &Node, fp: Option<usize>, parent_angle: f64, board: &mut Board) {
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
            let font = n.child("effects").and_then(|e| e.child("font"));
            let height = font.and_then(|f| f.child("size")).map(|s| s.num(1)).unwrap_or(1.0); // (size 高さ 幅)
            let thickness = font.and_then(|f| f.child_num("thickness")).unwrap_or(height * 0.15);
            let angle = n.child("at").map(|a| a.num_or(3, 0.0)).unwrap_or(0.0) + parent_angle;
            let hull = knockout_box(&glyphs, angle, (0.5 * thickness).max(height / 9.0));
            board.graphics.push(text_graphic(hull, glyphs));
        } else {
            board.graphics.extend(glyphs.into_iter().map(|pts| text_graphic(pts, Vec::new())));
        }
        return;
    }
    // property の場合 items[1] が名前、items[2] が値。fp_text は items[1] が種別、items[2] が文字
    let text = if n.tag() == "gr_text" { n.str(1) } else { n.str(2) };
    if text.is_empty() {
        return;
    }
    board.stroke_texts.push(StrokeText { text: text.to_string(), layer: layer.to_string(), pos: xy(n.child("at")), footprint: fp });
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

fn read_zone_fills(n: &Node, board: &mut Board) {
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
                read_text(it, idx, fp.angle_deg, board);
            }
            "fp_text" => {
                if it.str(1) == "reference" && fp.reference.is_empty() {
                    fp.reference = it.str(2).to_string();
                }
                if it.str(1) == "value" && fp.value.is_empty() {
                    fp.value = it.str(2).to_string();
                }
                read_text(it, idx, fp.angle_deg, board);
            }
            "pad" => read_pad(it, &xf, idx, board),
            "model" => fp.models.push(read_model(it)),
            "zone" => read_zone_fills(it, board),
            _ => {
                if let Some(kind) = tag.strip_prefix("fp_") {
                    if let Some(g) = read_graphic(it, kind, &xf, idx) {
                        board.graphics.push(g);
                    }
                }
            }
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

fn add_item(n: &Node, m: &ItemMark, b: &mut Board) {
    let hash = sexpr::hash_node(n);
    let mut key = n.child_str("uuid").or_else(|| n.child_str("tstamp")).unwrap_or("").to_string();
    if key.is_empty() {
        key = format!("h:{}", to_hex(hash));
    }
    let mut item = BoardItem { key, hash, ..Default::default() };
    let bb = &mut item.bbox;
    for g in &b.graphics[m.g..] {
        let w = g.width / 2.0;
        if matches!(g.kind, GrKind::Line | GrKind::Arc) {
            // 多角形などでは start / end は使わない
            bb.add_radius(g.start, w);
            bb.add_radius(g.end, w);
        }
        if g.kind == GrKind::Arc {
            bb.add_radius(g.mid, w);
        }
        if g.kind == GrKind::Circle {
            bb.add_radius(g.start, (g.end - g.start).length() + w);
        }
        for &p in &g.pts {
            bb.add_radius(p, w);
        }
        item.edge_cuts = item.edge_cuts || g.layer == "Edge.Cuts";
    }
    for p in &b.pads[m.p..] {
        // 回転とレジストの広がりを見込んで大きめに
        bb.add_radius(p.pos, 0.75 * p.size.x.max(p.size.y) + 0.2);
        item.holes = item.holes || p.drill.present;
    }
    for t in &b.tracks[m.t..] {
        bb.add_radius(t.start, t.width / 2.0);
        bb.add_radius(t.end, t.width / 2.0);
        if let Some(mid) = t.mid {
            bb.add_radius(mid, t.width / 2.0);
        }
    }
    for v in &b.vias[m.v..] {
        bb.add_radius(v.pos, v.size / 2.0 + 0.1);
        item.holes = true;
    }
    for z in &b.zones[m.z..] {
        for &p in &z.pts {
            bb.add(p);
        }
    }
    for s in &b.stroke_texts[m.s..] {
        if s.layer == "F.SilkS" || s.layer == "B.SilkS" {
            item.stroke_silk = true;
            bb.add_radius(s.pos, 1.0 + 0.8 * s.text.chars().count() as f64); // 文字の大きさは分からないので広めに
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
    for it in &root.items {
        if !it.is_list {
            continue;
        }
        let tag = it.tag();
        if matches!(tag, "general" | "layers" | "setup") {
            setup_key += &to_hex(sexpr::hash_node(it));
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
            "zone" => {
                let has_fill = it.children("filled_polygon").next().is_some();
                let wants_fill = it.child("fill").map(|f| f.has_atom("yes")).unwrap_or(false);
                if !has_fill && wants_fill && !it.has("keepout") {
                    board.unfilled_zones += 1;
                }
                read_zone_fills(it, &mut board);
            }
            "gr_text" => read_text(it, None, 0.0, &mut board),
            _ => {
                if let Some(kind) = tag.strip_prefix("gr_") {
                    if let Some(g) = read_graphic(it, kind, &ident, None) {
                        board.graphics.push(g);
                    }
                }
            }
        }
        if is_item {
            add_item(it, &mark, &mut board);
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
        assert!(b.items[4].stroke_silk);
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
        assert!((nearest - (2f64.sqrt() - 1.0) * 0.25).abs() < 0.005, "{nearest}"); // 頂点が 45° ちょうどに来るとは限らない
        assert_eq!(b.graphics[1].pts.len(), 4); // 半径なしは 4 点のまま
        // knockout：字を囲む四角形（余白 max(0.2 / 2, 0.9 / 9) = 0.1）から字を抜く。90° 回した文字でも、字の向きの四角形
        let k = &b.graphics[2];
        assert_eq!(k.cutouts.len(), 1);
        let (lo, hi) = k.pts.iter().fold((Vec2::splat(99.0), Vec2::splat(-99.0)), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        assert!((lo - Vec2::new(9.9, 9.9)).length() < 1e-9 && (hi - Vec2::new(10.3, 11.1)).length() < 1e-9, "{lo} {hi}");
        assert!(b.graphics[3].cutouts.is_empty() && b.graphics[3].pts.len() == 4);
    }
}
