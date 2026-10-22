// シェーダ（shaders/ の GLSL）と共有する定数の正本。
// ライブラリは crate::shader_consts として使い、build.rs はこのファイルを include! して glsl_defines() の値を
// glslc に -D で渡す（GLSL では KRBR_<名前> のマクロになる）。GLSL 側に同じ数字を書かないこと。
// build.rs からも読むので、use や内側の doc コメント（//!）は書かない

/// 記述子のバインディング番号：シーン（shaders/common.glsl）
pub const BINDING_TLAS: u32 = 0;
pub const BINDING_VERTICES: u32 = 1;
pub const BINDING_INDICES: u32 = 2;
pub const BINDING_TRI_MATS: u32 = 3;
pub const BINDING_MESHES: u32 = 4;
pub const BINDING_MATERIALS: u32 = 5;
pub const BINDING_ENV_TEX: u32 = 6;
pub const BINDING_ENV_MARGINAL: u32 = 7;
pub const BINDING_ENV_CONDITIONAL: u32 = 8;
pub const BINDING_ENV_PDF: u32 = 9;
pub const BINDING_BOARD_TEX: u32 = 10;
pub const BINDING_ACCUM_COLOR: u32 = 11;
pub const BINDING_ACCUM_ALBEDO: u32 = 12;
pub const BINDING_ACCUM_NORMAL: u32 = 13;
pub const BINDING_FRAME: u32 = 14;
pub const BINDING_INSTANCES: u32 = 15;
pub const BINDING_RGB2SPEC: u32 = 16;
pub const BINDING_RGB2SPEC_SCALE: u32 = 17;
/// 光源（shaders/lights.glsl）
pub const BINDING_EMITTERS: u32 = 18;
pub const BINDING_EMITTER_CDF: u32 = 19;
/// BDPT / VCM だけが使うもの（shaders/bidir.glsl）
pub const BINDING_LIGHT_VERTICES: u32 = 20;
pub const BINDING_PATH_HEADS: u32 = 21;
pub const BINDING_CELL_HEADS: u32 = 22;
pub const BINDING_COUNTERS: u32 = 23;
pub const BINDING_SPLAT: u32 = 24;
/// 金属の測定値の表（shaders/common.glsl）
pub const BINDING_CONDUCTORS: u32 = 25;

/// 金属の n, k の表の波長の格子（krbr_scene::conductor と同じ値。gpu_types.rs で確かめる）
pub const CONDUCTOR_LAMBDA_MIN: u32 = 360;
pub const CONDUCTOR_LAMBDA_STEP: u32 = 5;
pub const CONDUCTOR_SAMPLES: u32 = 95;

/// VCM の近傍探索の格子（空間ハッシュの表）のセルの数。cell_heads のバッファは u32 がこの数だけ
pub const VCM_GRID_SIZE: u32 = 1 << 21;
/// counters のバッファの並び（u32）：保存した光源側の頂点の数、容量、merging でセルの走査を打ち切った回数
pub const VCM_COUNTER_STORED: u32 = 0;
pub const VCM_COUNTER_CAPACITY: u32 = 1;
pub const VCM_COUNTER_MERGE_TRUNCATED: u32 = 2;
pub const VCM_COUNTER_COUNT: u32 = 4;
/// splat のバッファの、画素あたりの float の数（RGB か XYZ と、使わない 1 つ）
pub const VCM_SPLAT_FLOATS_PER_PIXEL: u32 = 4;
/// merging で 1 つのセルの連結リストを辿る上限（無限ループの番兵）。リストは繰り返しごとに作り直すので
/// 普通は届かない。届いたら VCM_COUNTER_MERGE_TRUNCATED に数え、Rust 側が警告する
pub const VCM_MAX_CELL_WALK: u32 = 4096;

/// 実レンズの面の数の上限（FrameUbo::lens_surfaces と GLSL の lensSurfaces の長さ）
pub const MAX_LENS_SURFACES: usize = 16;

/// 輝度の重み（リニア sRGB / Rec. 709 の原色での Y）。発光体の選び方（Rust の emitter_power と GLSL の emitterPdfA）は
/// 同じ重みでないと MIS の確率密度が食い違うので、どちらもこの値を使う
pub const LUMINANCE_WEIGHTS: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// glslc に渡す -D の一覧（名前には KRBR_ が付く）
pub fn glsl_defines() -> Vec<(String, String)> {
    let mut d: Vec<(String, String)> = [
        ("BINDING_TLAS", BINDING_TLAS),
        ("BINDING_VERTICES", BINDING_VERTICES),
        ("BINDING_INDICES", BINDING_INDICES),
        ("BINDING_TRI_MATS", BINDING_TRI_MATS),
        ("BINDING_MESHES", BINDING_MESHES),
        ("BINDING_MATERIALS", BINDING_MATERIALS),
        ("BINDING_ENV_TEX", BINDING_ENV_TEX),
        ("BINDING_ENV_MARGINAL", BINDING_ENV_MARGINAL),
        ("BINDING_ENV_CONDITIONAL", BINDING_ENV_CONDITIONAL),
        ("BINDING_ENV_PDF", BINDING_ENV_PDF),
        ("BINDING_BOARD_TEX", BINDING_BOARD_TEX),
        ("BINDING_ACCUM_COLOR", BINDING_ACCUM_COLOR),
        ("BINDING_ACCUM_ALBEDO", BINDING_ACCUM_ALBEDO),
        ("BINDING_ACCUM_NORMAL", BINDING_ACCUM_NORMAL),
        ("BINDING_FRAME", BINDING_FRAME),
        ("BINDING_INSTANCES", BINDING_INSTANCES),
        ("BINDING_RGB2SPEC", BINDING_RGB2SPEC),
        ("BINDING_RGB2SPEC_SCALE", BINDING_RGB2SPEC_SCALE),
        ("BINDING_EMITTERS", BINDING_EMITTERS),
        ("BINDING_EMITTER_CDF", BINDING_EMITTER_CDF),
        ("BINDING_LIGHT_VERTICES", BINDING_LIGHT_VERTICES),
        ("BINDING_PATH_HEADS", BINDING_PATH_HEADS),
        ("BINDING_CELL_HEADS", BINDING_CELL_HEADS),
        ("BINDING_COUNTERS", BINDING_COUNTERS),
        ("BINDING_SPLAT", BINDING_SPLAT),
        ("BINDING_CONDUCTORS", BINDING_CONDUCTORS),
        ("CONDUCTOR_SAMPLES", CONDUCTOR_SAMPLES),
        ("VCM_COUNTER_STORED", VCM_COUNTER_STORED),
        ("VCM_COUNTER_CAPACITY", VCM_COUNTER_CAPACITY),
        ("VCM_COUNTER_MERGE_TRUNCATED", VCM_COUNTER_MERGE_TRUNCATED),
        ("VCM_SPLAT_FLOATS_PER_PIXEL", VCM_SPLAT_FLOATS_PER_PIXEL),
        ("MAX_LENS_SURFACES", MAX_LENS_SURFACES as u32),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    // 符号なしの値として比べる・剰余を取るもの
    d.push(("VCM_GRID_SIZE".into(), format!("{VCM_GRID_SIZE}u")));
    d.push(("VCM_MAX_CELL_WALK".into(), format!("{VCM_MAX_CELL_WALK}u")));
    d.push(("CONDUCTOR_LAMBDA_MIN".into(), format!("{CONDUCTOR_LAMBDA_MIN}.0")));
    d.push(("CONDUCTOR_LAMBDA_STEP".into(), format!("{CONDUCTOR_LAMBDA_STEP}.0")));
    let w = LUMINANCE_WEIGHTS;
    d.push(("LUMINANCE_WEIGHTS".into(), format!("vec3({:?},{:?},{:?})", w[0], w[1], w[2])));
    d.into_iter().map(|(k, v)| (format!("KRBR_{k}"), v)).collect()
}
