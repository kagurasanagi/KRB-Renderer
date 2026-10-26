//! 基板外形を押し出した板（上下面・側面・穴の内壁）を作る

use std::collections::HashMap;

use clipper2::{FillRule, PointScaler};
use glam::Vec3;
use krbr_core::Mesh;
use krbr_kicad::{rotate_point, Board, Box2, GrKind, PadType};

use crate::shapes::*;

/// 基板メッシュの三角形ごとの材質区分。基板メッシュは側面と穴の内壁だけで、上下の面は銅・レジスト・シルクの厚みの
/// 形状（relief.rs、ReliefMat）が覆う
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BoardMat {
    /// 外形の側面・メッキなし穴（FR4 の断面）
    Edge = 0,
    /// メッキ穴の内壁
    Plated = 1,
}

#[derive(Debug, Clone, Default)]
pub struct BoardGeometry {
    /// 穴を開ける前の基板領域
    pub region: Paths,
    /// 穴を開けた後の基板領域
    pub body: Paths,
    /// 基板座標での外形範囲
    pub bounds: Box2,
    /// ワールド原点に置く基板座標
    pub center: Vec2,
    pub thickness: f64,
    /// 基材（側面・穴の内壁）の上下面の高さ ±core_half
    pub core_half: f64,
    /// ワールド座標（X = x - cx, Y = 上, Z = y - cy）
    pub mesh: Mesh,
    pub hole_count: usize,
    pub outline_from_fallback: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct BoardBuildOptions {
    /// 基材の半分の厚み（<= 0 なら板厚の半分）
    pub core_half: f64,
}

impl Default for BoardBuildOptions {
    fn default() -> Self {
        Self { core_half: -1.0 }
    }
}

/// 基板座標 -> ワールド座標
pub fn board_to_world(p: Vec2, y: f64, center: Vec2) -> Vec3 {
    Vec3::new((p.x - center.x) as f32, y as f32, (p.y - center.y) as f32)
}

const JOIN_TOL: f64 = 1e-3; // 外形の端点をつなぐ許容差（mm）

/// Edge.Cuts の図形を閉じた輪郭の集合にする
fn outline_loops(board: &Board, open_chains: &mut usize) -> Vec<Vec<Vec2>> {
    let mut loops: Vec<Vec<Vec2>> = Vec::new();
    let mut open: Vec<Vec<Vec2>> = Vec::new();
    for g in &board.graphics {
        if g.layer != "Edge.Cuts" {
            continue;
        }
        match g.kind {
            GrKind::Line => open.push(vec![g.start, g.end]),
            GrKind::Arc => open.push(arc_points(g.start, g.mid, g.end, ARC_TOL)),
            GrKind::Curve => open.push(bezier_points(&g.pts, ARC_TOL)),
            GrKind::Circle => loops.push(circle_points(g.start, (g.end - g.start).length(), ARC_TOL)),
            GrKind::Poly => {
                if g.pts.len() >= 3 {
                    loops.push(g.pts.clone());
                }
            }
        }
    }
    let near = |a: Vec2, b: Vec2| (a - b).length() < JOIN_TOL;
    let mut used = vec![false; open.len()];
    *open_chains = 0;
    for i in 0..open.len() {
        if used[i] {
            continue;
        }
        used[i] = true;
        let mut chain = open[i].clone();
        // 末尾の側へ伸ばし、行き止まりになったときだけ先頭の側へ 1 本伸ばす。閉じたらそこで止める。
        // 以前は 1 回の走査で両端へ伸ばし、閉じた後も続けたので、輪郭が閉じる点に触れる別の輪郭（面付けした基板、
        // 外形と頂点を共有する切り欠き）までつないで 1 本にしていた
        while !near(chain[0], *chain.last().unwrap()) {
            let mut extended = false;
            for j in 0..open.len() {
                if used[j] {
                    continue;
                }
                let p = &open[j];
                let back = *chain.last().unwrap();
                if near(back, p[0]) {
                    chain.extend_from_slice(&p[1..]);
                } else if near(back, *p.last().unwrap()) {
                    chain.extend(p[..p.len() - 1].iter().rev());
                } else {
                    continue;
                }
                used[j] = true;
                extended = true;
                if near(chain[0], *chain.last().unwrap()) {
                    break;
                }
            }
            if extended {
                continue;
            }
            let front = chain[0];
            let Some(j) = (0..open.len()).find(|&j| !used[j] && (near(front, open[j][0]) || near(front, *open[j].last().unwrap()))) else { break };
            let p = &open[j];
            let mut c: Vec<Vec2> = if near(front, *p.last().unwrap()) { p[..p.len() - 1].to_vec() } else { p[1..].iter().rev().cloned().collect() };
            c.extend(chain.iter());
            chain = c;
            used[j] = true;
        }
        if near(chain[0], *chain.last().unwrap()) {
            chain.pop();
        } else {
            *open_chains += 1;
        }
        if chain.len() >= 3 {
            loops.push(chain);
        }
    }
    loops
}

/// 2 つ以上の穴が同じ頂点を持つ印（どの穴の頂点とも決められない）
const SHARED_VERTEX: u32 = u32::MAX;

/// メッキ穴の形（長円は中心線 a-b、円なら a == b）
#[derive(Clone, Copy)]
struct PlatedHole {
    a: Vec2,
    b: Vec2,
    r: f64,
}

/// 穴の多角形の辺と、穴の円（長円の縁）との距離の上限：頂点は円の上にあり（整数座標への丸めで最大 1 単位ずれる）、
/// 辺は円より最大 ARC_TOL だけ内側を通る（circle_points の許容誤差）
const HOLE_EDGE_TOL: f64 = ARC_TOL + 2.0 / Nano::MULTIPLIER;

/// メッキ穴の空間索引の格子の大きさ（mm）。ビア（穴の直径 0.2〜0.4 mm 程度）が 1 マスに数個までに収まる大きさ
const HOLE_GRID_MM: f64 = 1.0;

fn dist_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f64 {
    let ab = b - a;
    let l2 = ab.dot(ab);
    let t = if l2 > 0.0 { ((p - a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (p - (a + t * ab)).length()
}

/// Clipper2 の整数座標（基板の本体の輪郭は from_path で mm に直してあるので、倍率を掛けて戻す。値はちょうど整数に戻る）
fn vertex_key(p: Vec2) -> (i64, i64) {
    ((p.x * Nano::MULTIPLIER).round() as i64, (p.y * Nano::MULTIPLIER).round() as i64)
}

fn cell_of(p: Vec2) -> (i64, i64) {
    ((p.x / HOLE_GRID_MM).floor() as i64, (p.y / HOLE_GRID_MM).floor() as i64)
}

/// 穴（合成する前の 1 つずつの形）と、その輪郭の頂点がどの穴から来たかの表。
/// Clipper2 のブール演算は入力の頂点を整数座標のまま出力に残し、新しく作るのは辺どうしの交点だけなので、基板の本体の輪郭の
/// 頂点をこの表で引けば、側壁がどの穴の内壁かを、距離の許容差に頼らずに決められる
#[derive(Default)]
struct Holes {
    paths: Paths,
    /// 頂点（Clipper2 の整数座標）→ 穴の番号（SHARED_VERTEX は複数の穴の頂点）
    vertex_owner: HashMap<(i64, i64), u32>,
    /// 穴の番号ごとのメッキの有無
    hole_plated: Vec<bool>,
    plated: Vec<PlatedHole>,
    /// メッキ穴の空間索引（格子のマス → plated の番号）。頂点で決まらない辺だけに使う
    grid: HashMap<(i64, i64), Vec<u32>>,
}

impl Holes {
    fn add(&mut self, center: Vec2, angle_deg: f64, size: Vec2, oval: bool, plated: bool) {
        let shape = drill_to_paths(center, angle_deg, size, oval);
        let id = self.hole_plated.len() as u32;
        self.hole_plated.push(plated);
        for path in shape.iter() {
            for q in path.iter() {
                self.vertex_owner
                    .entry((q.x_scaled(), q.y_scaled()))
                    .and_modify(|o| {
                        if *o != id {
                            *o = SHARED_VERTEX;
                        }
                    })
                    .or_insert(id);
            }
        }
        append(&mut self.paths, shape);
        if !plated {
            return;
        }
        let min_dim = size.x.min(size.y);
        let l = (size.x.max(size.y) - min_dim) / 2.0;
        let dir = if size.x > size.y { Vec2::X } else { Vec2::Y };
        let h = if !oval || l <= 0.0 {
            PlatedHole { a: center, b: center, r: size.x / 2.0 }
        } else {
            PlatedHole { a: center + rotate_point(-l * dir, angle_deg), b: center + rotate_point(l * dir, angle_deg), r: min_dim / 2.0 }
        };
        let index = self.plated.len() as u32;
        let reach = Vec2::splat(h.r + HOLE_EDGE_TOL);
        let (lo, hi) = (cell_of(h.a.min(h.b) - reach), cell_of(h.a.max(h.b) + reach));
        for cy in lo.1..=hi.1 {
            for cx in lo.0..=hi.0 {
                self.grid.entry((cx, cy)).or_default().push(index);
            }
        }
        self.plated.push(h);
    }

    /// 辺 p0-p1 が、メッキ穴の縁の上にあるか（空間索引で近くのメッキ穴だけを調べる）
    fn on_plated_hole(&self, p0: Vec2, p1: Vec2) -> bool {
        let mid = (p0 + p1) * 0.5;
        let Some(near) = self.grid.get(&cell_of(mid)) else { return false };
        near.iter().any(|&i| {
            let h = &self.plated[i as usize];
            [p0, p1, mid].iter().all(|&p| (dist_to_segment(p, h.a, h.b) - h.r).abs() <= HOLE_EDGE_TOL)
        })
    }

    /// 側壁の辺 p0-p1 の材質。両端が同じ穴の頂点なら、その穴の縁の辺（穴は凸なので、2 つの頂点を結ぶ線は縁か穴の中を通り、
    /// 穴の中の線は基板の本体の輪郭に残らない）。それ以外（ほかの輪郭との交点を端に持つ辺、外形の辺、穴の頂点がたまたま
    /// 外形の上にある場合など）は、近くのメッキ穴の縁に乗っているかを幾何で調べる
    fn wall_mat(&self, p0: Vec2, p1: Vec2) -> u32 {
        let o0 = self.vertex_owner.get(&vertex_key(p0)).copied();
        let o1 = self.vertex_owner.get(&vertex_key(p1)).copied();
        let plated = match (o0, o1) {
            (Some(a), Some(b)) if a == b && a != SHARED_VERTEX => self.hole_plated[a as usize],
            _ => self.on_plated_hole(p0, p1),
        };
        if plated { BoardMat::Plated as u32 } else { BoardMat::Edge as u32 }
    }
}

/// 外周＋穴を三角形に分けて、高さ y に水平な面を張る（up: 上向きの面か）
fn triangulate_face(rings: &[Ring], y: f64, up: bool, mat: u32, center: Vec2, mesh: &mut Mesh) {
    let normal_y = if up { 1.0 } else { -1.0 };
    let mut ear = earcut::Earcut::<f64>::new();
    let mut tri: Vec<u32> = Vec::new();
    for ring in rings {
        let mut flat: Vec<[f64; 2]> = Vec::new();
        let mut hole_indices: Vec<u32> = Vec::new();
        for p in &ring.outer {
            flat.push([p.x, p.y]);
        }
        for h in &ring.holes {
            hole_indices.push(flat.len() as u32);
            for p in h {
                flat.push([p.x, p.y]);
            }
        }
        ear.earcut(flat.iter().cloned(), &hole_indices, &mut tri);
        let base = mesh.positions.len() as u32;
        for v in &flat {
            mesh.add_vertex(board_to_world(Vec2::new(v[0], v[1]), y, center), Vec3::new(0.0, normal_y, 0.0));
        }
        for t in tri.chunks_exact(3) {
            if up {
                mesh.add_triangle(base + t[0], base + t[1], base + t[2], mat);
            } else {
                mesh.add_triangle(base + t[0], base + t[2], base + t[1], mat);
            }
        }
    }
}

/// 輪郭の側面。材質が左側にある向き（外周は正の面積、穴は負）で外向き法線を決める。mat_at は辺の両端から材質を決める
fn build_walls(pts: &[Vec2], y_top: f64, y_bot: f64, center: Vec2, mat_at: &dyn Fn(Vec2, Vec2) -> u32, mesh: &mut Mesh) {
    let n = pts.len();
    if n < 3 {
        return;
    }
    // 進行方向 d の左側に材質があるので、外向き法線は (dy, -dx)
    let en: Vec<Vec2> = (0..n)
        .map(|i| {
            let d = pts[(i + 1) % n] - pts[i];
            let len = d.length();
            if len > 0.0 {
                Vec2::new(d.y, -d.x) / len
            } else {
                Vec2::ZERO
            }
        })
        .collect();
    let cos_smooth = (40.0f64).to_radians().cos();
    let vtx_normal = |edge: usize, vtx_edge: usize| -> Vec2 {
        let (a, b) = (en[edge], en[vtx_edge]);
        if a.dot(b) >= cos_smooth {
            let s = a + b;
            let l = s.length();
            if l > 0.0 {
                return s / l;
            }
        }
        a
    };
    for i in 0..n {
        let j = (i + 1) % n;
        let (p0, p1) = (pts[i], pts[j]);
        if (p1 - p0).length() <= 0.0 {
            continue;
        }
        let n0 = vtx_normal(i, (i + n - 1) % n);
        let n1 = vtx_normal(i, j);
        let mat = mat_at(p0, p1);
        let wn0 = Vec3::new(n0.x as f32, 0.0, n0.y as f32);
        let wn1 = Vec3::new(n1.x as f32, 0.0, n1.y as f32);
        let a = mesh.add_vertex(board_to_world(p0, y_top, center), wn0);
        let b = mesh.add_vertex(board_to_world(p1, y_top, center), wn1);
        let c = mesh.add_vertex(board_to_world(p1, y_bot, center), wn1);
        let d = mesh.add_vertex(board_to_world(p0, y_bot, center), wn0);
        mesh.add_triangle(a, c, b, mat);
        mesh.add_triangle(a, d, c, mat);
    }
}

fn all_rings(rings: &[Ring]) -> Vec<&Vec<Vec2>> {
    let mut out = Vec::new();
    for r in rings {
        out.push(&r.outer);
        for h in &r.holes {
            out.push(h);
        }
    }
    out
}

/// region を押し出す：y_face の高さに面（face_up で向き）、y_face〜y_base に側壁を作る（高さ 0 なら面だけ）
pub fn extrude_region(region: &Paths, y_face: f64, y_base: f64, face_up: bool, face_mat: u32, wall_mat: u32, center: Vec2, mesh: &mut Mesh) {
    if region.is_empty() {
        return;
    }
    let united = unite(region, FillRule::NonZero);
    let rings = nest_rings(&united);
    triangulate_face(&rings, y_face, face_up, face_mat, center, mesh);
    if (y_face - y_base).abs() < 1e-9 {
        return;
    }
    let (hi, lo) = (y_face.max(y_base), y_face.min(y_base));
    for r in all_rings(&rings) {
        build_walls(r, hi, lo, center, &|_, _| wall_mat, mesh);
    }
}

pub fn build_board_geometry(board: &Board, opts: &BoardBuildOptions) -> BoardGeometry {
    let mut geo = BoardGeometry { thickness: board.stackup.thickness, ..Default::default() };
    geo.core_half = if opts.core_half > 0.0 { opts.core_half } else { geo.thickness / 2.0 };

    let mut open_chains = 0;
    let loops = outline_loops(board, &mut open_chains);
    if open_chains > 0 {
        krbr_core::warn!("Edge.Cuts: {} outline chain(s) are not closed; closing them", open_chains);
    }
    let loop_paths = Paths::new(loops.iter().map(|l| to_path(l)).collect());
    geo.region = unite(&loop_paths, FillRule::EvenOdd);

    if geo.region.is_empty() {
        // 外形が無い場合はパッド・配線の範囲から仮の外形を作る
        let mut b = Box2::default();
        for p in &board.pads {
            b.add(p.pos);
        }
        for t in &board.tracks {
            b.add(t.start);
            b.add(t.end);
        }
        if !b.valid() {
            b = Box2 { lo: Vec2::splat(-10.0), hi: Vec2::splat(10.0) };
        }
        b.lo -= Vec2::splat(2.0);
        b.hi += Vec2::splat(2.0);
        geo.region = rect_path(b.lo, b.hi);
        geo.outline_from_fallback = true;
        krbr_core::warn!("no closed Edge.Cuts outline; using bounding box of copper items");
    }
    for path in geo.region.iter() {
        for q in path_points(path) {
            geo.bounds.add(q);
        }
    }
    geo.center = geo.bounds.center();

    // 穴
    let mut holes = Holes::default();
    for p in &board.pads {
        if !p.drill.present || !matches!(p.kind, PadType::ThruHole | PadType::NpThruHole) {
            continue;
        }
        // 穴はパッドの位置（ファイルの (at)）に開ける。drill の offset は銅の形だけをずらす（Pad::shape_pos）
        holes.add(p.pos, p.angle_deg, p.drill.size, p.drill.oval, p.kind == PadType::ThruHole);
        geo.hole_count += 1;
    }
    for v in &board.vias {
        // ブラインド・ベリッドのビアは基板を貫通しない（外に届く面のランドとくぼみは relief が作る）
        if v.drill <= 0.0 || !v.is_through() {
            continue;
        }
        holes.add(v.pos, 0.0, Vec2::splat(v.drill), false, true);
        geo.hole_count += 1;
    }
    geo.body = subtract(&geo.region, &unite(&holes.paths, FillRule::NonZero));
    let rings = nest_rings(&geo.body);

    // 上下の面は作らない（基材の面は relief.rs の凹凸の形状が ±core_half の高さで覆う）
    let (y_top, y_bot) = (geo.core_half, -geo.core_half);
    for r in all_rings(&rings) {
        build_walls(r, y_top, y_bot, geo.center, &|p0, p1| holes.wall_mat(p0, p1), &mut geo.mesh);
    }

    krbr_core::info!(
        "board: {:.2} x {:.2} mm, thickness {:.2} mm, {} holes, {} triangles",
        geo.bounds.size().x,
        geo.bounds.size().y,
        geo.thickness,
        geo.hole_count,
        geo.mesh.triangle_count()
    );
    geo
}

#[cfg(test)]
mod tests {
    use super::*;
    use krbr_kicad::{Drill, Graphic, Pad, PadShape};

    fn line(a: Vec2, b: Vec2) -> Graphic {
        Graphic { kind: GrKind::Line, layer: "Edge.Cuts".into(), start: a, end: b, ..Default::default() }
    }

    /// 頂点を共有する 2 つの四角形（面付けした基板など）は、2 本の輪郭のまま（以前は閉じた後もつなぎ続けて 1 本になった）
    #[test]
    fn outlines_sharing_a_vertex_stay_separate() {
        let mut board = Board::default();
        let a = [Vec2::new(0.0, 0.0), Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0), Vec2::new(0.0, 10.0)];
        let b = [Vec2::new(0.0, 0.0), Vec2::new(-10.0, 0.0), Vec2::new(-10.0, -10.0), Vec2::new(0.0, -10.0)];
        // 共有する頂点は a の輪郭が閉じる点。a の最後の辺を b の辺より後に並べる（a が閉じた時点で、まだ b の辺が残っている）
        for i in 0..3 {
            board.graphics.push(line(a[i], a[i + 1]));
        }
        for i in 0..4 {
            board.graphics.push(line(b[i], b[(i + 1) % 4]));
        }
        board.graphics.push(line(a[3], a[0]));
        let mut open = 0;
        let loops = outline_loops(&board, &mut open);
        assert_eq!(open, 0);
        let mut sizes: Vec<usize> = loops.iter().map(|l| l.len()).collect();
        sizes.sort();
        assert_eq!(sizes, vec![4, 4]);
    }

    #[test]
    fn rectangle_board_with_a_hole() {
        let mut board = Board::default();
        let c = [Vec2::new(0.0, 0.0), Vec2::new(20.0, 0.0), Vec2::new(20.0, 10.0), Vec2::new(0.0, 10.0)];
        for i in 0..4 {
            board.graphics.push(line(c[i], c[(i + 1) % 4]));
        }
        board.pads.push(Pad {
            number: "1".into(),
            kind: PadType::ThruHole,
            shape: PadShape::Circle,
            pos: Vec2::new(10.0, 5.0),
            angle_deg: 0.0,
            size: Vec2::splat(2.0),
            rect_delta: Vec2::ZERO,
            round_ratio: 0.0,
            chamfer_ratio: 0.0,
            chamfer_corners: 0,
            drill: Drill { present: true, oval: false, size: Vec2::splat(1.0), offset: Vec2::ZERO },
            layers: vec!["*.Cu".into()],
            mask_margin: None,
            custom_anchor: PadShape::Circle,
            primitives: Vec::new(),
            footprint: None,
        });
        let geo = build_board_geometry(&board, &BoardBuildOptions::default());
        assert!((geo.bounds.size().x - 20.0).abs() < 1e-9 && (geo.bounds.size().y - 10.0).abs() < 1e-9);
        assert_eq!(geo.hole_count, 1);
        assert!(!geo.outline_from_fallback);
        assert!(geo.mesh.triangle_count() > 20);
        // 穴の内壁はメッキ、外形の側面は基材
        assert!(geo.mesh.tri_material.iter().any(|&m| m == BoardMat::Plated as u32));
        assert!(geo.mesh.tri_material.iter().any(|&m| m == BoardMat::Edge as u32));
        // ワールド座標の中心は原点
        let b = geo.mesh.bounds();
        assert!((b.lo.x + 10.0).abs() < 1e-4 && (b.hi.x - 10.0).abs() < 1e-4);
        assert!((b.hi.y - 0.8).abs() < 1e-5);
    }

    fn hole_pad(pos: Vec2, angle_deg: f64, drill: Vec2, oval: bool, kind: PadType) -> Pad {
        Pad {
            number: "1".into(),
            kind,
            shape: PadShape::Circle,
            pos,
            angle_deg,
            size: drill + Vec2::splat(0.5),
            rect_delta: Vec2::ZERO,
            round_ratio: 0.0,
            chamfer_ratio: 0.0,
            chamfer_corners: 0,
            drill: Drill { present: true, oval, size: drill, offset: Vec2::ZERO },
            layers: vec!["*.Cu".into()],
            mask_margin: None,
            custom_anchor: PadShape::Circle,
            primitives: Vec::new(),
            footprint: None,
        }
    }

    /// 穴の種類が混ざった基板：円・長円のメッキ穴、メッキなし穴、外形にかかる穴、ビアの格子、ビアのすぐ近くのメッキなし穴
    fn mixed_holes_board() -> Board {
        let mut board = Board::default();
        let c = [Vec2::new(0.0, 0.0), Vec2::new(30.0, 0.0), Vec2::new(30.0, 20.0), Vec2::new(0.0, 20.0)];
        for i in 0..4 {
            board.graphics.push(line(c[i], c[(i + 1) % 4]));
        }
        board.pads.push(hole_pad(Vec2::new(5.0, 5.0), 0.0, Vec2::splat(1.0), false, PadType::ThruHole));
        board.pads.push(hole_pad(Vec2::new(10.0, 5.0), 30.0, Vec2::new(2.0, 1.0), true, PadType::ThruHole));
        board.pads.push(hole_pad(Vec2::new(15.0, 5.0), 0.0, Vec2::splat(1.2), false, PadType::NpThruHole));
        board.pads.push(hole_pad(Vec2::new(0.0, 10.0), 0.0, Vec2::splat(1.0), false, PadType::ThruHole));
        for i in 0..10 {
            for j in 0..5 {
                board.vias.push(krbr_kicad::Via {
                    pos: Vec2::new(3.0 + 2.0 * i as f64, 10.0 + 1.5 * j as f64),
                    size: 0.6,
                    drill: 0.3,
                    layer_from: "F.Cu".into(),
                    layer_to: "B.Cu".into(),
                    tent_front: None,
                    tent_back: None,
                });
            }
        }
        // ビア (25, 16)（穴の半径 0.15）から 0.01 mm だけ離れたメッキなし穴
        board.vias.push(krbr_kicad::Via { pos: Vec2::new(25.0, 16.0), size: 0.6, drill: 0.3, layer_from: "F.Cu".into(), layer_to: "B.Cu".into(), tent_front: None, tent_back: None });
        board.pads.push(hole_pad(Vec2::new(25.0 + 0.15 + 0.01 + 0.2, 16.0), 0.0, Vec2::splat(0.4), false, PadType::NpThruHole));
        board
    }

    /// 側壁の材質の判定を変えても、基板のメッシュの形（頂点・三角形）は変わらない。材質は、穴ごとの正解と合う
    #[test]
    fn mixed_holes_walls() {
        let board = mixed_holes_board();
        let geo = build_board_geometry(&board, &BoardBuildOptions::default());
        let m = &geo.mesh;
        // 判定を作り直す前（全メッキ穴との距離を 0.02 mm の許容差で調べていたとき）と同じ形。値は上下の面を作らなくした後の
        // もの（作っていたときに、上下の面を除いて求めた値と一致することを確かめてある）
        let psum: f64 = m.positions.iter().map(|p| (p.x as f64) * 1.0 + (p.y as f64) * 3.0 + (p.z as f64) * 7.0).sum();
        let isum: u64 = m.indices.iter().enumerate().map(|(k, &i)| (k as u64 % 13 + 1) * i as u64).sum();
        assert_eq!((m.positions.len(), m.triangle_count(), isum), (4772, 2386, 119490904));
        assert!((psum - 59599.979777).abs() < 1e-3, "{psum}");
        // 上下の面は作らない（法線はすべて水平）
        assert!(m.normals.iter().all(|n| n.y == 0.0));

        // 側壁の四角形（三角形 2 枚）ごとに、上面の辺の中点が、どの穴の縁にあるかで正解を決める
        let to_board = |p: Vec3| Vec2::new(p.x as f64 + geo.center.x, p.z as f64 + geo.center.y);
        let mut plated_walls = 0;
        let mut checked = 0;
        for (t, &mat) in m.tri_material.iter().enumerate() {
            assert!(mat == BoardMat::Plated as u32 || mat == BoardMat::Edge as u32);
            let tri: Vec<Vec3> = (0..3).map(|k| m.positions[m.indices[3 * t + k] as usize]).collect();
            let top: Vec<Vec2> = tri.iter().filter(|p| p.y > 0.0).map(|&p| to_board(p)).collect();
            if top.len() != 2 {
                continue; // 四角形のもう 1 枚（上面の辺を 1 本の頂点しか持たない）
            }
            let mid = (top[0] + top[1]) * 0.5;
            let on = |c: Vec2, r: f64| ((mid - c).length() - r).abs() <= HOLE_EDGE_TOL;
            // ビアのすぐ近くのメッキなし穴、メッキなしの大きい穴は基材。ビアと外形にかかる穴はメッキ
            let want_plated = board.vias.iter().any(|v| on(v.pos, v.drill / 2.0)) || on(Vec2::new(5.0, 5.0), 0.5) || on(Vec2::new(0.0, 10.0), 0.5) || (mid - Vec2::new(10.0, 5.0)).length() < 1.01;
            let want_edge = on(Vec2::new(25.36, 16.0), 0.2) || on(Vec2::new(15.0, 5.0), 0.6) || mid.x.abs() < 1e-6 || (mid.x - 30.0).abs() < 1e-6 || mid.y.abs() < 1e-6 || (mid.y - 20.0).abs() < 1e-6;
            assert!(want_plated != want_edge, "ambiguous wall at {mid}");
            assert_eq!(mat == BoardMat::Plated as u32, want_plated, "wall at {mid}");
            plated_walls += want_plated as usize;
            checked += 1;
        }
        assert!(plated_walls > 0 && checked > plated_walls);
    }

    /// drill の offset（KiCad の PAD::ShapePos）：穴はパッドの位置に開き、銅とレジスト開口は形のずれた位置に置く
    #[test]
    fn drill_offset_moves_copper_and_mask_but_not_the_hole() {
        let mut board = Board::default();
        let c = [Vec2::new(0.0, 0.0), Vec2::new(20.0, 0.0), Vec2::new(20.0, 10.0), Vec2::new(0.0, 10.0)];
        for i in 0..4 {
            board.graphics.push(line(c[i], c[(i + 1) % 4]));
        }
        let mut pad = hole_pad(Vec2::new(10.0, 5.0), 90.0, Vec2::splat(0.6), false, PadType::ThruHole);
        pad.size = Vec2::new(3.0, 1.2);
        pad.shape = PadShape::Rect;
        pad.drill.offset = Vec2::new(0.8, 0.0);
        pad.layers = vec!["*.Cu".into(), "*.Mask".into()];
        pad.mask_margin = Some(0.1);
        board.pads.push(pad);
        // 90° 回したパッドなので、x 方向のずれは画面の上（-y）へ
        let shape_center = Vec2::new(10.0, 4.2);

        // 穴の内壁（メッキ）の頂点は、パッドの位置を中心に半径 0.3 の円の上
        let geo = build_board_geometry(&board, &BoardBuildOptions::default());
        let m = &geo.mesh;
        let mut wall = 0;
        for (t, &mat) in m.tri_material.iter().enumerate() {
            if mat != BoardMat::Plated as u32 {
                continue;
            }
            for k in 0..3 {
                let p = m.positions[m.indices[3 * t + k] as usize];
                let q = Vec2::new(p.x as f64 + geo.center.x, p.z as f64 + geo.center.y);
                assert!(((q - Vec2::new(10.0, 5.0)).length() - 0.3).abs() <= HOLE_EDGE_TOL, "hole vertex {q}");
                wall += 1;
            }
        }
        assert!(wall > 0);

        // 銅（3 x 1.2 を 90° 回した 1.2 x 3）とレジスト開口（0.1 広い）は、形の中心のまわり
        let sp = crate::raster::collect_side_paths(&board, krbr_kicad::Side::Front, &crate::LayerArtOptions::default(), None);
        let bbox = |p: &Paths| {
            let mut b = Box2::default();
            for path in p.iter() {
                for q in path_points(path) {
                    b.add(q);
                }
            }
            b
        };
        let (cu, mask) = (bbox(&sp.copper), bbox(&sp.mask_open));
        let near = |a: Vec2, b: Vec2| (a - b).length() < 1e-3;
        assert!(near(cu.lo, shape_center - Vec2::new(0.6, 1.5)) && near(cu.hi, shape_center + Vec2::new(0.6, 1.5)), "{cu:?}");
        assert!(near(mask.lo, shape_center - Vec2::new(0.7, 1.6)) && near(mask.hi, shape_center + Vec2::new(0.7, 1.6)), "{mask:?}");
        // タイルの範囲（Pad::extent）も、形と穴を含む
        let e = board.pads[0].extent(&board.setup);
        assert!(e.lo.x <= mask.lo.x && e.lo.y <= mask.lo.y && e.hi.x >= mask.hi.x && e.hi.y >= mask.hi.y && e.hi.y >= 5.3, "{e:?}");
    }

    #[test]
    fn hole_vertices_survive_boolean_ops() {
        // Clipper2 は入力の頂点を整数座標のまま残す（判定の前提）
        let mut holes = Holes::default();
        holes.add(Vec2::new(1.0, 1.0), 0.0, Vec2::splat(0.3), false, true);
        let region = rect_path(Vec2::ZERO, Vec2::splat(2.0));
        let body = subtract(&region, &unite(&holes.paths, FillRule::NonZero));
        let ring = nest_rings(&body).remove(0);
        assert_eq!(ring.holes.len(), 1);
        assert!(ring.holes[0].iter().all(|&p| holes.vertex_owner.get(&vertex_key(p)) == Some(&0)));
        assert!(ring.outer.iter().all(|&p| !holes.vertex_owner.contains_key(&vertex_key(p))));
    }

    /// 基板を貫通する穴になるのは貫通ビアだけ（ブラインド・ベリッドのビアは、以前は貫通する穴とメッキの壁になっていた）
    #[test]
    fn only_through_vias_drill_the_board() {
        let mut board = Board::default();
        let c = [Vec2::new(0.0, 0.0), Vec2::new(20.0, 0.0), Vec2::new(20.0, 10.0), Vec2::new(0.0, 10.0)];
        for i in 0..4 {
            board.graphics.push(line(c[i], c[(i + 1) % 4]));
        }
        for (x, from, to) in [(5.0, "F.Cu", "B.Cu"), (10.0, "F.Cu", "In1.Cu"), (15.0, "In1.Cu", "In2.Cu")] {
            board.vias.push(krbr_kicad::Via { pos: Vec2::new(x, 5.0), size: 0.6, drill: 0.3, layer_from: from.into(), layer_to: to.into(), tent_front: None, tent_back: None });
        }
        let geo = build_board_geometry(&board, &BoardBuildOptions::default());
        assert_eq!(geo.hole_count, 1);
    }
}
