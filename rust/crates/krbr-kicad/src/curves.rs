//! 円弧・円・ベジェ・角の丸い四角形の折れ線化（パーサの図形と、krbr-geom の多角形化で共有する唯一の実装）。
//! 許容誤差はすべて ARC_TOL にそろえる（同じ円弧が、どの経路で折れ線になっても同じ細かさになるように）

use std::f64::consts::PI;

use crate::board::{Box2, Vec2};

/// 円弧を折れ線にするときの許容誤差（弦と円弧の最大のずれ、mm）。銅箔の凹凸の厚み（数十 µm）より十分小さく、
/// 画面上でも見分けられない値
pub const ARC_TOL: f64 = 0.002;

/// 半径 r・中心角 sweep（rad）の円弧を、弦と円弧のずれが tol 以下になるように分ける数（4 以上）
pub fn segments_for(r: f64, sweep: f64, tol: f64) -> usize {
    if r <= tol {
        return ((sweep.abs() / (PI / 4.0)).ceil() as usize).max(4);
    }
    let step = 2.0 * (1.0 - tol / r).acos();
    ((sweep.abs() / step).ceil() as usize).clamp(4, 4096)
}

/// 3 点を通る円の中心と半径。3 点が一直線なら None
pub fn circle_through(a: Vec2, m: Vec2, b: Vec2) -> Option<(Vec2, f64)> {
    let d = 2.0 * (a.x * (m.y - b.y) + m.x * (b.y - a.y) + b.x * (a.y - m.y));
    if d.abs() < 1e-12 {
        return None;
    }
    let (a2, m2, b2) = (a.dot(a), m.dot(m), b.dot(b));
    let c = Vec2::new((a2 * (m.y - b.y) + m2 * (b.y - a.y) + b2 * (a.y - m.y)) / d, (a2 * (b.x - m.x) + m2 * (a.x - b.x) + b2 * (m.x - a.x)) / d);
    Some((c, (a - c).length()))
}

/// 3 点を通る円弧の折れ線（両端を含む。端点は a と b をそのまま使う）
pub fn arc_points(a: Vec2, m: Vec2, b: Vec2, tol: f64) -> Vec<Vec2> {
    let Some((c, r)) = circle_through(a, m, b) else { return vec![a, b] };
    let t0 = (a.y - c.y).atan2(a.x - c.x);
    let tm = (m.y - c.y).atan2(m.x - c.x);
    let t1 = (b.y - c.y).atan2(b.x - c.x);
    let norm = |t: f64| t.rem_euclid(2.0 * PI);
    let mut sweep = norm(t1 - t0);
    if norm(tm - t0) > sweep {
        sweep -= 2.0 * PI; // 逆回り
    }
    let n = segments_for(r, sweep, tol);
    let mut out = Vec::with_capacity(n + 1);
    out.push(a);
    for i in 1..n {
        let t = t0 + sweep * i as f64 / n as f64;
        out.push(c + r * Vec2::new(t.cos(), t.sin()));
    }
    out.push(b);
    out
}

/// 3 点を通る円弧を幅 width で描いたときの外接矩形（円弧のふくらみも含む）
pub fn arc_box(a: Vec2, m: Vec2, b: Vec2, width: f64) -> Box2 {
    let mut bb = Box2::default();
    // 折れ線の頂点は円周上にあり、弦は円弧より最大 ARC_TOL 内側なので、その分だけ広げれば円弧を含む
    for p in arc_points(a, m, b, ARC_TOL) {
        bb.add_radius(p, width / 2.0 + ARC_TOL);
    }
    bb
}

pub fn circle_points(c: Vec2, r: f64, tol: f64) -> Vec<Vec2> {
    let n = segments_for(r, 2.0 * PI, tol);
    (0..n)
        .map(|i| {
            let t = 2.0 * PI * i as f64 / n as f64;
            c + r * Vec2::new(t.cos(), t.sin())
        })
        .collect()
}

/// 3 次ベジェ（制御点 4 つ）の折れ線。分割数は制御点の折れ線の長さから見積もる
pub fn bezier_points(c: &[Vec2], tol: f64) -> Vec<Vec2> {
    if c.len() != 4 {
        return c.to_vec();
    }
    let len = (c[1] - c[0]).length() + (c[2] - c[1]).length() + (c[3] - c[2]).length();
    let n = ((len / tol).sqrt().ceil() as usize).clamp(4, 512);
    (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            let u = 1.0 - t;
            u * u * u * c[0] + 3.0 * u * u * t * c[1] + 3.0 * u * t * t * c[2] + t * t * t * c[3]
        })
        .collect()
}

/// 角の丸い四角形（lo-hi、角の半径 r）の頂点。r = 0 なら 4 点
pub fn rounded_rect_points(lo: Vec2, hi: Vec2, r: f64, tol: f64) -> Vec<Vec2> {
    let r = r.min(0.5 * (hi.x - lo.x)).min(0.5 * (hi.y - lo.y));
    if r <= 0.0 {
        return vec![lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)];
    }
    let n = segments_for(r, 0.5 * PI, tol);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_keeps_endpoints_and_stays_within_tolerance() {
        // 中心 (1, 0)、半径 1 の 3/4 周（ふくらみが 3 点の外接矩形の外に出る）
        let (a, m, b) = (Vec2::new(0.0, 0.0), Vec2::new(2.0, 0.0), Vec2::new(1.0, -1.0));
        let pts = arc_points(a, m, b, ARC_TOL);
        assert_eq!(pts[0], a);
        assert_eq!(*pts.last().unwrap(), b);
        for w in pts.windows(2) {
            let mid = (w[0] + w[1]) * 0.5;
            assert!((1.0 - (mid - Vec2::new(1.0, 0.0)).length()) <= ARC_TOL + 1e-12);
        }
        // 外接矩形は円弧のふくらみ（y = +1）を含む
        let bb = arc_box(a, m, b, 0.0);
        assert!(bb.hi.y >= 1.0 && bb.lo.y <= -1.0 && bb.lo.x <= 0.0 && bb.hi.x >= 2.0);
    }
}
