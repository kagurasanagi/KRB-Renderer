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

use crate::mask_presets::{find_mask_preset, DEFAULT_MASK_PRESET, SILK_BLACK, SILK_WHITE};
use crate::material::{parse_hex_rgb, srgb_to_linear, BumpPattern, Material, MaterialKind};
use crate::model_map::{MaterialQuery, MaterialResolver, MaterialStage};

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
    /// 何のインスタンスか（ビューアで部品を選ぶのに使う）
    pub owner: InstanceOwner,
}

/// インスタンスの持ち主
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstanceOwner {
    /// 基板の本体・凹凸のタイル
    Board,
    /// 部品の 3D モデル：footprint は Board::footprints の番号、model はその models の番号、
    /// assignments は Scene::assignments のうちこのインスタンスの部位の行の始まり（部位の番号 = 三角形の tri_material）
    Model { footprint: u32, model: u32, assignments: u32 },
    /// 3D モデルが無い（または裏で分割中の）部品の代わりの箱
    Placeholder { footprint: u32 },
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
    /// どの段階で決まったか（表示用。MaterialStage::label）
    pub source: String,
}

impl MaterialAssignment {
    /// どの段階で決まったか。段階で処理を分けるときは source の文字列の先頭を見ずにこれを使う
    pub fn stage(&self) -> Option<MaterialStage> {
        MaterialStage::from_label(&self.source)
    }
}

/// 基板の材質の番号（Scene::materials の番号）
#[derive(Debug, Clone, Copy, Default)]
pub struct BoardMaterialIds {
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

impl BoardMaterialIds {
    /// 基板メッシュ（krbr-geom の BoardMat）の区分の材質。match で網羅するので、krbr-geom で区分を足すとここでコンパイルが止まる
    pub fn for_board(&self, m: BoardMat) -> u32 {
        match m {
            BoardMat::Edge => self.edge,
            BoardMat::Plated => self.plated,
        }
    }

    /// 凹凸メッシュ（krbr-geom の ReliefMat）の区分の材質
    pub fn for_relief(&self, m: ReliefMat) -> u32 {
        match m {
            ReliefMat::Fr4 => self.fr4,
            ReliefMat::Finish => self.finish,
            ReliefMat::MaskOnFr4 => self.mask_on_fr4,
            ReliefMat::MaskOnCopper => self.mask_on_copper,
            ReliefMat::Silk => self.silk,
        }
    }
}

/// krbr-geom がメッシュの tri_material に入れた区分の番号を enum に戻す。区分を足したら for_board / for_relief と一緒にここにも足す。
/// 知らない番号は krbr-geom との食い違い（プログラムの誤り）なので、黙って別の材質に丸めずに止める
fn board_mat_of(id: u32) -> BoardMat {
    const ALL: [BoardMat; 2] = [BoardMat::Edge, BoardMat::Plated];
    ALL.into_iter().find(|m| *m as u32 == id).unwrap_or_else(|| panic!("unknown BoardMat {id} in the board mesh"))
}

fn relief_mat_of(id: u32) -> ReliefMat {
    const ALL: [ReliefMat; 5] = [ReliefMat::Fr4, ReliefMat::Finish, ReliefMat::MaskOnFr4, ReliefMat::MaskOnCopper, ReliefMat::Silk];
    ALL.into_iter().find(|m| *m as u32 == id).unwrap_or_else(|| panic!("unknown ReliefMat {id} in a relief tile"))
}

/// 銅の表面処理。設定（[board] finish）では大小を区別しない名前で、"auto" なら KiCad の stackup の copper_finish に従う
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    Auto,
    /// 無電解ニッケル金（金色）
    Enig,
    /// はんだレベラー（銀色）。浸漬銀・浸漬錫など銀色に見える処理もこれで表す
    Hasl,
    /// 水溶性プリフラックス（銅の色のまま、つやが少し落ちる）
    Osp,
    /// 表面処理なし（銅のまま）。設定で選んだときだけ（stackup の "None" は ENIG として扱う。from_kicad）
    None,
}

impl Finish {
    pub const ALL: [Finish; 5] = [Finish::Auto, Finish::Enig, Finish::Hasl, Finish::Osp, Finish::None];

    pub fn as_str(self) -> &'static str {
        match self {
            Finish::Auto => "auto",
            Finish::Enig => "ENIG",
            Finish::Hasl => "HASL",
            Finish::Osp => "OSP",
            Finish::None => "none",
        }
    }

    /// KiCad の stackup の copper_finish（大小を区別しない）。知らない値なら None。
    /// 見た目で分ける：金色は ENIG、銀色（はんだ・錫・銀・ニッケル）は HASL、プリフラックスは OSP。
    /// 指定なし（"Not specified" / "User defined" / 空）と "None" は既定の ENIG。"None" は KiCad の stackup の既定値で、
    /// 表面処理を選んでいない基板のほとんどがこの値なので、銅のままにはしない（銅のままにするなら設定で finish = "none"）
    pub fn from_kicad(copper_finish: &str) -> Option<Finish> {
        const TABLE: &[(&str, Finish)] = &[
            ("", Finish::Enig),
            ("not specified", Finish::Enig),
            ("user defined", Finish::Enig),
            ("none", Finish::Enig),
            ("enig", Finish::Enig),
            ("enepig", Finish::Enig),
            ("hard gold", Finish::Enig),
            ("immersion gold", Finish::Enig),
            ("hal snpb", Finish::Hasl),
            ("hal lead-free", Finish::Hasl),
            ("hasl", Finish::Hasl),
            ("immersion tin", Finish::Hasl),
            ("immersion silver", Finish::Hasl),
            ("immersion nickel", Finish::Hasl),
            ("osp", Finish::Osp),
            ("ht_osp", Finish::Osp),
        ];
        let key = copper_finish.trim().to_lowercase();
        TABLE.iter().find(|(k, _)| *k == key).map(|(_, f)| *f)
    }
}

impl std::str::FromStr for Finish {
    type Err = String;
    /// 設定の名前（auto / ENIG / HASL / OSP / none。大小を区別しない）。知らない名前は Err
    fn from_str(s: &str) -> Result<Self, String> {
        Self::ALL.into_iter().find(|f| f.as_str().eq_ignore_ascii_case(s.trim())).ok_or_else(|| format!("unknown finish '{s}' (auto / ENIG / HASL / OSP / none)"))
    }
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

/// レジストの厚み t(x) のモデル（mm）。shaders/scene.glsl の maskThickness と対応
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

impl MaskModel {
    /// 厚み（銅、銅の上・基材の上のレジスト）は凹凸の形状と同じ値を使う。縁の形（流れ・たまり）はここで決める
    pub fn from_relief(rp: &ReliefParams) -> Self {
        let on_copper = rp.mask_on_copper as f32;
        Self { on_copper, on_substrate: rp.mask_on_substrate as f32, at_edge: 0.004f32.min(on_copper), falloff: 0.06, pool: 0.010, pool_falloff: 0.05, copper: rp.copper as f32 }
    }
}

impl Default for MaskModel {
    fn default() -> Self {
        Self::from_relief(&ReliefParams::default())
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
    pub relief_copper_from_stackup: bool,
    pub relief_params: ReliefParams,
    /// 表面の凹凸（バンプ）の強さの倍率。0 で無効（GPU へ送るときに掛ける）
    pub bump_intensity: f32,
}

impl Default for SceneOptions {
    fn default() -> Self {
        // レジストの既定は緑のプリセットの値（同じ値を 2 か所に持たない）。mask_preset は空（設定で選んだのではない）
        let mask = find_mask_preset(DEFAULT_MASK_PRESET).expect("the default mask preset exists");
        Self {
            finish: Finish::Auto.as_str().into(),
            mask_color: mask.srgb,
            mask_color_set: false,
            mask_preset: String::new(),
            mask_roughness: mask.roughness,
            mask_scatter: mask.scatter,
            silk_color: mask.silk_srgb,
            silk_color_set: false,
            fr4_color: Vec3::new(0.62, 0.58, 0.42),
            placeholder_height: 0.0,
            placeholder_color: Vec3::new(0.92, 0.96, 1.0),
            include_dnp: true,
            layer_art: LayerArtOptions::default(),
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

/// KiCad の stackup の色名（"Green" など。プリセットと同じ表）または "#RRGGBB" / "#RRGGBBAA"（不透明度は使わない）
fn stackup_color(name: &str, fallback: Vec3) -> Vec3 {
    if let Some(p) = find_mask_preset(name) {
        return p.srgb;
    }
    // 16 進の数字（ASCII）かを先に確かめる。以前はバイト位置で切り出していて、# の後ろが多バイト文字だと panic した
    let hex = name.strip_prefix('#').filter(|h| h.len() == 6 || h.len() == 8).filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()));
    if let Some(rgb) = hex.and_then(|h| parse_hex_rgb(&h[..6])) {
        return Vec3::from_array(rgb.map(|v| v as f32 / 255.0));
    }
    fallback
}

/// レジストの色：設定（色かプリセット）> stackup の表面のレジストの色 > 既定。
/// 裏面のレジストも同じ材質（board_mats.mask は 1 つ）なので、stackup の裏面の色（mask_color_back）は使わない
fn resolved_mask_color(board: &Board, o: &SceneOptions) -> Vec3 {
    if o.mask_color_set {
        o.mask_color
    } else {
        stackup_color(&board.stackup.mask_color_front, o.mask_color)
    }
}

/// シルクの色：設定 > レジストのプリセットに合わせた色（黄・白なら黒）> stackup の表面のシルクの色 > 既定（白）。
/// レジストと同じく、設定で選んだものを stackup より優先する。裏面も同じ材質
fn resolved_silk_color(board: &Board, o: &SceneOptions) -> Vec3 {
    if o.silk_color_set || !o.mask_preset.is_empty() {
        return o.silk_color;
    }
    // KiCad のシルクの色名の白・黒は、レジストの表ではなくシルクの色で写す
    match board.stackup.silk_color_front.trim().to_lowercase().as_str() {
        "white" => SILK_WHITE,
        "black" => SILK_BLACK,
        name => stackup_color(name, o.silk_color),
    }
}

/// 銅の色（リニア）
const COPPER_BASE_COLOR: Vec3 = Vec3::new(0.955, 0.638, 0.538);
/// 銅の平均反射率：COPPER_BASE_COLOR の 3 成分の平均（約 0.71）を丸めた値。
/// 設定のレジストの色は「銅の上で見えるレジストの色」なので、これで割ってコートの透過色にする（銅で反射して見える色が設定の色に近づく）
const COPPER_MEAN_REFLECTANCE: f32 = 0.7;
/// レジストの顔料自身が散らす光の色：設定の色にこの割合を掛ける。目分量の値（測定や文献の値ではない）
const MASK_PIGMENT_ALBEDO_SCALE: f32 = 0.6;

fn make_mask_material(mask_srgb: Vec3, roughness: f32, scatter: f32) -> Material {
    Material {
        name: "solder_mask".into(),
        // コートとして使う：coat_color がレジストを通る光の色、base_* は顔料自身の散乱
        coat_weight: 1.0,
        coat_color: (srgb_to_linear(mask_srgb) / COPPER_MEAN_REFLECTANCE).min(Vec3::ONE),
        coat_roughness: roughness,
        coat_ior: 1.5,
        base_color: srgb_to_linear(mask_srgb) * MASK_PIGMENT_ALBEDO_SCALE,
        // OpenPBR の base_weight を「顔料が光を散らす確率」に流用する（この材質だけ。shaders/scene.glsl のレジストの処理と対応）
        base_weight: scatter,
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
        base_color: COPPER_BASE_COLOR,
        base_metalness: 1.0,
        specular_roughness: 0.25,
        bump_pattern: BumpPattern::Noise,
        bump_strength: 0.015,
        bump_scale: 0.05,
        roughness_variation: 0.3,
        ..Default::default()
    };

    // 表面処理：設定 > stackup の copper_finish > ENIG
    let finish = resolved_finish(board, o);
    let fin = match finish {
        Finish::Hasl => Material {
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
        Finish::Osp => Material { name: "finish_OSP".into(), specular_roughness: 0.3, ..copper.clone() },
        Finish::None => Material { name: "finish_none".into(), ..copper.clone() },
        Finish::Enig | Finish::Auto => Material {
            name: "finish_ENIG".into(),
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
    let silk = make_silk_material(resolved_silk_color(board, o));

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
        finish.as_str(),
        mask_srgb.x,
        mask_srgb.y,
        mask_srgb.z,
        if o.mask_preset.is_empty() { String::new() } else { format!(" [{}]", o.mask_preset) }
    );
}

/// 表面処理：設定（auto 以外）> stackup の copper_finish > ENIG。知らない名前は警告して auto / ENIG として扱う
fn resolved_finish(board: &Board, o: &SceneOptions) -> Finish {
    let f = o.finish.parse::<Finish>().unwrap_or_else(|e| {
        krbr_core::warn!("finish: {e}; using auto");
        Finish::Auto
    });
    if f != Finish::Auto {
        return f;
    }
    let k = &board.stackup.copper_finish;
    Finish::from_kicad(k).unwrap_or_else(|| {
        krbr_core::warn!("unknown copper finish '{k}' in the stackup; using ENIG");
        Finish::Enig
    })
}

/// レジストとシルクの材質だけを設定に合わせて作り直す（ビューアでの色の切り替え用）
pub fn update_mask_material(s: &mut Scene, board: &Board, o: &SceneOptions) {
    let (mi, si) = (s.board_mats.mask as usize, s.board_mats.silk as usize);
    if mi >= s.materials.len() || si >= s.materials.len() {
        return;
    }
    s.materials[mi] = make_mask_material(resolved_mask_color(board, o), o.mask_roughness, o.mask_scatter);
    s.materials[si] = make_silk_material(resolved_silk_color(board, o));
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

/// 代替の箱の高さ（mm）：設定（> 0）があればそれ。無ければ面積が大きいほど高く（√面積 × 0.25、0.3〜5 mm）。
/// 0603 の抵抗くらいで 0.3、5 x 4 mm の IC で約 1.1、10 x 10 mm で 2.5、20 x 20 mm 以上で 5
pub fn placeholder_height_for(fixed: f64, lo: Vec2, hi: Vec2) -> f64 {
    if fixed > 0.0 {
        return fixed;
    }
    let area = ((hi.x - lo.x) * (hi.y - lo.y)).max(0.0);
    (0.25 * area.sqrt()).clamp(0.3, 5.0)
}

/// 代替の箱を置く行列：部品の向きに回し、基板の表（裏）の面の上（下）に置く
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
    let bo = BoardBuildOptions { core_half: rp.core_half(board.stackup.thickness) };
    s.board = build_board_geometry(board, &bo);
    s.mask = MaskModel::from_relief(&rp);
    {
        let mut bm = SceneMesh { name: "board".into(), mesh: s.board.mesh.clone() };
        for m in &mut bm.mesh.tri_material {
            *m = s.board_mats.for_board(board_mat_of(*m));
        }
        s.meshes.push(bm);
        s.instances.push(Instance { mesh: 0, xform: Mat4::IDENTITY, material_base: 0, owner: InstanceOwner::Board });
        add_instance_bounds(&mut s, 0, &Mat4::IDENTITY);
    }
    // 銅・レジスト・シルクの厚みの形状（タイルごと）と、縁からの距離場
    {
        let mut local = ReliefTileCache::default();
        let cache: &mut ReliefTileCache = match ctx.relief_cache {
            Some(c) => c,
            None => &mut local,
        };
        build_relief_tiles(cache, board, &s.board, &rp, &opts.layer_art);
        for side in 0..2 {
            for t in 0..cache.count() {
                let m = &cache.tiles[side][t];
                if m.triangle_count() == 0 {
                    continue;
                }
                let mut mesh = m.clone();
                for id in &mut mesh.tri_material {
                    *id = s.board_mats.for_relief(relief_mat_of(*id));
                }
                let id = s.meshes.len() as u32;
                s.meshes.push(SceneMesh { name: format!("relief_{}_{}", if side == 0 { "front" } else { "back" }, t), mesh });
                s.instances.push(Instance { mesh: id, xform: Mat4::IDENTITY, material_base: 0, owner: InstanceOwner::Board });
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
        for (mi, m) in fp.models.iter().enumerate() {
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
            let first_assignment = s.assignments.len() as u32;
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
            s.instances.push(Instance { mesh: mesh_id, xform: xf, material_base, owner: InstanceOwner::Model { footprint: fi as u32, model: mi as u32, assignments: first_assignment } });
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
            s.instances.push(Instance { mesh: box_mesh, xform: xf, material_base: 0, owner: InstanceOwner::Placeholder { footprint: fi as u32 } });
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

/// 光線が最初に当たったもの
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pick {
    /// 距離（mm）
    pub t: f32,
    /// Scene::instances の番号
    pub instance: usize,
    /// 当たった三角形の tri_material（部品のモデルなら部位の番号）
    pub part: u32,
}

/// シーン中の三角形との最初の交点までの距離（Ctrl + クリックでピントを合わせる用）
pub fn raycast(s: &Scene, origin: Vec3, dir: Vec3) -> Option<f32> {
    raycast_pick(s, origin, dir).map(|p| p.t)
}

/// 光線と全三角形の交差（CPU、Möller–Trumbore。ピント合わせ・部品の選択の 1 回ごとに使う程度の速さ）
pub fn raycast_pick(s: &Scene, origin: Vec3, dir: Vec3) -> Option<Pick> {
    let mut best: Option<Pick> = None;
    for (ii, i) in s.instances.iter().enumerate() {
        // オブジェクト座標で交差を求める（t は変換しても同じ値になる）
        let inv = i.xform.inverse();
        let o = inv.transform_point3(origin);
        let d = inv.transform_vector3(dir);
        let m = &s.meshes[i.mesh as usize].mesh;
        for (ti, t) in m.indices.chunks_exact(3).enumerate() {
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
            if tt > 1e-4 && best.is_none_or(|b| tt < b.t) {
                best = Some(Pick { t: tt, instance: ii, part: m.tri_material.get(ti).copied().unwrap_or(0) });
            }
        }
    }
    best
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
    // 材質の上書き（[ref.*] / [[model]]）は基板の設定ファイル <名前>.krbr の中
    let project_file = crate::board_file::resolve(pcb);
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
    fn pick_returns_the_nearest_instance_and_its_part() {
        // 部位 0 と 1 の箱を 2 つ、z 方向に並べる（手前 z = 10、奥 z = 0）
        let mut s = Scene::default();
        let mut mesh = beveled_box(Vec3::splat(1.0), 0.1, 0);
        let n = mesh.tri_material.len();
        for (i, m) in mesh.tri_material.iter_mut().enumerate() {
            *m = if i < n / 2 { 0 } else { 1 };
        }
        s.meshes.push(SceneMesh { name: "box".into(), mesh });
        let owner = |fp| InstanceOwner::Model { footprint: fp, model: 0, assignments: 0 };
        s.instances.push(Instance { mesh: 0, xform: Mat4::IDENTITY, material_base: 0, owner: owner(0) });
        s.instances.push(Instance { mesh: 0, xform: Mat4::from_translation(Vec3::new(0.0, 0.0, 10.0)), material_base: 0, owner: owner(1) });
        let p = raycast_pick(&s, Vec3::new(0.0, 0.0, 50.0), Vec3::NEG_Z).unwrap();
        assert_eq!(p.instance, 1);
        assert!((p.t - 39.0).abs() < 1e-3, "{}", p.t);
        assert!(p.part <= 1);
        assert_eq!(raycast(&s, Vec3::new(0.0, 0.0, 50.0), Vec3::NEG_Z), Some(p.t));
        // 外れる光線
        assert!(raycast_pick(&s, Vec3::new(5.0, 0.0, 50.0), Vec3::NEG_Z).is_none());
    }

    #[test]
    fn finish_names_and_kicad_values() {
        for f in Finish::ALL {
            assert_eq!(f.as_str().parse::<Finish>(), Ok(f));
        }
        assert_eq!("hasl".parse::<Finish>(), Ok(Finish::Hasl)); // 大小を区別しない
        assert!("gold".parse::<Finish>().is_err());
        assert_eq!(Finish::from_kicad("HAL lead-free"), Some(Finish::Hasl));
        assert_eq!(Finish::from_kicad("Immersion silver"), Some(Finish::Hasl));
        assert_eq!(Finish::from_kicad("Immersion tin"), Some(Finish::Hasl)); // 以前は金になっていた
        assert_eq!(Finish::from_kicad("None"), Some(Finish::Enig)); // KiCad の既定値。選んでいないのと同じ扱い
        assert_eq!(Finish::from_kicad("HT_OSP"), Some(Finish::Osp));
        assert_eq!(Finish::from_kicad("ENEPIG"), Some(Finish::Enig));
        assert_eq!(Finish::from_kicad(""), Some(Finish::Enig));
        assert_eq!(Finish::from_kicad("Something new"), None);
    }

    #[test]
    fn stackup_colors() {
        let fb = Vec3::splat(0.5);
        assert_eq!(stackup_color("#FF000080", fb), Vec3::new(1.0, 0.0, 0.0));
        assert_eq!(stackup_color("#00FF00", fb), Vec3::new(0.0, 1.0, 0.0));
        assert_eq!(stackup_color("Green", fb), find_mask_preset("green").unwrap().srgb);
        // 多バイト文字・16 進でない文字・長さの違いは使わない（以前は多バイト文字で panic した）
        for bad in ["#あいう", "#ab\u{3042}cdef", "#GG0000", "#12345", "Not specified"] {
            assert_eq!(stackup_color(bad, fb), fb, "{bad}");
        }
    }

    #[test]
    fn geometry_categories_map_to_board_materials() {
        let ids = BoardMaterialIds { edge: 3, plated: 4, fr4: 5, finish: 6, mask_on_fr4: 7, mask_on_copper: 8, silk: 9, ..Default::default() };
        let board: Vec<u32> = (0..2).map(|i| ids.for_board(board_mat_of(i))).collect();
        assert_eq!(board, vec![3, 4]);
        let relief: Vec<u32> = (0..5).map(|i| ids.for_relief(relief_mat_of(i))).collect();
        assert_eq!(relief, vec![5, 6, 7, 8, 9]);
    }

    #[test]
    #[should_panic(expected = "unknown ReliefMat")]
    fn unknown_geometry_category_is_an_error() {
        relief_mat_of(5);
    }

    #[test]
    fn mask_model_follows_the_relief_thickness() {
        let rp = ReliefParams::default();
        let m = MaskModel::default();
        assert_eq!((m.on_copper, m.on_substrate, m.copper), (rp.mask_on_copper as f32, rp.mask_on_substrate as f32, rp.copper as f32));
        assert!(m.at_edge <= m.on_copper);
    }

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
