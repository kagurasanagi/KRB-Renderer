//! レンダラに渡すシーン（メッシュ・材質・配置・基板の距離場）を、基板と部品から組み立てる

use std::collections::{BTreeMap, HashMap};
use std::f64::consts::PI;
use std::path::PathBuf;
use std::sync::Arc;

use glam::{DMat4, DVec3, Mat4, Vec3, Vec4};
use krbr_core::{Bounds3, Mesh};
use krbr_geom::{build_board_geometry, shapes, tiles::build_relief_tiles, BoardBuildOptions, BoardGeometry, BoardMat, LayerArtOptions, ReliefMat, ReliefParams, ReliefTileCache, Vec2};
use krbr_kicad::{rotate_point, Board, Footprint, GrKind, Model3D, ModelResolver};
use krbr_model::{ModelLibrary, ModelMesh};

use crate::mask_presets::find_mask_preset;
use crate::material::{srgb_to_linear, BumpPattern, Material, MaterialKind};
use crate::model_map::{MaterialQuery, MaterialResolver};

#[derive(Debug, Clone, Default)]
pub struct SceneMesh {
    pub name: String,
    /// tri_material は Scene::materials の番号（モデルは部位の番号。インスタンスの material_base を足す）
    pub mesh: Mesh,
}

#[derive(Debug, Clone, Copy)]
pub struct Instance {
    pub mesh: u32,
    /// オブジェクト -> ワールド
    pub xform: Mat4,
    /// 材質の番号 = material_base + 三角形の tri_material（基板などは 0）
    pub material_base: u32,
}

/// 部品の部位ごとの材質の割り当て（krbr material show 用）
#[derive(Debug, Clone, Default)]
pub struct MaterialAssignment {
    pub reference: String,
    pub footprint: String,
    pub model_spec: String,
    pub model_file: String,
    pub model_hash: String,
    /// 部位のキー（空白区切り）
    pub part_keys: String,
    pub material: String,
    pub source: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BoardMaterialIds {
    pub top: u32,
    pub bottom: u32,
    pub edge: u32,
    pub plated: u32,
    pub mask: u32,
    pub copper: u32,
    pub finish: u32,
    pub fr4: u32,
    pub silk: u32,
    pub mask_on_fr4: u32,
    pub mask_on_copper: u32,
}

#[derive(Debug, Clone, Default)]
pub struct MissingModel {
    pub reference: String,
    pub footprint: String,
    pub value: String,
    /// ファイルに書かれた指定
    pub spec: String,
    /// 展開・解決を試みたパス
    pub resolved: String,
    pub reason: String,
}

/// レジストの厚み t(x) のモデル（mm）。shaders/pathtrace.comp の maskThickness と対応
#[derive(Debug, Clone, Copy)]
pub struct MaskModel {
    pub on_copper: f32,
    pub on_substrate: f32,
    /// 銅の縁の上（流れて薄くなる）
    pub at_edge: f32,
    /// 縁から中央の厚みに近づく距離
    pub falloff: f32,
    /// 銅の壁ぎわにたまる分
    pub pool: f32,
    pub pool_falloff: f32,
    pub copper: f32,
}

impl Default for MaskModel {
    fn default() -> Self {
        Self { on_copper: 0.012, on_substrate: 0.025, at_edge: 0.004, falloff: 0.06, pool: 0.010, pool_falloff: 0.05, copper: 0.035 }
    }
}

#[derive(Debug, Default)]
pub struct Scene {
    pub meshes: Vec<SceneMesh>,
    pub materials: Vec<Material>,
    pub instances: Vec<Instance>,
    pub board: BoardGeometry,
    /// 距離場の格子（タイルのキャッシュと同じ）
    pub sdf_grid: krbr_geom::Grid,
    /// 基板全体の距離場（RG8：R = 銅, G = レジスト開口）。表・裏
    pub sdf_image: [Vec<u8>; 2],
    /// 作り直したタイルの（面, 距離場の画素の矩形 [x0, y0, x1, y1]）。GPU にはこの範囲だけ送る
    pub sdf_dirty: Vec<(usize, [usize; 4])>,
    pub mask: MaskModel,
    pub board_mats: BoardMaterialIds,
    pub missing: Vec<MissingModel>,
    pub assignments: Vec<MaterialAssignment>,
    pub model_instances: usize,
    pub placeholders: usize,
    /// 裏で分割中の 3D モデル（箱で仮表示中）
    pub pending_models: usize,
    pub bounds: Bounds3,
    /// カメラの自動調整に使う点
    pub framing_points: Vec<Vec3>,
}

#[derive(Debug, Clone)]
pub struct SceneOptions {
    /// auto / ENIG / HASL / OSP
    pub finish: String,
    /// sRGB
    pub mask_color: Vec3,
    /// false なら stackup の色を使う
    pub mask_color_set: bool,
    /// 使ったプリセットの名前（表示用）
    pub mask_preset: String,
    pub mask_roughness: f32,
    /// レジストの顔料自身が散乱する割合
    pub mask_scatter: f32,
    pub silk_color: Vec3,
    pub silk_color_set: bool,
    pub fr4_color: Vec3,
    /// 代替の箱の高さ（mm）。0 なら面積から決める（placeholder_height_for）
    pub placeholder_height: f64,
    /// 代替の水晶の、中を 5 mm 進んだときの透過の色（sRGB）
    pub placeholder_color: Vec3,
    pub include_dnp: bool,
    pub layer_art: LayerArtOptions,
    /// 銅・レジスト・シルクの厚みを形状として作る
    pub relief: bool,
    pub relief_copper_from_stackup: bool,
    pub relief_params: ReliefParams,
    /// 表面の凹凸（バンプ）の強さの倍率。0 で無効（GPU へ送るときに掛ける）
    pub bump_intensity: f32,
}

impl Default for SceneOptions {
    fn default() -> Self {
        Self {
            finish: "auto".into(),
            mask_color: Vec3::new(0.05, 0.42, 0.16),
            mask_color_set: false,
            mask_preset: String::new(),
            mask_roughness: 0.18,
            mask_scatter: 0.3,
            silk_color: Vec3::new(0.93, 0.93, 0.90),
            silk_color_set: false,
            fr4_color: Vec3::new(0.62, 0.58, 0.42),
            placeholder_height: 0.0,
            placeholder_color: Vec3::new(0.92, 0.96, 1.0),
            include_dnp: true,
            layer_art: LayerArtOptions::default(),
            relief: true,
            relief_copper_from_stackup: true,
            relief_params: ReliefParams::default(),
            bump_intensity: 0.25,
        }
    }
}

pub struct SceneContext<'a> {
    pub board: &'a Board,
    pub resolver: &'a ModelResolver,
    pub library: &'a ModelLibrary,
    pub materials: &'a MaterialResolver<'a>,
    pub options: &'a SceneOptions,
    /// 保存の監視中は、前回のタイルを使い回す
    pub relief_cache: Option<&'a mut ReliefTileCache>,
    /// 未分割の STEP は裏で分割し、それまでは箱で仮表示する
    pub async_models: bool,
}

fn add_material(s: &mut Scene, m: Material) -> u32 {
    s.materials.push(m);
    (s.materials.len() - 1) as u32
}

/// KiCad の stackup の色名（"Green" など。プリセットと同じ表）または "#RRGGBBAA"
fn stackup_color(name: &str, fallback: Vec3) -> Vec3 {
    if let Some(p) = find_mask_preset(name) {
        return p.srgb;
    }
    if name.len() >= 7 && name.starts_with('#') {
        let hex = |i: usize| u8::from_str_radix(&name[i..i + 2], 16).map(|v| v as f32 / 255.0);
        if let (Ok(r), Ok(g), Ok(b)) = (hex(1), hex(3), hex(5)) {
            return Vec3::new(r, g, b);
        }
    }
    fallback
}

fn resolved_mask_color(board: &Board, o: &SceneOptions) -> Vec3 {
    if o.mask_color_set {
        o.mask_color
    } else {
        stackup_color(&board.stackup.mask_color_front, o.mask_color)
    }
}

fn make_mask_material(mask_srgb: Vec3, roughness: f32, scatter: f32) -> Material {
    Material {
        name: "solder_mask".into(),
        // コートとして使う：coat_color がレジストを通る光の色、base_* は顔料自身の散乱
        coat_weight: 1.0,
        // 設定の色は「銅の上で見えるレジストの色」とし、銅の平均反射率（約 0.7）で割って透過色にする
        coat_color: (srgb_to_linear(mask_srgb) / 0.7).min(Vec3::ONE),
        coat_roughness: roughness,
        coat_ior: 1.5,
        base_color: srgb_to_linear(mask_srgb) * 0.6,
        base_weight: scatter, // シェーダではこの値を「散乱を選ぶ確率」として使う
        specular_roughness: 0.5,
        bump_pattern: BumpPattern::Noise, // 塗膜のゆず肌
        bump_strength: 0.006,
        bump_scale: 0.45,
        roughness_variation: 0.1,
        ..Default::default()
    }
}

fn make_silk_material(silk_srgb: Vec3) -> Material {
    Material {
        name: "silkscreen".into(),
        base_color: srgb_to_linear(silk_srgb),
        specular_roughness: 0.65,
        bump_pattern: BumpPattern::Noise, // インクのざらつき
        bump_strength: 0.2,
        bump_scale: 0.06,
        roughness_variation: 0.3,
        ..Default::default()
    }
}

fn setup_board_materials(s: &mut Scene, board: &Board, o: &SceneOptions) {
    let top = Material { name: "board_top".into(), kind: MaterialKind::BoardSurface, board_side: 0, ..Default::default() };
    let bottom = Material { name: "board_bottom".into(), board_side: 1, ..top.clone() };

    let fr4 = Material {
        name: "fr4".into(),
        base_color: srgb_to_linear(o.fr4_color),
        specular_roughness: 0.55,
        specular_ior: 1.5,
        // 透光性：拡散して中へ入り、吸収されながら進む
        subsurface_weight: 0.15,
        subsurface_color: Vec3::new(0.85, 0.72, 0.38),
        subsurface_radius: 1.0,
        bump_pattern: BumpPattern::Weave, // ガラス繊維の織り
        bump_strength: 0.12,
        bump_scale: 0.45,
        roughness_variation: 0.25,
        ..Default::default()
    };

    let copper = Material {
        name: "copper".into(),
        base_color: Vec3::new(0.955, 0.638, 0.538),
        base_metalness: 1.0,
        specular_roughness: 0.25,
        bump_pattern: BumpPattern::Noise,
        bump_strength: 0.015,
        bump_scale: 0.05,
        roughness_variation: 0.3,
        ..Default::default()
    };

    // 表面処理：設定 > stackup の copper_finish > ENIG
    let mut finish = o.finish.clone();
    if finish == "auto" {
        let f = &board.stackup.copper_finish;
        finish = if f.contains("HAL") || f.contains("HASL") || f.contains("Silver") {
            "HASL".into()
        } else if f.contains("OSP") {
            "OSP".into()
        } else {
            "ENIG".into()
        };
    }
    let fin = match finish.as_str() {
        "HASL" => Material {
            name: "finish_HASL".into(),
            base_metalness: 1.0,
            bump_pattern: BumpPattern::Noise,
            base_color: Vec3::new(0.80, 0.80, 0.79),
            specular_roughness: 0.22,
            bump_strength: 0.10,
            bump_scale: 0.6,
            roughness_variation: 0.2,
            ..Default::default()
        },
        "OSP" => Material { name: "finish_OSP".into(), specular_roughness: 0.3, ..copper.clone() },
        _ => Material {
            name: format!("finish_{finish}"),
            base_metalness: 1.0,
            bump_pattern: BumpPattern::Noise,
            // 無電解ニッケル金：細かい粒状感
            base_color: Vec3::new(1.0, 0.766, 0.336),
            specular_roughness: 0.18,
            bump_strength: 0.012,
            bump_scale: 0.05,
            roughness_variation: 0.2,
            ..Default::default()
        },
    };

    let mask_srgb = resolved_mask_color(board, o);
    let mask = make_mask_material(mask_srgb, o.mask_roughness, o.mask_scatter);
    let silk = make_silk_material(o.silk_color);

    s.board_mats.top = add_material(s, top);
    s.board_mats.bottom = add_material(s, bottom);
    s.board_mats.fr4 = add_material(s, fr4);
    s.board_mats.edge = s.board_mats.fr4;
    s.board_mats.copper = add_material(s, copper);
    s.board_mats.finish = add_material(s, fin);
    s.board_mats.plated = s.board_mats.finish;
    s.board_mats.mask = add_material(s, mask);
    s.board_mats.silk = add_material(s, silk);
    let masked_fr4 = Material { name: "solder_mask_on_fr4".into(), kind: MaterialKind::Masked, masked_base: s.board_mats.fr4, masked_mask: s.board_mats.mask, ..Default::default() };
    let masked_cu = Material { name: "solder_mask_on_copper".into(), masked_base: s.board_mats.copper, ..masked_fr4.clone() };
    s.board_mats.mask_on_fr4 = add_material(s, masked_fr4);
    s.board_mats.mask_on_copper = add_material(s, masked_cu);
    krbr_core::info!(
        "board materials: finish {}, mask sRGB ({:.2}, {:.2}, {:.2}){}",
        finish,
        mask_srgb.x,
        mask_srgb.y,
        mask_srgb.z,
        if o.mask_preset.is_empty() { String::new() } else { format!(" [{}]", o.mask_preset) }
    );
}

/// レジストとシルクの材質だけを設定に合わせて作り直す（ビューアでの色の切り替え用）
pub fn update_mask_material(s: &mut Scene, board: &Board, o: &SceneOptions) {
    let (mi, si) = (s.board_mats.mask as usize, s.board_mats.silk as usize);
    if mi >= s.materials.len() || si >= s.materials.len() {
        return;
    }
    s.materials[mi] = make_mask_material(resolved_mask_color(board, o), o.mask_roughness, o.mask_scatter);
    s.materials[si] = make_silk_material(o.silk_color);
}

/// KiCad の 3D ビューアと同じ並びで部品・モデルの変換を作る（KiCad 3D 座標、mm、Z 上向き）
fn kicad_model_matrix(fp: &Footprint, m: &Model3D, thickness: f64) -> DMat4 {
    let mut mat = DMat4::from_translation(DVec3::new(fp.pos.x, -fp.pos.y, if fp.back { -thickness / 2.0 } else { thickness / 2.0 }));
    mat *= DMat4::from_axis_angle(DVec3::Z, fp.angle_deg.to_radians());
    if fp.back {
        mat *= DMat4::from_axis_angle(DVec3::Y, PI);
        mat *= DMat4::from_axis_angle(DVec3::Z, PI);
    }
    mat *= DMat4::from_translation(m.offset);
    mat *= DMat4::from_axis_angle(DVec3::Z, (-m.rotate.z).to_radians());
    mat *= DMat4::from_axis_angle(DVec3::Y, (-m.rotate.y).to_radians());
    mat *= DMat4::from_axis_angle(DVec3::X, (-m.rotate.x).to_radians());
    mat *= DMat4::from_scale(m.scale);
    mat
}

/// KiCad 3D 座標（x, y=上が正, z=基板の上方向）-> ワールド（X = x - cx, Y = z, Z = -y - cy）
fn kicad_to_world(center: Vec2) -> DMat4 {
    DMat4::from_cols(
        glam::DVec4::new(1.0, 0.0, 0.0, 0.0),
        glam::DVec4::new(0.0, 0.0, -1.0, 0.0),
        glam::DVec4::new(0.0, 1.0, 0.0, 0.0),
        glam::DVec4::new(-center.x, 0.0, -center.y, 1.0),
    )
}

/// 縁を 45° に面取りした箱（中心が原点、半分の大きさ h、面取りの幅 r）。凸な立体で、三角形は外向き
fn beveled_box(h: Vec3, r: f32, mat: u32) -> Mesh {
    let mut m = Mesh::default();
    let r = r.min(h.x * 0.95).min(h.y * 0.95).min(h.z * 0.95);
    // 角 s（各成分 ±1）の近くの 3 点：軸 k の面の上にある点
    let p = |s: Vec3, k: usize| {
        let mut p = s * (h - Vec3::splat(r));
        p[k] = s[k] * h[k];
        p
    };
    let mut poly = |mut v: Vec<Vec3>| {
        let c = v.iter().sum::<Vec3>() / v.len() as f32;
        let mut n = (v[1] - v[0]).cross(v[2] - v[0]).normalize();
        if n.dot(c) < 0.0 {
            v.reverse();
            n = -n;
        }
        let base = m.positions.len() as u32;
        for q in &v {
            m.add_vertex(*q, n);
        }
        for i in 1..v.len() as u32 - 1 {
            m.add_triangle(base, base + i, base + i + 1, mat);
        }
    };
    for k in 0..3 {
        let (a, b) = ((k + 1) % 3, (k + 2) % 3);
        for sk in [-1.0f32, 1.0] {
            let mut f = Vec::new();
            for (sa, sb) in [(1.0f32, 1.0f32), (1.0, -1.0), (-1.0, -1.0), (-1.0, 1.0)] {
                let mut s = Vec3::ZERO;
                s[k] = sk;
                s[a] = sa;
                s[b] = sb;
                f.push(p(s, k));
            }
            poly(f);
            for sa in [-1.0f32, 1.0] {
                let (mut s1, mut s2) = (Vec3::ZERO, Vec3::ZERO);
                s1[k] = sk;
                s2[k] = sk;
                s1[a] = sa;
                s2[a] = sa;
                s1[b] = 1.0;
                s2[b] = -1.0;
                poly(vec![p(s1, k), p(s2, k), p(s2, a), p(s1, a)]);
            }
        }
    }
    for i in 0..8 {
        let s = Vec3::new(if i & 1 != 0 { 1.0 } else { -1.0 }, if i & 2 != 0 { 1.0 } else { -1.0 }, if i & 4 != 0 { 1.0 } else { -1.0 });
        poly(vec![p(s, 0), p(s, 1), p(s, 2)]);
    }
    m
}

/// 部品の外形線（Courtyard）またはパッドから、部品座標系（未回転）での範囲を求める
fn footprint_local_box(board: &Board, fp_index: usize) -> Option<(Vec2, Vec2)> {
    let fp = &board.footprints[fp_index];
    let crt = if fp.back { "B.CrtYd" } else { "F.CrtYd" };
    let mut pts: Vec<Vec2> = Vec::new();
    let local = |p: Vec2| rotate_point(p - fp.pos, -fp.angle_deg);
    for g in &board.graphics {
        if g.footprint != Some(fp_index) || g.layer != crt {
            continue;
        }
        match g.kind {
            GrKind::Line => {
                pts.push(local(g.start));
                pts.push(local(g.end));
            }
            GrKind::Arc => {
                for p in shapes::arc_points(g.start, g.mid, g.end, 0.01) {
                    pts.push(local(p));
                }
            }
            GrKind::Circle => {
                for p in shapes::circle_points(g.start, (g.end - g.start).length(), 0.01) {
                    pts.push(local(p));
                }
            }
            GrKind::Poly | GrKind::Curve => {
                for &p in &g.pts {
                    pts.push(local(p));
                }
            }
        }
    }
    let bounds = |pts: &[Vec2]| -> Option<(Vec2, Vec2)> {
        let (mut lo, mut hi) = (Vec2::splat(1e30), Vec2::splat(-1e30));
        for p in pts {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        (lo.x <= hi.x).then_some((lo, hi))
    };
    if let Some(b) = bounds(&pts) {
        return Some(b);
    }
    for p in &board.pads {
        if p.footprint != Some(fp_index) {
            continue;
        }
        for path in shapes::pad_to_paths(p, 0.0).iter() {
            for q in shapes::path_points(path) {
                pts.push(local(q));
            }
        }
    }
    bounds(&pts).map(|(lo, hi)| (lo - Vec2::splat(0.1), hi + Vec2::splat(0.1)))
}

/// 代替の箱を置く行列：部品の向きに回し、基板の表（裏）の面の上（下）に置く
/// 代替の箱の高さ（mm）：設定（> 0）があればそれ。無ければ面積が大きいほど高く（√面積 × 0.25、0.3〜5 mm）。
/// 0603 の抵抗くらいで 0.3、5 x 4 mm の IC で約 1.1、10 x 10 mm で 2.5、20 x 20 mm 以上で 5
pub fn placeholder_height_for(fixed: f64, lo: Vec2, hi: Vec2) -> f64 {
    if fixed > 0.0 {
        return fixed;
    }
    let area = ((hi.x - lo.x) * (hi.y - lo.y)).max(0.0);
    (0.25 * area.sqrt()).clamp(0.3, 5.0)
}

fn placeholder_matrix(fp: &Footprint, lo: Vec2, hi: Vec2, height: f64, thickness: f64, center: Vec2) -> Mat4 {
    let ex = rotate_point(Vec2::X, fp.angle_deg);
    let ez = rotate_point(Vec2::Y, fp.angle_deg);
    let c = fp.pos + rotate_point((lo + hi) * 0.5, fp.angle_deg);
    const LIFT: f64 = 0.01; // 基板の面とぴったり重ならないように少し浮かせる
    let yc = (thickness / 2.0 + LIFT + height / 2.0) * if fp.back { -1.0 } else { 1.0 };
    Mat4::from_cols(
        Vec4::new(ex.x as f32, 0.0, ex.y as f32, 0.0),
        Vec4::new(0.0, 1.0, 0.0, 0.0),
        Vec4::new(ez.x as f32, 0.0, ez.y as f32, 0.0),
        Vec4::new((c.x - center.x) as f32, yc as f32, (c.y - center.y) as f32, 1.0),
    )
}

fn add_instance_bounds(s: &mut Scene, mesh: u32, xf: &Mat4) {
    let b = s.meshes[mesh as usize].mesh.bounds();
    if !b.valid() {
        return;
    }
    for i in 0..8 {
        let c = Vec3::new(if i & 1 != 0 { b.hi.x } else { b.lo.x }, if i & 2 != 0 { b.hi.y } else { b.lo.y }, if i & 4 != 0 { b.hi.z } else { b.lo.z });
        let w = xf.transform_point3(c);
        s.bounds.add(w);
        s.framing_points.push(w);
    }
}

pub fn build_scene(ctx: SceneContext<'_>) -> Scene {
    let board = ctx.board;
    let opts = ctx.options;
    let mut s = Scene::default();
    setup_board_materials(&mut s, board, opts);

    // 基板
    let mut rp = opts.relief_params;
    if opts.relief_copper_from_stackup && board.stackup.copper_thickness > 0.0 {
        rp.copper = board.stackup.copper_thickness;
    }
    let bo = if opts.relief {
        BoardBuildOptions { core_half: rp.core_half(board.stackup.thickness), caps: false }
    } else {
        BoardBuildOptions::default()
    };
    s.board = build_board_geometry(board, &bo);
    s.mask.on_copper = rp.mask_on_copper as f32;
    s.mask.on_substrate = rp.mask_on_substrate as f32;
    s.mask.copper = rp.copper as f32;
    s.mask.at_edge = s.mask.at_edge.min(s.mask.on_copper);
    {
        let mut bm = SceneMesh { name: "board".into(), mesh: s.board.mesh.clone() };
        let remap = [s.board_mats.top, s.board_mats.bottom, s.board_mats.edge, s.board_mats.plated];
        for m in &mut bm.mesh.tri_material {
            *m = remap[(*m as usize).min(3)];
        }
        s.meshes.push(bm);
        s.instances.push(Instance { mesh: 0, xform: Mat4::IDENTITY, material_base: 0 });
        add_instance_bounds(&mut s, 0, &Mat4::IDENTITY);
    }
    if opts.relief {
        let mut local = ReliefTileCache::default();
        let cache: &mut ReliefTileCache = match ctx.relief_cache {
            Some(c) => c,
            None => &mut local,
        };
        build_relief_tiles(cache, board, &s.board, &rp, &opts.layer_art);
        let rmap = [s.board_mats.fr4, s.board_mats.finish, s.board_mats.mask_on_fr4, s.board_mats.mask_on_copper, s.board_mats.silk];
        for side in 0..2 {
            for t in 0..cache.count() {
                let m = &cache.tiles[side][t];
                if m.triangle_count() == 0 {
                    continue;
                }
                let mut mesh = m.clone();
                for id in &mut mesh.tri_material {
                    *id = rmap[(*id as usize).min(4)];
                }
                let id = s.meshes.len() as u32;
                s.meshes.push(SceneMesh { name: format!("relief_{}_{}", if side == 0 { "front" } else { "back" }, t), mesh });
                s.instances.push(Instance { mesh: id, xform: Mat4::IDENTITY, material_base: 0 });
            }
        }
        s.sdf_grid = cache.sdf_grid;
        s.sdf_image = [cache.sdf_image[0].clone(), cache.sdf_image[1].clone()];
        s.sdf_dirty = cache
            .last_rebuilt
            .iter()
            .map(|&(side, t)| {
                let (x0, y0, x1, y1) = cache.tile_pixels(t);
                (side, [x0, y0, x1, y1])
            })
            .collect();
        let _ = ReliefMat::Fr4; // 材質の番号は rmap で写した
        let _ = BoardMat::Top;
    } else {
        krbr_core::warn!("relief = false (layer textures) is not supported in the Rust port yet; rendering the bare board");
    }

    // 代替表示の箱：縁を面取りした透明な水晶（石英：nd 1.544、アッベ数 70）
    let ph = Material {
        name: "placeholder".into(),
        base_color: Vec3::ONE,
        specular_roughness: 0.0,
        specular_ior: 1.544,
        transmission_weight: 1.0,
        transmission_color: srgb_to_linear(opts.placeholder_color),
        transmission_depth: 5.0,
        transmission_dispersion_scale: 1.0,
        transmission_dispersion_abbe_number: 70.0,
        ..Default::default()
    };
    let ph_mat = add_material(&mut s, ph);

    let to_world = kicad_to_world(s.board.center);
    let mut mesh_of: HashMap<*const ModelMesh, u32> = HashMap::new();
    let mut material_set_of: BTreeMap<String, u32> = BTreeMap::new();

    for (fi, fp) in board.footprints.iter().enumerate() {
        if fp.dnp && !opts.include_dnp {
            continue;
        }
        let (mut wanted, mut placed) = (0, 0);
        for m in &fp.models {
            if m.hidden || m.opacity <= 0.0 {
                continue;
            }
            wanted += 1;
            let mut miss = MissingModel { reference: fp.reference.clone(), footprint: fp.lib_id.clone(), value: fp.value.clone(), spec: m.path.clone(), ..Default::default() };
            let res = ctx.resolver.resolve(&m.path);
            miss.resolved = res.path.to_string_lossy().into_owned();
            if !res.found {
                miss.reason = res.reason;
                s.missing.push(miss);
                continue;
            }
            let lr = if ctx.async_models { ctx.library.request(&res.path) } else { ctx.library.get(&res.path) };
            if lr.pending {
                s.pending_models += 1; // 分割が終わるまで箱で仮表示（不足とは扱わない）
                continue;
            }
            let Some(model) = &lr.mesh else {
                miss.reason = lr.error;
                s.missing.push(miss);
                continue;
            };
            let model: &Arc<ModelMesh> = model;
            let key = Arc::as_ptr(model);
            let mesh_id = match mesh_of.get(&key) {
                Some(&id) => id,
                None => {
                    let mut sm = SceneMesh { name: res.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(), mesh: model.mesh.clone() };
                    let nparts = model.parts.len().max(1) as u32;
                    for t in &mut sm.mesh.tri_material {
                        *t = (*t).min(nparts - 1);
                    }
                    let id = s.meshes.len() as u32;
                    s.meshes.push(sm);
                    mesh_of.insert(key, id);
                    id
                }
            };
            // 部位ごとに材質を決める（部品番号・モデル・規則の順）
            let model_file = res.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            let mut resolved = Vec::new();
            let mut set_key = String::new();
            let nparts = model.parts.len().max(1);
            for pi in 0..nparts {
                let part = model.parts.get(pi);
                let q = MaterialQuery { reference: &fp.reference, model_spec: &m.path, model_hash: &lr.file_hash, model_file: &model_file, part };
                let r = ctx.materials.resolve(&q);
                s.assignments.push(MaterialAssignment {
                    reference: fp.reference.clone(),
                    footprint: fp.lib_id.clone(),
                    model_spec: m.path.clone(),
                    model_file: model_file.clone(),
                    model_hash: lr.file_hash.clone(),
                    part_keys: part.map(|p| p.keys().join(" ")).unwrap_or_default(),
                    material: r.name.clone(),
                    source: r.source.clone(),
                });
                let c = r.material.base_color;
                set_key += &format!("{}|{:.4},{:.4},{:.4};", r.name, c.x, c.y, c.z);
                resolved.push(r);
            }
            let material_base = match material_set_of.get(&set_key) {
                Some(&b) => b,
                None => {
                    let base = s.materials.len() as u32;
                    for mut r in resolved {
                        r.material.name = format!("{}:{}", model_file, r.name);
                        s.materials.push(r.material);
                    }
                    material_set_of.insert(set_key, base);
                    base
                }
            };
            let xf = (to_world * kicad_model_matrix(fp, m, s.board.thickness)).as_mat4();
            s.instances.push(Instance { mesh: mesh_id, xform: xf, material_base });
            add_instance_bounds(&mut s, mesh_id, &xf);
            placed += 1;
            s.model_instances += 1;
        }
        if wanted > 0 && placed == 0 {
            let (lo, hi) = footprint_local_box(board, fi).unwrap_or((Vec2::splat(-0.5), Vec2::splat(0.5)));
            let height = placeholder_height_for(opts.placeholder_height, lo, hi);
            krbr_core::debug!("placeholder {}: {:.1} x {:.1} mm, height {:.2} mm", fp.reference, hi.x - lo.x, hi.y - lo.y, height);
            let half = Vec3::new(((hi.x - lo.x) / 2.0) as f32, (height / 2.0) as f32, ((hi.y - lo.y) / 2.0) as f32);
            // 面取りの幅：0.2 mm（縁が細く光って見える程度。小さい箱では beveled_box が辺の長さに合わせて縮める）
            let bevel = 0.2;
            let box_mesh = s.meshes.len() as u32;
            s.meshes.push(SceneMesh { name: format!("placeholder_{}", fp.reference), mesh: beveled_box(half, bevel, ph_mat) });
            let xf = placeholder_matrix(fp, lo, hi, height, s.board.thickness, s.board.center);
            s.instances.push(Instance { mesh: box_mesh, xform: xf, material_base: 0 });
            add_instance_bounds(&mut s, box_mesh, &xf);
            s.placeholders += 1;
        }
    }
    // まだ描画に反映していない OpenPBR の要素を知らせる
    {
        let mut unsupported: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for m in &s.materials {
            if m.transmission_weight > 0.0 && m.geometry_thin_walled {
                unsupported.entry("thin_walled").or_default().push(m.name.clone());
            }
            if m.transmission_weight > 0.0 && m.transmission_scatter.length() > 0.0 {
                unsupported.entry("transmission_scatter").or_default().push(m.name.clone());
            }
            if m.fuzz_weight > 0.0 {
                unsupported.entry("fuzz").or_default().push(m.name.clone());
            }
            if m.thin_film_weight > 0.0 {
                unsupported.entry("thin_film").or_default().push(m.name.clone());
            }
            if m.geometry_opacity < 1.0 {
                unsupported.entry("geometry_opacity").or_default().push(m.name.clone());
            }
            if m.specular_roughness_anisotropy > 0.0 || m.coat_roughness_anisotropy > 0.0 {
                unsupported.entry("anisotropy").or_default().push(m.name.clone());
            }
        }
        for (what, names) in unsupported {
            krbr_core::warn!("not rendered yet: {} used by {} material(s), e.g. {}", what, names.len(), names[0]);
        }
    }
    krbr_core::info!(
        "scene: {} meshes, {} instances ({} models, {} placeholders), {} materials",
        s.meshes.len(),
        s.instances.len(),
        s.model_instances,
        s.placeholders,
        s.materials.len()
    );
    s
}

/// シーン中の三角形との最初の交点までの距離（Ctrl + クリックでピントを合わせる用）。Möller–Trumbore
pub fn raycast(s: &Scene, origin: Vec3, dir: Vec3) -> Option<f32> {
    let mut best = 1e30f32;
    for i in &s.instances {
        // オブジェクト座標で交差を求める（t は変換しても同じ値になる）
        let inv = i.xform.inverse();
        let o = inv.transform_point3(origin);
        let d = inv.transform_vector3(dir);
        let m = &s.meshes[i.mesh as usize].mesh;
        for t in m.indices.chunks_exact(3) {
            let (a, b, c) = (m.positions[t[0] as usize], m.positions[t[1] as usize], m.positions[t[2] as usize]);
            let (e1, e2) = (b - a, c - a);
            let p = d.cross(e2);
            let det = e1.dot(p);
            if det.abs() < 1e-12 {
                continue;
            }
            let inv_det = 1.0 / det;
            let tv = o - a;
            let u = tv.dot(p) * inv_det;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let q = tv.cross(e1);
            let v = d.dot(q) * inv_det;
            if v < 0.0 || u + v > 1.0 {
                continue;
            }
            let tt = e2.dot(q) * inv_det;
            if tt > 1e-4 && tt < best {
                best = tt;
            }
        }
    }
    (best < 1e30).then_some(best)
}

/// 読み込んだ設定ファイルなど、シーンを作るのに要るもの一式（C++ 版の PreparedScene の一部）
pub struct MaterialSetup {
    pub library: crate::material_library::MaterialLibrary,
    pub global: crate::model_map::GlobalModelMap,
    pub project: crate::model_map::ProjectOverrides,
    pub global_file: PathBuf,
    pub project_file: PathBuf,
}

/// 材質ライブラリ（組み込み＋全体設定 materials/*.toml）、全体の対応表、プロジェクトの上書きを読む
pub fn load_material_setup(pcb: &std::path::Path) -> MaterialSetup {
    let config_dir = krbr_core::platform::config_dir();
    let library = crate::material_library::MaterialLibrary::load(&config_dir);
    let global_file = config_dir.join("model_map.toml");
    let global = if global_file.exists() {
        match crate::model_map::GlobalModelMap::load(&global_file) {
            Ok(g) => {
                krbr_core::info!("model map: {}", global_file.display());
                g
            }
            Err(e) => {
                krbr_core::warn!("model map {}: {} - ignored", global_file.display(), e);
                Default::default()
            }
        }
    } else {
        Default::default()
    };
    let project_file = crate::config::project_config_dir(pcb).join("overrides.toml");
    let project = if project_file.exists() {
        match crate::model_map::ProjectOverrides::load(&project_file) {
            Ok(p) => {
                krbr_core::info!("overrides: {}", project_file.display());
                p
            }
            Err(e) => {
                krbr_core::warn!("overrides {}: {} - ignored", project_file.display(), e);
                Default::default()
            }
        }
    } else {
        Default::default()
    };
    MaterialSetup { library, global, project, global_file, project_file }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_height_grows_with_area_up_to_5mm() {
        let h = |w: f64, d: f64| placeholder_height_for(0.0, Vec2::new(-w / 2.0, -d / 2.0), Vec2::new(w / 2.0, d / 2.0));
        assert_eq!(h(1.6, 0.8), 0.3); // 小さい部品は下限
        assert!((h(10.0, 10.0) - 2.5).abs() < 1e-9);
        assert!(h(5.0, 4.0) < h(10.0, 10.0));
        assert_eq!(h(20.0, 20.0), 5.0);
        assert_eq!(h(60.0, 40.0), 5.0); // 上限
        // 設定で固定した高さはそのまま
        assert_eq!(placeholder_height_for(1.0, Vec2::ZERO, Vec2::new(30.0, 30.0)), 1.0);
    }
}
