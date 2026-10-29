//! はんだフィレット：部品の 3D モデルとパッドから、はんだの盛り（メニスカス）の形を作る。
//!
//! パッドごと（表と裏）に、パッドを覆う格子の点で次を求める（パッドの座標系：基板座標の x, y と、パッドの面から外へ測った高さ u）。
//! - 接地部：モデルの金属の部位を u = SLICE_HEIGHTS の高さで切った断面の中（リードの足・チップ部品の電極・スルーホールのピン）。
//!   低い方から試し、パッドの上に断面がある高さを使う（リードが少し浮いたモデルもある）。はんだが付くのは金属だけ。
//!   金属の部位が無いか、金属がパッドの上で断面を作らないモデルは、全部の部位で探す
//! - リードの高さ：接地部の真上の金属の面（はんだが登れる高さ）
//! - 上の面：その点の真上（u0 より上）で一番近いモデルの面（はんだが超えてはいけない高さ：部品の腹・曲がったリードの下）
//! - 樹脂の中：一番近い面が金属でない部位の上向きの面（フラットリードの部品の本体の底のように、樹脂が基板に接している）
//!
//! 表面実装：はんだは金属の接地部に引き上げられ、離れるほど下がる凹んだ形 h = H' · (1 − d / L)²（d は一番近い接地部までの距離）。
//! 際の高さ H' はそこのリードの高さ H まで（それより上には行かない）、ただし際の傾きが MAX_CONTACT_SLOPE を超えない高さまで。
//! 広がる距離 L は、はんだの体積（パッドの面積 × メタルマスクの厚み × 溶けた後に残る割合）に合うように決める。樹脂の中に隠れる分は体積に数えない（そこでも高さは周りから続けるので、本体の縁で崖にならない）。
//! パッドの縁では一番近い縁までの距離の SMD_EDGE_SLOPE 倍まで（丸く下がる）、電極の横の狭い帯では幅の SMD_SLOPE 倍まで（壁にしない）。
//! スルーホール：ランドの幅に傾きを掛けた高さ（ピンの高さまで）から、ランドの縁へ下がる。
//! どこでも上の面より上には出さないので、リードや部品を突き抜けない。
//! 表面実装はペーストの層（F.Paste / B.Paste）があるパッドの、部品の面だけ。スルーホールは表と裏（レジストの開いた面）。

use std::collections::HashMap;

use glam::{DVec2, DVec3};
use krbr_core::Mesh;
use krbr_geom::shapes::{pad_to_paths, path_points};
use krbr_kicad::{Board, Pad, PadType, Side};

/// メタルマスクの厚み（mm）
const STENCIL_MM: f64 = 0.12;
/// はんだペーストのうち、溶けた後にはんだとして残る体積の割合（残りはフラックス）
const PASTE_METAL: f64 = 0.5;
/// 表面実装のはんだの高さの上限：接地部から縁までの幅（接地部までの距離 + 一番近い縁までの距離）× これ。チップ部品の横のように、
/// 電極の際にパッドが少ししか残らないところで、はんだが壁のように立たないようにする
const SMD_SLOPE: f64 = 1.0;
/// 表面実装のはんだの接地部の際の傾きの上限。はんだが少なくて広がる距離 L が短いときは、リードの高さまで登らずに低くなる
/// （際の高さ H' = min(H, L × MAX_CONTACT_SLOPE / 2)。(1 − d / L)² の際の傾きは 2H' / L）
const MAX_CONTACT_SLOPE: f64 = 1.5;
/// 表面実装のはんだの高さの上限：パッドの一番近い縁までの距離 × これ。つま先の盛りの脇（流れと平行な縁）が、
/// 垂直な崖にならずに丸く下がるように
const SMD_EDGE_SLOPE: f64 = 3.0;
/// スルーホールのフィレットの傾き（高さ / ランドの幅）：はんだ面、部品面（部品面へは穴を通って少しだけ上がる）
const THT_SLOPE_SOLDER_SIDE: f64 = 0.9;
const THT_SLOPE_COMPONENT_SIDE: f64 = 0.4;
/// スルーホールのフィレットの高さの上限（mm）
const MAX_THT_HEIGHT: f64 = 1.2;
/// リードの断面を取る高さ（パッドの面から、mm）
const SLICE_HEIGHTS: [f64; 3] = [0.03, 0.06, 0.1];
/// はんだの外周の高さ（これより薄いところは作らない。パッドの面と重ならないように）
const MIN_HEIGHT: f64 = 0.002;
/// はんだとモデルの面の間に残す隙間（mm）
const CLEARANCE: f64 = 0.004;
/// 上の面が見つからない接地部（閉じていないモデル）のリードの高さ（mm）
const DEFAULT_WALL: f64 = 0.3;
/// 格子の間隔：パッドの短い辺をこの数に分ける（0.012〜0.05 mm）。法線は高さの場から滑らかに求めるので、粗くても形は滑らか
const GRID_DIVISIONS: f64 = 24.0;
/// 格子の点の数の上限（大きなパッドは格子を粗くする）
const MAX_POINTS: usize = 250_000;
/// 表面実装のはんだが登れるかを確かめる高さの段の数（接地部からリードの上面まで）
const CLIMB_LEVELS: usize = 12;
/// 段の境目の段差をならす回数（3 × 3 の平均）
const LEVEL_SMOOTHING: usize = 2;
/// 接地部までの距離をならす回数（3 × 3 の平均。格子の点から測った距離の段差を、鏡のようなはんだに映さない）
const DIST_SMOOTHING: usize = 2;

/// 部品ごとのフィレットのキャッシュ：保存の監視中に、パッドもモデルも動いていない部品を計算し直さない。
/// キーは footprint_key。組み立てのたびに、使わなかった部品の分は捨てる
#[derive(Default)]
pub struct SolderCache {
    prev: HashMap<u64, Mesh>,
    next: HashMap<u64, Mesh>,
    /// 最後の組み立てで計算した部品の数（キャッシュに無かったもの）
    pub last_built: usize,
}

impl SolderCache {
    /// key の結果（前の組み立てか、今回の組み立てで覚えたもの）
    pub fn take(&mut self, key: u64) -> Option<Mesh> {
        let m = self.prev.remove(&key).or_else(|| self.next.get(&key).cloned())?;
        self.next.insert(key, m.clone());
        Some(m)
    }

    /// 作った結果を覚える
    pub fn keep(&mut self, key: u64, m: &Mesh) {
        self.last_built += 1;
        self.next.insert(key, m.clone());
    }

    /// 組み立ての始め
    pub fn begin(&mut self) {
        self.last_built = 0;
    }

    /// 組み立ての終わり：今回使った分だけを残す
    pub fn finish(&mut self) {
        self.prev = std::mem::take(&mut self.next);
    }
}

/// footprint_fillets の結果を決めるもののハッシュ：部品のパッド、モデルの三角形（と金属か）、パッドの面の高さ、原点、材質
pub fn footprint_key(board: &Board, fi: usize, tris: &[[DVec3; 3]], metal: &[bool], pad_top: f64, center: DVec2, material: u32) -> u64 {
    let mut bytes: Vec<u8> = Vec::with_capacity(tris.len() * 37 + 256);
    bytes.extend(metal.iter().map(|&m| m as u8));
    for t in tris {
        for p in t {
            for v in [p.x, p.y, p.z] {
                bytes.extend_from_slice(&(v as f32).to_le_bytes());
            }
        }
    }
    for p in board.pads.iter().filter(|p| p.footprint == Some(fi)) {
        bytes.extend_from_slice(format!("{p:?}").as_bytes());
    }
    if let Some(fp) = board.footprints.get(fi) {
        bytes.push(fp.back as u8);
    }
    for v in [pad_top, center.x, center.y] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    bytes.extend_from_slice(&material.to_le_bytes());
    krbr_core::xxh64(&bytes)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Smd,
    /// スルーホール：はんだ面（部品の反対側）か
    Tht { solder_side: bool },
}

fn paste_layer(side: Side) -> &'static str {
    match side {
        Side::Front => "F.Paste",
        Side::Back => "B.Paste",
    }
}

/// 部品 fi のフィレット（ワールド座標のメッシュ。三角形の材質は material）。
/// tris は部品の 3D モデルの三角形で、各点は (基板座標 x, 基板座標 y, ワールドの高さ Y)。
/// metal は tris と同じ数の、金属の部位の三角形か（はんだが付くのは金属だけ。フラットリードの部品などは、樹脂の本体の底も
/// 基板に接しているので、断面だけでは電極と見分けられない）。空か、金属が 1 つも無ければ、どの三角形も電極になりうるとみなす。
/// pad_top は表のパッドの面の高さ（ワールドの Y。裏は −pad_top）、center は基板座標でのワールドの原点
pub fn footprint_fillets(board: &Board, fi: usize, tris: &[[DVec3; 3]], metal: &[bool], pad_top: f64, center: DVec2, material: u32) -> Mesh {
    let mut out = Mesh::default();
    let Some(fp) = board.footprints.get(fi) else { return out };
    if tris.is_empty() {
        return out;
    }
    let component_side = if fp.back { Side::Back } else { Side::Front };
    for pad in board.pads.iter().filter(|p| p.footprint == Some(fi)) {
        for side in [Side::Front, Side::Back] {
            if !pad.on_layer(side.copper_layer()) || !pad.on_layer(side.mask_layer()) {
                continue;
            }
            let kind = match pad.kind {
                PadType::Smd if pad.on_layer(paste_layer(side)) => Kind::Smd,
                PadType::ThruHole => Kind::Tht { solder_side: side != component_side },
                _ => continue,
            };
            pad_fillet(pad, side, kind, tris, metal, pad_top, center, material, &mut out);
        }
    }
    out
}

/// 点が多角形（偶奇の規則。穴のある形も）の中か
fn inside_rings(rings: &[Vec<DVec2>], p: DVec2) -> bool {
    let mut inside = false;
    for r in rings {
        let n = r.len();
        for k in 0..n {
            let (a, b) = (r[k], r[(k + 1) % n]);
            if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) * (b.x - a.x) / (b.y - a.y) {
                inside = !inside;
            }
        }
    }
    inside
}

/// 格子：原点、間隔、点の数
struct Lattice {
    origin: DVec2,
    cell: f64,
    nx: usize,
    ny: usize,
}

impl Lattice {
    fn at(&self, k: usize) -> DVec2 {
        self.origin + DVec2::new((k % self.nx) as f64, (k / self.nx) as f64) * self.cell
    }
    fn len(&self) -> usize {
        self.nx * self.ny
    }
}

/// 各点から一番近い種（seed が true の点）。2 回の走査で近い種を隣から受け継ぐ（8 近傍。誤差は格子の間隔程度）
fn nearest_seed(seed: &[bool], g: &Lattice) -> Vec<Option<usize>> {
    let (nx, ny) = (g.nx as i64, g.ny as i64);
    let mut near: Vec<Option<usize>> = seed.iter().enumerate().map(|(k, &b)| b.then_some(k)).collect();
    let d2 = |k: usize, q: usize| {
        let (dx, dy) = ((k % g.nx) as i64 - (q % g.nx) as i64, (k / g.nx) as i64 - (q / g.nx) as i64);
        dx * dx + dy * dy
    };
    let relax = |i: i64, j: i64, offsets: &[(i64, i64)], near: &mut Vec<Option<usize>>| {
        let k = (j * nx + i) as usize;
        for &(di, dj) in offsets {
            let (a, b) = (i + di, j + dj);
            if a < 0 || b < 0 || a >= nx || b >= ny {
                continue;
            }
            if let Some(q) = near[(b * nx + a) as usize] {
                if near[k].is_none_or(|c| d2(k, q) < d2(k, c)) {
                    near[k] = Some(q);
                }
            }
        }
    };
    const FORWARD: [(i64, i64); 4] = [(-1, -1), (0, -1), (1, -1), (-1, 0)];
    const BACKWARD: [(i64, i64); 4] = [(1, 1), (0, 1), (-1, 1), (1, 0)];
    for j in 0..ny {
        for i in 0..nx {
            relax(i, j, &FORWARD, &mut near);
        }
    }
    for j in (0..ny).rev() {
        for i in (0..nx).rev() {
            relax(i, j, &BACKWARD, &mut near);
        }
    }
    near
}

/// 隣（8 近傍）に同じ印の点が 2 つ未満の点を外す
fn remove_specks(mark: &[bool], g: &Lattice) -> Vec<bool> {
    let (nx, ny) = (g.nx as i64, g.ny as i64);
    (0..mark.len())
        .map(|k| {
            if !mark[k] {
                return false;
            }
            let (i, j) = ((k % g.nx) as i64, (k / g.nx) as i64);
            let mut n = 0;
            for dj in -1..=1 {
                for di in -1..=1 {
                    let (a, b) = (i + di, j + dj);
                    if (di, dj) != (0, 0) && a >= 0 && b >= 0 && a < nx && b < ny && mark[(b * nx + a) as usize] {
                        n += 1;
                    }
                }
            }
            n >= 2
        })
        .collect()
}

/// v を、印のある点どうしの 3 × 3 の平均で passes 回ならす（印の無い点は変えず、混ぜない）
fn smooth(g: &Lattice, v: &mut [f64], mark: impl Fn(usize) -> bool, passes: usize) {
    let (nx, ny) = (g.nx as i64, g.ny as i64);
    for _ in 0..passes {
        let prev = v.to_vec();
        for k in (0..g.len()).filter(|&k| mark(k)) {
            let (i, j) = ((k % g.nx) as i64, (k / g.nx) as i64);
            let (mut sum, mut count) = (0.0, 0.0);
            for b in (j - 1).max(0)..=(j + 1).min(ny - 1) {
                for a in (i - 1).max(0)..=(i + 1).min(nx - 1) {
                    let q = (b * nx + a) as usize;
                    if mark(q) {
                        sum += prev[q];
                        count += 1.0;
                    }
                }
            }
            v[k] = sum / count;
        }
    }
}

/// 点から多角形の輪郭（線分）までの距離
fn distance_to_rings(rings: &[Vec<DVec2>], p: DVec2) -> f64 {
    let mut best = f64::INFINITY;
    for r in rings {
        for k in 0..r.len() {
            let (a, b) = (r[k], r[(k + 1) % r.len()]);
            let ab = b - a;
            let t = if ab.length_squared() > 0.0 { ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
            best = best.min((a + ab * t - p).length());
        }
    }
    best
}

/// 三角形（パッドの座標系）を高さ u0 で切った断面の中にある格子の点（偶奇の規則）
fn slice_inside(tris: &[[DVec3; 3]], u0: f64, g: &Lattice) -> Vec<bool> {
    let mut segs: Vec<(DVec2, DVec2)> = Vec::new();
    for t in tris {
        let d = t.map(|p| p.z - u0);
        let mut pts = Vec::with_capacity(2);
        for (a, b) in [(0, 1), (1, 2), (2, 0)] {
            if (d[a] > 0.0) != (d[b] > 0.0) {
                let p = t[a] + (t[b] - t[a]) * (d[a] / (d[a] - d[b]));
                pts.push(p.truncate());
            }
        }
        if pts.len() == 2 {
            segs.push((pts[0], pts[1]));
        }
    }
    let mut inside = vec![false; g.len()];
    let mut xs = Vec::new();
    for j in 0..g.ny {
        let y = g.origin.y + j as f64 * g.cell;
        xs.clear();
        xs.extend(segs.iter().filter(|(a, b)| (a.y > y) != (b.y > y)).map(|(a, b)| a.x + (y - a.y) * (b.x - a.x) / (b.y - a.y)));
        xs.sort_by(f64::total_cmp);
        let mut crossed = 0;
        for i in 0..g.nx {
            let x = g.origin.x + i as f64 * g.cell;
            while crossed < xs.len() && xs[crossed] < x {
                crossed += 1;
            }
            inside[j * g.nx + i] = crossed % 2 == 1;
        }
    }
    inside
}

#[allow(clippy::too_many_arguments)]
fn pad_fillet(pad: &Pad, side: Side, kind: Kind, tris: &[[DVec3; 3]], metal: &[bool], pad_top: f64, center: DVec2, material: u32, out: &mut Mesh) {
    let s = if side == Side::Front { 1.0 } else { -1.0 };
    let rings: Vec<Vec<DVec2>> = pad_to_paths(pad, 0.0).iter().map(|p| path_points(p).collect::<Vec<_>>()).filter(|r| r.len() >= 3).collect();
    if rings.is_empty() {
        return;
    }
    let (mut lo, mut hi) = (DVec2::splat(f64::MAX), DVec2::splat(f64::MIN));
    for p in rings.iter().flatten() {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    let size = hi - lo;
    let mut cell = (size.min_element() / GRID_DIVISIONS).clamp(0.012, 0.05);
    let points = |c: f64| ((size.x / c) as usize + 5) * ((size.y / c) as usize + 5);
    while points(cell) > MAX_POINTS {
        cell *= 1.5;
    }
    let margin = 2.0 * cell;
    let g = Lattice { origin: lo - DVec2::splat(margin), cell, nx: ((size.x + 2.0 * margin) / cell).ceil() as usize + 1, ny: ((size.y + 2.0 * margin) / cell).ceil() as usize + 1 };
    let g_hi = g.origin + DVec2::new((g.nx - 1) as f64, (g.ny - 1) as f64) * cell;

    // パッドの座標系の三角形（格子の範囲に重なり、パッドの面より上に届くもの）と、金属か
    let (local, local_metal): (Vec<[DVec3; 3]>, Vec<bool>) = tris
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let q = t.map(|p| DVec3::new(p.x, p.y, s * p.z - pad_top));
            let (qlo, qhi) = (q[0].min(q[1]).min(q[2]), q[0].max(q[1]).max(q[2]));
            (qhi.x >= g.origin.x && qlo.x <= g_hi.x && qhi.y >= g.origin.y && qlo.y <= g_hi.y && qhi.z > 0.0).then_some((q, metal.get(i).copied().unwrap_or(true)))
        })
        .unzip();
    if local.is_empty() {
        return;
    }
    // 接地部を探す三角形：断面を取る高さを横切り、格子の行に掛かるもの。断面の中か外かは行ごとの偶奇で決めるので、
    // 横（x）の範囲では絞らない（格子の外にあるリードの奥の面が抜けると、中と外が入れ替わる）。
    // 金属の部位で探し、金属が無いか、金属がパッドの上で断面を作らなければ（セラミックの台の底に薄い電極を付けたモデルなど）、全部で
    let (u_lo, u_hi) = (SLICE_HEIGHTS[0], SLICE_HEIGHTS[SLICE_HEIGHTS.len() - 1]);
    let (band, band_metal): (Vec<[DVec3; 3]>, Vec<bool>) = tris
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let q = t.map(|p| DVec3::new(p.x, p.y, s * p.z - pad_top));
            let (qlo, qhi) = (q[0].min(q[1]).min(q[2]), q[0].max(q[1]).max(q[2]));
            (qhi.y >= g.origin.y && qlo.y <= g_hi.y && qhi.z >= u_lo && qlo.z <= u_hi).then_some((q, metal.get(i).copied().unwrap_or(true)))
        })
        .unzip();
    let metal_tris: Vec<[DVec3; 3]> = band.iter().zip(&band_metal).filter(|(_, &m)| m).map(|(t, _)| *t).collect();
    let n = g.len();
    let inside: Vec<bool> = (0..n).map(|k| inside_rings(&rings, g.at(k))).collect();

    // 接地部（パッドの上に断面がある、一番低い高さ）
    // 孤立した点（閉じていないモデルの断面の雑音）は除く
    let find_contact = |tris: &[[DVec3; 3]]| SLICE_HEIGHTS.iter().map(|&u0| (u0, remove_specks(&slice_inside(tris, u0, &g), &g))).find(|(_, c)| (0..n).any(|k| c[k] && inside[k]));
    let metal_only = metal_tris.len() < band.len() && !metal_tris.is_empty();
    let (metal_only, found) = match metal_only.then(|| find_contact(&metal_tris)).flatten() {
        Some(f) => (true, Some(f)),
        None => (false, find_contact(&band)),
    };
    let Some((u0, contact)) = found else {
        return;
    };
    // 各点の真上（u0 より上）で一番近いモデルの面（above）と、一番近い金属の面（above_metal）。
    // 真上にある金属でない面の数（同じ高さは 1 つ）が奇数なら、その点は樹脂の本体の中（本体が基板に接している。本体の中に
    // 入ったリードの上も）。三角形の向きは STEP の面によってそろっていないことがあるので使わない。
    // 覆われたところのはんだは見えないので、はんだを引き上げる元にも体積にも数えない
    let mut above = vec![f64::INFINITY; n];
    let mut above_metal = vec![f64::INFINITY; n];
    let mut plastic_hits: Vec<Vec<f64>> = vec![Vec::new(); n];
    for (t, &is_metal) in local.iter().zip(&local_metal) {
        let (a, b, c) = (t[0].truncate(), t[1].truncate(), t[2].truncate());
        let area = (b - a).perp_dot(c - a);
        if area.abs() < 1e-12 {
            continue; // 真上から見て線になる面（側面）
        }
        let (tlo, thi) = (a.min(b).min(c), a.max(b).max(c));
        let i0 = (((tlo.x - g.origin.x) / cell).floor().max(0.0)) as usize;
        let j0 = (((tlo.y - g.origin.y) / cell).floor().max(0.0)) as usize;
        let i1 = (((thi.x - g.origin.x) / cell).ceil().max(0.0) as usize).min(g.nx - 1);
        let j1 = (((thi.y - g.origin.y) / cell).ceil().max(0.0) as usize).min(g.ny - 1);
        for j in j0..=j1 {
            for i in i0..=i1 {
                let k = j * g.nx + i;
                let p = g.at(k);
                let w0 = (b - p).perp_dot(c - p) / area;
                let w1 = (c - p).perp_dot(a - p) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < -1e-9 || w1 < -1e-9 || w2 < -1e-9 {
                    continue;
                }
                let u = w0 * t[0].z + w1 * t[1].z + w2 * t[2].z;
                if u <= u0 {
                    continue;
                }
                above[k] = above[k].min(u);
                if is_metal {
                    above_metal[k] = above_metal[k].min(u);
                } else if metal_only {
                    plastic_hits[k].push(u);
                }
            }
        }
    }

    let covered: Vec<bool> = plastic_hits
        .iter_mut()
        .map(|hits| {
            hits.sort_by(f64::total_cmp);
            hits.dedup_by(|a, b| (*a - *b).abs() < 1e-6); // 隣り合う三角形の辺の上の点は、両方に入る
            hits.len() % 2 == 1
        })
        .collect();
    // はんだを引き上げる接地部：本体に覆われていないところ（無ければ全部）
    let mut seeds: Vec<bool> = (0..n).map(|k| contact[k] && !covered[k]).collect();
    if !seeds.iter().any(|&b| b) {
        seeds = contact.clone();
    }
    let near_contact = nearest_seed(&seeds, &g);
    // パッドの縁までの距離は、輪郭の線分から正確に測る（はんだの外周が格子の階段にならず、縁で高さがちょうど 0 になる）
    let edge: Vec<f64> = (0..n).map(|k| distance_to_rings(&rings, g.at(k))).collect();
    // 接地部のリードの高さ（はんだが登れる高さ）：真上の金属の面（樹脂の本体に入ったリードでも、本体ではなくリードの上面）
    let wall = |q: usize| {
        let u = if metal_only { above_metal[q] } else { above[q] };
        if u.is_finite() {
            u
        } else {
            DEFAULT_WALL
        }
    };
    // リードの高さの上限はパッドごとに 1 つ（接地部の 95 パーセンタイル）。点ごとの値は、板を曲げた電極などで大きくばらつく
    let mut walls: Vec<f64> = (0..n).filter(|&k| seeds[k]).map(wall).collect();
    walls.sort_by(f64::total_cmp);
    let lead = walls.get(((walls.len() as f64 * 0.95) as usize).min(walls.len().saturating_sub(1))).copied().unwrap_or(DEFAULT_WALL);
    // 一番近い接地部までの距離（格子の点から測るので、半分の間隔を引いて境目からの距離にする）
    let mut d: Vec<f64> = (0..n).map(|k| near_contact[k].map_or(f64::INFINITY, |q| ((g.at(k) - g.at(q)).length() - 0.5 * cell).max(0.0))).collect();
    let free = |k: usize| inside[k] && !seeds[k] && near_contact[k].is_some();
    // 形にはならした距離、幅の上限にはならす前の距離（ならすと接地部の際で距離が大きくなり、狭い帯の上限が緩む）
    let d_raw = d.clone();
    smooth(&g, &mut d, free, DIST_SMOOTHING);
    // 点ごとの登れる高さ：一番近い接地部のリードの高さ（隙間を残す）
    let reach = |k: usize| near_contact[k].map_or(0.0, |q| (wall(q).min(lead) - CLEARANCE).max(0.0));

    let h: Vec<f64> = match kind {
        Kind::Smd => {
            // 表面実装：はんだは金属に引き上げられ、離れるほど下がる凹んだ形 (1 − d / L)²。金属の断面を CLIMB_LEVELS 段の高さ u_j で取り、
            // 点 k のはんだがその段まで登れるのは、その高さの金属が近いときだけ（樹脂の壁の横は登らない。チップ抵抗の側面など）：
            //   h = max_j min(u_j, H' · (1 − d_j / L)²)   d_j は段 j の金属の断面までの距離、H' は際の高さ（一番上の段は H' まで）
            // 際の高さ H' はリードの上面まで、ただし際の傾きが MAX_CONTACT_SLOPE を超えない高さまで。広がる距離 L を、はんだの体積が
            // パッドの面積 × メタルマスクの厚み × 溶けた後に残る割合 に合うように決める（樹脂の本体に覆われた分は数えない）。
            // 段の境目の段差は、まわりの点でならす。パッドの縁では丸く下がり、部品の腹・曲がったリードの下は突き抜けない
            let top = (lead - CLEARANCE).max(u0);
            let climb: Vec<[DVec3; 3]> = tris
                .iter()
                .enumerate()
                .filter(|&(i, _)| !metal_only || metal.get(i).copied().unwrap_or(true))
                .filter_map(|(_, t)| {
                    let q = t.map(|p| DVec3::new(p.x, p.y, s * p.z - pad_top));
                    let (qlo, qhi) = (q[0].min(q[1]).min(q[2]), q[0].max(q[1]).max(q[2]));
                    (qhi.y >= g.origin.y && qlo.y <= g_hi.y && qhi.z >= u0 && qlo.z <= top).then_some(q)
                })
                .collect();
            // 段ごとの、金属の断面（樹脂に覆われたところを除く）までの距離。段 0 は接地部
            let levels: Vec<f64> = (0..=CLIMB_LEVELS).map(|j| u0 + (top - u0) * j as f64 / CLIMB_LEVELS as f64).collect();
            let dists: Vec<Vec<f64>> = levels
                .iter()
                .enumerate()
                .map(|(j, &u)| {
                    let mask: Vec<bool> = if j == 0 { seeds.clone() } else { slice_inside(&climb, u, &g).iter().enumerate().map(|(k, &m)| m && !covered[k]).collect() };
                    let near = nearest_seed(&mask, &g);
                    let mut dj: Vec<f64> = (0..n).map(|k| near[k].map_or(f64::INFINITY, |q| ((g.at(k) - g.at(q)).length() - 0.5 * cell).max(0.0))).collect();
                    let finite: Vec<bool> = dj.iter().map(|d| d.is_finite()).collect();
                    smooth(&g, &mut dj, |k| free(k) && finite[k], DIST_SMOOTHING);
                    dj
                })
                .collect();
            let cap = |k: usize| (SMD_SLOPE * (d_raw[k] + edge[k])).min(SMD_EDGE_SLOPE * edge[k]).min(above[k] - CLEARANCE).max(0.0);
            let heights = |l: f64| -> Vec<f64> {
                let h_edge = top.min(0.5 * MAX_CONTACT_SLOPE * l);
                let mut h: Vec<f64> = (0..n)
                    .map(|k| {
                        if !inside[k] {
                            return 0.0;
                        }
                        if seeds[k] {
                            return reach(k).min(h_edge);
                        }
                        let mut best = 0.0f64;
                        for (j, dj) in dists.iter().enumerate() {
                            if levels[j] > h_edge || dj[k] >= l {
                                continue;
                            }
                            let r = 1.0 - dj[k] / l;
                            // 金属があると確かめた段の高さまで（一番上の段はリードの上面）
                            let ceiling = if j + 1 < levels.len() { levels[j].min(h_edge) } else { h_edge };
                            best = best.max((h_edge * r * r).min(ceiling));
                        }
                        best.min(cap(k))
                    })
                    .collect();
                smooth(&g, &mut h, free, LEVEL_SMOOTHING);
                for k in (0..n).filter(|&k| free(k)) {
                    h[k] = h[k].min(cap(k));
                }
                h
            };
            let cell_area = cell * cell;
            let target = inside.iter().filter(|&&b| b).count() as f64 * cell_area * STENCIL_MM * PASTE_METAL;
            let volume = |l: f64| heights(l).iter().enumerate().filter(|&(k, _)| free(k) && !covered[k]).map(|(_, h)| h).sum::<f64>() * cell_area;
            let l_max = 2.0 * size.length() + cell;
            let l = if volume(l_max) <= target {
                l_max
            } else {
                let (mut a, mut b) = (0.0, l_max);
                for _ in 0..30 {
                    let m = 0.5 * (a + b);
                    if volume(m) < target {
                        a = m;
                    } else {
                        b = m;
                    }
                }
                0.5 * (a + b)
            };
            heights(l)
        }
        Kind::Tht { solder_side } => {
            // スルーホール：ランドの幅に傾きを掛けた高さ（ピンの高さまで）から、ランドの縁へ下がる
            let slope = if solder_side { THT_SLOPE_SOLDER_SIDE } else { THT_SLOPE_COMPONENT_SIDE };
            (0..n)
                .map(|k| {
                    if !inside[k] || near_contact[k].is_none() {
                        return 0.0;
                    }
                    let h0 = lead.min(slope * (d_raw[k] + edge[k])).min(MAX_THT_HEIGHT);
                    if contact[k] {
                        h0.min(wall(k))
                    } else {
                        let r = edge[k] / (d_raw[k] + edge[k]).max(1e-12);
                        (h0 * r * r).min(above[k] - CLEARANCE).max(0.0)
                    }
                })
                .collect()
        }
    };

    // 高さの場を三角形にする。外周は高さ MIN_HEIGHT の等値線で切る（マーチングスクエア。パッドの外は縁からの距離で負にして、
    // 等値線がパッドの輪郭に沿うようにする）。4 隅とも接地部のセルはリードの中に隠れるので作らない
    let level: Vec<f64> = (0..n).map(|k| if inside[k] { h[k] - MIN_HEIGHT } else { -MIN_HEIGHT - edge[k] }).collect();
    let height_at = |i: i64, j: i64, k: usize| -> f64 {
        if i < 0 || j < 0 || i >= g.nx as i64 || j >= g.ny as i64 {
            h[k]
        } else {
            h[j as usize * g.nx + i as usize]
        }
    };
    // 格子の点の法線（パッドの座標系の (−hx, −hy, 1) を、ワールド（X = x, Y = s·u, Z = y）へ）
    let normal_at = |k: usize| -> DVec3 {
        let (i, j) = ((k % g.nx) as i64, (k / g.nx) as i64);
        let hx = (height_at(i + 1, j, k) - height_at(i - 1, j, k)) / (2.0 * cell);
        let hy = (height_at(i, j + 1, k) - height_at(i, j - 1, k)) / (2.0 * cell);
        DVec3::new(-hx, s, -hy).normalize()
    };
    // 頂点：格子の点は (k, k)、辺の上の点は (小さい番号, 大きい番号) で共有する
    let mut verts: HashMap<(usize, usize), u32> = HashMap::new();
    let mut vertex = |a: usize, b: usize, out: &mut Mesh| -> u32 {
        let key = (a.min(b), a.max(b));
        if let Some(&v) = verts.get(&key) {
            return v;
        }
        let t = if a == b { 0.0 } else { (level[a] / (level[a] - level[b])).clamp(0.0, 1.0) };
        let p = g.at(a).lerp(g.at(b), t);
        let hh = (h[a] + (h[b] - h[a]) * t).max(0.0);
        let normal = normal_at(a).lerp(normal_at(b), t).normalize();
        let pos = DVec3::new(p.x - center.x, s * (pad_top + hh), p.y - center.y);
        let v = out.add_vertex(pos.as_vec3(), normal.as_vec3());
        verts.insert(key, v);
        v
    };
    for j in 0..g.ny - 1 {
        for i in 0..g.nx - 1 {
            let c = [j * g.nx + i, j * g.nx + i + 1, (j + 1) * g.nx + i + 1, (j + 1) * g.nx + i];
            if c.iter().all(|&k| level[k] < 0.0) || c.iter().all(|&k| contact[k]) {
                continue;
            }
            // セルの四角を、等値線の内側（level >= 0）だけに切る
            let mut poly: Vec<u32> = Vec::with_capacity(8);
            for e in 0..4 {
                let (a, b) = (c[e], c[(e + 1) % 4]);
                if level[a] >= 0.0 {
                    poly.push(vertex(a, a, out));
                }
                if (level[a] >= 0.0) != (level[b] >= 0.0) {
                    poly.push(vertex(a, b, out));
                }
            }
            for k in 1..poly.len().saturating_sub(1) {
                let (a, b, d) = (poly[0], poly[k], poly[k + 1]);
                // 外向き（表は +Y、裏は −Y）にそろえる
                let (pa, pb, pd) = (out.positions[a as usize], out.positions[b as usize], out.positions[d as usize]);
                let cr = (pb - pa).cross(pd - pa);
                if cr.length_squared() == 0.0 {
                    continue;
                }
                if cr.y * s as f32 >= 0.0 {
                    out.add_triangle(a, b, d, material);
                } else {
                    out.add_triangle(a, d, b, material);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krbr_kicad::{Drill, Footprint, PadShape};

    const PAD_TOP: f64 = 0.788;

    fn pad(pos: DVec2, size: DVec2, kind: PadType, layers: &[&str]) -> Pad {
        Pad {
            number: "1".into(),
            kind,
            shape: if kind == PadType::ThruHole { PadShape::Circle } else { PadShape::Rect },
            pos,
            angle_deg: 0.0,
            size,
            rect_delta: DVec2::ZERO,
            round_ratio: 0.0,
            chamfer_ratio: 0.0,
            chamfer_corners: 0,
            drill: if kind == PadType::ThruHole { Drill { present: true, oval: false, size: DVec2::splat(0.8), offset: DVec2::ZERO } } else { Drill::default() },
            layers: layers.iter().map(|s| s.to_string()).collect(),
            mask_margin: None,
            custom_anchor: PadShape::Circle,
            primitives: Vec::new(),
            footprint: Some(0),
        }
    }

    /// 箱（基板座標の範囲と、ワールドの高さ Y の範囲）の三角形。外向き
    fn box_tris(lo: DVec3, hi: DVec3) -> Vec<[DVec3; 3]> {
        let c = |x: usize, y: usize, z: usize| DVec3::new([lo.x, hi.x][x], [lo.y, hi.y][y], [lo.z, hi.z][z]);
        let quads = [
            [c(0, 0, 0), c(0, 1, 0), c(1, 1, 0), c(1, 0, 0)],
            [c(0, 0, 1), c(1, 0, 1), c(1, 1, 1), c(0, 1, 1)],
            [c(0, 0, 0), c(1, 0, 0), c(1, 0, 1), c(0, 0, 1)],
            [c(0, 1, 0), c(0, 1, 1), c(1, 1, 1), c(1, 1, 0)],
            [c(0, 0, 0), c(0, 0, 1), c(0, 1, 1), c(0, 1, 0)],
            [c(1, 0, 0), c(1, 1, 0), c(1, 1, 1), c(1, 0, 1)],
        ];
        quads.iter().flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect()
    }

    /// 0603 の抵抗：パッド 0.8 × 0.95 mm を中心 ±0.8 mm に、電極（高さ 0.45 mm、長さ 0.3 mm）を ±(0.5..0.8) mm に置く
    fn chip_board(with_model: bool) -> (Board, Vec<[DVec3; 3]>) {
        let layers = ["F.Cu", "F.Paste", "F.Mask"];
        let board = Board {
            footprints: vec![Footprint { reference: "R1".into(), smd: true, ..Default::default() }],
            pads: vec![pad(DVec2::new(-0.8, 0.0), DVec2::new(0.8, 0.95), PadType::Smd, &layers), pad(DVec2::new(0.8, 0.0), DVec2::new(0.8, 0.95), PadType::Smd, &layers)],
            ..Default::default()
        };
        let z0 = PAD_TOP + 0.012; // 部品は基板の面（レジストの上面）に置く。パッドの面はその 12 µm 下
        let mut tris = Vec::new();
        if with_model {
            for sx in [-1.0f64, 1.0] {
                let (a, b) = (sx * 0.5, sx * 0.8);
                tris.extend(box_tris(DVec3::new(a.min(b), -0.4, z0), DVec3::new(a.max(b), 0.4, z0 + 0.45)));
            }
            // 本体（電極の間、少し浮いている）
            tris.extend(box_tris(DVec3::new(-0.5, -0.4, z0 + 0.05), DVec3::new(0.5, 0.4, z0 + 0.45)));
        }
        (board, tris)
    }

    fn heights_over(m: &Mesh) -> (f32, f32) {
        let ys: Vec<f32> = m.positions.iter().map(|p| p.y - PAD_TOP as f32).collect();
        (ys.iter().copied().fold(f32::MAX, f32::min), ys.iter().copied().fold(f32::MIN, f32::max))
    }

    #[test]
    fn chip_fillets_rise_against_the_terminals_and_hold_the_paste_volume() {
        let (board, tris) = chip_board(true);
        let m = footprint_fillets(&board, 0, &tris, &[], PAD_TOP, DVec2::ZERO, 7);
        assert!(m.triangle_count() > 100, "{} triangles", m.triangle_count());
        assert!(m.tri_material.iter().all(|&t| t == 7));
        let (lo, hi) = heights_over(&m);
        // パッドより下には出ない。高さは電極の上面（0.462 mm）まで
        assert!(lo >= MIN_HEIGHT as f32 - 1e-6 && hi <= 0.462 + 1e-4, "{lo}..{hi}");
        assert!(hi > 0.15, "fillet height {hi}");
        // 電極の外側（つま先）：電極の際で高く、パッドの縁へ下がる
        let at = |x: f32| m.positions.iter().filter(|p| (p.x - x).abs() < 0.02 && p.z.abs() < 0.1).map(|p| p.y - PAD_TOP as f32).fold(0.0f32, f32::max);
        assert!(at(0.85) > at(1.0) && at(1.0) > at(1.15), "{} {} {}", at(0.85), at(1.0), at(1.15));
        // 上から見た体積（つま先と横の盛り。電極の下を除く）は、ペーストから残る体積 0.8 × 0.95 × 0.12 × 0.5 = 0.0456 mm³（パッドごと）に
        // 合う（体積は格子の点で数えるので、三角形にした形とは電極の際の半セル分ずれる）
        let mut vol = 0.0f64;
        for t in m.mesh_tris() {
            let [a, b, c] = t;
            let area = ((b.x - a.x) * (c.z - a.z) - (c.x - a.x) * (b.z - a.z)).abs() as f64 / 2.0;
            let hmean = ((a.y + b.y + c.y) / 3.0 - PAD_TOP as f32) as f64;
            let under_terminal = ((a.x + b.x + c.x) / 3.0).abs() < 0.8 && ((a.z + b.z + c.z) / 3.0).abs() < 0.4;
            if !under_terminal {
                vol += area * hmean;
            }
        }
        let per_pad = vol / 2.0;
        assert!(per_pad < 0.0456 * 1.15 && per_pad > 0.0456 * 0.85, "volume per pad {per_pad}");
        // 法線は上向き（表）
        assert!(m.normals.iter().all(|n| n.y > 0.0));
        // 三角形は外向き（上から見て、法線と同じ向き）
        for [a, b, c] in m.mesh_tris() {
            assert!((b - a).cross(c - a).y >= 0.0);
        }
    }

    #[test]
    fn a_narrow_strip_beside_the_terminal_gets_a_low_fillet_not_a_wall() {
        // 電極の幅 0.8 mm に対してパッドの幅 0.95 mm：横に残るパッドは片側 0.075 mm
        let (board, tris) = chip_board(true);
        let m = footprint_fillets(&board, 0, &tris, &[], PAD_TOP, DVec2::ZERO, 0);
        let side = m.positions.iter().filter(|p| p.x.abs() > 0.55 && p.x.abs() < 0.75 && p.z.abs() > 0.4).map(|p| p.y - PAD_TOP as f32).fold(0.0f32, f32::max);
        assert!(side <= (SMD_SLOPE * 0.075) as f32 + 0.01, "beside the terminal {side}");
        // つま先（0.4 mm 残る）は高く盛る
        let toe = m.positions.iter().filter(|p| p.x.abs() > 0.8 && p.z.abs() < 0.3).map(|p| p.y - PAD_TOP as f32).fold(0.0f32, f32::max);
        assert!(toe > 0.2, "toe {toe}");
    }

    #[test]
    fn solder_never_rises_through_the_part() {
        let (board, tris) = chip_board(true);
        let m = footprint_fillets(&board, 0, &tris, &[], PAD_TOP, DVec2::ZERO, 0);
        // 本体の下（|x| < 0.5、浮いた高さ 0.05 mm + 隙間）より上には出ない
        let z_body = 0.012 + 0.05;
        for p in m.positions.iter().filter(|p| p.x.abs() < 0.48 && p.z.abs() < 0.38) {
            assert!(p.y - (PAD_TOP as f32) <= z_body as f32, "{p:?}");
        }
    }

    /// フラットリードの部品：樹脂の本体の底もリードと同じ高さで基板に接し、パッドの内側に掛かっている。
    /// はんだは金属のリードにだけ付き、本体を電極とみなして登ったり、本体の下に入ったりしない
    #[test]
    fn a_plastic_body_on_the_board_is_not_a_terminal() {
        let (board, _) = chip_board(false);
        let z0 = PAD_TOP + 0.012;
        let mut tris = Vec::new();
        let mut metal = Vec::new();
        for sx in [-1.0f64, 1.0] {
            // リード：本体から出た厚さ 0.1 mm の板（パッドの中ほどまで）
            let (a, b) = (sx * 0.55, sx * 0.95);
            let lead = box_tris(DVec3::new(a.min(b), -0.15, z0), DVec3::new(a.max(b), 0.15, z0 + 0.1));
            metal.extend(std::iter::repeat_n(true, lead.len()));
            tris.extend(lead);
        }
        // 本体：基板に接し、パッドの内側の縁（±0.4 mm）から ±0.6 mm まで掛かる
        let body = box_tris(DVec3::new(-0.6, -0.4, z0), DVec3::new(0.6, 0.4, z0 + 0.7));
        metal.extend(std::iter::repeat_n(false, body.len()));
        tris.extend(body);
        let m = footprint_fillets(&board, 0, &tris, &metal, PAD_TOP, DVec2::ZERO, 0);
        assert!(m.triangle_count() > 100, "{} triangles", m.triangle_count());
        let (_, hi) = heights_over(&m);
        assert!(hi <= 0.112 + 1e-4, "solder climbs above the leads: {hi}");
        // 本体の下は、高さを周りから続けて本体の中に隠す（本体の縁で崖にしない）。本体より高くはならない（上の hi）
        // 金属の印が無ければ、今までどおり（本体の底も接地部とみなすので、本体を登る）
        let (_, hi_all) = heights_over(&footprint_fillets(&board, 0, &tris, &[], PAD_TOP, DVec2::ZERO, 0));
        assert!(hi_all > 0.2, "{hi_all}");
    }

    /// はんだが登るのは金属の壁だけ：チップ抵抗（金属は端面と底の電極だけ、側面は樹脂の本体）の側面には登らず、
    /// セラミックコンデンサ（電極が側面まで覆う）の側面には登る
    #[test]
    fn solder_climbs_only_metal_walls() {
        // 電極の横に 0.25 mm のパッドが残る（狭い帯の上限で抑えられないように）
        let layers = ["F.Cu", "F.Paste", "F.Mask"];
        let board = Board {
            footprints: vec![Footprint { reference: "R1".into(), smd: true, ..Default::default() }],
            pads: vec![pad(DVec2::new(-0.8, 0.0), DVec2::new(0.8, 1.3), PadType::Smd, &layers), pad(DVec2::new(0.8, 0.0), DVec2::new(0.8, 1.3), PadType::Smd, &layers)],
            ..Default::default()
        };
        let z0 = PAD_TOP + 0.012;
        let side_height = |resistor: bool| {
            let mut tris = Vec::new();
            let mut metal = Vec::new();
            let mut add = |t: Vec<[DVec3; 3]>, m: bool, tris: &mut Vec<[DVec3; 3]>| {
                metal.extend(std::iter::repeat_n(m, t.len()));
                tris.extend(t);
            };
            for sx in [-1.0f64, 1.0] {
                let span = |a: f64, b: f64| (sx * a).min(sx * b)..(sx * a).max(sx * b);
                if resistor {
                    // 底の電極（薄い）と端面の電極
                    let (x0, x1) = (span(0.5, 0.75).start, span(0.5, 0.75).end);
                    add(box_tris(DVec3::new(x0, -0.4, z0), DVec3::new(x1, 0.4, z0 + 0.04)), true, &mut tris);
                    let (x0, x1) = (span(0.75, 0.8).start, span(0.75, 0.8).end);
                    add(box_tris(DVec3::new(x0, -0.4, z0), DVec3::new(x1, 0.4, z0 + 0.45)), true, &mut tris);
                } else {
                    let (x0, x1) = (span(0.5, 0.8).start, span(0.5, 0.8).end);
                    add(box_tris(DVec3::new(x0, -0.4, z0), DVec3::new(x1, 0.4, z0 + 0.45)), true, &mut tris);
                }
            }
            // 樹脂の本体（抵抗は電極の上まで、コンデンサは電極の間）
            let body = if resistor { box_tris(DVec3::new(-0.75, -0.4, z0 + 0.04), DVec3::new(0.75, 0.4, z0 + 0.45)) } else { box_tris(DVec3::new(-0.5, -0.4, z0 + 0.05), DVec3::new(0.5, 0.4, z0 + 0.45)) };
            add(body, false, &mut tris);
            let m = footprint_fillets(&board, 0, &tris, &metal, PAD_TOP, DVec2::ZERO, 0);
            // 電極の横の帯（端面から離れたところ）
            m.positions.iter().filter(|p| p.x.abs() > 0.5 && p.x.abs() < 0.65 && p.z.abs() > 0.41).map(|p| p.y - PAD_TOP as f32).fold(0.0f32, f32::max)
        };
        let (r, c) = (side_height(true), side_height(false));
        // 抵抗の側面にも、端面のはんだが角を回り込む分はある（調べるのは端面から 0.15〜0.3 mm）
        assert!(c > 0.15 && r < 0.75 * c, "beside the terminal: resistor {r}, capacitor {c}");
    }

    #[test]
    fn no_fillet_without_a_model_or_paste_or_contact() {
        let (board, _) = chip_board(false);
        let (_, tris) = chip_board(true);
        assert_eq!(footprint_fillets(&board, 0, &[], &[], PAD_TOP, DVec2::ZERO, 0).triangle_count(), 0);
        // ペーストの無いパッド（テストポイントなど）
        let mut no_paste = board.clone();
        for p in &mut no_paste.pads {
            p.layers.retain(|l| l != "F.Paste");
        }
        assert_eq!(footprint_fillets(&no_paste, 0, &tris, &[], PAD_TOP, DVec2::ZERO, 0).triangle_count(), 0);
        // 浮いた部品（パッドから 0.5 mm 上）には付かない
        let lifted: Vec<[DVec3; 3]> = tris.iter().map(|t| t.map(|p| p + DVec3::new(0.0, 0.0, 0.5))).collect();
        assert_eq!(footprint_fillets(&board, 0, &lifted, &[], PAD_TOP, DVec2::ZERO, 0).triangle_count(), 0);
    }

    #[test]
    fn back_side_parts_get_fillets_below_the_board() {
        let (mut board, tris) = chip_board(true);
        board.footprints[0].back = true;
        for p in &mut board.pads {
            p.layers = vec!["B.Cu".into(), "B.Paste".into(), "B.Mask".into()];
        }
        let mirrored: Vec<[DVec3; 3]> = tris.iter().map(|t| t.map(|p| DVec3::new(p.x, p.y, -p.z))).collect();
        let m = footprint_fillets(&board, 0, &mirrored, &[], PAD_TOP, DVec2::ZERO, 0);
        assert!(m.triangle_count() > 100);
        assert!(m.positions.iter().all(|p| p.y <= -(PAD_TOP as f32)));
        assert!(m.normals.iter().all(|n| n.y < 0.0));
        for [a, b, c] in m.mesh_tris() {
            assert!((b - a).cross(c - a).y <= 0.0);
        }
    }

    #[test]
    fn through_hole_pins_get_a_cone_on_the_solder_side() {
        // 部品面は表。ピン（0.6 mm 角）が基板を貫いて、裏へ 2 mm 出ている。ランドは直径 1.7 mm
        let board = Board {
            footprints: vec![Footprint { reference: "J1".into(), through_hole: true, ..Default::default() }],
            pads: vec![pad(DVec2::ZERO, DVec2::splat(1.7), PadType::ThruHole, &["*.Cu", "*.Mask"])],
            ..Default::default()
        };
        let tris = box_tris(DVec3::new(-0.3, -0.3, -PAD_TOP - 2.0), DVec3::new(0.3, 0.3, PAD_TOP + 3.0));
        let m = footprint_fillets(&board, 0, &tris, &[], PAD_TOP, DVec2::ZERO, 0);
        let bottom: Vec<_> = m.positions.iter().filter(|p| p.y < 0.0).collect();
        let top: Vec<_> = m.positions.iter().filter(|p| p.y > 0.0).collect();
        assert!(!bottom.is_empty() && !top.is_empty(), "{} below, {} above", bottom.len(), top.len());
        // はんだ面（裏）の方が高く盛る。ピンの際で高く、ランドの縁で低い（円すい）
        let height = |ps: &[&glam::Vec3], r0: f32, r1: f32| ps.iter().filter(|p| (p.x * p.x + p.z * p.z).sqrt() >= r0 && (p.x * p.x + p.z * p.z).sqrt() < r1).map(|p| p.y.abs() - PAD_TOP as f32).fold(0.0f32, f32::max);
        let (near, far) = (height(&bottom, 0.3, 0.45), height(&bottom, 0.7, 0.8));
        assert!(near > 0.3 && far < 0.5 * near, "solder side: near the pin {near}, at the rim {far}");
        assert!(height(&top, 0.3, 0.45) < near, "the component side is lower");
    }

    trait MeshTris {
        fn mesh_tris(&self) -> Vec<[glam::Vec3; 3]>;
    }
    impl MeshTris for Mesh {
        fn mesh_tris(&self) -> Vec<[glam::Vec3; 3]> {
            self.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]].map(|i| self.positions[i as usize])).collect()
        }
    }
}
