// 共通の定義とバインディング。構造体の並びは rust/crates/krbr-render/src/gpu_types.rs と一致させること（大きさは
// そこで const assert している）。バインディング番号などの KRBR_ の定数は rust/crates/krbr-render/src/shader_consts.rs が
// 正本で、build.rs が glslc に -D で渡す
#ifndef KRBR_COMMON_GLSL
#define KRBR_COMMON_GLSL

#ifndef KRBR_BINDING_TLAS
#error "compile the shaders with rust/crates/krbr-render/build.rs (it passes the shared constants as -D KRBR_...)"
#endif

#define PI 3.14159265358979323846
#define INV_PI 0.31830988618379067154

// 材質の種別（rust/crates/krbr-scene/src/material.rs の MaterialKind と同じ値）
#define MAT_GENERIC 0u
#define MAT_MASKED 1u  // レジストの下に別の材質がある面（params1.z: 下の材質, params1.w: レジスト）

#define BUMP_NONE 0u
#define BUMP_NOISE 1u
#define BUMP_WEAVE 2u

struct Vertex {
    vec3 pos;
    float pad0;
    vec3 nrm;
    float pad1;
};

struct MeshInfo {
    uint vertexOffset;
    uint indexOffset;
    uint triOffset;
    uint pad;
};

// OpenPBR Surface の部分集合
struct Material {
    vec4 baseColor;   // rgb: base_color, a: base_weight
    vec4 specColor;   // rgb: specular_color, a: specular_weight
    vec4 params0;     // x: base_metalness, y: specular_roughness, z: specular_ior, w: coat_weight
    vec4 coatColor;   // rgb: coat_color, a: coat_roughness
    vec4 params1;     // x: coat_ior, y: kind, z: 下の材質（MAT_MASKED）, w: レジストの材質（MAT_MASKED）
    vec4 params2;     // x: 凹凸の模様, y: 強さ（傾き）, z: 大きさ（mm）, w: 粗さのむら
    vec4 emission;    // rgb: 放射輝度, a: geometry_opacity
    vec4 transmission;  // x: transmission_weight, y: アッベ数, z: 分散の倍率, w: thin_walled
    vec4 transmission2; // rgb: transmission_color, a: transmission_depth（mm）
    vec4 subsurface;    // rgb: subsurface_color（radius 進んだときの透過率）, a: subsurface_radius（mm）
    vec4 params3;       // x: subsurface_weight, y, z: Cauchy の A, B（λ は nm）, w: 1 なら分散あり
    // 分光モード用の RGB → スペクトルの係数（xyz）。specEmission.w は倍率
    vec4 specBase;
    vec4 specSpecular;
    vec4 specCoat;
    vec4 specTrans;
    vec4 specSubsurface;
    vec4 specEmission;
};

struct InstanceInfo {
    uint mesh;
    uint materialBase;
    uint pad0;
    uint pad1;
    vec4 w2o0;  // ワールド → オブジェクトの 3x3（列）
    vec4 w2o1;
    vec4 w2o2;
};

layout(set = 0, binding = KRBR_BINDING_TLAS) uniform accelerationStructureEXT tlas;
layout(set = 0, binding = KRBR_BINDING_VERTICES, std430) readonly buffer Vertices { Vertex vertices[]; };
layout(set = 0, binding = KRBR_BINDING_INDICES, std430) readonly buffer Indices { uint indices[]; };
layout(set = 0, binding = KRBR_BINDING_TRI_MATS, std430) readonly buffer TriMats { uint triMats[]; };
layout(set = 0, binding = KRBR_BINDING_MESHES, std430) readonly buffer Meshes { MeshInfo meshes[]; };
layout(set = 0, binding = KRBR_BINDING_MATERIALS, std430) readonly buffer Materials { Material materials[]; };
layout(set = 0, binding = KRBR_BINDING_ENV_TEX) uniform sampler2D envTex;
layout(set = 0, binding = KRBR_BINDING_ENV_MARGINAL, std430) readonly buffer EnvMarginal { float envMarginalCdf[]; };
layout(set = 0, binding = KRBR_BINDING_ENV_CONDITIONAL, std430) readonly buffer EnvConditional { float envConditionalCdf[]; };
layout(set = 0, binding = KRBR_BINDING_ENV_PDF, std430) readonly buffer EnvPdf { float envPdf[]; };
// 基板の表・裏（層 0, 1）の縁からの距離場（RG8。r: 銅の縁, g: レジスト開口の縁）。maskParams2.w が 0 なら距離場は無い（読まない）
layout(set = 0, binding = KRBR_BINDING_BOARD_TEX) uniform sampler2DArray boardTex;
layout(set = 0, binding = KRBR_BINDING_ACCUM_COLOR, rgba32f) uniform image2D accumColor;
layout(set = 0, binding = KRBR_BINDING_ACCUM_ALBEDO, rgba32f) uniform image2D accumAlbedo;
layout(set = 0, binding = KRBR_BINDING_ACCUM_NORMAL, rgba32f) uniform image2D accumNormal;

layout(set = 0, binding = KRBR_BINDING_FRAME, std140) uniform Frame {
    vec4 camPos;
    vec4 camRight;    // xyz: 右方向 * tan(半画角) * アスペクト
    vec4 camUp;       // xyz: 上方向 * tan(半画角)
    vec4 camForward;
    vec4 boardMap;    // x,y: 距離場（boardTex）の原点のワールド X,Z、z,w: 1/幅, 1/奥行き
    vec4 envParams;   // x: 強度, y: 回転（rad）, z: 間接光の放射輝度の上限（0 で無効）
    uvec4 dims;       // x,y: 出力解像度, z,w: 環境マップ分布の解像度
    uvec4 counts;     // x: 最大深さ（経路の線分は x + 1 本まで。maxPathLength）, y: ロシアンルーレット開始深さ（PT のみ）
    vec4 maskParams;  // x: 銅の上のレジストの厚み, y: 基材の上, z: 銅の縁の上, w: 縁から中央への変化の距離（mm）
    vec4 maskParams2; // x: 銅の壁ぎわのたまり, y: その距離, z: 銅の厚み, w: 距離場の範囲（mm。0 なら距離場は無い）
    vec4 spectral;    // x: 1 なら分光モード（累積は XYZ）, y: 1 / ∫ȳ dλ
    vec4 rgbToXyz0;   // リニア sRGB → XYZ の列
    vec4 rgbToXyz1;
    vec4 rgbToXyz2;
    vec4 lightParams; // x: 環境光を選ぶ確率, y: 発光三角形の Σ(輝度 × 面積), z: シーンを囲む球の半径, w: 発光三角形の数
    vec4 sceneCenter;
    vec4 vcmParams;   // x: 近傍の半径の初期値（mm）, y: α, z: 光源側の経路の数, w: 1 なら merging を使う
    vec4 lens0;       // レンズ（camera.glsl。値の作り方は rust/crates/krbr-render/src/kernel.rs の fill_lens）
    vec4 lens1;
    vec4 lens2;
    vec4 lens3;
    vec4 lens4;
    vec4 lensSurfaces[KRBR_MAX_LENS_SURFACES];
} F;

layout(set = 0, binding = KRBR_BINDING_INSTANCES, std430) readonly buffer Instances { InstanceInfo instanceInfos[]; };
// RGB → スペクトルの表（Jakob & Hanika 2019）：64 x 64 x (3 x 64) の係数と、明るさの格子
layout(set = 0, binding = KRBR_BINDING_RGB2SPEC, std430) readonly buffer Rgb2Spec { vec4 rgb2specData[]; };
layout(set = 0, binding = KRBR_BINDING_RGB2SPEC_SCALE, std430) readonly buffer Rgb2SpecScale { float rgb2specScale[]; };

layout(push_constant) uniform Push {
    uint sampleIndex;
    uint seed;
} pc;

// ---------------------------------------------------------------- フレームの定数から求める値（全積分器で共通）

// 経路の線分の数（カメラ・光源を含めた頂点の数 − 1）の上限。PT・BDPT・VCM で同じ意味にする
uint maxPathLength() { return F.counts.x + 1u; }

float tanHalfFovY() { return length(F.camUp.xyz); }
// 画素を単位とした、ピンホールの像面までの距離
float imagePlaneDist() { return float(F.dims.y) / (2.0 * tanHalfFovY()); }

// 累積画像へ 1 サンプル分を足す（sampleIndex 0 で初期化）。A はサンプル数（表示と読み戻しで割る数）。
// color が NaN / Inf なら、このサンプルの寄与を 0 にする（1 つの値が画素の平均を壊さないように）。発生源を追うときは
// -DKRBR_DEBUG_NAN を付けてコンパイルすると、その画素に目立つ色（マゼンタ）を足して場所が分かるようにする
void accumulate(ivec2 ip, vec3 color, vec3 albedo, vec3 normal) {
    if (any(isnan(color)) || any(isinf(color))) {
#ifdef KRBR_DEBUG_NAN
        color = vec3(1000.0, 0.0, 1000.0);
#else
        color = vec3(0.0);
#endif
    }
    if (pc.sampleIndex == 0u) {
        imageStore(accumColor, ip, vec4(color, 1.0));
        imageStore(accumAlbedo, ip, vec4(albedo, 1.0));
        imageStore(accumNormal, ip, vec4(normal, 1.0));
    } else {
        imageStore(accumColor, ip, imageLoad(accumColor, ip) + vec4(color, 1.0));
        imageStore(accumAlbedo, ip, imageLoad(accumAlbedo, ip) + vec4(albedo, 1.0));
        imageStore(accumNormal, ip, imageLoad(accumNormal, ip) + vec4(normal, 1.0));
    }
}

#endif
