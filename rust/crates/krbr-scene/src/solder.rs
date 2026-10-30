//! はんだフィレット：部品の 3D モデルとパッドから、はんだの盛り（メニスカス）の形を作る。
//!
//! 表面実装：溶けたはんだが落ち着く形（平衡形状）を、表面エネルギーの最小化として解く（crate::solder_sim）。パッドの銅と、部品の金属の部位
//! （電極・リード）は濡れてはんだを引き寄せ、樹脂・セラミックの本体・レジスト・基材は濡れない（はんだを弾き、引き寄せない）。
//! はんだの体積は パッドの面積 × メタルマスクの厚み × 溶けた後に残る割合。ペーストの層（F.Paste / B.Paste）があるパッドの、部品の面だけ。
//! 電極を樹脂の色で作ったモデル（同梱の水晶など）でも、樹脂は濡らさない（はんだはパッドの上で丸く膨らむ。実物もそう見える）。
//! 部品の種類（フットプリントと 3D モデルの組。PartModel::source_key）ごとに、フットプリントの座標系で 1 回だけ解き（TypeCache）、
//! 同じ種類の部品はその結果を位置と向きに合わせて動かす。解いた結果は種類ごとにディスク（`solder/<名前>_<キー>.bin`）に置く。
//! くり返しは GPU でも行える（solder_sim::set_relaxer。krbr-app の solder_gpu）。
//!
//! スルーホール（表と裏のレジストの開いた面）：パッドを覆う格子の点で、ピンの断面（接地部）・真上のモデルの面・接地部までの距離 d・
//! ランドの縁までの距離 e を求め、高さ h = h0 · (1 − s)²、s = d / (d + e)（接地部の際で h0、ランドの縁で 0 の凹んだ形）。
//! h0 はランドの幅に傾きを掛けた高さ（ピンの高さまで）。どこでも上の面より上には出さないので、ピンや部品を突き抜けない。
//!
//! 確認用の環境変数：KRBR_SOLDER_REFS=R1,C3 でその部品だけにはんだを作り、KRBR_SOLDER_DUMP=<フォルダ> で表面実装のパッドごとの断面の画像
//! （solder_sim::write_section）を書き出す。KRBR_SOLDER_FRESH でパッドごとの解いた結果のキャッシュを読まない。

use std::collections::HashMap;

use glam::{DVec2, DVec3};
use krbr_core::Mesh;
use krbr_geom::shapes::{pad_to_paths, path_points};
use krbr_kicad::{rotate_point, Board, Pad, PadType, Side};

/// メタルマスクの厚み（mm）
const STENCIL_MM: f64 = 0.12;
/// はんだペーストのうち、溶けた後にはんだとして残る体積の割合（残りはフラックス）
const PASTE_METAL: f64 = 0.5;
/// 表面実装のフィレットが登るのは、リードの高さのこの割合まで（チップ部品は薄いので、電極の上の縁まで濡れ上がる）
const MAX_WETTING: f64 = 1.0;
/// 表面実装のフィレットの傾き（高さ / 接地部から外へ向かってパッドの縁までの幅）の上限。チップ部品の横のように、
/// 電極の際にパッドが少ししか残らないところで、はんだが壁のように立たないようにする
const SMD_SLOPE: f64 = 1.0;
/// 表面実装のはんだの高さの上限：パッドの一番近い縁までの距離 × これ。つま先の盛りの脇（流れと平行な縁）が、
/// 垂直な崖にならずに丸く下がるように
const SMD_EDGE_SLOPE: f64 = 3.0;
/// スルーホールのフィレットの傾き（高さ / ランドの幅）：はんだ面、部品面（部品面へは穴を通って少しだけ上がる）
const THT_SLOPE_SOLDER_SIDE: f64 = 0.9;
const THT_SLOPE_COMPONENT_SIDE: f64 = 0.4;
/// スルーホールのフィレットの高さの上限（mm）
const MAX_THT_HEIGHT: f64 = 1.2;
/// ピンの断面を取る高さ（パッドの面から、mm）
const SLICE_HEIGHTS: [f64; 3] = [0.03, 0.06, 0.1];
/// はんだの外周の高さ（これより薄いところは作らない。パッドの面と重ならないように）
const MIN_HEIGHT: f64 = 0.002;
/// はんだとモデルの面の間に残す隙間（mm）
const CLEARANCE: f64 = 0.004;
/// 上の面が見つからない接地部（閉じていないモデル）のピンの高さ（mm）
const DEFAULT_WALL: f64 = 0.3;
/// 格子の間隔：パッドの短い辺をこの数に分ける（0.012〜0.05 mm）。法線は高さの場から滑らかに求めるので、粗くても形は滑らか
const GRID_DIVISIONS: f64 = 24.0;
/// 格子の点の数の上限（大きなパッドは格子を粗くする）
const MAX_POINTS: usize = 250_000;

/// パッドの面の高さ（表のパッドの銅の上面のワールドの Y。裏は −pad_top）と、銅箔・基材の上のレジストの厚み（mm）
#[derive(Debug, Clone, Copy)]
pub struct BoardLayers {
    pub pad_top: f64,
    pub copper: f64,
    pub mask_on_substrate: f64,
}

/// 部品の 3D モデル：三角形（各点は (基板座標 x, 基板座標 y, ワールドの高さ Y)）、三角形ごとの部位の番号、部位ごとの金属か。
/// source_key は部品の種類（フットプリントと置いた 3D モデルの組）のハッシュで、置いた位置と向きを含まない（scene::add_solder_fillets）。
/// name はキャッシュのファイル名に使う（フットプリントの名前とモデルのファイル名）
#[derive(Debug, Clone, Default)]
pub struct PartModel {
    pub tris: Vec<[DVec3; 3]>,
    pub tri_part: Vec<u32>,
    pub part_metal: Vec<bool>,
    pub source_key: u64,
    pub name: String,
}

/// 部品の種類ごとの表面実装のはんだ（フットプリントの座標系のメッシュ）。1 回の組み立ての中で、同じ種類の部品を 1 回だけ解く
/// （同時に来たときは、先に来た方が解き終わるのを待つ）
#[derive(Default)]
pub struct TypeCache {
    map: std::sync::Mutex<HashMap<u64, std::sync::Arc<std::sync::OnceLock<std::sync::Arc<Mesh>>>>>,
}

impl TypeCache {
    fn get_or_solve(&self, key: u64, solve: impl FnOnce() -> Mesh) -> std::sync::Arc<Mesh> {
        let cell = self.map.lock().unwrap().entry(key).or_default().clone();
        cell.get_or_init(|| std::sync::Arc::new(solve())).clone()
    }
}

/// 部品ごとのフィレットのキャッシュ：保存の監視中に、パッドもモデルも動いていない部品を計算し直さない。
/// キーは footprint_key。組み立てのたびに、使わなかった部品の分は捨てる。表面実装のパッドごとの解いた結果は、別にディスクに置く
/// （smd_fillet。メッシュは解いた結果から数秒で作れるので、ディスクには置かない）
#[derive(Default)]
pub struct SolderCache {
    prev: HashMap<u64, Mesh>,
    next: HashMap<u64, Mesh>,
    /// 最後の組み立てで計算した部品の数（キャッシュに無かったもの）
    pub last_built: usize,
}

/// 形の作り方の版（footprint_key に入れる。パッドごとの解いた結果の版は solder_sim::SOLUTION_VERSION）
const SHAPE_VERSION: u32 = 2;

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

/// footprint_fillets の結果を決めるもののハッシュ：部品のパッド、モデルの三角形と部位、パッドの面の高さと厚み、原点、材質
pub fn footprint_key(board: &Board, fi: usize, model: &PartModel, layers: BoardLayers, center: DVec2, material: u32, precise: bool) -> u64 {
    let mut bytes: Vec<u8> = Vec::with_capacity(model.tris.len() * 40 + 256);
    for (t, &part) in model.tris.iter().zip(&model.tri_part) {
        for p in t {
            for v in [p.x, p.y, p.z] {
                bytes.extend_from_slice(&(v as f32).to_le_bytes());
            }
        }
        bytes.extend_from_slice(&part.to_le_bytes());
    }
    bytes.extend(model.part_metal.iter().map(|&m| m as u8));
    for p in board.pads.iter().filter(|p| p.footprint == Some(fi)) {
        bytes.extend_from_slice(format!("{p:?}").as_bytes());
    }
    if let Some(fp) = board.footprints.get(fi) {
        bytes.push(fp.back as u8);
        bytes.push(wanted(&fp.reference) as u8);
    }
    bytes.push(precise as u8);
    bytes.extend_from_slice(&board.setup.pad_to_mask_clearance.to_le_bytes());
    // 形の作り方：シミュレーションの設定と版
    bytes.extend_from_slice(format!("{:?}", sim_params()).as_bytes());
    bytes.extend_from_slice(&SHAPE_VERSION.to_le_bytes());
    for v in [layers.pad_top, layers.copper, layers.mask_on_substrate, center.x, center.y] {
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

/// 部品 fi のフィレット（ワールド座標のメッシュ。三角形の材質は material）。center は基板座標でのワールドの原点。
/// 表面実装のはんだは部品の種類ごとにフットプリントの座標系で解き（types と、ディスクのキャッシュで使い回す）、この部品の位置と向きへ動かす
#[allow(clippy::too_many_arguments)]
pub fn footprint_fillets(board: &Board, fi: usize, model: &PartModel, layers: BoardLayers, center: DVec2, material: u32, precise: bool, types: &TypeCache) -> Mesh {
    let mut out = Mesh::default();
    let Some(fp) = board.footprints.get(fi) else { return out };
    if model.tris.is_empty() || !wanted(&fp.reference) {
        return out;
    }
    let component_side = if fp.back { Side::Back } else { Side::Front };
    let mut has_smd = false;
    for pad in board.pads.iter().filter(|p| p.footprint == Some(fi)) {
        for side in [Side::Front, Side::Back] {
            if !pad.on_layer(side.copper_layer()) || !pad.on_layer(side.mask_layer()) {
                continue;
            }
            match pad.kind {
                PadType::Smd if pad.on_layer(paste_layer(side)) && side == component_side => {
                    if precise {
                        has_smd = true;
                    } else {
                        pad_fillet(pad, side, Kind::Smd, &model.tris, layers.pad_top, center, material, &mut out);
                    }
                }
                PadType::ThruHole => pad_fillet(pad, side, Kind::Tht { solder_side: side != component_side }, &model.tris, layers.pad_top, center, material, &mut out),
                _ => {}
            }
        }
    }
    if has_smd {
        let key = type_key(board, fi, model, layers);
        let local = types.get_or_solve(key, || solve_type(board, fi, model, layers, key));
        // フットプリントの座標系（X = x、Z = y）から、この部品の位置と向きへ
        let base = out.positions.len() as u32;
        for (p, n) in local.positions.iter().zip(&local.normals) {
            let b = fp.pos + rotate_point(DVec2::new(p.x as f64, p.z as f64), fp.angle_deg);
            let nb = rotate_point(DVec2::new(n.x as f64, n.z as f64), fp.angle_deg);
            out.add_vertex(DVec3::new(b.x - center.x, p.y as f64, b.y - center.y).as_vec3(), DVec3::new(nb.x, n.y as f64, nb.y).as_vec3());
        }
        for t in local.indices.chunks_exact(3) {
            out.add_triangle(base + t[0], base + t[1], base + t[2], material);
        }
    }
    out
}

/// 部品の種類のキー：source_key（フットプリントと 3D モデルの組）、フットプリントの座標系でのパッドの形、厚み、シミュレーションの設定と版
fn type_key(board: &Board, fi: usize, model: &PartModel, layers: BoardLayers) -> u64 {
    let mut b: Vec<u8> = Vec::new();
    b.extend_from_slice(&model.source_key.to_le_bytes());
    for (_, pad, side) in local_smd_pads(board, fi) {
        // 位置と角度は丸める（回転を戻したときの 1e-15 程度の違いでキーが変わらないように）
        let q = |v: f64| (v * 1e6).round() as i64;
        let ang = pad.angle_deg.rem_euclid(360.0);
        b.extend_from_slice(format!("{}|{:?}|{:?}|{}|{}|{}|{:?}|{:?}|{}|{}|{}|{:?}|{:?}|{:?}|{:?}|{}|", pad.number, pad.kind, pad.shape, q(pad.pos.x), q(pad.pos.y), q(ang), pad.size, pad.rect_delta, pad.round_ratio, pad.chamfer_ratio, pad.chamfer_corners, pad.drill, pad.layers, pad.custom_anchor, pad.primitives, side == Side::Back).as_bytes());
        b.extend_from_slice(&pad.mask_expansion(&board.setup).to_le_bytes());
    }
    for v in [layers.pad_top, layers.copper, layers.mask_on_substrate] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(format!("{:?}", sim_params()).as_bytes());
    b.extend_from_slice(&crate::solder_sim::SOLUTION_VERSION.to_le_bytes());
    krbr_core::xxh64(&b)
}

/// 部品 fi の、はんだを作る表面実装のパッド（番号、フットプリントの座標系に移したパッド、面）
fn local_smd_pads(board: &Board, fi: usize) -> Vec<(usize, Pad, Side)> {
    let Some(fp) = board.footprints.get(fi) else { return Vec::new() };
    let component_side = if fp.back { Side::Back } else { Side::Front };
    board
        .pads
        .iter()
        .filter(|p| p.footprint == Some(fi) && p.kind == PadType::Smd && p.on_layer(component_side.copper_layer()) && p.on_layer(component_side.mask_layer()) && p.on_layer(paste_layer(component_side)))
        .enumerate()
        .map(|(i, p)| {
            let mut q = p.clone();
            q.pos = rotate_point(p.pos - fp.pos, -fp.angle_deg);
            q.angle_deg = p.angle_deg - fp.angle_deg;
            (i, q, component_side)
        })
        .collect()
}

/// 部品の種類の表面実装のはんだを、フットプリントの座標系で作る（位置 (x, ワールドの高さ, y)、法線も同じ並び）。
/// パッドごとの解いた結果は、ディスクの `solder/<名前>_<キー>.bin` にまとめて置き、あれば読む
fn solve_type(board: &Board, fi: usize, model: &PartModel, layers: BoardLayers, key: u64) -> Mesh {
    let fp = &board.footprints[fi];
    let pads = local_smd_pads(board, fi);
    let to_local = |p: DVec3| {
        let q = rotate_point(DVec2::new(p.x, p.y) - fp.pos, -fp.angle_deg);
        DVec3::new(q.x, q.y, p.z)
    };
    let local = PartModel { tris: model.tris.iter().map(|t| t.map(to_local)).collect(), ..model.clone() };
    let safe: String = model.name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' { c } else { '_' }).collect();
    let path = krbr_core::platform::cache_dir().join("solder").join(format!("{safe}_{key:016x}.bin"));
    let t0 = std::time::Instant::now();
    let cached = if env_flag("KRBR_SOLDER_FRESH") || cfg!(test) { None } else { std::fs::read(&path).ok().and_then(|b| decode_type(&b, pads.len())) };
    let from_disk = cached.is_some();
    let solutions: Vec<Option<crate::solder_sim::Solution>> = match cached {
        Some(s) => s,
        None => {
            // パッドは並列に解く（同時に解く数は solder_sim が抑える）
            PROGRESS_TOTAL.fetch_add(pads.len(), std::sync::atomic::Ordering::Relaxed);
            let next = std::sync::atomic::AtomicUsize::new(0);
            let done: std::sync::Mutex<Vec<(usize, Option<crate::solder_sim::Solution>)>> = std::sync::Mutex::new(Vec::new());
            std::thread::scope(|sc| {
                for _ in 0..krbr_core::platform::worker_threads().min(pads.len()) {
                    sc.spawn(|| {
                        krbr_core::platform::lower_thread_priority();
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some((_, pad, side)) = pads.get(i) else { break };
                            let sol = solve_pad(board, pad, *side, &local, layers);
                            done.lock().unwrap().push((i, sol));
                            progress_step();
                        }
                    });
                }
            });
            let mut done = done.into_inner().unwrap();
            done.sort_by_key(|(i, _)| *i);
            let sols: Vec<_> = done.into_iter().map(|(_, s)| s).collect();
            if !cfg!(test) {
                let tmp = path.with_extension("tmp");
                let ok = path.parent().is_some_and(|d| std::fs::create_dir_all(d).is_ok()) && std::fs::write(&tmp, encode_type(&sols)).is_ok() && std::fs::rename(&tmp, &path).is_ok();
                if !ok {
                    krbr_core::debug!("solder: could not write the cache {}", path.display());
                }
            }
            sols
        }
    };
    let t_read = t0.elapsed();
    // パッドごとの表面（並列に作り、パッドの順に足す）
    let surfaces: Vec<Option<crate::solder_sim::SurfaceMesh>> = {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let done: std::sync::Mutex<Vec<(usize, crate::solder_sim::SurfaceMesh)>> = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|sc| {
            for _ in 0..krbr_core::platform::worker_threads().min(pads.len()) {
                sc.spawn(|| {
                    krbr_core::platform::lower_thread_priority();
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some(sol) = solutions.get(i) else { break };
                        if let Some(sol) = sol {
                            let m = crate::solder_sim::surface(sol);
                            done.lock().unwrap().push((i, m));
                        }
                    }
                });
            }
        });
        let mut v: Vec<Option<crate::solder_sim::SurfaceMesh>> = (0..pads.len()).map(|_| None).collect();
        for (i, m) in done.into_inner().unwrap() {
            v[i] = Some(m);
        }
        v
    };
    let mut out = Mesh::default();
    for (((_, pad, side), sol), mesh) in pads.iter().zip(&solutions).zip(surfaces) {
        let (Some(sol), Some(mesh)) = (sol, mesh) else { continue };
        if let Some(dir) = std::env::var_os("KRBR_SOLDER_DUMP") {
            let dir = std::path::PathBuf::from(dir);
            let _ = std::fs::create_dir_all(&dir);
            let name: String = format!("{}_{}", fp.reference, pad.number).chars().map(|ch| if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' { ch } else { '_' }).collect();
            for (axis, at, tag) in [(0, pad.pos.x, "x"), (1, pad.pos.y, "y")] {
                if let Err(e) = crate::solder_sim::write_section(sol, axis, at, 4, &dir.join(format!("{name}_{tag}.png"))) {
                    krbr_core::warn!("solder: could not write the section image: {e}");
                }
            }
        }
        // パッドの座標系（x, y, u）から、フットプリントの座標系の (x, ワールドの高さ, y) へ
        let s = if *side == Side::Front { 1.0 } else { -1.0 };
        let base = out.positions.len() as u32;
        for (p, n) in mesh.positions.iter().zip(&mesh.normals) {
            out.add_vertex(DVec3::new(p.x, s * (layers.pad_top + p.z), p.y).as_vec3(), DVec3::new(n.x, s * n.z, n.y).as_vec3());
        }
        for t in &mesh.triangles {
            let [a, b, c] = t.map(|v| base + v);
            let [pa, pb, pc] = [a, b, c].map(|v| out.positions[v as usize]);
            let want = out.normals[a as usize] + out.normals[b as usize] + out.normals[c as usize];
            if (pb - pa).cross(pc - pa).dot(want) >= 0.0 {
                out.add_triangle(a, b, c, 0);
            } else {
                out.add_triangle(a, c, b, 0);
            }
        }
    }
    krbr_core::debug!(
        "solder {} ({}): {} pads {} in {:.2} s, surfaces {:.2} s, {} triangles",
        fp.reference,
        model.name,
        pads.len(),
        if from_disk { "read" } else { "solved" },
        t_read.as_secs_f64(),
        (t0.elapsed() - t_read).as_secs_f64(),
        out.triangle_count()
    );
    out
}

/// 部品の種類のキャッシュのファイル：パッドの数と、パッドごとの solder_sim::encode（解けなかったパッドは長さ 0）
const TYPE_FILE_VERSION: u32 = 1;

fn encode_type(sols: &[Option<crate::solder_sim::Solution>]) -> Vec<u8> {
    let mut b: Vec<u8> = b"KRSF".to_vec();
    for v in [TYPE_FILE_VERSION, sols.len() as u32] {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for s in sols {
        let e = s.as_ref().map(crate::solder_sim::encode).unwrap_or_default();
        b.extend_from_slice(&(e.len() as u32).to_le_bytes());
        b.extend_from_slice(&e);
    }
    b
}

fn decode_type(b: &[u8], pads: usize) -> Option<Vec<Option<crate::solder_sim::Solution>>> {
    let u32_at = |i: usize| -> Option<u32> { Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?)) };
    if b.get(..4)? != b"KRSF" || u32_at(4)? != TYPE_FILE_VERSION || u32_at(8)? as usize != pads {
        return None;
    }
    let mut at = 12;
    let mut out = Vec::with_capacity(pads);
    for _ in 0..pads {
        let len = u32_at(at)? as usize;
        at += 4;
        out.push(if len == 0 { None } else { Some(crate::solder_sim::decode(b.get(at..at + len)?)?) });
        at += len;
    }
    Some(out)
}

/// 高精度のはんだの計算の進み具合：解き終えたパッドの数、解くパッドの数（キャッシュから読む分は数えない。解き始めた部品の種類の分だけ
/// 増える）、最後にログに出した時刻
static PROGRESS_DONE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static PROGRESS_TOTAL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static PROGRESS_LOGGED: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/// 高精度のはんだを解いている間の (解き終えたパッド, 解くパッド)。解いていなければ None（ビューアのステータスバー）
pub fn progress() -> Option<(usize, usize)> {
    let total = PROGRESS_TOTAL.load(std::sync::atomic::Ordering::Relaxed);
    (total > 0).then(|| (PROGRESS_DONE.load(std::sync::atomic::Ordering::Relaxed).min(total), total))
}

/// 進み具合を 0 に戻す（組み立ての始めと終わり）。終わりのときは、解いたパッドの数を返す
pub fn progress_reset() -> usize {
    PROGRESS_TOTAL.store(0, std::sync::atomic::Ordering::Relaxed);
    *PROGRESS_LOGGED.lock().unwrap() = None;
    PROGRESS_DONE.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// パッドを 1 つ解き終えた。2 秒おきにログへ進み具合を出す
fn progress_step() {
    let done = PROGRESS_DONE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let total = PROGRESS_TOTAL.load(std::sync::atomic::Ordering::Relaxed);
    let mut last = PROGRESS_LOGGED.lock().unwrap();
    if last.is_none_or(|t| t.elapsed() >= std::time::Duration::from_secs(2)) {
        *last = Some(std::time::Instant::now());
        krbr_core::info!("solder: solved {done} / {total} pads");
    }
}

/// 確認用の環境変数が有効か（空・"0" は無効）
pub fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.trim().is_empty() && v.trim() != "0")
}

/// KRBR_SOLDER_REFS（カンマ区切りの部品番号）があれば、その部品だけにはんだを作る（確認用）
fn wanted(reference: &str) -> bool {
    match std::env::var("KRBR_SOLDER_REFS") {
        Ok(list) if !list.trim().is_empty() => list.split(',').any(|r| r.trim() == reference),
        _ => true,
    }
}

/// 表面実装のシミュレーションの設定（テストでは格子を粗くして速くする）
fn sim_params() -> crate::solder_sim::Params {
    let p = crate::solder_sim::Params::default();
    if cfg!(test) {
        crate::solder_sim::Params { voxels: 250_000, max_voxels: 250_000, ..p }
    } else {
        p
    }
}

/// 多角形の輪郭の符号付きの面積
fn signed_area(r: &[DVec2]) -> f64 {
    (0..r.len()).map(|k| r[k].perp_dot(r[(k + 1) % r.len()])).sum::<f64>() / 2.0
}

/// 表面実装のパッドのはんだの平衡形状を解く（crate::solder_sim）。pad と model はフットプリントの座標系
fn solve_pad(board: &Board, pad: &Pad, side: Side, model: &PartModel, layers: BoardLayers) -> Option<crate::solder_sim::Solution> {
    let s = if side == Side::Front { 1.0 } else { -1.0 };
    let rings_of = |inflate: f64| -> Vec<Vec<DVec2>> { pad_to_paths(pad, inflate).iter().map(|p| path_points(p).collect::<Vec<_>>()).filter(|r| r.len() >= 3).collect() };
    let rings = rings_of(0.0);
    if rings.is_empty() {
        return None;
    }
    let opening = rings_of(pad.mask_expansion(&board.setup).max(0.0));
    // パッドの座標系（x, y, パッドの面から外へ測った高さ）
    let tris: Vec<[DVec3; 3]> = model.tris.iter().map(|t| t.map(|p| DVec3::new(p.x, p.y, s * p.z - layers.pad_top))).collect();
    let area = rings.iter().map(|r| signed_area(r)).sum::<f64>().abs();
    let pr = crate::solder_sim::Problem {
        pad: &rings,
        opening: &opening,
        tris: &tris,
        tri_part: &model.tri_part,
        part_metal: &model.part_metal,
        volume: area * STENCIL_MM * PASTE_METAL,
        copper: layers.copper,
        mask_on_substrate: layers.mask_on_substrate,
    };
    let t0 = std::time::Instant::now();
    let sol = crate::solder_sim::solve(&pr, &sim_params())?;
    let g = sol.grid;
    krbr_core::debug!("solder pad {}: {}x{}x{} cells of {:.1} um, {:.1} s, steps {:?}", pad.number, g.nx, g.ny, g.nz, g.cell * 1000.0, t0.elapsed().as_secs_f64(), sol.log);
    Some(sol)
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

/// 点 p から向き dir へ進んで、多角形の輪郭に当たるまでの距離（向きが 0 か、当たらなければ None）
fn distance_along(rings: &[Vec<DVec2>], p: DVec2, dir: DVec2) -> Option<f64> {
    let dir = dir.try_normalize()?;
    let mut best = f64::INFINITY;
    for r in rings {
        for k in 0..r.len() {
            let (a, b) = (r[k], r[(k + 1) % r.len()]);
            let ab = b - a;
            let den = dir.perp_dot(ab);
            if den.abs() < 1e-12 {
                continue;
            }
            let ap = a - p;
            let t = ap.perp_dot(ab) / den;
            let u = ap.perp_dot(dir) / den;
            if t >= 0.0 && (0.0..=1.0).contains(&u) {
                best = best.min(t);
            }
        }
    }
    best.is_finite().then_some(best)
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

/// 標準の精度のフィレット（0.2.1 の決め打ちの形）：表面実装とスルーホール
#[allow(clippy::too_many_arguments)]
fn pad_fillet(pad: &Pad, side: Side, kind: Kind, tris: &[[DVec3; 3]], pad_top: f64, center: DVec2, material: u32, out: &mut Mesh) {
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

    // パッドの座標系の三角形（格子の範囲に重なり、パッドの面より上に届くもの）
    let local: Vec<[DVec3; 3]> = tris
        .iter()
        .filter_map(|t| {
            let q = t.map(|p| DVec3::new(p.x, p.y, s * p.z - pad_top));
            let (qlo, qhi) = (q[0].min(q[1]).min(q[2]), q[0].max(q[1]).max(q[2]));
            (qhi.x >= g.origin.x && qlo.x <= g_hi.x && qhi.y >= g.origin.y && qlo.y <= g_hi.y && qhi.z > 0.0).then_some(q)
        })
        .collect();
    if local.is_empty() {
        return;
    }
    let n = g.len();
    let inside: Vec<bool> = (0..n).map(|k| inside_rings(&rings, g.at(k))).collect();

    // 接地部（パッドの上に断面がある、一番低い高さ）
    // 孤立した点（閉じていないモデルの断面の雑音）は除く
    let Some((u0, contact)) = SLICE_HEIGHTS.iter().map(|&u0| (u0, remove_specks(&slice_inside(&local, u0, &g), &g))).find(|(_, c)| (0..n).any(|k| c[k] && inside[k])) else {
        return;
    };

    // 各点の真上（u0 より上）で一番近いモデルの面
    let mut above = vec![f64::INFINITY; n];
    for t in &local {
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
                if u > u0 && u < above[k] {
                    above[k] = u;
                }
            }
        }
    }

    let near_contact = nearest_seed(&contact, &g);
    let dist = |k: usize, q: Option<usize>| q.map(|q| (g.at(k) - g.at(q)).length()).unwrap_or(f64::INFINITY);
    // パッドの縁までの距離は、輪郭の線分から正確に測る（はんだの外周が格子の階段にならず、縁で高さがちょうど 0 になる）
    let edge: Vec<f64> = (0..n).map(|k| distance_to_rings(&rings, g.at(k))).collect();
    let wall = |q: usize| if above[q].is_finite() { above[q] } else { DEFAULT_WALL };
    // リードの高さはパッドごとに 1 つ（接地部の 95 パーセンタイル）。点ごとの値は、板を曲げた電極などで大きくばらつき、
    // そのまま使うと盛りがぎざぎざになる。表面実装では、実際の高さはこれにはんだの体積で決まる倍率を掛けたもの
    let mut walls: Vec<f64> = (0..n).filter(|&k| contact[k]).map(wall).collect();
    walls.sort_by(f64::total_cmp);
    let lead = walls.get(((walls.len() as f64 * 0.95) as usize).min(walls.len().saturating_sub(1))).copied().unwrap_or(DEFAULT_WALL);

    // 倍率を掛ける前の形：接地部の際の高さ h0、倍率を掛けた後の h0 の上限、凹んだ形の割合 (1 − s)²、高さの上限、接地部か。
    // 表面実装の s と h0 の上限は、接地部から外へ向かう向きで縁までの幅を測る（つま先は電極の幅いっぱいに同じ高さで盛り、
    // 電極の横の狭い帯では低い。一番近い縁で測ると、つま先も横の縁に近いところが下がって、真ん中だけが尖った形になる）
    let shape: Vec<(f64, f64, f64, f64, bool)> = (0..n)
        .map(|k| {
            if !inside[k] {
                return (0.0, 0.0, 0.0, 0.0, false);
            }
            let Some(q) = near_contact[k] else { return (0.0, 0.0, 0.0, 0.0, false) };
            // 接地部までの距離は格子の点から測るので、半分の間隔を引いて境目からの距離にする
            let d = (dist(k, Some(q)) - 0.5 * cell).max(0.0);
            let (h0, e, h0_cap, cap) = match kind {
                Kind::Smd => {
                    // 接地部の点は向きが無いので、一番近い縁まで
                    let ray = distance_along(&rings, g.at(k), g.at(k) - g.at(q)).unwrap_or(edge[k]);
                    (lead, ray, SMD_SLOPE * (d + ray), SMD_EDGE_SLOPE * edge[k])
                }
                Kind::Tht { solder_side } => {
                    let slope = if solder_side { THT_SLOPE_SOLDER_SIDE } else { THT_SLOPE_COMPONENT_SIDE };
                    (lead.min(slope * (d + edge[k])).min(MAX_THT_HEIGHT), edge[k], f64::INFINITY, f64::INFINITY)
                }
            };
            if contact[k] {
                (h0, h0_cap, 1.0, cap, true)
            } else {
                let s = if d + e > 0.0 { d / (d + e) } else { 1.0 };
                (h0, h0_cap, (1.0 - s) * (1.0 - s), cap, false)
            }
        })
        .collect();
    // 高さ：倍率を掛けて、モデルの面の下に収める（パッドの外は 0）
    let clamp = |k: usize, h: f64| -> f64 {
        if !inside[k] {
            0.0
        } else if contact[k] {
            h.min(wall(k))
        } else {
            h.min(above[k] - CLEARANCE).max(0.0)
        }
    };
    let heights = |scale: f64| -> Vec<f64> {
        (0..n)
            .map(|k| {
                let (h0, h0_cap, profile, cap, _) = shape[k];
                clamp(k, ((scale * h0).min(h0_cap) * profile).min(cap))
            })
            .collect()
    };
    let scale = match kind {
        Kind::Tht { .. } => 1.0,
        Kind::Smd => {
            // はんだの体積（接地部の外）が、ペーストから残る体積に合う倍率（二分法）
            let cell_area = cell * cell;
            let target = inside.iter().filter(|&&b| b).count() as f64 * cell_area * STENCIL_MM * PASTE_METAL;
            let volume = |scale: f64| heights(scale).iter().zip(&shape).filter(|(_, sh)| !sh.4).map(|(h, _)| h * cell_area).sum::<f64>();
            if volume(MAX_WETTING) <= target {
                MAX_WETTING
            } else {
                let (mut a, mut b) = (0.0, MAX_WETTING);
                for _ in 0..30 {
                    let m = 0.5 * (a + b);
                    if volume(m) < target {
                        a = m;
                    } else {
                        b = m;
                    }
                }
                0.5 * (a + b)
            }
        }
    };
    let h = heights(scale);

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

    const LAYERS: BoardLayers = BoardLayers { pad_top: PAD_TOP, copper: 0.035, mask_on_substrate: 0.025 };

    /// 部位ごとの三角形から PartModel を作る
    fn part_model(parts: &[(Vec<[DVec3; 3]>, bool)]) -> PartModel {
        let mut m = PartModel::default();
        for (k, (tris, metal)) in parts.iter().enumerate() {
            m.tris.extend_from_slice(tris);
            m.tri_part.extend(std::iter::repeat_n(k as u32, tris.len()));
            m.part_metal.push(*metal);
        }
        m
    }

    /// 0603 の抵抗：パッド 0.8 × 0.95 mm を中心 ±0.8 mm に、金属の電極（高さ 0.45 mm、長さ 0.3 mm）を ±(0.5..0.8) mm に、
    /// その間に樹脂の本体を置く
    fn chip_board(with_model: bool) -> (Board, PartModel) {
        let layers = ["F.Cu", "F.Paste", "F.Mask"];
        let board = Board {
            footprints: vec![Footprint { reference: "R1".into(), smd: true, ..Default::default() }],
            pads: vec![pad(DVec2::new(-0.8, 0.0), DVec2::new(0.8, 0.95), PadType::Smd, &layers), pad(DVec2::new(0.8, 0.0), DVec2::new(0.8, 0.95), PadType::Smd, &layers)],
            ..Default::default()
        };
        let z0 = PAD_TOP + 0.012; // 部品は基板の面（レジストの上面）に置く。パッドの面はその 12 µm 下
        if !with_model {
            return (board, PartModel::default());
        }
        let mut terminals = Vec::new();
        for sx in [-1.0f64, 1.0] {
            let (a, b) = (sx * 0.5, sx * 0.8);
            terminals.extend(box_tris(DVec3::new(a.min(b), -0.4, z0), DVec3::new(a.max(b), 0.4, z0 + 0.45)));
        }
        // 本体（電極の間、少し浮いている）
        let body = box_tris(DVec3::new(-0.5, -0.4, z0 + 0.05), DVec3::new(0.5, 0.4, z0 + 0.45));
        (board, part_model(&[(terminals, true), (body, false)]))
    }

    /// 高さ（パッドの面から）の上限：位置 (x, z) の近くの頂点の一番上
    fn top_near(m: &Mesh, x: f32, z: f32, r: f32) -> f32 {
        m.positions.iter().filter(|p| (p.x - x).abs() < r && (p.z - z).abs() < r).map(|p| p.y - PAD_TOP as f32).fold(0.0f32, f32::max)
    }

    #[test]
    fn chip_fillets_climb_the_metal_terminals_and_fall_to_the_pad_edge() {
        let (board, model) = chip_board(true);
        let m = footprint_fillets(&board, 0, &model, LAYERS, DVec2::ZERO, 7, true, &TypeCache::default());
        assert!(m.triangle_count() > 100, "{} triangles", m.triangle_count());
        assert!(m.tri_material.iter().all(|&t| t == 7));
        // つま先（電極の端面 x = ±0.8 の外）：電極の際で高く（金属に引き上げられる）、パッドの縁（±1.2）へ下がる
        for sx in [-1.0f32, 1.0] {
            let (wall, mid, edge) = (top_near(&m, sx * 0.83, 0.0, 0.03), top_near(&m, sx * 1.0, 0.0, 0.03), top_near(&m, sx * 1.17, 0.0, 0.03));
            assert!(wall > 0.15 && wall > mid && mid > edge, "toe {sx}: {wall} {mid} {edge}");
            // 電極の上面（0.462）より上には行かない
            assert!(wall < 0.5, "{wall}");
        }
        // パッドの外（レジストの上）へは広がらない
        assert!(m.positions.iter().all(|p| p.x.abs() < 1.2 + 0.05 && p.z.abs() < 0.475 + 0.05), "spilled");
        // 三角形の向きは頂点の法線とそろう（外向き）
        for t in m.indices.chunks_exact(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.positions[i as usize]);
            let n = m.normals[t[0] as usize] + m.normals[t[1] as usize] + m.normals[t[2] as usize];
            assert!((b - a).cross(c - a).dot(n) >= 0.0);
        }
    }

    #[test]
    fn solder_does_not_climb_the_plastic_body() {
        let (board, model) = chip_board(true);
        let m = footprint_fillets(&board, 0, &model, LAYERS, DVec2::ZERO, 0, true, &TypeCache::default());
        // 本体の下（|x| < 0.5）、本体の底（0.012 + 0.05 mm）より上には、はんだの面が無い（固体の中へ押し込んだ分の数セルを除く）
        let z_body = 0.012 + 0.05 + 0.05;
        for p in m.positions.iter().filter(|p| p.x.abs() < 0.45 && p.z.abs() < 0.38) {
            assert!(p.y - (PAD_TOP as f32) <= z_body as f32, "{p:?}");
        }
    }

    #[test]
    fn no_fillet_without_a_model_or_paste() {
        let (board, model) = chip_board(true);
        assert_eq!(footprint_fillets(&board, 0, &PartModel::default(), LAYERS, DVec2::ZERO, 0, true, &TypeCache::default()).triangle_count(), 0);
        // ペーストの無いパッド（テストポイントなど）
        let mut no_paste = board.clone();
        for p in &mut no_paste.pads {
            p.layers.retain(|l| l != "F.Paste");
        }
        assert_eq!(footprint_fillets(&no_paste, 0, &model, LAYERS, DVec2::ZERO, 0, true, &TypeCache::default()).triangle_count(), 0);
    }

    #[test]
    fn a_lifted_part_leaves_a_low_dome_on_the_pad() {
        // 浮いた部品（パッドから 0.5 mm 上）：はんだは電極に届かず、パッドの上で低い丘になる
        let (board, model) = chip_board(true);
        let lifted = PartModel { tris: model.tris.iter().map(|t| t.map(|p| p + DVec3::new(0.0, 0.0, 0.5))).collect(), ..model };
        let m = footprint_fillets(&board, 0, &lifted, LAYERS, DVec2::ZERO, 0, true, &TypeCache::default());
        assert!(m.triangle_count() > 0);
        let hi = m.positions.iter().map(|p| p.y - PAD_TOP as f32).fold(0.0f32, f32::max);
        assert!(hi < 0.2, "dome height {hi}");
    }

    #[test]
    fn back_side_parts_get_fillets_below_the_board() {
        let (mut board, model) = chip_board(true);
        board.footprints[0].back = true;
        for p in &mut board.pads {
            p.layers = vec!["B.Cu".into(), "B.Paste".into(), "B.Mask".into()];
        }
        let mirrored = PartModel { tris: model.tris.iter().map(|t| t.map(|p| DVec3::new(p.x, p.y, -p.z))).collect(), ..model };
        let m = footprint_fillets(&board, 0, &mirrored, LAYERS, DVec2::ZERO, 0, true, &TypeCache::default());
        assert!(m.triangle_count() > 100);
        // はんだは基板の下（パッドの面より下。銅の中へ押し込んだ分を除く）、つま先は下へ盛り上がる
        assert!(m.positions.iter().all(|p| p.y <= -(PAD_TOP as f32) + 0.05));
        assert!(m.positions.iter().any(|p| p.y < -(PAD_TOP as f32) - 0.15));
        for t in m.indices.chunks_exact(3) {
            let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.positions[i as usize]);
            let n = m.normals[t[0] as usize] + m.normals[t[1] as usize] + m.normals[t[2] as usize];
            assert!((b - a).cross(c - a).dot(n) >= 0.0);
        }
    }

    /// 同じ部品を回して動かしても、はんだは 1 回だけ解き（フットプリントの座標系で使い回す）、形はその位置と向きへ動いたものになる
    #[test]
    fn a_rotated_copy_reuses_the_solution() {
        let (mut board, model) = chip_board(true);
        let (offset, angle) = (DVec2::new(5.0, 3.0), 90.0);
        let mut fp1 = board.footprints[0].clone();
        fp1.pos = offset;
        fp1.angle_deg = angle;
        board.footprints.push(fp1);
        let copies: Vec<Pad> = board.pads.iter().map(|p| Pad { pos: offset + rotate_point(p.pos, angle), angle_deg: p.angle_deg + angle, footprint: Some(1), ..p.clone() }).collect();
        board.pads.extend(copies);
        let moved = |p: DVec3| {
            let q = offset + rotate_point(DVec2::new(p.x, p.y), angle);
            DVec3::new(q.x, q.y, p.z)
        };
        let model1 = PartModel { tris: model.tris.iter().map(|t| t.map(moved)).collect(), ..model.clone() };
        let types = TypeCache::default();
        let m0 = footprint_fillets(&board, 0, &model, LAYERS, DVec2::ZERO, 0, true, &types);
        let m1 = footprint_fillets(&board, 1, &model1, LAYERS, DVec2::ZERO, 0, true, &types);
        assert_eq!(types.map.lock().unwrap().len(), 1, "solved once");
        assert_eq!(m0.positions.len(), m1.positions.len());
        for (a, b) in m0.positions.iter().zip(&m1.positions) {
            let q = moved(DVec3::new(a.x as f64, a.z as f64, a.y as f64));
            assert!((q.x as f32 - b.x).abs() < 1e-4 && (q.y as f32 - b.z).abs() < 1e-4 && (a.y - b.y).abs() < 1e-6, "{a:?} -> {b:?}");
        }
    }

    /// 標準の精度（0.2.1 の決め打ちの形）：つま先は電極の際で高く、パッドの縁へ下がり、本体の下より上には出ない
    #[test]
    fn standard_precision_uses_the_shaped_profile() {
        let (board, model) = chip_board(true);
        let m = footprint_fillets(&board, 0, &model, LAYERS, DVec2::ZERO, 0, false, &TypeCache::default());
        assert!(m.triangle_count() > 100);
        let at = |x: f32| m.positions.iter().filter(|p| (p.x - x).abs() < 0.02 && p.z.abs() < 0.1).map(|p| p.y - PAD_TOP as f32).fold(0.0f32, f32::max);
        assert!(at(0.85) > 0.15 && at(0.85) > at(1.0) && at(1.0) > at(1.15), "{} {} {}", at(0.85), at(1.0), at(1.15));
        for p in m.positions.iter().filter(|p| p.x.abs() < 0.48 && p.z.abs() < 0.38) {
            assert!(p.y - (PAD_TOP as f32) <= 0.062, "{p:?}");
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
        let model = part_model(&[(box_tris(DVec3::new(-0.3, -0.3, -PAD_TOP - 2.0), DVec3::new(0.3, 0.3, PAD_TOP + 3.0)), true)]);
        let m = footprint_fillets(&board, 0, &model, LAYERS, DVec2::ZERO, 0, true, &TypeCache::default());
        let bottom: Vec<_> = m.positions.iter().filter(|p| p.y < 0.0).collect();
        let top: Vec<_> = m.positions.iter().filter(|p| p.y > 0.0).collect();
        assert!(!bottom.is_empty() && !top.is_empty(), "{} below, {} above", bottom.len(), top.len());
        // はんだ面（裏）の方が高く盛る。ピンの際で高く、ランドの縁で低い（円すい）
        let height = |ps: &[&glam::Vec3], r0: f32, r1: f32| ps.iter().filter(|p| (p.x * p.x + p.z * p.z).sqrt() >= r0 && (p.x * p.x + p.z * p.z).sqrt() < r1).map(|p| p.y.abs() - PAD_TOP as f32).fold(0.0f32, f32::max);
        let (near, far) = (height(&bottom, 0.3, 0.45), height(&bottom, 0.7, 0.8));
        assert!(near > 0.3 && far < 0.5 * near, "solder side: near the pin {near}, at the rim {far}");
        assert!(height(&top, 0.3, 0.45) < near, "the component side is lower");
    }
}
