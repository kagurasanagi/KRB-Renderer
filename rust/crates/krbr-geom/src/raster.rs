//! 層ごとの多角形の集め方（銅・レジスト開口・シルク）、被覆率のラスタライズ、縁からの距離場

use clipper2::FillRule;
use krbr_kicad::{Board, Box2, Pad, PadType};
/// 基板の面（krbr-kicad に移した。ここからも使えるように残す）
pub use krbr_kicad::Side;

use crate::shapes::*;

#[derive(Debug, Clone)]
pub struct LayerArtOptions {
    /// 20 µm/px
    pub px_per_mm: f64,
    pub clip_silk_by_mask: bool,
    /// kicad-cli から得た面ごとの線フォントの文字・文字の枠（銅箔・マスクの開口・シルク）。設定ごと複製されるので共有にしてある
    pub stroke_text: [Option<std::sync::Arc<SidePaths>>; 2],
}

impl Default for LayerArtOptions {
    fn default() -> Self {
        Self { px_per_mm: 50.0, clip_silk_by_mask: true, stroke_text: [None, None] }
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

fn path_box(p: &Path) -> Box2 {
    let mut b = Box2::default();
    for q in path_points(p) {
        b.add(q);
    }
    b
}

/// clip を渡すと、その矩形にかかる項目だけを集め、結果も矩形で切り抜く（タイル単位の作り直し用）
pub fn collect_side_paths(board: &Board, side: Side, opts: &LayerArtOptions, clip: Option<&Box2>) -> SidePaths {
    let (cu, mask, silk) = (side.copper_layer(), side.mask_layer(), side.silk_layer());
    let mut sp = SidePaths::default();
    // clip があれば、その範囲にかかる項目だけを集める。項目の範囲（Graphic::bbox、Pad::extent など）は線幅・レジストの広がり・
    // カスタムパッドのプリミティブまで含めた外接矩形なので、広げずに判定してよい（差分の検出も同じ範囲を使う）
    let near = |b: &Box2| clip.map(|c| c.intersects(b)).unwrap_or(true);

    for t in &board.tracks {
        if t.layer == cu && near(&t.bbox()) {
            append(&mut sp.copper, track_to_paths(t));
        }
    }
    for p in &board.pads {
        if !near(&p.extent(&board.setup)) {
            continue;
        }
        if p.on_layer(cu) && pad_has_copper(p) {
            append(&mut sp.copper, pad_to_paths(p, 0.0));
        }
        if p.on_layer(mask) {
            append(&mut sp.mask_open, pad_to_paths(p, p.mask_expansion(&board.setup)));
        }
    }
    for v in &board.vias {
        if !v.reaches(side) || !near(&v.bbox()) {
            continue;
        }
        append(&mut sp.copper, disc(v.pos, v.size / 2.0));
        if !v.tented(side, &board.setup) {
            // KiCad 7 以降、ビアの開口も基板のレジストの広がり（pad_to_mask_clearance）だけ広げる
            let r = v.size / 2.0 + board.setup.pad_to_mask_clearance;
            if r > 0.0 {
                append(&mut sp.mask_open, disc(v.pos, r));
            }
        }
    }
    for z in &board.zones {
        if z.layer != cu && z.layer != mask {
            continue;
        }
        if clip.is_some() && !near(&z.bbox()) {
            continue;
        }
        // 向きをそろえ（outer_path）、範囲があれば先に切る（大きなベタを丸ごと和に入れると、タイルごとの和と切り抜きが重い）
        let pts = match clip {
            Some(c) => clip_to_box(&z.pts, c.lo, c.hi),
            None => z.pts.clone(),
        };
        if pts.len() < 3 {
            continue;
        }
        if z.layer == cu {
            sp.copper.push(outer_path(&pts));
        } else {
            sp.mask_open.push(outer_path(&pts));
        }
    }
    for g in &board.graphics {
        if g.layer != cu && g.layer != mask && g.layer != silk {
            continue;
        }
        if clip.is_some() && !near(&g.bbox()) {
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
    if let Some(extra) = &opts.stroke_text[side as usize] {
        for (to, from) in [(&mut sp.copper, &extra.copper), (&mut sp.mask_open, &extra.mask_open), (&mut sp.silk, &extra.silk)] {
            for p in from.iter() {
                if clip.is_none() || near(&path_box(p)) {
                    to.push(p.clone());
                }
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

/// 行 row_begin..row_end を描く。cov はその帯だけの画素（先頭が row_begin の行）
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
        let r = row - row_begin;
        let out = &mut cov[r * width..(r + 1) * width];
        for (o, a) in out.iter_mut().zip(acc.iter()) {
            *o = a.min(1.0);
        }
    }
}

/// 非ゼロ規則で塗った多角形の被覆率（0..1）。縦は 4 本のサブ走査線、横は厳密な被覆面積でアンチエイリアスする。
/// 横の帯に分けて krbr_core::platform::worker_threads() 本で並列に描く
pub fn rasterize_coverage(paths: &Paths, g: &Grid) -> Vec<f32> {
    rasterize_coverage_with(paths, g, krbr_core::platform::worker_threads())
}

/// rasterize_coverage を threads 本で描く。1 なら呼んだスレッドだけで描く（並列の処理の中から呼ぶときは 1 にして、
/// スレッドの入れ子を作らない）
pub fn rasterize_coverage_with(paths: &Paths, g: &Grid, threads: usize) -> Vec<f32> {
    let mut cov = vec![0.0f32; g.width * g.height];
    if cov.is_empty() {
        return cov;
    }
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

    let width = g.width;
    let threads = threads.clamp(1, g.height.max(1));
    if threads == 1 {
        raster_band(&edges, width, 0, g.height, &mut cov);
        return cov;
    }
    let band = g.height.div_ceil(threads);
    std::thread::scope(|s| {
        for (t, chunk) in cov.chunks_mut(band * width).enumerate() {
            let edges = &edges;
            let r0 = t * band;
            let r1 = r0 + chunk.len() / width;
            s.spawn(move || {
                krbr_core::platform::lower_thread_priority();
                raster_band(edges, width, r0, r1, chunk);
            });
        }
    });
    cov
}

// ---------------------------------------------------------------- 距離場

/// これより遠い距離は同じ値にまとめる（mm）
pub const SDF_RANGE_MM: f64 = 0.3;

/// 2 値の画像（inside）から、各画素の中心と反対側の画素の中心との最短距離（画素単位）。符号付き（外側が正）
pub fn signed_distance_px(inside: &[u8], w: usize, h: usize) -> Vec<f32> {
    signed_distance_px_capped(inside, w, h, w + h)
}

/// signed_distance_px の、cap 画素未満だけを正確に求めるもの（それより遠い所の絶対値は cap - 0.5 以上）。
/// 縦は、列ごとに反対側の最寄りの画素までの距離を上下 2 回の走査で求め（行を順に読むのでキャッシュに乗る）、
/// 横は ±cap の窓の中で dx² + dy² の最小を探す。cap 未満の距離を決める画素は必ず窓の中にあるので、そこは正確
pub fn signed_distance_px_capped(inside: &[u8], w: usize, h: usize, cap: usize) -> Vec<f32> {
    let cap = cap.max(1).min(u16::MAX as usize - 1) as u16;
    // 画素ごとに、縦に反対側（内側の画素なら外側、外側なら内側）の画素までの距離（cap で打ち切り）
    let mut dy = vec![cap; w * h];
    let (mut to_in, mut to_out) = (vec![cap; w], vec![cap; w]);
    let step = |r: u16| r.saturating_add(1).min(cap);
    for pass in 0..2 {
        to_in.fill(cap);
        to_out.fill(cap);
        for k in 0..h {
            let y = if pass == 0 { k } else { h - 1 - k };
            let row = y * w;
            for x in 0..w {
                let ins = inside[row + x] != 0;
                to_in[x] = if ins { 0 } else { step(to_in[x]) };
                to_out[x] = if ins { step(to_out[x]) } else { 0 };
                let e = if ins { to_out[x] } else { to_in[x] };
                if e < dy[row + x] {
                    dy[row + x] = e;
                }
            }
        }
    }
    let cap_sq = cap as u32 * cap as u32;
    let mut out = vec![0.0f32; w * h];
    // 行の中で、窓に効く列（縦の距離が cap 未満か、横の隣と側が変わる列）までの最短の横の距離。
    // cap 以上離れていれば、窓の中はすべて同じ側で縦の距離も cap なので、答えは cap²（縁から遠い大部分の画素）
    let mut near = vec![cap; w];
    for y in 0..h {
        let row = y * w;
        let useful = |x: usize| dy[row + x] < cap || (x > 0 && inside[row + x - 1] != inside[row + x]) || (x + 1 < w && inside[row + x + 1] != inside[row + x]);
        let mut run = cap;
        for x in 0..w {
            run = if useful(x) { 0 } else { step(run) };
            near[x] = run;
        }
        run = cap;
        for x in (0..w).rev() {
            run = if near[x] == 0 { 0 } else { step(run) };
            near[x] = near[x].min(run);
        }
        for x in 0..w {
            let ins = inside[row + x] != 0;
            if near[x] >= cap {
                let d = cap as f32 - 0.5;
                out[row + x] = if ins { -d } else { d };
                continue;
            }
            let mut best = (dy[row + x] as u32).pow(2).min(cap_sq);
            // 反対側の画素の縦の距離を、横にずらしながら見る（dx² が今の最小を超えたら打ち切り）
            let mut dx = 1u32;
            while dx * dx < best {
                for xx in [x.wrapping_sub(dx as usize), x + dx as usize] {
                    if xx >= w {
                        continue;
                    }
                    // 隣の列の画素が反対側なら縦の距離 0、同じ側ならその画素の dy（同じ側から見た反対側までの縦の距離）
                    let e = if (inside[row + xx] != 0) != ins { 0 } else { dy[row + xx] as u32 };
                    best = best.min(dx * dx + e * e);
                }
                dx += 1;
            }
            let d = (best as f32).sqrt() - 0.5;
            out[row + x] = if ins { -d } else { d };
        }
    }
    out
}

/// 被覆率（0..1）から符号付き距離を求め、8bit に詰める（128 が縁、+ が外側、1 単位 = SDF_RANGE_MM / 127 mm）
pub fn encode_sdf(coverage: &[f32], w: usize, h: usize, px_per_mm: f64) -> Vec<u8> {
    let inside: Vec<u8> = coverage.iter().map(|&c| if c >= 0.5 { 1 } else { 0 }).collect();
    // 符号化で端に丸まる距離（SDF_RANGE_MM）より 2 画素遠い所までを正確に求めれば、結果は全体を求めたときと同じ
    let range_px = SDF_RANGE_MM * px_per_mm;
    let sd = signed_distance_px_capped(&inside, w, h, range_px.ceil() as usize + 2);
    let scale = (127.0 / range_px) as f32;
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

    fn area(p: &Paths) -> f64 {
        p.iter().map(signed_area).sum()
    }

    /// 向きの違うベタが重なっても、銅は消えない（以前は非ゼロ規則の回転数が打ち消し合って穴になった）。
    /// タイルの範囲で切ったときも、切らずに集めて範囲で切ったものと同じ
    #[test]
    fn zones_of_either_orientation_add_up() {
        let mut board = Board::default();
        let sq = |x0: f64, y0: f64, x1: f64, y1: f64| vec![Vec2::new(x0, y0), Vec2::new(x1, y0), Vec2::new(x1, y1), Vec2::new(x0, y1)];
        let mut cw = sq(1.0, 1.0, 3.0, 3.0);
        cw.reverse();
        board.zones.push(krbr_kicad::ZoneFill { layer: "F.Cu".into(), pts: sq(0.0, 0.0, 2.0, 2.0) });
        board.zones.push(krbr_kicad::ZoneFill { layer: "F.Cu".into(), pts: cw });
        let opts = LayerArtOptions::default();
        let all = collect_side_paths(&board, Side::Front, &opts, None);
        assert!((area(&all.copper) - 7.0).abs() < 1e-6, "{}", area(&all.copper));
        let clip = Box2 { lo: Vec2::new(0.5, 0.5), hi: Vec2::new(2.5, 1.5) };
        let tile = collect_side_paths(&board, Side::Front, &opts, Some(&clip));
        let expect = intersect(&all.copper, &rect_path(clip.lo, clip.hi));
        assert!((area(&tile.copper) - area(&expect)).abs() < 1e-6 && (area(&expect) - 1.75).abs() < 1e-6, "{} {}", area(&tile.copper), area(&expect));
    }

    /// kicad-cli から得た線フォントの文字は、銅箔・マスクの開口・シルクのそれぞれに足され、マスクの開口はシルクを削る
    #[test]
    fn stroke_texts_join_their_layers() {
        let board = Board::default();
        let sq = |x0: f64, y0: f64, x1: f64, y1: f64| rect_path(Vec2::new(x0, y0), Vec2::new(x1, y1));
        let mut opts = LayerArtOptions::default();
        let text = SidePaths { copper: sq(0.0, 0.0, 1.0, 1.0), mask_open: sq(10.0, 0.0, 12.0, 1.0), silk: sq(11.0, 0.0, 14.0, 1.0) };
        opts.stroke_text[Side::Back as usize] = Some(std::sync::Arc::new(text));
        let back = collect_side_paths(&board, Side::Back, &opts, None);
        assert!((area(&back.copper).abs() - 1.0).abs() < 1e-6);
        assert!((area(&back.mask_open).abs() - 2.0).abs() < 1e-6);
        assert!((area(&back.silk).abs() - 2.0).abs() < 1e-6, "{}", area(&back.silk));
        // 表には足さない。タイルの範囲の外の文字は集めない
        let front = collect_side_paths(&board, Side::Front, &opts, None);
        assert!(front.copper.is_empty() && front.mask_open.is_empty() && front.silk.is_empty());
        let far = Box2 { lo: Vec2::new(50.0, 50.0), hi: Vec2::new(60.0, 60.0) };
        assert!(collect_side_paths(&board, Side::Back, &opts, Some(&far)).copper.is_empty());
    }

    #[test]
    fn clip_to_box_keeps_the_area_inside() {
        // 凹んだ U 字（面積 5）を、くぼみを横切る矩形で切る
        let u = [(0.0, 0.0), (3.0, 0.0), (3.0, 3.0), (2.0, 3.0), (2.0, 1.0), (1.0, 1.0), (1.0, 3.0), (0.0, 3.0)].map(|(x, y)| Vec2::new(x, y));
        let c = clip_to_box(&u, Vec2::new(0.5, 0.5), Vec2::new(2.5, 2.0));
        let got = area(&unite(&to_path(&c).into(), FillRule::NonZero));
        // 下の帯 2 x 0.5 と、左右の柱 0.5 x 1 が 2 本
        assert!((got - 2.0).abs() < 1e-9, "{got}");
        assert!(clip_to_box(&u, Vec2::new(5.0, 5.0), Vec2::new(6.0, 6.0)).is_empty());
        assert!(signed_area_pts(&u) > 0.0 && signed_area(&outer_path(&u.iter().rev().copied().collect::<Vec<_>>())) > 0.0);
    }

    /// 打ち切った距離変換は、打ち切りより近い所では全体を求めたものと同じ
    #[test]
    fn capped_distance_matches_the_full_one_inside_the_cap() {
        let (w, h) = (64usize, 48usize);
        let inside: Vec<u8> = (0..w * h).map(|i| (((i % w) as f64 - 30.0).hypot((i / w) as f64 - 20.0) < 11.0 || (i % w > 50 && i / w > 30)) as u8).collect();
        let full = signed_distance_px(&inside, w, h);
        let capped = signed_distance_px_capped(&inside, w, h, 6);
        for (a, b) in full.iter().zip(&capped) {
            if a.abs() < 5.5 {
                assert_eq!(a, b);
            } else {
                assert!(b.abs() >= 5.5 && a.signum() == b.signum());
            }
        }
    }

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
    fn coverage_does_not_depend_on_the_number_of_bands() {
        // 帯の境をまたぐ斜めの辺と円
        let g = Grid { origin: Vec2::ZERO, px_per_mm: 10.0, width: 37, height: 41 };
        let mut p = to_path(&[Vec2::new(0.3, 0.2), Vec2::new(3.4, 1.1), Vec2::new(1.2, 3.9)]).into();
        append(&mut p, disc(Vec2::new(2.5, 2.5), 1.1));
        let one = rasterize_coverage_with(&p, &g, 1);
        for threads in [2, 3, 7, 64] {
            assert_eq!(rasterize_coverage_with(&p, &g, threads), one, "{threads}");
        }
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
