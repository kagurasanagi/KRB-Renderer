//! KiCad の図形・パッド・配線を多角形（Clipper2、1 単位 = 10 nm）に変換する

use clipper2::{Clipper, ClipperError, EndType, FillRule, JoinType, Point, PointScaler};
use krbr_kicad::{rotate_point, Graphic, GrKind, Pad, PadShape, Track};

pub use glam::DVec2 as Vec2;
/// 円弧・円・ベジェの折れ線化は krbr-kicad の curves にある 1 つの実装を使う（パーサの図形と同じ細かさになるように）
pub use krbr_kicad::curves::{arc_points, bezier_points, circle_points, segments_for, ARC_TOL};

/// 座標の倍率：1 単位 = 10 nm（ARC_TOL の 1/200 で、折れ線化の誤差より十分細かい。i64 なので基板の大きさでは溢れない）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Nano;

impl PointScaler for Nano {
    const MULTIPLIER: f64 = 1e5;
}

pub type Path = clipper2::Path<Nano>;
pub type Paths = clipper2::Paths<Nano>;

pub fn to_path(pts: &[Vec2]) -> Path {
    Path::new(pts.iter().map(|p| Point::new(p.x, p.y)).collect())
}

pub fn from_path(p: &Path) -> Vec<Vec2> {
    p.iter().map(|q| Vec2::new(q.x(), q.y())).collect()
}

pub fn path_points(p: &Path) -> impl Iterator<Item = Vec2> + '_ {
    p.iter().map(|q| Vec2::new(q.x(), q.y()))
}

/// 符号付き面積（シューレース公式）。Clipper2 の出力は外周が正、穴が負
pub fn signed_area(p: &Path) -> f64 {
    let pts: Vec<Vec2> = from_path(p);
    let n = pts.len();
    if n < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    for i in 0..n {
        let (p0, p1) = (pts[i], pts[(i + 1) % n]);
        a += p0.x * p1.y - p1.x * p0.y;
    }
    a * 0.5
}

/// 折れ線を幅 width で太らせた多角形（丸い継ぎ目・端）
pub fn stroke_polyline(pts: &[Vec2], width: f64, closed: bool) -> Paths {
    if pts.is_empty() || width <= 0.0 {
        return Paths::new(Vec::new());
    }
    if pts.len() == 1 || (!closed && pts.len() == 2 && pts[0] == pts[1]) {
        return disc(pts[0], width / 2.0);
    }
    let input: Paths = to_path(pts).into();
    input.inflate(width / 2.0, JoinType::Round, if closed { EndType::Joined } else { EndType::Round }, 2.0)
}

pub fn disc(c: Vec2, r: f64) -> Paths {
    if r <= 0.0 {
        return Paths::new(Vec::new());
    }
    to_path(&circle_points(c, r, ARC_TOL)).into()
}

pub fn graphic_to_paths(g: &Graphic) -> Paths {
    let mut out = Paths::new(Vec::new());
    match g.kind {
        GrKind::Line => append(&mut out, stroke_polyline(&[g.start, g.end], g.width, false)),
        GrKind::Arc => append(&mut out, stroke_polyline(&arc_points(g.start, g.mid, g.end, ARC_TOL), g.width, false)),
        GrKind::Circle => {
            let r = (g.end - g.start).length();
            if g.filled {
                append(&mut out, disc(g.start, r + g.width / 2.0));
            } else {
                append(&mut out, stroke_polyline(&circle_points(g.start, r, ARC_TOL), g.width, true));
            }
        }
        GrKind::Poly => {
            if g.pts.len() < 2 {
                return out;
            }
            if g.filled && g.pts.len() >= 3 {
                let fill = unite(&to_path(&g.pts).into(), FillRule::NonZero);
                if g.cutouts.is_empty() {
                    append(&mut out, fill);
                } else {
                    let holes: Paths = g.cutouts.iter().map(|c| to_path(c)).collect::<Vec<_>>().into();
                    append(&mut out, subtract(&fill, &unite(&holes, FillRule::NonZero)));
                }
            }
            if g.width > 0.0 {
                append(&mut out, stroke_polyline(&g.pts, g.width, true));
            }
        }
        GrKind::Curve => append(&mut out, stroke_polyline(&bezier_points(&g.pts, ARC_TOL), g.width, false)),
    }
    out
}

fn rect_pts(size: Vec2) -> Vec<Vec2> {
    let h = size / 2.0;
    vec![Vec2::new(-h.x, -h.y), Vec2::new(h.x, -h.y), Vec2::new(h.x, h.y), Vec2::new(-h.x, h.y)]
}

fn chamfered_rect_pts(size: Vec2, chamfer: f64, corners: u32) -> Vec<Vec2> {
    let h = size / 2.0;
    let c = chamfer.min(h.x).min(h.y);
    let mut out = Vec::new();
    // 左上から時計回り（y 下向き）
    if corners & 1 != 0 {
        out.push(Vec2::new(-h.x, -h.y + c));
        out.push(Vec2::new(-h.x + c, -h.y));
    } else {
        out.push(Vec2::new(-h.x, -h.y));
    }
    if corners & 2 != 0 {
        out.push(Vec2::new(h.x - c, -h.y));
        out.push(Vec2::new(h.x, -h.y + c));
    } else {
        out.push(Vec2::new(h.x, -h.y));
    }
    if corners & 8 != 0 {
        out.push(Vec2::new(h.x, h.y - c));
        out.push(Vec2::new(h.x - c, h.y));
    } else {
        out.push(Vec2::new(h.x, h.y));
    }
    if corners & 4 != 0 {
        out.push(Vec2::new(-h.x + c, h.y));
        out.push(Vec2::new(-h.x, h.y - c));
    } else {
        out.push(Vec2::new(-h.x, h.y));
    }
    out
}

/// 原点中心・未回転の基本形
fn basic_shape(shape: PadShape, size: Vec2, p: &Pad) -> Paths {
    let min_dim = size.x.min(size.y);
    match shape {
        PadShape::Circle => disc(Vec2::ZERO, size.x / 2.0),
        PadShape::Rect => to_path(&rect_pts(size)).into(),
        PadShape::Oval => {
            if (size.x - size.y).abs() < 1e-9 {
                return disc(Vec2::ZERO, size.x / 2.0);
            }
            let l = (size.x.max(size.y) - min_dim) / 2.0;
            let seg = if size.x > size.y { [Vec2::new(-l, 0.0), Vec2::new(l, 0.0)] } else { [Vec2::new(0.0, -l), Vec2::new(0.0, l)] };
            stroke_polyline(&seg, min_dim, false)
        }
        PadShape::Trapezoid => {
            let (h, d) = (size / 2.0, p.rect_delta / 2.0);
            to_path(&[
                Vec2::new(-h.x - d.y, h.y + d.x),
                Vec2::new(h.x + d.y, h.y - d.x),
                Vec2::new(h.x - d.y, -h.y + d.x),
                Vec2::new(-h.x + d.y, -h.y - d.x),
            ])
            .into()
        }
        PadShape::RoundRect => {
            let r = p.round_ratio.clamp(0.0, 0.5) * min_dim;
            let base: Paths = to_path(&if p.chamfer_corners != 0 {
                chamfered_rect_pts(size, p.chamfer_ratio * min_dim, p.chamfer_corners)
            } else {
                rect_pts(size)
            })
            .into();
            if r <= 0.0 {
                return base;
            }
            let shrunk = base.inflate(-r, JoinType::Miter, EndType::Polygon, 10.0);
            if shrunk.is_empty() {
                return disc(Vec2::ZERO, min_dim / 2.0);
            }
            shrunk.inflate(r, JoinType::Round, EndType::Polygon, 2.0)
        }
        PadShape::Custom => Paths::new(Vec::new()),
    }
}

fn transform_paths(input: &Paths, pos: Vec2, angle_deg: f64) -> Paths {
    Paths::new(
        input
            .iter()
            .map(|path| {
                Path::new(
                    path.iter()
                        .map(|pt| {
                            let q = pos + rotate_point(Vec2::new(pt.x(), pt.y()), angle_deg);
                            Point::new(q.x, q.y)
                        })
                        .collect(),
                )
            })
            .collect(),
    )
}

/// パッドの形（inflate > 0 でレジスト開口などの拡大）。形は穴の位置（p.pos）ではなく、drill の offset だけずらした
/// p.shape_pos() に置く（KiCad の PAD::ShapePos()）
pub fn pad_to_paths(p: &Pad, inflate: f64) -> Paths {
    let mut local = if p.shape == PadShape::Custom {
        let mut l = basic_shape(p.custom_anchor, p.size, p);
        for g in &p.primitives {
            append(&mut l, graphic_to_paths(g));
        }
        unite(&l, FillRule::NonZero)
    } else {
        basic_shape(p.shape, p.size, p)
    };
    if inflate != 0.0 && !local.is_empty() {
        local = local.inflate(inflate, JoinType::Round, EndType::Polygon, 2.0);
    }
    transform_paths(&local, p.shape_pos(), p.angle_deg)
}

pub fn track_to_paths(t: &Track) -> Paths {
    match t.mid {
        Some(mid) => stroke_polyline(&arc_points(t.start, mid, t.end, ARC_TOL), t.width, false),
        None => stroke_polyline(&[t.start, t.end], t.width, false),
    }
}

/// 穴の形（円または長円）
pub fn drill_to_paths(center: Vec2, angle_deg: f64, size: Vec2, oval: bool) -> Paths {
    if !oval || (size.x - size.y).abs() < 1e-9 {
        return disc(center, size.x / 2.0);
    }
    let min_dim = size.x.min(size.y);
    let l = (size.x.max(size.y) - min_dim) / 2.0;
    let dir = if size.x > size.y { Vec2::X } else { Vec2::Y };
    let a = center + rotate_point(-l * dir, angle_deg);
    let b = center + rotate_point(l * dir, angle_deg);
    stroke_polyline(&[a, b], min_dim, false)
}

fn point_count(p: &Paths) -> usize {
    p.iter().map(|q| q.len()).sum()
}

/// Clipper2 のブール演算が失敗したとき（座標が i64 の範囲を超えるなど、実際の基板では起きない内部の異常）に、
/// 記録を残して、演算ごとに決めた代わりの値を返す。黙って空にすると、銅箔や基板の本体が理由もなく消えるので
fn on_clipper_error(op: &str, a: &Paths, b: &Paths, e: ClipperError, fallback: &Paths) -> Paths {
    krbr_core::warn!("polygon {} failed ({:?}; {} + {} points); using the {} instead", op, e, point_count(a), point_count(b), if fallback.is_empty() { "empty set" } else { "input" });
    fallback.clone()
}

/// 和。失敗したら入力のまま（重なりが残るだけで、塗る範囲は同じ）
pub fn unite(p: &Paths, rule: FillRule) -> Paths {
    if p.is_empty() {
        return Paths::new(Vec::new());
    }
    let empty = Paths::new(Vec::new());
    Clipper::new().add_subject(p.clone()).add_clip(empty.clone()).union(rule).unwrap_or_else(|e| on_clipper_error("union", p, &empty, e, p))
}

/// 差 a − b。失敗したら a のまま（引くものが残るだけで、a が消えるよりよい）
pub fn subtract(a: &Paths, b: &Paths) -> Paths {
    if a.is_empty() {
        return Paths::new(Vec::new());
    }
    Clipper::new().add_subject(a.clone()).add_clip(b.clone()).difference(FillRule::NonZero).unwrap_or_else(|e| on_clipper_error("difference", a, b, e, a))
}

/// 積。失敗したら空（切り抜きの範囲の外まで出すと、隣のタイルと重なるので）
pub fn intersect(a: &Paths, b: &Paths) -> Paths {
    if a.is_empty() || b.is_empty() {
        return Paths::new(Vec::new());
    }
    let empty = Paths::new(Vec::new());
    Clipper::new().add_subject(a.clone()).add_clip(b.clone()).intersect(FillRule::NonZero).unwrap_or_else(|e| on_clipper_error("intersection", a, b, e, &empty))
}

pub fn append(dst: &mut Paths, src: Paths) {
    let v: Vec<Path> = src.into();
    dst.append(v);
}

pub fn rect_path(lo: Vec2, hi: Vec2) -> Paths {
    to_path(&[lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)]).into()
}

/// 外周と、その中の穴の組（Clipper2 の出力の向きで判定：外周が正の面積、穴が負）
pub struct Ring {
    pub outer: Vec<Vec2>,
    pub holes: Vec<Vec<Vec2>>,
}

fn point_in_polygon(p: Vec2, poly: &[Vec2]) -> bool {
    let mut inside = false;
    let n = poly.len();
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (poly[i], poly[j]);
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// 多角形の集合を「外周＋穴」の組に分ける（Clipper2 の PolyTree 相当）
pub fn nest_rings(paths: &Paths) -> Vec<Ring> {
    let mut outers: Vec<(Vec<Vec2>, f64)> = Vec::new();
    let mut holes: Vec<Vec<Vec2>> = Vec::new();
    for p in paths.iter() {
        let pts = from_path(p);
        if pts.len() < 3 {
            continue;
        }
        let a = signed_area(p);
        if a > 0.0 {
            outers.push((pts, a));
        } else if a < 0.0 {
            holes.push(pts);
        }
    }
    let mut rings: Vec<Ring> = outers.iter().map(|(o, _)| Ring { outer: o.clone(), holes: Vec::new() }).collect();
    for h in holes {
        // 含む外周のうち、いちばん小さいもの
        let mut best: Option<(usize, f64)> = None;
        for (i, (o, area)) in outers.iter().enumerate() {
            if point_in_polygon(h[0], o) && best.map(|(_, a)| *area < a).unwrap_or(true) {
                best = Some((i, *area));
            }
        }
        if let Some((i, _)) = best {
            rings[i].holes.push(h);
        }
    }
    rings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knockout_fill_subtracts_the_glyphs() {
        let sq = |lo: Vec2, hi: Vec2| vec![lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)];
        let g = Graphic { kind: GrKind::Poly, filled: true, pts: sq(Vec2::ZERO, Vec2::new(4.0, 2.0)), cutouts: vec![sq(Vec2::new(1.0, 0.5), Vec2::new(2.0, 1.5))], ..Default::default() };
        let area: f64 = graphic_to_paths(&g).iter().map(signed_area).sum::<f64>().abs();
        assert!((area - 7.0).abs() < 1e-6, "{area}");
    }

    #[test]
    fn disc_and_boolean_ops() {
        let a = disc(Vec2::ZERO, 1.0);
        let b = disc(Vec2::new(1.0, 0.0), 1.0);
        let u = unite(&{
            let mut p = a.clone();
            append(&mut p, b.clone());
            p
        }, FillRule::NonZero);
        assert_eq!(u.len(), 1);
        let area: f64 = u.iter().map(signed_area).sum();
        assert!(area > std::f64::consts::PI && area < 2.0 * std::f64::consts::PI);
        let d = subtract(&a, &b);
        assert!(d.iter().map(signed_area).sum::<f64>() < std::f64::consts::PI);
        assert!(intersect(&a, &b).iter().map(signed_area).sum::<f64>() > 0.0);
    }

    #[test]
    fn rings_with_holes() {
        let outer = rect_path(Vec2::new(0.0, 0.0), Vec2::new(10.0, 10.0));
        let hole = rect_path(Vec2::new(4.0, 4.0), Vec2::new(6.0, 6.0));
        let body = subtract(&outer, &hole);
        let rings = nest_rings(&body);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].holes.len(), 1);
    }

    #[test]
    fn stroke_width_matches_area() {
        let s = stroke_polyline(&[Vec2::ZERO, Vec2::new(10.0, 0.0)], 1.0, false);
        let area: f64 = s.iter().map(signed_area).sum();
        // 長さ 10、幅 1 の帯 ＋ 両端の半円（π/4）
        assert!((area - (10.0 + std::f64::consts::PI / 4.0)).abs() < 0.05, "{area}");
    }
}
