//! 表面実装のはんだの平衡形状：溶けたはんだが落ち着く形を、表面エネルギーの最小化として求める（solder::footprint_fillets から使う）。
//!
//! エネルギーは、はんだと空気の界面の広さ × 表面張力 γ と、固体ごとのはんだと固体の界面の広さ × (γ_SL − γ_SV) と、重力の位置エネルギー。
//! ヤングの式 γ_SV − γ_SL = γ cos θ から、固体の性質は接触角 θ だけで決まる：
//!   E / γ = |はんだと空気の界面| − Σ_固体 cos θ_s |はんだと固体 s の界面| + (ρ g / γ) ∫ はんだ u dV
//! 銅のパッド・電極（金属）は θ が小さく（濡れる：はんだを引き寄せる）、樹脂・セラミックの本体・レジスト・基材は θ > 90°（濡れない：はんだを弾き、
//! 引き寄せない）。体積はペーストから残るはんだの量に固定する。
//!
//! 解き方は threshold dynamics（MBO 法）の濡れへの拡張（Xu, Wang, Wang, "An efficient threshold dynamics method for wetting on rough surfaces",
//! J. Comput. Phys. 330 (2017)。体積の保存は Ruuth & Wetton 2003）。界面の広さを熱核 G_τ との畳み込みで近似し
//! （Esedoglu & Otto 2015：|Γ_ij| ≈ √π/√τ ∫ χ_i G_τ * χ_j）、はんだの領域 χ について次をくり返す：
//!   φ = 2 G_τ * χ + G_τ * χ_固体 + Σ_s (cos θ_s / η_s) G_{η_s² τ} * χ_s − (ρ g / γ) (√τ / √π) u
//!   新しい χ = 固体でない格子の点のうち、φ の大きい方から体積の分
//! η_s = π cos θ_s / (π − 2θ_s) は接触点でヤングの角を正す補正（Wang, Wang, Xu 2019）。η = 1 なら固体の重みは 1 + cos θ で、
//! θ = 0 で 2（はんだと同じ：完全に濡れる）、90° で 1（中立）、180° で 0（空気と同じ：完全に弾く）。
//! τ（熱核の幅 σ = √(2τ)）は大きい方から小さい方へ下げる（大きいと速く動き、小さいと形が正確）。
//! 「体積の分」は段差にせず、χ = clamp((φ − λ) / 幅 + ½, 0, 1) と界面を約 1 点の幅にならす。界面の位置が格子より細かく動けるので、
//! 曲率の小さいところでも止まらない（段差のしきい値では、σ が √(格子の間隔 × 曲率半径) より小さいと界面が格子に引っかかって止まる）。
//!
//! パッドの銅ははんだと金属的に結びつくので、一度濡れたところからは引かない（引く向きの接触角がほぼ 0）。ペーストはパッド全体に
//! 刷ってあるので、パッドの面に接する 1 層は、はじめから最後まではんだのままにする。
//!
//! 座標はパッドの座標系（基板座標の x, y と、パッドの銅の上面から外へ測った高さ u。mm）。

use glam::{DVec2, DVec3};

/// 格子の点の中身
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    /// 空気（はんだが入れる）
    Vapor = 0,
    /// パッドの銅（表面処理）
    Copper = 1,
    /// 基材（FR4）
    Laminate = 2,
    /// レジスト
    Mask = 3,
    /// 部品の金属（電極・リード）
    Metal = 4,
    /// 部品の樹脂・セラミック
    Resin = 5,
}

pub const PHASES: usize = 6;

/// 物性と解き方の設定
#[derive(Debug, Clone)]
pub struct Params {
    /// 溶けたはんだの表面張力（N/m）
    pub surface_tension: f64,
    /// 溶けたはんだの密度（kg/m³）
    pub density: f64,
    /// 固体ごとの接触角（度。Phase の順。Vapor は使わない）
    pub contact_deg: [f64; PHASES],
    /// 格子の点の数の目安（間隔はこれに合わせて決める）。パッドの面積 1 mm² あたりこの数、ただし voxels..max_voxels
    pub voxels: usize,
    pub max_voxels: usize,
    /// 格子の間隔の範囲（mm）
    pub min_cell: f64,
    pub max_cell: f64,
    /// 熱核の幅 σ（mm）を下げていく段。それぞれ格子の間隔の MIN_SIGMA_CELLS 倍より細くはしない。
    /// 大きいと界面が速く動くが、離れた固体の影響が届く（σ の数倍の隙間を越えて濡れる）。小さいと形が正確だが、
    /// 曲率の小さいところは動かなくなる（σ ≳ √(格子の間隔 × 曲率半径) が要る：Xu, Wang, Wang 2017）
    pub sigmas: Vec<f64>,
    /// 段ごとのくり返しの上限
    pub max_iterations: usize,
    /// はんだが登れる高さの上限（パッドの面から。mm）
    pub max_height: f64,
    /// パッドの面に接する 1 層をはんだのままにする（濡れた銅から引かない）。false は接触角の検証用
    pub pin_pad: bool,
}

impl Default for Params {
    fn default() -> Self {
        // 接触角（調査の値）：SAC はんだとフラックスで銅 45°（Moser et al. 2007、メニスコグラフ）、ENIG のニッケル 35°、錫めっきの部品の
        // ウェッティングバランスで 30〜50°。アルミナ（チップ抵抗の本体）122〜140°（Kaptay 2021）。レジスト・モールド樹脂は測った値が
        // 見つからない（濡れない：120〜150°）。電極・リードを小さくしすぎる（15° など）と、金属を薄く覆う膜の損 γ(1 − cos θ) がほぼ 0 になり、
        // 曲がったリードの内側を膜が這い上がる
        let mut contact_deg = [0.0; PHASES];
        contact_deg[Phase::Copper as usize] = 40.0;
        contact_deg[Phase::Laminate as usize] = 130.0;
        contact_deg[Phase::Mask as usize] = 130.0;
        contact_deg[Phase::Metal as usize] = 30.0;
        contact_deg[Phase::Resin as usize] = 140.0;
        Self {
            // SAC はんだとフラックスの界面張力 0.44〜0.46 N/m（Moser et al. 2007）、液体の密度 約 7.0 g/cm³
            surface_tension: 0.45,
            density: 7000.0,
            contact_deg,
            voxels: 2_000_000,
            max_voxels: 6_000_000,
            min_cell: 0.003,
            max_cell: 0.03,
            sigmas: vec![0.032, 0.022, 0.015, 0.010],
            max_iterations: 300,
            max_height: 1.0,
            pin_pad: true,
        }
    }
}

/// 固体の項の熱核の幅の倍率 η（Wang, Wang, Xu 2019：h2 = η² h1、η = π cos θ / (π − 2θ)）。接触点でヤングの角が正しく出る
fn eta(theta_deg: f64) -> f64 {
    let t = theta_deg.to_radians();
    let d = std::f64::consts::PI - 2.0 * t;
    if d.abs() < 1e-6 {
        std::f64::consts::FRAC_PI_2
    } else {
        std::f64::consts::PI * t.cos() / d
    }
}

/// 1 つのパッドの問題
pub struct Problem<'a> {
    /// パッドの銅の輪郭（基板座標。偶奇の規則）
    pub pad: &'a [Vec<DVec2>],
    /// レジストの開口の輪郭
    pub opening: &'a [Vec<DVec2>],
    /// 部品のモデルの三角形（パッドの座標系）と、三角形ごとの部位の番号、部位ごとの金属か
    pub tris: &'a [[DVec3; 3]],
    pub tri_part: &'a [u32],
    pub part_metal: &'a [bool],
    /// はんだの体積（mm³）
    pub volume: f64,
    /// 銅箔の厚み、基材の上のレジストの厚み（mm）
    pub copper: f64,
    pub mask_on_substrate: f64,
}

/// 格子：点 (i, j, l) の中心は origin + (i + ½, j + ½, l + ½) · cell
#[derive(Debug, Clone, Copy)]
pub struct Grid {
    pub origin: DVec3,
    pub cell: f64,
    pub nx: usize,
    pub ny: usize,
    pub nz: usize,
}

impl Grid {
    pub fn len(&self) -> usize {
        self.nx * self.ny * self.nz
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn index(&self, i: usize, j: usize, l: usize) -> usize {
        (l * self.ny + j) * self.nx + i
    }
    pub fn center(&self, i: usize, j: usize, l: usize) -> DVec3 {
        self.origin + (DVec3::new(i as f64, j as f64, l as f64) + 0.5) * self.cell
    }
    fn dims(&self) -> [usize; 3] {
        [self.nx, self.ny, self.nz]
    }
}

/// 解いた結果
pub struct Solution {
    pub grid: Grid,
    pub phase: Vec<Phase>,
    /// はんだか（割合が ½ 以上）
    pub liquid: Vec<bool>,
    /// はんだの割合（0〜1。界面で間の値）
    pub fill: Vec<f32>,
    /// 段ごとの熱核の幅（格子の間隔の倍数）、くり返しの回数、最後の変化（割合の差の和）
    pub log: Vec<(f64, usize, usize)>,
}

/// 熱核の幅の下限（格子の間隔の倍数）
const MIN_SIGMA_CELLS: f64 = 1.5;

/// 光線の位置を格子の点の中心から少しずらす（モデルの頂点・辺が格子の点のちょうど上に来ると、交差を 2 回数える）
const JITTER: [f64; 3] = [1.234_567e-7, 2.345_678e-7, 3.456_789e-7];

/// 点が多角形（偶奇の規則）の中か
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

/// 同時に解くパッドの数（solve の中の数）と、その上限を待つための条件変数。上限は作業用のスレッドの数（1 つのパッドで 100〜250 MB 使う）
static RUNNING: (std::sync::Mutex<usize>, std::sync::Condvar) = (std::sync::Mutex::new(0), std::sync::Condvar::new());

/// 解いている間持つ枠（落とすと空く）
struct Slot;

impl Slot {
    fn acquire() -> Slot {
        let limit = krbr_core::platform::worker_threads().max(1);
        let mut n = RUNNING.0.lock().unwrap();
        while *n >= limit {
            n = RUNNING.1.wait(n).unwrap();
        }
        *n += 1;
        Slot
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        *RUNNING.0.lock().unwrap() -= 1;
        RUNNING.1.notify_one();
    }
}

/// out を chunk 個ずつに分け、スレッドで f(先頭の chunk の番号, 部分) を呼ぶ。スレッドの数は、作業用のスレッドを同時に解いているパッドで分けた数
fn par_chunks<T: Send>(out: &mut [T], chunk: usize, f: impl Fn(usize, &mut [T]) + Sync) {
    let chunks = out.len().div_ceil(chunk.max(1));
    let running = (*RUNNING.0.lock().unwrap()).max(1);
    let threads = (krbr_core::platform::worker_threads() / running).clamp(1, chunks.max(1));
    if threads <= 1 {
        f(0, out);
        return;
    }
    let per = chunks.div_ceil(threads);
    std::thread::scope(|sc| {
        for (t, part) in out.chunks_mut(per * chunk).enumerate() {
            let f = &f;
            sc.spawn(move || f(t * per, part));
        }
    });
}

/// ガウス関数（σ は格子の間隔の倍数）での畳み込み。3 つの軸に分けて行う。replicate なら範囲の外は端の値が続くとし、そうでなければ 0
pub fn blur(g: &Grid, src: &[f32], sigma: f64, replicate: bool) -> Vec<f32> {
    let r = (3.0 * sigma).ceil().max(1.0) as i64;
    let mut w: Vec<f32> = (-r..=r).map(|t| (-(t * t) as f64 / (2.0 * sigma * sigma)).exp() as f32).collect();
    let sum: f32 = w.iter().sum();
    w.iter_mut().for_each(|v| *v /= sum);
    let (nx, ny, nz) = (g.nx as i64, g.ny as i64, g.nz as i64);
    let w = &w;
    let fetch = move |line: &dyn Fn(i64) -> f32, i: i64, n: i64| -> f32 {
        let mut acc = 0.0;
        for (t, &wt) in w.iter().enumerate() {
            let q = i + t as i64 - r;
            if q >= 0 && q < n {
                acc += wt * line(q);
            } else if replicate {
                acc += wt * line(q.clamp(0, n - 1));
            }
        }
        acc
    };
    let plane = (nx * ny) as usize;
    // x
    let mut a = vec![0.0f32; src.len()];
    par_chunks(&mut a, nx as usize, |row0, part| {
        for (r_, row) in part.chunks_mut(nx as usize).enumerate() {
            let base = (row0 + r_) * nx as usize;
            let line = |q: i64| src[base + q as usize];
            for (i, v) in row.iter_mut().enumerate() {
                *v = fetch(&line, i as i64, nx);
            }
        }
    });
    // y
    let mut b = vec![0.0f32; src.len()];
    par_chunks(&mut b, plane, |l0, part| {
        for (dl, slab) in part.chunks_mut(plane).enumerate() {
            let base = (l0 + dl) * plane;
            for j in 0..ny {
                for i in 0..nx {
                    let line = |q: i64| a[base + (q * nx + i) as usize];
                    slab[(j * nx + i) as usize] = fetch(&line, j, ny);
                }
            }
        }
    });
    // z
    par_chunks(&mut a, plane, |l0, part| {
        for (dl, slab) in part.chunks_mut(plane).enumerate() {
            let l = (l0 + dl) as i64;
            for (k, v) in slab.iter_mut().enumerate() {
                let line = |q: i64| b[q as usize * plane + k];
                *v = fetch(&line, l, nz);
            }
        }
    });
    a
}

/// src が 0 でない点を含む箱を、各軸に margin 点広げたもの（格子の中に収める。[lo, hi)）。0 でない点が無ければ None
fn active_box(g: &Grid, src: &[f32], margin: usize) -> Option<([usize; 3], [usize; 3])> {
    let (mut lo, mut hi) = ([usize::MAX; 3], [0usize; 3]);
    for l in 0..g.nz {
        for j in 0..g.ny {
            let row = &src[g.index(0, j, l)..g.index(0, j, l) + g.nx];
            let (Some(a), Some(b)) = (row.iter().position(|&v| v != 0.0), row.iter().rposition(|&v| v != 0.0)) else { continue };
            lo = [lo[0].min(a), lo[1].min(j), lo[2].min(l)];
            hi = [hi[0].max(b), hi[1].max(j), hi[2].max(l)];
        }
    }
    if lo[0] == usize::MAX {
        return None;
    }
    let dims = g.dims();
    Some((std::array::from_fn(|d| lo[d].saturating_sub(margin)), std::array::from_fn(|d| (hi[d] + margin + 1).min(dims[d]))))
}

/// 箱 [lo, hi) の中だけの blur（範囲の外は 0）。結果は箱の大きさの配列（箱の中の点 (i, j, l) は ((l·ny' + j)·nx' + i)）。
/// 箱が src の 0 でない点から熱核の幅（3σ）以上離れていれば、格子全体で畳み込むのと同じ
fn blur_box(g: &Grid, src: &[f32], sigma: f64, (lo, hi): ([usize; 3], [usize; 3])) -> (Grid, Vec<f32>) {
    let sub = Grid { origin: g.origin, cell: g.cell, nx: hi[0] - lo[0], ny: hi[1] - lo[1], nz: hi[2] - lo[2] };
    let mut part = vec![0.0f32; sub.len()];
    for l in 0..sub.nz {
        for j in 0..sub.ny {
            let a = g.index(lo[0], lo[1] + j, lo[2] + l);
            let b = sub.index(0, j, l);
            part[b..b + sub.nx].copy_from_slice(&src[a..a + sub.nx]);
        }
    }
    let out = blur(&sub, &part, sigma, false);
    (sub, out)
}

/// 部品の部位ごとに、中にある格子の点を求める。3 つの軸の向きの光線で交差の偶奇を数え、多数決にする（三角形の向きがそろっていない
/// モデル、ひびのあるモデルでも崩れにくい。交差の数が奇数の光線は数えない）。金属と樹脂の両方の中にある点は金属（端子のキャップが本体の
/// 端を包むモデルで、外側の面が金属になるように）
fn voxelize_parts(g: &Grid, tris: &[[DVec3; 3]], tri_part: &[u32], part_metal: &[bool]) -> Vec<Option<bool>> {
    let n = g.len();
    let dims = g.dims();
    let lo = g.origin;
    let hi = g.origin + DVec3::new(g.nx as f64, g.ny as f64, g.nz as f64) * g.cell;
    let mut out: Vec<Option<bool>> = vec![None; n];
    let parts = part_metal.len();
    let mut by_part: Vec<Vec<usize>> = vec![Vec::new(); parts];
    for (t, &p) in tri_part.iter().enumerate() {
        if let Some(v) = by_part.get_mut(p as usize) {
            v.push(t);
        }
    }
    for (p, list) in by_part.iter().enumerate() {
        if list.is_empty() {
            continue;
        }
        // 部位の外接箱が格子に重ならなければ飛ばす
        let (mut plo, mut phi) = (DVec3::splat(f64::MAX), DVec3::splat(f64::MIN));
        for &t in list {
            for q in tris[t] {
                plo = plo.min(q);
                phi = phi.max(q);
            }
        }
        if phi.x < lo.x || phi.y < lo.y || phi.z < lo.z || plo.x > hi.x || plo.y > hi.y || plo.z > hi.z {
            continue;
        }
        let mut votes = vec![0u8; n];
        let mut valid = vec![0u8; n];
        for a in 0..3 {
            let (b, c) = ((a + 1) % 3, (a + 2) % 3);
            let (nb, nc, na) = (dims[b], dims[c], dims[a]);
            let mut hits: Vec<Vec<f64>> = vec![Vec::new(); nb * nc];
            for &t in list {
                let q = tris[t];
                let (b0, b1) = (q[0][b].min(q[1][b]).min(q[2][b]), q[0][b].max(q[1][b]).max(q[2][b]));
                let (c0, c1) = (q[0][c].min(q[1][c]).min(q[2][c]), q[0][c].max(q[1][c]).max(q[2][c]));
                let ib0 = ((b0 - lo[b]) / g.cell - 0.5).ceil().max(0.0) as usize;
                let ic0 = ((c0 - lo[c]) / g.cell - 0.5).ceil().max(0.0) as usize;
                let ib1 = ((b1 - lo[b]) / g.cell - 0.5).floor();
                let ic1 = ((c1 - lo[c]) / g.cell - 0.5).floor();
                if ib1 < 0.0 || ic1 < 0.0 {
                    continue;
                }
                let (ib1, ic1) = ((ib1 as usize).min(nb - 1), (ic1 as usize).min(nc - 1));
                let p2 = q.map(|v| DVec2::new(v[b], v[c]));
                let area = (p2[1] - p2[0]).perp_dot(p2[2] - p2[0]);
                if area.abs() < 1e-18 {
                    continue;
                }
                for ic in ic0..=ic1 {
                    for ib in ib0..=ib1 {
                        let s = DVec2::new(lo[b] + (ib as f64 + 0.5) * g.cell + JITTER[b], lo[c] + (ic as f64 + 0.5) * g.cell + JITTER[c]);
                        let w0 = (p2[1] - s).perp_dot(p2[2] - s) / area;
                        let w1 = (p2[2] - s).perp_dot(p2[0] - s) / area;
                        let w2 = 1.0 - w0 - w1;
                        if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                            continue;
                        }
                        hits[ic * nb + ib].push(w0 * q[0][a] + w1 * q[1][a] + w2 * q[2][a]);
                    }
                }
            }
            for ic in 0..nc {
                for ib in 0..nb {
                    let h = &mut hits[ic * nb + ib];
                    if h.len() % 2 == 1 {
                        continue;
                    }
                    h.sort_by(f64::total_cmp);
                    let mut idx = [0usize; 3];
                    idx[b] = ib;
                    idx[c] = ic;
                    let mut pair = 0;
                    for ia in 0..na {
                        idx[a] = ia;
                        let k = g.index(idx[0], idx[1], idx[2]);
                        valid[k] += 1;
                        let x = lo[a] + (ia as f64 + 0.5) * g.cell + JITTER[a];
                        while pair + 1 < h.len() && h[pair + 1] < x {
                            pair += 2;
                        }
                        if pair + 1 < h.len() && h[pair] < x && x < h[pair + 1] {
                            votes[k] += 1;
                        }
                    }
                }
            }
        }
        let metal = part_metal[p];
        for k in 0..n {
            if valid[k] > 0 && 2 * votes[k] > valid[k] {
                out[k] = Some(out[k].unwrap_or(false) || metal);
            }
        }
    }
    out
}

/// 1 つのパッドのはんだの平衡形状を解く。はんだが置けない（パッドの上がすべて固体など）ときは None
pub fn solve(pr: &Problem, prm: &Params) -> Option<Solution> {
    let _slot = Slot::acquire();
    let (mut plo, mut phi) = (DVec2::splat(f64::MAX), DVec2::splat(f64::MIN));
    for p in pr.pad.iter().flatten() {
        plo = plo.min(*p);
        phi = phi.max(*p);
    }
    if !(plo.x < phi.x && plo.y < phi.y) || pr.volume <= 0.0 {
        return None;
    }
    // 高さ：パッドの上（少し広く）にある部品の一番上まで（上限 max_height）
    let near = |q: &[DVec3; 3], m: f64| q.iter().any(|v| v.x > plo.x - m && v.x < phi.x + m && v.y > plo.y - m && v.y < phi.y + m);
    let mut top = 0.0f64;
    for q in pr.tris.iter().filter(|q| near(q, 0.05)) {
        for v in q {
            top = top.max(v.z);
        }
    }
    // はんだが部品より高く盛り上がることもある（部品の無いパッド、低いリード）ので、体積の立方根の高さは取る
    let top = top.max(1.2 * pr.volume.cbrt()).clamp(0.15, prm.max_height) + 0.05;
    // 格子の間隔：点の数が voxels くらいになるように
    let margin0 = 0.08;
    let area = (phi.x - plo.x + 2.0 * margin0) * (phi.y - plo.y + 2.0 * margin0);
    // 格子の点の数：パッドが大きいほど増やす（粗いと接触角が合わない：間隔 11 µm で 40° が 51°）
    let budget = (prm.voxels as f64 * ((phi.x - plo.x) * (phi.y - plo.y)).max(1.0)).min(prm.max_voxels.max(prm.voxels) as f64);
    let cell = (area * (top + pr.copper) / budget).cbrt().clamp(prm.min_cell, prm.max_cell);
    // 熱核の幅（格子の間隔の倍数）。同じになった段はまとめる
    let mut sigmas: Vec<f64> = Vec::new();
    for &s in &prm.sigmas {
        let c = (s / cell).max(MIN_SIGMA_CELLS);
        if sigmas.last().is_none_or(|&l| (l - c).abs() > 0.25) {
            sigmas.push(c);
        }
    }
    let sig_max = prm.sigmas.iter().copied().fold(0.0, f64::max);
    let eta_max = (1..PHASES).map(|p| eta(prm.contact_deg[p])).fold(1.0, f64::max);
    let margin = margin0.max(3.0 * sig_max.max(MIN_SIGMA_CELLS * cell) * eta_max);
    let below = ((pr.copper / cell).ceil() as usize + 2) as f64 * cell;
    let origin = DVec3::new(plo.x - margin, plo.y - margin, -below);
    let g = Grid {
        origin,
        cell,
        nx: ((phi.x - plo.x + 2.0 * margin) / cell).ceil() as usize,
        ny: ((phi.y - plo.y + 2.0 * margin) / cell).ceil() as usize,
        nz: ((top + below) / cell).ceil() as usize,
    };
    let n = g.len();

    // 固体：基板（パッドの銅・基材・レジスト）、その上に部品（金属・樹脂）
    let mut phase = vec![Phase::Vapor; n];
    let solid_parts = voxelize_parts(&g, pr.tris, pr.tri_part, pr.part_metal);
    let mut in_pad = vec![false; g.nx * g.ny];
    for j in 0..g.ny {
        for i in 0..g.nx {
            let c = g.center(i, j, 0);
            let p = DVec2::new(c.x + JITTER[0], c.y + JITTER[1]);
            let pad = inside_rings(pr.pad, p);
            in_pad[j * g.nx + i] = pad;
            let open = pad || inside_rings(pr.opening, p);
            for l in 0..g.nz {
                let u = g.center(i, j, l).z;
                let k = g.index(i, j, l);
                phase[k] = if u < -pr.copper {
                    Phase::Laminate
                } else if pad && u < 0.0 {
                    Phase::Copper
                } else if !open && u < -pr.copper + pr.mask_on_substrate {
                    Phase::Mask
                } else {
                    match solid_parts[k] {
                        Some(true) => Phase::Metal,
                        Some(false) => Phase::Resin,
                        None => Phase::Vapor,
                    }
                };
            }
        }
    }
    let first_free = (below / cell).round() as usize; // パッドの面のすぐ上の層
    // はんだの体積（格子の点の数）と、はじめの形：パッドの上に下から積む（刷ったペーストが溶けた形）
    let target = (pr.volume / (cell * cell * cell)).round() as usize;
    let mut liquid = vec![false; n];
    let mut sticky = vec![false; n];
    let mut placed = 0usize;
    if prm.pin_pad {
        for l in first_free..g.nz {
            if placed >= target {
                break;
            }
            for j in 0..g.ny {
                for i in 0..g.nx {
                    let k = g.index(i, j, l);
                    if in_pad[j * g.nx + i] && phase[k] == Phase::Vapor && placed < target {
                        liquid[k] = true;
                        placed += 1;
                        sticky[k] = l == first_free;
                    }
                }
            }
        }
    } else {
        // 検証用：パッドの中心に置いた半球から始める
        let mid = DVec3::new(0.5 * (plo.x + phi.x), 0.5 * (plo.y + phi.y), 0.0);
        let mut order: Vec<(f64, usize)> = Vec::new();
        for l in first_free..g.nz {
            for j in 0..g.ny {
                for i in 0..g.nx {
                    let k = g.index(i, j, l);
                    if in_pad[j * g.nx + i] && phase[k] == Phase::Vapor {
                        order.push(((g.center(i, j, l) - mid).length(), k));
                    }
                }
            }
        }
        order.sort_by(|a, b| a.0.total_cmp(&b.0));
        for &(_, k) in order.iter().take(target) {
            liquid[k] = true;
            placed += 1;
        }
    }
    if placed == 0 {
        return None;
    }
    let target = placed.max(sticky.iter().filter(|&&s| s).count());
    let mut fill: Vec<f32> = liquid.into_iter().map(|b| b as u8 as f32).collect();

    // 固体の項：G_σ * χ_固体 + Σ_s (cos θ_s / η_s) G_{η_s σ} * χ_s（Wang, Wang, Xu 2019。η_s = 1 なら 2017 年の (1 + cos θ_s) G_σ * χ_s と同じ）
    let terms: Vec<(Phase, f32, f64)> = [Phase::Copper, Phase::Laminate, Phase::Mask, Phase::Metal, Phase::Resin]
        .into_iter()
        .filter(|p| phase.contains(p))
        .map(|p| {
            let theta = prm.contact_deg[p as usize];
            let e = eta(theta);
            (p, (theta.to_radians().cos() / e) as f32, e)
        })
        .collect();
    let stages: Vec<Stage> = sigmas
        .iter()
        .map(|&sigma| {
            // 重力：(ρ g / γ)(√τ / √π) u。σ = √(2τ)、長さは m（grav は高さ 1 mm あたり）
            let tau_sqrt = sigma * cell * 1e-3 / std::f64::consts::SQRT_2;
            let grav = prm.density * 9.81 / prm.surface_tension * tau_sqrt / std::f64::consts::PI.sqrt() * 1e-3;
            // しきい値は段差にせず、界面の幅を約 1 点にならす（はんだの割合 0〜1。界面の位置が格子より細かく動けるので、曲率の小さいところでも
            // 止まらない）。φ の界面を横切る傾きは 1 点あたり約 2 / (√(2π) σ)
            let width = (2.0 / ((2.0 * std::f64::consts::PI).sqrt() * sigma)) as f32;
            Stage { sigma, grav, width }
        })
        .collect();
    let relax = Relax { grid: g, phase: &phase, sticky: &sticky, target, terms, stages, max_iterations: prm.max_iterations };
    let backend = if cfg!(test) { None } else { RELAXER.read().ok().and_then(|r| r.clone()) };
    let log = match backend.map(|b| {
        let mut f = fill.clone();
        b.relax(&relax, &mut f).map(|log| (f, log))
    }) {
        Some(Ok((f, log))) => {
            fill = f;
            log
        }
        Some(Err(e)) => {
            krbr_core::warn!("solder: the GPU solver failed ({e}); solving on the CPU");
            relax_cpu(&relax, &mut fill)
        }
        None => relax_cpu(&relax, &mut fill),
    };
    let liquid = fill.iter().map(|&f| f >= 0.5).collect();
    Some(Solution { grid: g, phase, liquid, fill, log })
}

/// 熱核の幅の 1 段：σ（格子の間隔の倍数）、重力の項の係数（高さ 1 mm あたり）、しきい値をならす幅
#[derive(Debug, Clone, Copy)]
pub struct Stage {
    pub sigma: f64,
    pub grav: f64,
    pub width: f32,
}

/// くり返しの入力（CPU 版と GPU 版で同じ）。terms は固体の項（中身、係数 cos θ / η、熱核の幅の倍率 η）、
/// target ははんだの体積（格子の点の数）、sticky ははんだのままにする点
pub struct Relax<'a> {
    pub grid: Grid,
    pub phase: &'a [Phase],
    pub sticky: &'a [bool],
    pub target: usize,
    pub terms: Vec<(Phase, f32, f64)>,
    pub stages: Vec<Stage>,
    pub max_iterations: usize,
}

/// 1 回の変化 change、2 回前との差 back、界面の点の数から、その段を終えるか。界面が 1 回に動く距離（格子の間隔の何倍か）=
/// 変化 ÷ 界面の点の数 が 1/1000 以下か、2 回前の形とほとんど同じ（割合の小さな揺れが行き来するだけ。動き続けているときは
/// 2 回前との差は 1 回の変化より大きい）
pub fn stage_done(it: usize, change: f64, back: f64, interface: usize) -> bool {
    change / interface.max(1) as f64 <= 1e-3 || (it >= 4 && back < 0.25 * change)
}

/// くり返しを別の方法（GPU）で行うもの。失敗したら Err（CPU で解き直す）
pub trait Relaxer: Send + Sync {
    /// fill をはじめの形から釣り合いの形にし、段ごとの (σ, くり返しの回数, 最後の変化) を返す
    fn relax(&self, r: &Relax, fill: &mut Vec<f32>) -> Result<Vec<(f64, usize, usize)>, String>;
}

static RELAXER: std::sync::RwLock<Option<std::sync::Arc<dyn Relaxer>>> = std::sync::RwLock::new(None);

/// くり返しを GPU などで行うようにする（None で CPU に戻す）
pub fn set_relaxer(r: Option<std::sync::Arc<dyn Relaxer>>) {
    if let Ok(mut w) = RELAXER.write() {
        *w = r;
    }
}

/// CPU でのくり返し（はんだを含む箱の中だけを計算する）
pub fn relax_cpu(r: &Relax, fill: &mut Vec<f32>) -> Vec<(f64, usize, usize)> {
    let g = r.grid;
    let n = g.len();
    let (phase, sticky, target) = (r.phase, r.sticky, r.target);
    let solid_f: Vec<f32> = phase.iter().map(|&p| (p != Phase::Vapor) as u8 as f32).collect();
    let mut log = Vec::new();
    let mut cand: Vec<(f32, u32)> = Vec::new();
    let plane = g.nx * g.ny;
    let forced = sticky.iter().filter(|&&b| b).count() as f64;
    for st in &r.stages {
        let (sigma, width) = (st.sigma, st.width);
        let mut ws = blur(&g, &solid_f, sigma, true);
        for &(p, coef, e) in &r.terms {
            let chi: Vec<f32> = phase.iter().map(|&q| (q == p) as u8 as f32).collect();
            for (w, v) in ws.iter_mut().zip(blur(&g, &chi, sigma * e, true)) {
                *w += coef * v;
            }
        }
        let grav_l: Vec<f32> = (0..g.nz).map(|l| (st.grav * g.center(0, 0, l).z) as f32).collect();
        let mut iterations = 0;
        let mut change = 0.0f64;
        let mut before: Vec<f32> = fill.clone();
        // 畳み込みと候補は、はんだを含む箱に熱核の幅（と 1 回に広がる 1 点）を足した範囲だけ
        let margin = (3.0 * sigma).ceil() as usize + 2;
        for it in 0..r.max_iterations {
            iterations += 1;
            let Some(bx) = active_box(&g, fill, margin) else { break };
            let (sub, c) = blur_box(&g, fill, sigma, bx);
            let (lo, hi) = bx;
            cand.clear();
            for l in lo[2]..hi[2] {
                for j in lo[1]..hi[1] {
                    for i in lo[0]..hi[0] {
                        let k = g.index(i, j, l);
                        if phase[k] != Phase::Vapor || sticky[k] {
                            continue;
                        }
                        // はんだと、それに隣り合う点だけ（つながっていないところへ飛ばない）
                        const SOME: f32 = 0.01;
                        let near = fill[k] > SOME
                            || (i > 0 && fill[k - 1] > SOME)
                            || (i + 1 < g.nx && fill[k + 1] > SOME)
                            || (j > 0 && fill[k - g.nx] > SOME)
                            || (j + 1 < g.ny && fill[k + g.nx] > SOME)
                            || (l > 0 && fill[k - plane] > SOME)
                            || (l + 1 < g.nz && fill[k + plane] > SOME);
                        if near {
                            let cb = c[sub.index(i - lo[0], j - lo[1], l - lo[2])];
                            cand.push((2.0 * cb + ws[k] - grav_l[l], k as u32));
                        }
                    }
                }
            }
            // 体積が合うしきい値 λ（二分法）：Σ clamp((φ − λ) / width + ½, 0, 1) = 残りの体積
            let need = (target as f64 - forced).max(0.0);
            let amount = |lam: f32| -> f64 { cand.iter().map(|&(p, _)| ((p - lam) / width + 0.5).clamp(0.0, 1.0) as f64).sum() };
            let (mut a, mut b) = cand.iter().fold((f32::MAX, f32::MIN), |(a, b), &(p, _)| (a.min(p), b.max(p)));
            a -= width;
            b += width;
            for _ in 0..28 {
                let mid = 0.5 * (a + b);
                if amount(mid) > need {
                    a = mid;
                } else {
                    b = mid;
                }
            }
            let lam = 0.5 * (a + b);
            let mut next = vec![0.0f32; n];
            for l in lo[2]..hi[2] {
                for j in lo[1]..hi[1] {
                    let k0 = g.index(lo[0], j, l);
                    for k in k0..k0 + (hi[0] - lo[0]) {
                        if sticky[k] {
                            next[k] = 1.0;
                        }
                    }
                }
            }
            for &(p, k) in &cand {
                next[k as usize] = ((p - lam) / width + 0.5).clamp(0.0, 1.0);
            }
            // 変化・2 回前との差・界面の点の数（箱の外はどれも 0）
            let (mut d1, mut d2, mut interface) = (0.0f64, 0.0f64, 0usize);
            for l in lo[2]..hi[2] {
                for j in lo[1]..hi[1] {
                    let k0 = g.index(lo[0], j, l);
                    for k in k0..k0 + (hi[0] - lo[0]) {
                        d1 += (next[k] - fill[k]).abs() as f64;
                        d2 += (next[k] - before[k]).abs() as f64;
                        interface += (next[k] > 0.02 && next[k] < 0.98) as usize;
                    }
                }
            }
            change = d1;
            before = std::mem::replace(fill, next);
            if stage_done(it, d1, d2, interface) {
                break;
            }
        }
        log.push((sigma, iterations, change.round() as usize));
    }
    log
}

/// 解いた結果の保存の形の版（解き方を変えたら上げる。表面の作り方だけを変えたときは上げなくてよい）
pub const SOLUTION_VERSION: u32 = 1;

/// 問題と設定のハッシュ（解いた結果のキャッシュのキー）
pub fn problem_key(pr: &Problem, prm: &Params) -> u64 {
    let mut b: Vec<u8> = Vec::with_capacity(pr.tris.len() * 40 + 1024);
    for r in pr.pad.iter().chain([Vec::new()].iter()).chain(pr.opening.iter()) {
        for p in r {
            b.extend_from_slice(&p.x.to_le_bytes());
            b.extend_from_slice(&p.y.to_le_bytes());
        }
        b.push(0xff);
    }
    for (t, &part) in pr.tris.iter().zip(pr.tri_part) {
        for p in t {
            for v in [p.x, p.y, p.z] {
                b.extend_from_slice(&(v as f32).to_le_bytes());
            }
        }
        b.extend_from_slice(&part.to_le_bytes());
    }
    b.extend(pr.part_metal.iter().map(|&m| m as u8));
    for v in [pr.volume, pr.copper, pr.mask_on_substrate] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(format!("{prm:?}").as_bytes());
    b.extend_from_slice(&SOLUTION_VERSION.to_le_bytes());
    krbr_core::xxh64(&b)
}

/// 解いた結果をバイト列にする（格子と、点ごとの (中身, はんだの割合を 0〜255 にしたもの) の連長圧縮）
pub fn encode(sol: &Solution) -> Vec<u8> {
    let g = sol.grid;
    let mut b: Vec<u8> = Vec::new();
    b.extend_from_slice(b"KRSS");
    b.extend_from_slice(&SOLUTION_VERSION.to_le_bytes());
    for v in [g.origin.x, g.origin.y, g.origin.z, g.cell] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for v in [g.nx, g.ny, g.nz] {
        b.extend_from_slice(&(v as u32).to_le_bytes());
    }
    let code = |k: usize| -> u16 { ((sol.phase[k] as u16) << 8) | (sol.fill[k].clamp(0.0, 1.0) * 255.0).round() as u16 };
    let mut k = 0;
    while k < g.len() {
        let c = code(k);
        let mut run = 1u32;
        while k + (run as usize) < g.len() && code(k + run as usize) == c {
            run += 1;
        }
        b.extend_from_slice(&c.to_le_bytes());
        b.extend_from_slice(&run.to_le_bytes());
        k += run as usize;
    }
    b
}

/// encode の逆（形が合わなければ None）
pub fn decode(b: &[u8]) -> Option<Solution> {
    let f64_at = |i: usize| -> Option<f64> { Some(f64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?)) };
    let u32_at = |i: usize| -> Option<u32> { Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?)) };
    if b.get(..4)? != b"KRSS" || u32_at(4)? != SOLUTION_VERSION {
        return None;
    }
    let g = Grid { origin: DVec3::new(f64_at(8)?, f64_at(16)?, f64_at(24)?), cell: f64_at(32)?, nx: u32_at(40)? as usize, ny: u32_at(44)? as usize, nz: u32_at(48)? as usize };
    let n = g.len();
    let (mut phase, mut fill) = (Vec::with_capacity(n), Vec::with_capacity(n));
    let mut at = 52;
    while at < b.len() {
        let c = u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?);
        let run = u32_at(at + 2)? as usize;
        let p = match c >> 8 {
            0 => Phase::Vapor,
            1 => Phase::Copper,
            2 => Phase::Laminate,
            3 => Phase::Mask,
            4 => Phase::Metal,
            5 => Phase::Resin,
            _ => return None,
        };
        let f = (c & 0xff) as f32 / 255.0;
        if phase.len() + run > n {
            return None;
        }
        phase.extend(std::iter::repeat_n(p, run));
        fill.extend(std::iter::repeat_n(f, run));
        at += 6;
    }
    if phase.len() != n {
        return None;
    }
    let liquid = fill.iter().map(|&f| f >= 0.5).collect();
    Some(Solution { grid: g, phase, liquid, fill, log: Vec::new() })
}

/// はんだの表面の三角形（パッドの座標系）
#[derive(Default)]
pub struct SurfaceMesh {
    pub positions: Vec<DVec3>,
    pub normals: Vec<DVec3>,
    pub triangles: Vec<[u32; 3]>,
}

/// 固体に接するはんだの面を固体の中へ押し込む深さ（格子の点の数）。表面の三角形のうち、固体に接するところは固体の中に隠れる
const INTO_SOLID: usize = 2;
/// 表面を作る前に、はんだの領域をならす幅（格子の間隔の倍数）。形用と、法線用（鏡のようなはんだは法線の細かい揺れがそのまま映るので、
/// 法線はもっと広くならした場から求める）
const SURFACE_SIGMA: f64 = 1.2;
const NORMAL_SIGMA: f64 = 2.0;
/// 面を作る格子の間引き（シミュレーションの格子の何点おきか）
const SURFACE_STRIDE: usize = 2;
/// 頂点をならす回数（Taubin の λ|μ 法：縮みにくい）
const TAUBIN_PASSES: usize = 10;

/// はんだと空気の界面を三角形にする（surface nets）。固体に接するところは固体の中へ少し押し込み、固体の中に隠れる三角形は作らない。
/// はんだを含む箱に、固体へ押し込む分とならす幅を足した範囲だけで作る（その外は格子全体で作っても何も無い）
pub fn surface(sol: &Solution) -> SurfaceMesh {
    // 中のならし（blur）のスレッドを、同時に作っている表面の数で分ける
    let _slot = Slot::acquire();
    let g = sol.grid;
    let margin = INTO_SOLID + (3.0 * SURFACE_SIGMA.max(NORMAL_SIGMA)).ceil() as usize + 2;
    let Some((lo, hi)) = active_box(&g, &sol.fill, margin) else { return SurfaceMesh::default() };
    let sub = Grid { origin: g.origin + DVec3::new(lo[0] as f64, lo[1] as f64, lo[2] as f64) * g.cell, cell: g.cell, nx: hi[0] - lo[0], ny: hi[1] - lo[1], nz: hi[2] - lo[2] };
    let n = sub.len();
    let (mut phase, mut fill) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for l in lo[2]..hi[2] {
        for j in lo[1]..hi[1] {
            let k = g.index(lo[0], j, l);
            phase.extend_from_slice(&sol.phase[k..k + sub.nx]);
            fill.extend_from_slice(&sol.fill[k..k + sub.nx]);
        }
    }
    let liquid = fill.iter().map(|&f| f >= 0.5).collect();
    surface_full(&Solution { grid: sub, phase, liquid, fill, log: Vec::new() })
}

fn surface_full(sol: &Solution) -> SurfaceMesh {
    let g = sol.grid;
    let n = g.len();
    let solid = |k: usize| sol.phase[k] != Phase::Vapor;
    // はんだを、接する固体の中へ INTO_SOLID 点だけ広げる
    let mut ext = sol.liquid.clone();
    for _ in 0..INTO_SOLID {
        let prev = ext.clone();
        for l in 0..g.nz {
            for j in 0..g.ny {
                for i in 0..g.nx {
                    let k = g.index(i, j, l);
                    if prev[k] || !solid(k) {
                        continue;
                    }
                    let nb = [(i > 0).then(|| k - 1), (i + 1 < g.nx).then(|| k + 1), (j > 0).then(|| k - g.nx), (j + 1 < g.ny).then(|| k + g.nx), (l > 0).then(|| k - g.nx * g.ny), (l + 1 < g.nz).then(|| k + g.nx * g.ny)];
                    if nb.iter().flatten().any(|&q| prev[q]) {
                        ext[k] = true;
                    }
                }
            }
        }
    }
    // はんだの割合（界面の位置を格子より細かく持つ）に、固体の中へ広げた分を足して、少しならす
    let field: Vec<f32> = (0..n).map(|k| if solid(k) { ext[k] as u8 as f32 } else { sol.fill[k] }).collect();
    let f = blur(&g, &field, SURFACE_SIGMA, false);
    let fnorm = blur(&g, &field, NORMAL_SIGMA, false);
    const ISO: f32 = 0.5;
    // 面は SURFACE_STRIDE 点おきの粗い格子で作る（場はならしてあるので形は変わらず、三角形が減る）。法線と固体かの判定は細かい格子で
    let gf = g;
    let st = SURFACE_STRIDE;
    let g = Grid { origin: gf.origin + DVec3::splat(0.5 * gf.cell * (1.0 - st as f64)), cell: gf.cell * st as f64, nx: (gf.nx - 1) / st + 1, ny: (gf.ny - 1) / st + 1, nz: (gf.nz - 1) / st + 1 };
    let mut fc = Vec::with_capacity(g.len());
    for l in 0..g.nz {
        for j in 0..g.ny {
            for i in 0..g.nx {
                fc.push(f[gf.index(i * st, j * st, l * st)]);
            }
        }
    }
    let at = |i: usize, j: usize, l: usize| fc[g.index(i, j, l)];
    // セル (i, j, l)（点 i..i+1, j..j+1, l..l+1 の間）の頂点
    let mut cell_vertex = vec![u32::MAX; g.len()];
    let mut out = SurfaceMesh::default();
    let corners: [[usize; 3]; 8] = [[0, 0, 0], [1, 0, 0], [0, 1, 0], [1, 1, 0], [0, 0, 1], [1, 0, 1], [0, 1, 1], [1, 1, 1]];
    let edges: [(usize, usize); 12] = [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)];
    // 位置での法線用の場の値（三線形補間）と、その勾配（中心差分）
    let sample = |p: DVec3| -> f64 {
        let q = (p - gf.origin) / gf.cell - 0.5;
        let i0 = q.x.floor().clamp(0.0, (gf.nx - 2) as f64) as usize;
        let j0 = q.y.floor().clamp(0.0, (gf.ny - 2) as f64) as usize;
        let l0 = q.z.floor().clamp(0.0, (gf.nz - 2) as f64) as usize;
        let t = (q - DVec3::new(i0 as f64, j0 as f64, l0 as f64)).clamp(DVec3::ZERO, DVec3::ONE);
        let mut v = 0.0;
        for c in corners {
            let w = (if c[0] == 1 { t.x } else { 1.0 - t.x }) * (if c[1] == 1 { t.y } else { 1.0 - t.y }) * (if c[2] == 1 { t.z } else { 1.0 - t.z });
            v += w * fnorm[gf.index(i0 + c[0], j0 + c[1], l0 + c[2])] as f64;
        }
        v
    };
    let outward = |p: DVec3| -> DVec3 {
        let h = 0.5 * gf.cell;
        let gx = sample(p + DVec3::X * h) - sample(p - DVec3::X * h);
        let gy = sample(p + DVec3::Y * h) - sample(p - DVec3::Y * h);
        let gz = sample(p + DVec3::Z * h) - sample(p - DVec3::Z * h);
        (-DVec3::new(gx, gy, gz)).normalize_or_zero()
    };
    for l in 0..g.nz - 1 {
        for j in 0..g.ny - 1 {
            for i in 0..g.nx - 1 {
                let v: [f32; 8] = corners.map(|c| at(i + c[0], j + c[1], l + c[2]));
                let inside = v.iter().filter(|&&x| x >= ISO).count();
                if inside == 0 || inside == 8 {
                    continue;
                }
                let mut sum = DVec3::ZERO;
                let mut count = 0.0;
                for (a, b) in edges {
                    if (v[a] >= ISO) != (v[b] >= ISO) {
                        let t = ((ISO - v[a]) / (v[b] - v[a])) as f64;
                        let pa = DVec3::new(corners[a][0] as f64, corners[a][1] as f64, corners[a][2] as f64);
                        let pb = DVec3::new(corners[b][0] as f64, corners[b][1] as f64, corners[b][2] as f64);
                        sum += pa.lerp(pb, t);
                        count += 1.0;
                    }
                }
                let p = g.center(i, j, l) + sum / count * g.cell;
                cell_vertex[g.index(i, j, l)] = out.positions.len() as u32;
                out.positions.push(p);
                out.normals.push(outward(p));
            }
        }
    }
    // 頂点が固体の中か（一番近い格子の点で）
    let in_solid = |p: DVec3| -> bool {
        let q = ((p - gf.origin) / gf.cell).floor();
        let (i, j, l) = (q.x.clamp(0.0, (gf.nx - 1) as f64) as usize, q.y.clamp(0.0, (gf.ny - 1) as f64) as usize, q.z.clamp(0.0, (gf.nz - 1) as f64) as usize);
        solid(gf.index(i, j, l))
    };
    // 符号の変わる格子の辺ごとに、まわりの 4 つのセルの頂点で四角形
    let dims = g.dims();
    for l in 0..g.nz {
        for j in 0..g.ny {
            for i in 0..g.nx {
                let p = [i, j, l];
                let inside0 = at(i, j, l) >= ISO;
                for a in 0..3 {
                    if p[a] + 1 >= dims[a] {
                        continue;
                    }
                    let mut q = p;
                    q[a] += 1;
                    if (at(q[0], q[1], q[2]) >= ISO) == inside0 {
                        continue;
                    }
                    let (b, c) = ((a + 1) % 3, (a + 2) % 3);
                    if p[b] == 0 || p[c] == 0 {
                        continue;
                    }
                    let mut quad = [0u32; 4];
                    let mut ok = true;
                    for (s, (db, dc)) in [(1usize, 1usize), (0, 1), (0, 0), (1, 0)].into_iter().enumerate() {
                        let mut cidx = p;
                        cidx[b] -= db;
                        cidx[c] -= dc;
                        let v = cell_vertex[g.index(cidx[0], cidx[1], cidx[2])];
                        if v == u32::MAX {
                            ok = false;
                            break;
                        }
                        quad[s] = v;
                    }
                    if !ok || quad.iter().all(|&v| in_solid(out.positions[v as usize])) {
                        continue;
                    }
                    for tri in [[quad[0], quad[1], quad[2]], [quad[0], quad[2], quad[3]]] {
                        let [pa, pb, pc] = tri.map(|v| out.positions[v as usize]);
                        let nrm = (pb - pa).cross(pc - pa);
                        if nrm.length_squared() == 0.0 {
                            continue;
                        }
                        let want = out.normals[tri[0] as usize] + out.normals[tri[1] as usize] + out.normals[tri[2] as usize];
                        out.triangles.push(if nrm.dot(want) >= 0.0 { tri } else { [tri[0], tri[2], tri[1]] });
                    }
                }
            }
        }
    }
    // 頂点をならし（格子の階段の名残を消す）、法線をならした位置で求め直す
    let mut nbrs: Vec<Vec<u32>> = vec![Vec::new(); out.positions.len()];
    for t in &out.triangles {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            if !nbrs[a as usize].contains(&b) {
                nbrs[a as usize].push(b);
            }
            if !nbrs[b as usize].contains(&a) {
                nbrs[b as usize].push(a);
            }
        }
    }
    for pass in 0..2 * TAUBIN_PASSES {
        let factor = if pass % 2 == 0 { 0.5 } else { -0.53 };
        let prev = out.positions.clone();
        for (v, nb) in nbrs.iter().enumerate() {
            if nb.is_empty() {
                continue;
            }
            let mean = nb.iter().map(|&q| prev[q as usize]).sum::<DVec3>() / nb.len() as f64;
            out.positions[v] = prev[v] + factor * (mean - prev[v]);
        }
    }
    for (p, nrm) in out.positions.iter().zip(out.normals.iter_mut()) {
        let n2 = outward(*p);
        if n2 != DVec3::ZERO {
            *nrm = n2;
        }
    }
    out
}

/// 断面の画像の色（sRGB）：空気・銅・基材・レジスト・金属・樹脂、はんだ
const SECTION_COLORS: [[u8; 3]; PHASES] = [[255, 255, 255], [230, 140, 40], [190, 170, 110], [40, 140, 60], [60, 90, 220], [150, 150, 150]];
const SECTION_LIQUID: [u8; 3] = [220, 30, 30];

/// 格子の断面を PNG に書く（確認用）。axis = 0 なら x = 一定の面（横軸 y）、1 なら y = 一定の面（横軸 x）。縦軸は高さ u（上が上）。
/// 1 つの点を scale × scale の画素にする。はんだは赤、金属は青、樹脂は灰色、銅は橙、レジストは緑、基材は黄土色、空気は白
pub fn write_section(sol: &Solution, axis: usize, at: f64, scale: usize, path: &std::path::Path) -> std::io::Result<()> {
    let g = sol.grid;
    let (w, h) = if axis == 0 { (g.ny, g.nz) } else { (g.nx, g.nz) };
    let fixed = if axis == 0 { ((at - g.origin.x) / g.cell).floor().clamp(0.0, (g.nx - 1) as f64) as usize } else { ((at - g.origin.y) / g.cell).floor().clamp(0.0, (g.ny - 1) as f64) as usize };
    let (pw, ph) = (w * scale, h * scale);
    let mut img = vec![0u8; pw * ph * 3];
    for y in 0..h {
        let l = h - 1 - y;
        for x in 0..w {
            let k = if axis == 0 { g.index(fixed, x, l) } else { g.index(x, fixed, l) };
            let c = if sol.liquid[k] { SECTION_LIQUID } else { SECTION_COLORS[sol.phase[k] as usize] };
            for dy in 0..scale {
                for dx in 0..scale {
                    let o = ((y * scale + dy) * pw + x * scale + dx) * 3;
                    img[o..o + 3].copy_from_slice(&c);
                }
            }
        }
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, pw as u32, ph as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().map_err(std::io::Error::other)?;
    wr.write_image_data(&img).map_err(std::io::Error::other)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(c: DVec2, h: f64) -> Vec<Vec<DVec2>> {
        vec![vec![c + DVec2::new(-h, -h), c + DVec2::new(h, -h), c + DVec2::new(h, h), c + DVec2::new(-h, h)]]
    }

    /// 箱の三角形（外向き）
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

    #[test]
    fn encode_and_decode_round_trip() {
        let g = Grid { origin: DVec3::new(0.1, -0.2, -0.05), cell: 0.01, nx: 7, ny: 5, nz: 4 };
        let phase: Vec<Phase> = (0..g.len()).map(|k| [Phase::Vapor, Phase::Copper, Phase::Metal, Phase::Resin][k % 7 % 4]).collect();
        let fill: Vec<f32> = (0..g.len()).map(|k| if phase[k] == Phase::Vapor { (k % 3) as f32 / 2.0 } else { 0.0 }).collect();
        let sol = Solution { grid: g, liquid: fill.iter().map(|&f| f >= 0.5).collect(), phase, fill, log: Vec::new() };
        let back = decode(&encode(&sol)).unwrap();
        assert_eq!(back.phase, sol.phase);
        assert_eq!(back.liquid, sol.liquid);
        assert!(back.fill.iter().zip(&sol.fill).all(|(a, b)| (a - b).abs() < 0.003));
        assert_eq!((back.grid.nx, back.grid.ny, back.grid.nz, back.grid.cell), (7, 5, 4, 0.01));
    }

    #[test]
    fn blur_keeps_sum() {
        let g = Grid { origin: DVec3::ZERO, cell: 1.0, nx: 20, ny: 15, nz: 12 };
        let mut v = vec![0.0f32; g.len()];
        v[g.index(10, 7, 6)] = 1.0;
        let b = blur(&g, &v, 2.0, false);
        let s: f32 = b.iter().sum();
        assert!((s - 1.0).abs() < 5e-3, "{s}");
        assert!(b[g.index(10, 7, 6)] > b[g.index(12, 7, 6)]);
    }

    #[test]
    fn voxelize_box() {
        let g = Grid { origin: DVec3::ZERO, cell: 0.1, nx: 10, ny: 10, nz: 10 };
        let tris = box_tris(DVec3::splat(0.2), DVec3::splat(0.6));
        let part = vec![0u32; tris.len()];
        let v = voxelize_parts(&g, &tris, &part, &[true]);
        assert_eq!(v.iter().filter(|x| x.is_some()).count(), 64);
        assert_eq!(v[g.index(3, 3, 3)], Some(true));
        assert_eq!(v[g.index(1, 3, 3)], None);
    }

    /// 部品の無いパッドでは、はんだはパッドの縁まで広がり、平らに近い丘になる（レジストへははみ出さない）
    #[test]
    fn bare_pad_stays_on_pad() {
        let pad = square(DVec2::ZERO, 0.3);
        let opening = square(DVec2::ZERO, 0.3);
        let pr = Problem { pad: &pad, opening: &opening, tris: &[], tri_part: &[], part_metal: &[], volume: 0.36 * 0.06, copper: 0.035, mask_on_substrate: 0.025 };
        let prm = Params { voxels: 300_000, max_voxels: 300_000, ..Default::default() };
        let s = solve(&pr, &prm).unwrap();
        let g = s.grid;
        for l in 0..g.nz {
            for j in 0..g.ny {
                for i in 0..g.nx {
                    if s.liquid[g.index(i, j, l)] {
                        let c = g.center(i, j, l);
                        assert!(c.x.abs() < 0.3 + 2.0 * g.cell && c.y.abs() < 0.3 + 2.0 * g.cell, "spilled at {c}");
                    }
                }
            }
        }
    }

    /// 平らな面の液滴は球の一部になり、底の半径 a と高さ h から求めた接触角 2 atan(h / a) が、与えた接触角に近い
    /// （改良した threshold dynamics の確認。Wang, Wang, Xu 2019）
    #[test]
    fn sessile_drop_has_the_given_contact_angle() {
        for theta in [40.0, 110.0] {
            let pad = square(DVec2::ZERO, 0.6);
            let voxels = std::env::var("KRBR_TEST_VOXELS").ok().and_then(|v| v.parse().ok()).unwrap_or(2_000_000);
            let mut prm = Params { voxels, pin_pad: false, ..Default::default() };
            prm.contact_deg[Phase::Copper as usize] = theta;
            // 体積は、パッドの縁に届かない大きさ（θ = 40° で底の半径 約 0.37 mm）
            let pr = Problem { pad: &pad, opening: &pad, tris: &[], tri_part: &[], part_metal: &[], volume: 0.03, copper: 0.035, mask_on_substrate: 0.025 };
            let s = solve(&pr, &prm).unwrap();
            let g = s.grid;
            let first = (0..g.nz).find(|&l| g.center(0, 0, l).z > 0.0).unwrap();
            let base = (0..g.nx * g.ny).filter(|&q| s.fill[first * g.nx * g.ny + q] >= 0.5).count() as f64 * g.cell * g.cell;
            let a = (base / std::f64::consts::PI).sqrt();
            // 高さ：割合を足し合わせた柱の高さの一番大きいもの（界面を格子より細かく測る）
            let h = (0..g.nx * g.ny).map(|q| (first..g.nz).map(|l| s.fill[l * g.nx * g.ny + q] as f64).sum::<f64>() * g.cell).fold(0.0, f64::max);
            let measured = 2.0 * (h / a).atan().to_degrees();
            if let Ok(dir) = std::env::var("KRBR_TEST_DUMP") {
                write_section(&s, 1, 0.0, 3, &std::path::Path::new(&dir).join(format!("drop_{theta}.png"))).unwrap();
                eprintln!("theta {theta}: measured {measured:.1} (a {a:.3}, h {h:.3}, cell {:.4})", g.cell);
            }
            // 本番の細かさ（200 万点、格子の間隔 6.6 µm）で 40° → 43.8°、110° → 108.8°（60 万点・11 µm では 40° → 51° と粗い）
            assert!((measured - theta).abs() < 4.0, "theta {theta}: measured {measured:.1} (a {a:.3}, h {h:.3}, cell {:.4}, {:?})", g.cell, s.log);
        }
    }

    /// 金属の壁ははんだを引き上げ、樹脂の壁は引き上げない
    #[test]
    fn metal_wall_pulls_resin_wall_does_not() {
        let pad = vec![vec![DVec2::new(-0.4, -0.3), DVec2::new(0.4, -0.3), DVec2::new(0.4, 0.3), DVec2::new(-0.4, 0.3)]];
        let height_at_wall = |metal: bool| {
            // パッドの左半分の上に、高さ 0.4 mm の箱（金属か樹脂）。底は 12 µm 浮かせる
            let tris = box_tris(DVec3::new(-0.6, -0.4, 0.012), DVec3::new(-0.05, 0.4, 0.4));
            let part = vec![0u32; tris.len()];
            let pr = Problem { pad: &pad, opening: &pad, tris: &tris, tri_part: &part, part_metal: &[metal], volume: 0.48 * 0.06, copper: 0.035, mask_on_substrate: 0.025 };
            let prm = Params { voxels: 400_000, max_voxels: 400_000, ..Default::default() };
            let s = solve(&pr, &prm).unwrap();
            let g = s.grid;
            if let Ok(dir) = std::env::var("KRBR_TEST_DUMP") {
                write_section(&s, 1, 0.0, 3, &std::path::Path::new(&dir).join(format!("wall_{}.png", if metal { "metal" } else { "resin" }))).unwrap();
                eprintln!("{} {:?} cell {:.4}", metal, s.log, g.cell);
            }
            // 壁のすぐ外と、0.1 mm 離れたところのはんだの一番上
            let top_at = |x: f64| {
                let i = ((x - g.origin.x) / g.cell) as usize;
                let j = g.ny / 2;
                (0..g.nz).filter(|&l| s.liquid[g.index(i, j, l)]).map(|l| g.center(i, j, l).z).fold(0.0, f64::max)
            };
            (top_at(-0.05 + 1.5 * g.cell), top_at(0.05))
        };
        // 金属：壁際で高く（引き上げられる）、離れると下がる
        let (metal_wall, metal_away) = height_at_wall(true);
        assert!(metal_wall > 0.15 && metal_away < metal_wall, "metal wall {metal_wall}, away {metal_away}");
        // 樹脂：壁際は低く、離れたところの方が高い（はんだは壁を避けて丘になる）
        let (resin_wall, resin_away) = height_at_wall(false);
        assert!(resin_wall < resin_away && resin_wall < metal_wall * 0.7, "resin wall {resin_wall}, away {resin_away}");
    }
}
