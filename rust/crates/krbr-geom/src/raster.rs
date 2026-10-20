//! 層ごとの多角形の集め方（銅・レジスト開口・シルク）、被覆率のラスタライズ、縁からの距離場

use clipper2::FillRule;
use krbr_kicad::{Board, Box2, Graphic, GrKind, Pad, PadType, Via};

use crate::shapes::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Front = 0,
    Back = 1,
}

#[derive(Debug, Clone)]
pub struct LayerArtOptions {
    /// 20 µm/px
    pub px_per_mm: f64,
    pub max_dim: usize,
    pub margin_mm: f64,
    pub clip_silk_by_mask: bool,
    /// kicad-cli から得たシルク（線フォントの文字など）。設定ごと複製されるので共有にしてある
    pub extra_silk: [Option<std::sync::Arc<Paths>>; 2],
}

impl Default for LayerArtOptions {
    fn default() -> Self {
        Self { px_per_mm: 50.0, max_dim: 8192, margin_mm: 0.5, clip_silk_by_mask: true, extra_silk: [None, None] }
    }
}

/// 面ごとの銅箔・レジスト開口・シルクの多角形（和をとった後）
#[derive(Debug, Clone, Default)]
pub struct SidePaths {
    pub copper: Paths,
    pub mask_open: Paths,
    pub silk: Paths,
}

fn pad_has_copper(p: &Pad) -> bool {
    // メッキなし穴は、パッドが穴より大きいときだけ銅箔を持つ（KiCad と同じ扱い）
    if p.kind != PadType::NpThruHole || !p.drill.present {
        return true;
    }
    p.size.x > p.drill.size.x + 1e-6 || p.size.y > p.drill.size.y + 1e-6
}

fn via_on_side(v: &Via, cu: &str) -> bool {
    v.layer_from == cu || v.layer_to == cu
}

pub fn graphic_box(g: &Graphic) -> Box2 {
    let mut b = Box2::default();
    let w = g.width / 2.0;
    if matches!(g.kind, GrKind::Line | GrKind::Arc) {
        b.add_radius(g.start, w);
        b.add_radius(g.end, w);
    }
    if g.kind == GrKind::Arc {
        b.add_radius(g.mid, w);
    }
    if g.kind == GrKind::Circle {
        b.add_radius(g.start, (g.end - g.start).length() + w);
    }
    for &p in &g.pts {
        b.add_radius(p, w);
    }
    b
}

fn path_box(p: &Path) -> Box2 {
    let mut b = Box2::default();
    for q in path_points(p) {
        b.add(q);
    }
    b
}

/// clip を渡すと、その矩形にかかる項目だけを集め、結果も矩形で切り抜く（タイル単位の作り直し用）
pub fn collect_side_paths(board: &Board, side: Side, opts: &LayerArtOptions, clip: Option<&Box2>) -> SidePaths {
    let (cu, mask, silk) = match side {
        Side::Front => ("F.Cu", "F.Mask", "F.SilkS"),
        Side::Back => ("B.Cu", "B.Mask", "B.SilkS"),
    };
    let tent_default = if side == Side::Front { board.setup.tent_front } else { board.setup.tent_back };
    let mut sp = SidePaths::default();
    // clip があれば、その範囲（少し広げた範囲）にかかる項目だけを集める
    let area = clip.map(|c| c.expanded(1.0));
    let near = |b: &Box2| area.map(|a| a.intersects(b)).unwrap_or(true);
    let near_point = |p: Vec2, r: f64| {
        let mut b = Box2::default();
        b.add_radius(p, r);
        near(&b)
    };

    for t in &board.tracks {
        if t.layer != cu {
            continue;
        }
        let mut b = Box2::default();
        b.add_radius(t.start, t.width / 2.0);
        b.add_radius(t.end, t.width / 2.0);
        if let Some(m) = t.mid {
            b.add_radius(m, t.width / 2.0);
        }
        if near(&b) {
            append(&mut sp.copper, track_to_paths(t));
        }
    }
    for p in &board.pads {
        if !near_point(p.pos, p.size.x.max(p.size.y) + 0.5) {
            continue;
        }
        if p.on_layer(cu) && pad_has_copper(p) {
            append(&mut sp.copper, pad_to_paths(p, 0.0));
        }
        if p.on_layer(mask) {
            append(&mut sp.mask_open, pad_to_paths(p, p.mask_margin.unwrap_or(board.setup.pad_to_mask_clearance)));
        }
    }
    for v in &board.vias {
        if !via_on_side(v, cu) || !near_point(v.pos, v.size) {
            continue;
        }
        append(&mut sp.copper, disc(v.pos, v.size / 2.0));
        let tent = if side == Side::Front { v.tent_front } else { v.tent_back };
        if !tent.unwrap_or(tent_default) {
            append(&mut sp.mask_open, disc(v.pos, v.size / 2.0));
        }
    }
    for z in &board.zones {
        if z.layer != cu && z.layer != mask {
            continue;
        }
        if clip.is_some() {
            let mut b = Box2::default();
            for &p in &z.pts {
                b.add(p);
            }
            if !near(&b) {
                continue;
            }
        }
        if z.layer == cu {
            sp.copper.push(to_path(&z.pts));
        } else {
            sp.mask_open.push(to_path(&z.pts));
        }
    }
    for g in &board.graphics {
        if g.layer != cu && g.layer != mask && g.layer != silk {
            continue;
        }
        if clip.is_some() && !near(&graphic_box(g)) {
            continue;
        }
        let paths = graphic_to_paths(g);
        if g.layer == cu {
            append(&mut sp.copper, paths);
        } else if g.layer == mask {
            append(&mut sp.mask_open, paths);
        } else {
            append(&mut sp.silk, paths);
        }
    }
    if let Some(extra) = &opts.extra_silk[side as usize] {
        for p in extra.iter() {
            if clip.is_none() || near(&path_box(p)) {
                sp.silk.push(p.clone());
            }
        }
    }

    sp.copper = unite(&sp.copper, FillRule::NonZero);
    sp.mask_open = unite(&sp.mask_open, FillRule::NonZero);
    sp.silk = unite(&sp.silk, FillRule::NonZero);
    if opts.clip_silk_by_mask {
        sp.silk = subtract(&sp.silk, &sp.mask_open);
    }
    if let Some(c) = clip {
        let rect = rect_path(c.lo, c.hi);
        sp.copper = intersect(&sp.copper, &rect);
        sp.mask_open = intersect(&sp.mask_open, &rect);
        sp.silk = intersect(&sp.silk, &rect);
    }
    sp
}

// ---------------------------------------------------------------- ラスタライズ

/// 基板座標の矩形を画素に対応づける。画素 (0,0) の左上角が origin、y は下向き（基板座標と同じ）
#[derive(Debug, Clone, Copy)]
pub struct Grid {
    pub origin: Vec2,
    pub px_per_mm: f64,
    pub width: usize,
    pub height: usize,
}

impl Default for Grid {
    fn default() -> Self {
        Self { origin: Vec2::ZERO, px_per_mm: 50.0, width: 0, height: 0 }
    }
}

const SUB: usize = 4;

#[derive(Clone, Copy)]
struct Edge {
    y0: f64,
    y1: f64, // y0 < y1
    x0: f64, // y0 での x
    dxdy: f64,
    dir: i32,
}

fn fill_span(acc: &mut [f32], width: usize, xa: f64, xb: f64, w: f32) {
    let xa = xa.clamp(0.0, width as f64);
    let xb = xb.clamp(0.0, width as f64);
    if xb <= xa {
        return;
    }
    let (ia, ib) = (xa as usize, xb as usize);
    if ia == ib {
        acc[ia] += (xb - xa) as f32 * w;
        return;
    }
    acc[ia] += (ia as f64 + 1.0 - xa) as f32 * w;
    for a in acc.iter_mut().take(ib).skip(ia + 1) {
        *a += w;
    }
    if ib < width {
        acc[ib] += (xb - ib as f64) as f32 * w;
    }
}

fn raster_band(edges: &[Edge], width: usize, row_begin: usize, row_end: usize, cov: &mut [f32]) {
    let mut active: Vec<&Edge> = Vec::new();
    // 帯の開始より前から続いている辺も含めて、帯に関係する辺を y0 順に処理する
    let mut next = 0;
    let band_top = row_begin as f64;
    while next < edges.len() && edges[next].y0 <= band_top {
        if edges[next].y1 > band_top {
            active.push(&edges[next]);
        }
        next += 1;
    }
    let mut acc = vec![0.0f32; width + 1];
    let mut xs: Vec<(f64, i32)> = Vec::new();
    let w = 1.0 / SUB as f32;
    for row in row_begin..row_end {
        acc.iter_mut().for_each(|a| *a = 0.0);
        for s in 0..SUB {
            let y = row as f64 + (s as f64 + 0.5) / SUB as f64;
            while next < edges.len() && edges[next].y0 <= y {
                active.push(&edges[next]);
                next += 1;
            }
            xs.clear();
            active.retain(|e| e.y1 > y);
            for e in &active {
                if e.y0 <= y {
                    xs.push((e.x0 + (y - e.y0) * e.dxdy, e.dir));
                }
            }
            if xs.is_empty() {
                continue;
            }
            xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let mut winding = 0;
            for i in 0..xs.len().saturating_sub(1) {
                winding += xs[i].1;
                if winding != 0 {
                    fill_span(&mut acc, width, xs[i].0, xs[i + 1].0, w);
                }
            }
        }
        let out = &mut cov[row * width..(row + 1) * width];
        for (o, a) in out.iter_mut().zip(acc.iter()) {
            *o = a.min(1.0);
        }
    }
}

/// 非ゼロ規則で塗った多角形の被覆率（0..1）。縦は 4 本のサブ走査線、横は厳密な被覆面積でアンチエイリアスする
pub fn rasterize_coverage(paths: &Paths, g: &Grid) -> Vec<f32> {
    let mut cov = vec![0.0f32; g.width * g.height];
    let mut edges: Vec<Edge> = Vec::new();
    for path in paths.iter() {
        let pts = from_path(path);
        let n = pts.len();
        if n < 3 {
            continue;
        }
        for i in 0..n {
            let (a, b) = (pts[i], pts[(i + 1) % n]);
            let (ax, ay) = ((a.x - g.origin.x) * g.px_per_mm, (a.y - g.origin.y) * g.px_per_mm);
            let (bx, by) = ((b.x - g.origin.x) * g.px_per_mm, (b.y - g.origin.y) * g.px_per_mm);
            if ay == by {
                continue;
            }
            let e = if ay < by {
                Edge { y0: ay, y1: by, x0: ax, dxdy: (bx - ax) / (by - ay), dir: 1 }
            } else {
                Edge { y0: by, y1: ay, x0: bx, dxdy: (ax - bx) / (ay - by), dir: -1 }
            };
            if e.y1 <= 0.0 || e.y0 >= g.height as f64 {
                continue;
            }
            edges.push(e);
        }
    }
    if edges.is_empty() {
        return cov;
    }
    edges.sort_by(|a, b| a.y0.partial_cmp(&b.y0).unwrap());

    let threads = (std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2) / 2).clamp(1, 8);
    let band = g.height.div_ceil(threads);
    let width = g.width;
    std::thread::scope(|s| {
        for (t, chunk) in cov.chunks_mut(band * width).enumerate() {
            let edges = &edges;
            let r0 = t * band;
            let r1 = r0 + chunk.len() / width;
            s.spawn(move || {
                // 帯の中の行は chunk の先頭から並ぶので、行番号を引き直す
                let mut local = vec![0.0f32; chunk.len()];
                raster_band_offset(edges, width, r0, r1, &mut local);
                chunk.copy_from_slice(&local);
            });
        }
    });
    cov
}

fn raster_band_offset(edges: &[Edge], width: usize, r0: usize, r1: usize, out: &mut [f32]) {
    // raster_band は行番号で cov を引くので、帯の先頭が 0 になるよう辺の y をずらす
    let shifted: Vec<Edge> = edges.iter().map(|e| Edge { y0: e.y0 - r0 as f64, y1: e.y1 - r0 as f64, ..*e }).collect();
    raster_band(&shifted, width, 0, r1 - r0, out);
}

// ---------------------------------------------------------------- 距離場

/// これより遠い距離は同じ値にまとめる（mm）
pub const SDF_RANGE_MM: f64 = 0.3;

const INF: f64 = 1e20;

/// 1 次元の距離変換（放物線の下側包絡線）。f は 0（対象）か INF
fn dt1d(f: &[f64], n: usize, d: &mut [f64], v: &mut [usize], z: &mut [f64]) {
    let mut k = 0usize;
    v[0] = 0;
    z[0] = -INF;
    z[1] = INF;
    let inter = |q: usize, p: usize| ((f[q] + (q * q) as f64) - (f[p] + (p * p) as f64)) / (2.0 * (q as f64 - p as f64));
    for q in 1..n {
        let mut s = inter(q, v[k]);
        while s <= z[k] && k > 0 {
            k -= 1;
            s = inter(q, v[k]);
        }
        if s <= z[k] {
            // k == 0 で、最初の放物線を置き換える
            v[0] = q;
            continue;
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = INF;
    }
    k = 0;
    for q in 0..n {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let dq = q as f64 - v[k] as f64;
        d[q] = dq * dq + f[v[k]];
    }
}

/// target の画素までの距離の 2 乗
fn squared_distance(mask: &[u8], target: u8, w: usize, h: usize) -> Vec<f32> {
    let mut g: Vec<f64> = mask.iter().map(|&m| if m == target { 0.0 } else { INF }).collect();
    let n = w.max(h);
    let (mut f, mut d, mut z, mut v) = (vec![0.0; n], vec![0.0; n], vec![0.0; n + 1], vec![0usize; n]);
    for x in 0..w {
        for y in 0..h {
            f[y] = g[y * w + x];
        }
        dt1d(&f, h, &mut d, &mut v, &mut z);
        for y in 0..h {
            g[y * w + x] = d[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&g[y * w..(y + 1) * w]);
        dt1d(&f, w, &mut d, &mut v, &mut z);
        g[y * w..(y + 1) * w].copy_from_slice(&d[..w]);
    }
    g.iter().map(|&x| x.min(1e12) as f32).collect()
}

/// 2 値の画像（inside）から、各画素の中心と反対側の画素の中心との最短距離（画素単位）。符号付き（外側が正）
pub fn signed_distance_px(inside: &[u8], w: usize, h: usize) -> Vec<f32> {
    let to_inside = squared_distance(inside, 1, w, h);
    let to_outside = squared_distance(inside, 0, w, h);
    (0..w * h)
        .map(|i| {
            // 縁は隣り合う画素の中心の中間にあるので 0.5 画素ずらす
            if inside[i] != 0 {
                -(to_outside[i].min(1e12).sqrt() - 0.5)
            } else {
                to_inside[i].min(1e12).sqrt() - 0.5
            }
        })
        .collect()
}

/// 被覆率（0..1）から符号付き距離を求め、8bit に詰める（128 が縁、+ が外側、1 単位 = SDF_RANGE_MM / 127 mm）
pub fn encode_sdf(coverage: &[f32], w: usize, h: usize, px_per_mm: f64) -> Vec<u8> {
    let inside: Vec<u8> = coverage.iter().map(|&c| if c >= 0.5 { 1 } else { 0 }).collect();
    let sd = signed_distance_px(&inside, w, h);
    let scale = (127.0 / (SDF_RANGE_MM * px_per_mm)) as f32;
    sd.iter()
        .zip(coverage)
        .map(|(&s, &c)| {
            // 縁の画素は被覆率で位置を細かく補う（2 値化で失った画素以下の位置）
            let s = if s.abs() < 1.0 { 0.5 - c } else { s };
            (128.0 + s * scale).round().clamp(0.0, 255.0) as u8
        })
        .collect()
}

pub fn decode_sdf_mm(v: u8) -> f32 {
    (v as f32 - 128.0) / 127.0 * SDF_RANGE_MM as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_of_a_square() {
        let g = Grid { origin: Vec2::ZERO, px_per_mm: 10.0, width: 20, height: 20 };
        let sq = rect_path(Vec2::new(0.5, 0.5), Vec2::new(1.5, 1.5)); // 10x10 px
        let cov = rasterize_coverage(&sq, &g);
        let sum: f32 = cov.iter().sum();
        assert!((sum - 100.0).abs() < 0.5, "{sum}");
        assert_eq!(cov[10 * 20 + 10], 1.0);
        assert_eq!(cov[0], 0.0);
        // 半画素ずらすと縁が 0.5 になる
        let sq2 = rect_path(Vec2::new(0.55, 0.5), Vec2::new(1.55, 1.5));
        let cov2 = rasterize_coverage(&sq2, &g);
        assert!((cov2[10 * 20 + 5] - 0.5).abs() < 1e-3);
    }

    #[test]
    fn sdf_distance_from_edge() {
        let (w, h) = (16usize, 16usize);
        let mut inside = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..8 {
                inside[y * w + x] = 1;
            }
        }
        let sd = signed_distance_px(&inside, w, h);
        // 縁（x = 7.5）からの距離：x = 7 は -0.5、x = 8 は +0.5、x = 11 は +3.5
        assert!((sd[7] + 0.5).abs() < 1e-5 && (sd[8] - 0.5).abs() < 1e-5 && (sd[11] - 3.5).abs() < 1e-5);
        let cov: Vec<f32> = inside.iter().map(|&i| i as f32).collect();
        let enc = encode_sdf(&cov, w, h, 50.0);
        assert!(enc[0] < 128 && enc[15] > 128);
        assert!((decode_sdf_mm(enc[12]) - 4.5 / 50.0).abs() < 0.003);
    }

    #[test]
    fn sdf_of_a_single_pixel_and_clamping() {
        // 中心 (32,32) の 1 画素だけが内側：外側の距離は中心からの距離 − 0.5
        let n = 64usize;
        let mut dot = vec![0u8; n * n];
        dot[32 * n + 32] = 1;
        let sd = signed_distance_px(&dot, n, n);
        assert!((sd[32 * n + 32 + 7] - 6.5).abs() < 1e-4);
        assert!((sd[(32 + 3) * n + 32 + 4] - 4.5).abs() < 1e-4); // 3-4-5
        // 範囲（0.3 mm）より遠い所は上限に張り付く
        let (w, h) = (200usize, 4usize);
        let cov: Vec<f32> = (0..w * h).map(|i| if i % w < 100 { 1.0 } else { 0.0 }).collect();
        let enc = encode_sdf(&cov, w, h, 50.0);
        let range = SDF_RANGE_MM as f32;
        assert!((decode_sdf_mm(enc[w + 105]) - 0.11).abs() < 0.11 * 0.05);
        assert!((decode_sdf_mm(enc[w + 94]) + 0.11).abs() < 0.11 * 0.05);
        assert!((decode_sdf_mm(enc[w + 190]) - range).abs() < range * 0.01);
        assert!((decode_sdf_mm(enc[w + 2]) + range).abs() < range * 0.01);
    }
}
