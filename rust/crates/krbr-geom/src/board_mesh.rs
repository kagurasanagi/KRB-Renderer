//! 基板外形を押し出した板（上下面・側面・穴の内壁）を作る

use clipper2::FillRule;
use glam::Vec3;
use krbr_core::Mesh;
use krbr_kicad::{rotate_point, Board, GrKind, PadType};

use crate::shapes::*;

/// 基板メッシュの三角形ごとの材質区分
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BoardMat {
    /// 上面（層テクスチャで材質を決める）
    Top = 0,
    Bottom = 1,
    /// 外形の側面・メッキなし穴（FR4 の断面）
    Edge = 2,
    /// メッキ穴の内壁
    Plated = 3,
}

#[derive(Debug, Clone, Copy)]
pub struct Bounds2 {
    pub lo: Vec2,
    pub hi: Vec2,
}

impl Default for Bounds2 {
    fn default() -> Self {
        Self { lo: Vec2::splat(1e30), hi: Vec2::splat(-1e30) }
    }
}

impl Bounds2 {
    pub fn add(&mut self, p: Vec2) {
        self.lo = self.lo.min(p);
        self.hi = self.hi.max(p);
    }
    pub fn valid(&self) -> bool {
        self.lo.x <= self.hi.x
    }
    pub fn center(&self) -> Vec2 {
        (self.lo + self.hi) * 0.5
    }
    pub fn size(&self) -> Vec2 {
        self.hi - self.lo
    }
}

#[derive(Debug, Clone, Default)]
pub struct BoardGeometry {
    /// 穴を開ける前の基板領域
    pub region: Paths,
    /// 穴を開けた後の基板領域
    pub body: Paths,
    /// 基板座標での外形範囲
    pub bounds: Bounds2,
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
    /// 上下面を作る（表面の凹凸を別に作る場合は false）
    pub caps: bool,
}

impl Default for BoardBuildOptions {
    fn default() -> Self {
        Self { core_half: -1.0, caps: true }
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
        let mut extended = true;
        while extended && !near(chain[0], *chain.last().unwrap()) {
            extended = false;
            for j in 0..open.len() {
                if used[j] {
                    continue;
                }
                let p = &open[j];
                let (front, back) = (chain[0], *chain.last().unwrap());
                if near(back, p[0]) {
                    chain.extend_from_slice(&p[1..]);
                } else if near(back, *p.last().unwrap()) {
                    chain.extend(p[..p.len() - 1].iter().rev());
                } else if near(front, *p.last().unwrap()) {
                    let mut c = p[..p.len() - 1].to_vec();
                    c.extend(chain.iter());
                    chain = c;
                } else if near(front, p[0]) {
                    let mut c: Vec<Vec2> = p[1..].iter().rev().cloned().collect();
                    c.extend(chain.iter());
                    chain = c;
                } else {
                    continue;
                }
                used[j] = true;
                extended = true;
            }
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

#[derive(Clone, Copy)]
struct PlatedHole {
    a: Vec2,
    b: Vec2, // 長円の中心線（円なら a == b）
    r: f64,
}

fn dist_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f64 {
    let ab = b - a;
    let l2 = ab.dot(ab);
    let t = if l2 > 0.0 { ((p - a).dot(ab) / l2).clamp(0.0, 1.0) } else { 0.0 };
    (p - (a + t * ab)).length()
}

fn add_hole(center: Vec2, angle_deg: f64, size: Vec2, oval: bool, plated: bool, holes: &mut Paths, plated_holes: &mut Vec<PlatedHole>) {
    append(holes, drill_to_paths(center, angle_deg, size, oval));
    if !plated {
        return;
    }
    let min_dim = size.x.min(size.y);
    let l = (size.x.max(size.y) - min_dim) / 2.0;
    let dir = if size.x > size.y { Vec2::X } else { Vec2::Y };
    if !oval || l <= 0.0 {
        plated_holes.push(PlatedHole { a: center, b: center, r: size.x / 2.0 });
    } else {
        plated_holes.push(PlatedHole {
            a: center + rotate_point(-l * dir, angle_deg),
            b: center + rotate_point(l * dir, angle_deg),
            r: min_dim / 2.0,
        });
    }
}

struct Cap {
    y: f64,
    normal_y: f32, // +1: 上向きの面, -1: 下向き
    mat: u32,
}

/// 外周＋穴を三角形に分けて、各 cap の高さに面を張る
fn triangulate_faces(rings: &[Ring], caps: &[Cap], center: Vec2, mesh: &mut Mesh) {
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
        for cap in caps {
            let base = mesh.positions.len() as u32;
            for v in &flat {
                mesh.add_vertex(board_to_world(Vec2::new(v[0], v[1]), cap.y, center), Vec3::new(0.0, cap.normal_y, 0.0));
            }
            for t in tri.chunks_exact(3) {
                if cap.normal_y > 0.0 {
                    mesh.add_triangle(base + t[0], base + t[1], base + t[2], cap.mat);
                } else {
                    mesh.add_triangle(base + t[0], base + t[2], base + t[1], cap.mat);
                }
            }
        }
    }
}

/// 輪郭の側面。材質が左側にある向き（外周は正の面積、穴は負）で外向き法線を決める
fn build_walls(pts: &[Vec2], y_top: f64, y_bot: f64, center: Vec2, mat_at: &dyn Fn(Vec2) -> u32, mesh: &mut Mesh) {
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
        let mat = mat_at((p0 + p1) * 0.5);
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
    triangulate_faces(&rings, &[Cap { y: y_face, normal_y: if face_up { 1.0 } else { -1.0 }, mat: face_mat }], center, mesh);
    if (y_face - y_base).abs() < 1e-9 {
        return;
    }
    let (hi, lo) = (y_face.max(y_base), y_face.min(y_base));
    for r in all_rings(&rings) {
        build_walls(r, hi, lo, center, &|_| wall_mat, mesh);
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
        let mut b = Bounds2::default();
        for p in &board.pads {
            b.add(p.pos);
        }
        for t in &board.tracks {
            b.add(t.start);
            b.add(t.end);
        }
        if !b.valid() {
            b = Bounds2 { lo: Vec2::splat(-10.0), hi: Vec2::splat(10.0) };
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
    let mut holes = Paths::new(Vec::new());
    let mut plated: Vec<PlatedHole> = Vec::new();
    for p in &board.pads {
        if !p.drill.present || !matches!(p.kind, PadType::ThruHole | PadType::NpThruHole) {
            continue;
        }
        let c = p.pos + rotate_point(p.drill.offset, p.angle_deg);
        add_hole(c, p.angle_deg, p.drill.size, p.drill.oval, p.kind == PadType::ThruHole, &mut holes, &mut plated);
        geo.hole_count += 1;
    }
    for v in &board.vias {
        if v.drill <= 0.0 {
            continue;
        }
        add_hole(v.pos, 0.0, Vec2::splat(v.drill), false, true, &mut holes, &mut plated);
        geo.hole_count += 1;
    }
    geo.body = subtract(&geo.region, &unite(&holes, FillRule::NonZero));
    let rings = nest_rings(&geo.body);

    let (y_top, y_bot) = (geo.core_half, -geo.core_half);
    if opts.caps {
        triangulate_faces(
            &rings,
            &[Cap { y: y_top, normal_y: 1.0, mat: BoardMat::Top as u32 }, Cap { y: y_bot, normal_y: -1.0, mat: BoardMat::Bottom as u32 }],
            geo.center,
            &mut geo.mesh,
        );
    }
    let wall_mat = |mid: Vec2| -> u32 {
        for h in &plated {
            if (dist_to_segment(mid, h.a, h.b) - h.r).abs() < 0.02 {
                return BoardMat::Plated as u32;
            }
        }
        BoardMat::Edge as u32
    };
    for r in all_rings(&rings) {
        build_walls(r, y_top, y_bot, geo.center, &wall_mat, &mut geo.mesh);
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
}
