//! 凹凸メッシュのタイルのキャッシュ（保存のたびに、変わった範囲のタイルだけを作り直す）と、
//! 銅・レジスト開口の縁からの距離場（基板全体の RG8 画像。タイルごとに書き換える）

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use krbr_core::Mesh;
use krbr_kicad::{Board, Box2};

use crate::board_mesh::BoardGeometry;
use crate::raster::{collect_side_paths, encode_sdf, rasterize_coverage_with, Grid, LayerArtOptions, Side, SDF_RANGE_MM};
use crate::relief::{build_relief, relief_base, ReliefDimple, ReliefInput, ReliefParams};
use crate::shapes::*;

/// 外周のタイルを基板の外へ広げる幅（mm）。基板の外形はタイルの範囲にちょうど収まるが、Clipper2 の座標の丸め（10 nm）で
/// 縁がタイルの境からわずかにはみ出しても切れないように広げる。距離場の画像もこの分だけ外側を持つ（基板の縁の画素を
/// 補間するときに、画像の外を引かないように）。丸めの分よりずっと大きいが、画像が数 % 大きくなるだけなので余裕を取る
const OUTER_TILE_MARGIN_MM: f64 = 1.0;

/// タイルの距離場を求めるときに、タイルの外側へ広げる画素数：距離場は縁から SDF_RANGE_MM 先までを表すので、その先まで
/// の図形を集める（＋2 画素は、縁の画素の補正と丸めの分）
fn sdf_extent_px(px_per_mm: f64) -> usize {
    (SDF_RANGE_MM * px_per_mm).ceil() as usize + 2
}

#[derive(Debug, Clone, Default)]
pub struct SdfPatch {
    pub x0: usize,
    pub y0: usize,
    pub w: usize,
    pub h: usize,
    pub copper: Vec<u8>,
    pub mask_open: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct ReliefTileCache {
    /// mm
    pub tile_size: f64,
    pub bounds: Box2,
    pub center: Vec2,
    pub core_half: f64,
    pub nx: usize,
    pub ny: usize,
    /// 表・裏。三角形の材質は ReliefMat（呼び出し側で材質表の番号に写す）
    pub tiles: [Vec<Mesh>; 2],
    pub valid: [Vec<bool>; 2],
    pub sdf: [Vec<SdfPatch>; 2],
    /// 距離場の格子（基板の範囲＋OUTER_TILE_MARGIN_MM）
    pub sdf_grid: Grid,
    /// 基板全体の距離場（RG8：R = 銅, G = レジスト開口）
    pub sdf_image: [Vec<u8>; 2],
    /// 直前の構築で作り直した（面, タイル）
    pub last_rebuilt: Vec<(usize, usize)>,
}

impl Default for ReliefTileCache {
    fn default() -> Self {
        Self {
            tile_size: 8.0,
            bounds: Box2::default(),
            center: Vec2::ZERO,
            core_half: 0.0,
            nx: 0,
            ny: 0,
            tiles: [Vec::new(), Vec::new()],
            valid: [Vec::new(), Vec::new()],
            sdf: [Vec::new(), Vec::new()],
            sdf_grid: Grid::default(),
            sdf_image: [Vec::new(), Vec::new()],
            last_rebuilt: Vec::new(),
        }
    }
}

impl ReliefTileCache {
    pub fn count(&self) -> usize {
        self.nx * self.ny
    }

    pub fn matches(&self, g: &BoardGeometry) -> bool {
        self.nx > 0 && self.bounds.lo == g.bounds.lo && self.bounds.hi == g.bounds.hi && self.center == g.center && self.core_half == g.core_half
    }

    pub fn reset(&mut self, g: &BoardGeometry, px_per_mm: f64) {
        self.bounds = Box2 { lo: g.bounds.lo, hi: g.bounds.hi };
        self.center = g.center;
        self.core_half = g.core_half;
        self.nx = (((self.bounds.hi.x - self.bounds.lo.x) / self.tile_size).ceil() as usize).max(1);
        self.ny = (((self.bounds.hi.y - self.bounds.lo.y) / self.tile_size).ceil() as usize).max(1);
        let n = self.count();
        for s in 0..2 {
            self.tiles[s] = vec![Mesh::default(); n];
            self.valid[s] = vec![false; n];
            self.sdf[s] = vec![SdfPatch::default(); n];
        }
        // 距離場の格子：外周のタイルの広げ分（OUTER_TILE_MARGIN_MM）を含める。画素はタイルの境と揃える
        let m = OUTER_TILE_MARGIN_MM;
        self.sdf_grid = Grid {
            px_per_mm,
            origin: self.bounds.lo - Vec2::splat(m),
            width: ((self.nx as f64 * self.tile_size + 2.0 * m) * px_per_mm).ceil() as usize,
            height: ((self.ny as f64 * self.tile_size + 2.0 * m) * px_per_mm).ceil() as usize,
        };
    }

    pub fn invalidate_all(&mut self) {
        for v in &mut self.valid {
            v.iter_mut().for_each(|x| *x = false);
        }
    }

    /// 変わった範囲（差分の検出の BoardItem::bbox。レジストの広がりなどを含む）にかかるタイルを無効にする
    pub fn invalidate(&mut self, dirty: &[Box2]) {
        // タイルの距離場は、タイルの外側（sdf_extent_px）の図形からも求めるので、その範囲までの変更で作り直す。
        // ＋1 画素は、タイルの範囲を画素に丸めるときのずれ
        let ppm = self.sdf_grid.px_per_mm;
        let influence = if ppm > 0.0 { (sdf_extent_px(ppm) + 1) as f64 / ppm } else { 0.0 };
        for t in 0..self.count() {
            let r = self.tile_rect(t).expanded(influence);
            if dirty.iter().any(|d| r.intersects(d)) {
                self.valid[0][t] = false;
                self.valid[1][t] = false;
            }
        }
    }

    pub fn tile_rect(&self, index: usize) -> Box2 {
        let (i, j) = (index % self.nx, index / self.nx);
        let lo = self.bounds.lo + Vec2::new(i as f64 * self.tile_size, j as f64 * self.tile_size);
        let mut r = Box2 { lo, hi: lo + Vec2::splat(self.tile_size) };
        // 外周のタイルは基板の外まで少し広げる（端の取りこぼしを防ぐ）
        let m = OUTER_TILE_MARGIN_MM;
        if i == 0 {
            r.lo.x -= m;
        }
        if j == 0 {
            r.lo.y -= m;
        }
        if i == self.nx - 1 {
            r.hi.x += m;
        }
        if j == self.ny - 1 {
            r.hi.y += m;
        }
        r
    }

    /// タイルの範囲の画素（距離場の格子での）：(x0, y0, x1, y1)
    pub fn tile_pixels(&self, index: usize) -> (usize, usize, usize, usize) {
        let r = self.tile_rect(index);
        let g = &self.sdf_grid;
        let px = |v: f64, o: f64, max: usize| (((v - o) * g.px_per_mm).round().max(0.0) as usize).min(max);
        (px(r.lo.x, g.origin.x, g.width), px(r.lo.y, g.origin.y, g.height), px(r.hi.x, g.origin.x, g.width), px(r.hi.y, g.origin.y, g.height))
    }
}

/// 凹凸メッシュをタイルごとに作る（キャッシュにある有効なタイルはそのまま使う）。タイルは並列に作る。
/// 戻り値は作り直したタイルの数
pub fn build_relief_tiles(cache: &mut ReliefTileCache, board: &Board, geo: &BoardGeometry, rp: &ReliefParams, layer_art: &LayerArtOptions) -> usize {
    let t0 = Instant::now();
    if !cache.matches(geo) {
        cache.reset(geo, layer_art.px_per_mm);
    }

    // 面を張る範囲（表裏それぞれ、ふさがれたビアの穴を含める）
    let mut base: [Paths; 2] = [Paths::new(Vec::new()), Paths::new(Vec::new())];
    let mut tented_vias: [Vec<ReliefDimple>; 2] = [Vec::new(), Vec::new()];
    for side in Side::BOTH {
        let mut tented = Paths::new(Vec::new());
        for v in &board.vias {
            if v.tented(side, &board.setup) && v.drill > 0.0 {
                append(&mut tented, disc(v.pos, v.drill / 2.0));
                tented_vias[side as usize].push(ReliefDimple { pos: v.pos, radius: v.drill / 2.0, build: true });
            }
        }
        base[side as usize] = relief_base(geo, &tented);
    }

    let mut todo: Vec<(Side, usize)> = Vec::new();
    for side in Side::BOTH {
        for t in 0..cache.count() {
            if !cache.valid[side as usize][t] {
                todo.push((side, t));
            }
        }
    }
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(Side, usize, Mesh, SdfPatch)>> = Mutex::new(Vec::new());
    let cache_ref: &ReliefTileCache = cache;
    let worker = || {
        loop {
            let k = next.fetch_add(1, Ordering::Relaxed);
            if k >= todo.len() {
                break;
            }
            let (side, t) = todo[k];
            let rect = cache_ref.tile_rect(t);
            // 距離場のために、タイルより少し広い範囲（SDF_RANGE_MM 以上）の図形を集める
            let (px0, py0, px1, py1) = cache_ref.tile_pixels(t);
            let g = &cache_ref.sdf_grid;
            let ext = sdf_extent_px(g.px_per_mm);
            let (ex0, ey0) = (px0.saturating_sub(ext), py0.saturating_sub(ext));
            let (ex1, ey1) = ((px1 + ext).min(g.width), (py1 + ext).min(g.height));
            let wide = Box2 {
                lo: g.origin + Vec2::new(ex0 as f64, ey0 as f64) / g.px_per_mm,
                hi: g.origin + Vec2::new(ex1 as f64, ey1 as f64) / g.px_per_mm,
            };
            let sp = collect_side_paths(board, side, layer_art, Some(&wide));
            let rect_paths = rect_path(rect.lo, rect.hi);
            let tile_base = intersect(&base[side as usize], &rect_paths);
            let mut input = ReliefInput {
                copper: intersect(&sp.copper, &rect_paths),
                mask_open: intersect(&sp.mask_open, &rect_paths),
                silk: intersect(&sp.silk, &rect_paths),
                tented_holes: Paths::new(Vec::new()),
                base: Some(tile_base),
                dimples: Vec::new(),
            };
            for d in &tented_vias[side as usize] {
                // 円がこのタイルにかかるものは平らな面から除く。くぼみの面は中心を含むタイルだけが作る
                if d.pos.x + d.radius < rect.lo.x || d.pos.x - d.radius > rect.hi.x || d.pos.y + d.radius < rect.lo.y || d.pos.y - d.radius > rect.hi.y {
                    continue;
                }
                let center_here = d.pos.x >= rect.lo.x && d.pos.x < rect.hi.x && d.pos.y >= rect.lo.y && d.pos.y < rect.hi.y;
                input.dimples.push(ReliefDimple { pos: d.pos, radius: d.radius, build: center_here });
            }
            // 距離場（広い範囲で求め、タイルの画素だけを残す）
            let eg = Grid { origin: wide.lo, px_per_mm: g.px_per_mm, width: ex1 - ex0, height: ey1 - ey0 };
            let mut patch = SdfPatch { x0: px0, y0: py0, w: px1 - px0, h: py1 - py0, copper: Vec::new(), mask_open: Vec::new() };
            for layer in 0..2 {
                // タイルごとにすでに並列なので、ラスタライズは今のスレッドだけで行う（スレッドを入れ子にしない）
                let cov = rasterize_coverage_with(if layer == 0 { &sp.copper } else { &sp.mask_open }, &eg, 1);
                let enc = encode_sdf(&cov, eg.width, eg.height, g.px_per_mm);
                let dst = if layer == 0 { &mut patch.copper } else { &mut patch.mask_open };
                dst.resize(patch.w * patch.h, 0);
                for y in 0..patch.h {
                    let src = (y + py0 - ey0) * eg.width + (px0 - ex0);
                    dst[y * patch.w..(y + 1) * patch.w].copy_from_slice(&enc[src..src + patch.w]);
                }
            }
            let mut m = Mesh::default();
            build_relief(geo, side, &input, rp, &mut m, Some(&rect));
            results.lock().unwrap().push((side, t, m, patch));
        }
    };
    let nthreads = krbr_core::platform::worker_threads().min(todo.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..nthreads {
            // 作業用のスレッドは低優先度（ビューアのウィンドウのスレッドより後回しにする）
            s.spawn(|| {
                krbr_core::platform::lower_thread_priority();
                worker()
            });
        }
    });
    let results = results.into_inner().unwrap();
    for (side, t, m, patch) in results {
        let s = side as usize;
        cache.tiles[s][t] = m;
        cache.sdf[s][t] = patch;
        cache.valid[s][t] = true;
    }

    // 作り直したタイルの距離場だけを、基板全体の画像（RG8）に書き込む。範囲外は「遠く外側」
    let g = cache.sdf_grid;
    let bytes = g.width * g.height * 2;
    for side in 0..2 {
        if cache.sdf_image[side].len() != bytes {
            cache.sdf_image[side] = vec![255; bytes];
        }
    }
    for &(side, t) in &todo {
        let s = side as usize;
        let p = &cache.sdf[s][t];
        for y in 0..p.h {
            let row = ((p.y0 + y) * g.width + p.x0) * 2;
            for x in 0..p.w {
                cache.sdf_image[s][row + x * 2] = p.copper[y * p.w + x];
                cache.sdf_image[s][row + x * 2 + 1] = p.mask_open[y * p.w + x];
            }
        }
    }
    cache.last_rebuilt = todo.iter().map(|&(side, t)| (side as usize, t)).collect();

    let tris: usize = (0..2).map(|s| cache.tiles[s].iter().map(|m| m.triangle_count()).sum::<usize>()).sum();
    krbr_core::info!(
        "relief: rebuilt {} of {} tiles ({}x{}, {:.0} mm) in {:.0} ms, {} triangles",
        todo.len(),
        2 * cache.count(),
        cache.nx,
        cache.ny,
        cache.tile_size,
        t0.elapsed().as_secs_f64() * 1000.0,
        tris
    );
    todo.len()
}
