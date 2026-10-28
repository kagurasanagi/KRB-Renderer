//! 基板表面の凹凸（銅箔・レジスト・シルクの厚み）を実際の形状として作る。
//! 領域ごとに上面と側壁を作り、側壁は基材の面まで下ろす（隣の高い領域の中に入る部分は見えない）。

use glam::Vec3;
use krbr_core::Mesh;
use krbr_kicad::{Box2, Side};

use crate::board_mesh::{board_to_world, extrude_region, BoardGeometry};
use crate::shapes::*;

#[derive(Debug, Clone, Copy)]
pub struct ReliefParams {
    /// 銅箔（mm）
    pub copper: f64,
    /// 銅の上のレジスト
    pub mask_on_copper: f64,
    /// 基材の上のレジスト（銅の間を埋めるので厚い）
    pub mask_on_substrate: f64,
    pub silk: f64,
    /// レジストでふさがれたビアのくぼみの深さ（0 で平ら）
    pub via_dimple: f64,
}

impl Default for ReliefParams {
    fn default() -> Self {
        Self { copper: 0.035, mask_on_copper: 0.012, mask_on_substrate: 0.025, silk: 0.015, via_dimple: 0.05 }
    }
}

impl ReliefParams {
    /// 銅上のレジストの上面が板厚の半分（t/2）に来るように、基材の半分の厚みを決める
    pub fn core_half(&self, thickness: f64) -> f64 {
        thickness / 2.0 - self.copper - self.mask_on_copper
    }
}

/// 凹凸メッシュの三角形ごとの材質区分
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ReliefMat {
    /// むき出しの基材
    Fr4 = 0,
    /// むき出しの銅（表面処理）
    Finish = 1,
    MaskOnFr4 = 2,
    MaskOnCopper = 3,
    Silk = 4,
}

/// レジストでふさがれたビア：レジストが穴に落ち込んでできる浅いすり鉢状のくぼみ
#[derive(Debug, Clone, Copy)]
pub struct ReliefDimple {
    pub pos: Vec2,
    pub radius: f64,
    /// くぼみの面を作る（タイル単位のときは中心を含むタイルだけ。平らな面からの除去は全タイル）
    pub build: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ReliefInput {
    pub copper: Paths,
    pub mask_open: Paths,
    pub silk: Paths,
    /// レジストでふさがれたビアの穴（面を張る）
    pub tented_holes: Paths,
    /// 面を張る範囲を前もって求めてある場合（タイル単位で作るとき）
    pub base: Option<Paths>,
    pub dimples: Vec<ReliefDimple>,
}

/// 面を張る範囲：穴を開けた基板＋レジストでふさがれたビアの穴
pub fn relief_base(geo: &BoardGeometry, tented_holes: &Paths) -> Paths {
    if tented_holes.is_empty() {
        return geo.body.clone();
    }
    let mut a = geo.body.clone();
    append(&mut a, intersect(tented_holes, &geo.region));
    unite(&a, clipper2::FillRule::NonZero)
}

const DIMPLE_SEGMENTS: usize = 24; // くぼみの縁の分割数（平らな面から除く円と、くぼみの面で同じ頂点を使う）
const DIMPLE_RINGS: usize = 4;

fn dimple_rim(d: &ReliefDimple) -> Path {
    use std::f64::consts::PI;
    let pts: Vec<Vec2> = (0..DIMPLE_SEGMENTS)
        .map(|i| {
            let a = 2.0 * PI * i as f64 / DIMPLE_SEGMENTS as f64;
            Vec2::new(d.pos.x + d.radius * a.cos(), d.pos.y + d.radius * a.sin())
        })
        .collect();
    to_path(&pts)
}

/// すり鉢状のくぼみ：縁の高さ h_rim から中心で depth だけ下がる（余弦の断面。縁では傾き 0 で周りの面につながる）
fn build_dimple(d: &ReliefDimple, h_rim: f64, depth: f64, side: Side, center: Vec2, mat: u32, out: &mut Mesh) {
    use std::f64::consts::PI;
    let sign = if side == Side::Front { 1.0 } else { -1.0 };
    let r_max = d.radius;
    let height = |r: f64| h_rim - depth * (0.5 + 0.5 * (PI * r / r_max).cos());
    let slope = |r: f64| depth * 0.5 * PI / r_max * (PI * r / r_max).sin();
    let mut rings: Vec<Vec<u32>> = vec![Vec::new(); DIMPLE_RINGS + 1];
    let c = out.add_vertex(board_to_world(d.pos, sign * height(0.0), center), Vec3::new(0.0, sign as f32, 0.0));
    for (k, ring) in rings.iter_mut().enumerate().skip(1) {
        let r = r_max * k as f64 / DIMPLE_RINGS as f64;
        let dh = slope(r);
        for i in 0..DIMPLE_SEGMENTS {
            let a = 2.0 * PI * i as f64 / DIMPLE_SEGMENTS as f64;
            let radial = Vec2::new(a.cos(), a.sin());
            let p = d.pos + r * radial;
            let mut n = Vec3::new((-dh * radial.x) as f32, 1.0, (-dh * radial.y) as f32).normalize();
            n.y *= sign as f32;
            ring.push(out.add_vertex(board_to_world(p, sign * height(r), center), n));
        }
    }
    let tri = |a: u32, b: u32, cc: u32, out: &mut Mesh| {
        // 表は +Y、裏は -Y を向くように巻く
        let n = (out.positions[b as usize] - out.positions[a as usize]).cross(out.positions[cc as usize] - out.positions[a as usize]);
        if (n.y > 0.0) == (sign > 0.0) {
            out.add_triangle(a, b, cc, mat);
        } else {
            out.add_triangle(a, cc, b, mat);
        }
    };
    for i in 0..DIMPLE_SEGMENTS {
        let j = (i + 1) % DIMPLE_SEGMENTS;
        tri(c, rings[1][i], rings[1][j], out);
        for k in 1..DIMPLE_RINGS {
            tri(rings[k][i], rings[k + 1][i], rings[k + 1][j], out);
            tri(rings[k][i], rings[k + 1][j], rings[k][j], out);
        }
    }
}

/// 表は +Y 向き、裏は -Y 向きに作る。基材の面は ±core_half。
/// clip を渡すと、その矩形の中だけを作る（タイル単位の作り直し用。入力も同じ矩形で切り抜いておくこと）
pub fn build_relief(geo: &BoardGeometry, side: Side, input: &ReliefInput, p: &ReliefParams, out: &mut Mesh, clip: Option<&Box2>) {
    let mut base = match &input.base {
        Some(b) => b.clone(),
        None => relief_base(geo, &input.tented_holes),
    };
    if let Some(c) = clip {
        base = intersect(&base, &rect_path(c.lo, c.hi));
    }
    // ふさがれたビアのくぼみ：平らな面からその円を除き、あとで同じ縁の点を使ってくぼみの面を張る
    let dimples = p.via_dimple > 0.0 && !input.dimples.is_empty();
    if dimples {
        let discs = Paths::new(input.dimples.iter().map(dimple_rim).collect());
        base = subtract(&base, &discs);
    }
    if base.is_empty() && !dimples {
        return;
    }
    let c_paths = intersect(&input.copper, &base);
    let o_paths = intersect(&input.mask_open, &base);
    let m_paths = subtract(&base, &o_paths);
    let no_cu = subtract(&base, &c_paths);
    let s_paths = intersect(&input.silk, &base);

    // 高さ（基材の面からの距離）
    let h_fr4 = 0.0;
    let h_fin = p.copper;
    let h_mask_fr4 = p.mask_on_substrate;
    let h_mask_cu = p.copper + p.mask_on_copper;

    let no_cu_open = intersect(&no_cu, &o_paths);
    let cu_open = intersect(&c_paths, &o_paths);
    let no_cu_mask = intersect(&no_cu, &m_paths);
    let cu_mask = intersect(&c_paths, &m_paths);
    let regions: [(Paths, f64, ReliefMat); 8] = [
        (subtract(&no_cu_open, &s_paths), h_fr4, ReliefMat::Fr4),
        (subtract(&cu_open, &s_paths), h_fin, ReliefMat::Finish),
        (subtract(&no_cu_mask, &s_paths), h_mask_fr4, ReliefMat::MaskOnFr4),
        (subtract(&cu_mask, &s_paths), h_mask_cu, ReliefMat::MaskOnCopper),
        // シルクは下にあるものの上にさらに乗る
        (intersect(&s_paths, &no_cu_open), h_fr4 + p.silk, ReliefMat::Silk),
        (intersect(&s_paths, &cu_open), h_fin + p.silk, ReliefMat::Silk),
        (intersect(&s_paths, &no_cu_mask), h_mask_fr4 + p.silk, ReliefMat::Silk),
        (intersect(&s_paths, &cu_mask), h_mask_cu + p.silk, ReliefMat::Silk),
    ];

    let sign = if side == Side::Front { 1.0 } else { -1.0 };
    let y0 = sign * geo.core_half;
    for (paths, h, mat) in &regions {
        if paths.is_empty() {
            continue;
        }
        // 高さ 0 の領域（むき出しの基材）も、基材の側面とつながる面として作る
        extrude_region(paths, y0 + sign * h, y0, side == Side::Front, *mat as u32, *mat as u32, geo.center, out);
    }
    if dimples {
        for d in &input.dimples {
            if !d.build {
                continue;
            }
            // 縁は銅の上のレジストの高さ（ビアのランドは銅で、レジストに覆われている）。深さは半径の 6 割まで
            let depth = p.via_dimple.min(d.radius * 0.6);
            build_dimple(d, h_mask_cu, depth, side, geo.center, ReliefMat::MaskOnCopper as u32, out);
        }
    }
}
