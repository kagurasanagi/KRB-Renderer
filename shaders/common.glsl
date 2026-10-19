// 共通の定義とバインディング（C++ 側 render/gpu_types.hpp と一致させること）
#ifndef KRBR_COMMON_GLSL
#define KRBR_COMMON_GLSL

#define PI 3.14159265358979323846
#define INV_PI 0.31830988618379067154

// 材質の種別
#define MAT_GENERIC 0u
#define MAT_BOARD 1u   // 基板の表裏面。層テクスチャで材質を決める（凹凸を形状にしない場合）
#define MAT_MASKED 2u  // レジストの下に別の材質がある面（params1.z: 下の材質, params1.w: レジスト）

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
    vec4 params1;     // x: coat_ior, y: kind, z: side（基板面: 0=表, 1=裏）/ 下の材質, w: レジストの材質
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

layout(set = 0, binding = 0) uniform accelerationStructureEXT tlas;
layout(set = 0, binding = 1, std430) readonly buffer Vertices { Vertex vertices[]; };
layout(set = 0, binding = 2, std430) readonly buffer Indices { uint indices[]; };
layout(set = 0, binding = 3, std430) readonly buffer TriMats { uint triMats[]; };
layout(set = 0, binding = 4, std430) readonly buffer Meshes { MeshInfo meshes[]; };
layout(set = 0, binding = 5, std430) readonly buffer Materials { Material materials[]; };
layout(set = 0, binding = 6) uniform sampler2D envTex;
layout(set = 0, binding = 7, std430) readonly buffer EnvMarginal { float envMarginalCdf[]; };
layout(set = 0, binding = 8, std430) readonly buffer EnvConditional { float envConditionalCdf[]; };
layout(set = 0, binding = 9, std430) readonly buffer EnvPdf { float envPdf[]; };
layout(set = 0, binding = 10) uniform sampler2DArray boardTex;
layout(set = 0, binding = 11, rgba32f) uniform image2D accumColor;
layout(set = 0, binding = 12, rgba32f) uniform image2D accumAlbedo;
layout(set = 0, binding = 13, rgba32f) uniform image2D accumNormal;

layout(set = 0, binding = 14, std140) uniform Frame {
    vec4 camPos;
    vec4 camRight;    // xyz: 右方向 * tan(半画角) * アスペクト
    vec4 camUp;       // xyz: 上方向 * tan(半画角)
    vec4 camForward;
    vec4 boardMap;    // x,y: テクスチャ原点のワールド X,Z、z,w: 1/幅, 1/奥行き
    vec4 envParams;   // x: 強度, y: 回転（rad）, z: 間接光の放射輝度の上限（0 で無効）
    uvec4 dims;       // x,y: 出力解像度, z,w: 環境マップ分布の解像度
    uvec4 counts;     // x: 最大深さ, y: ロシアンルーレット開始深さ
    uvec4 boardMats;  // x: レジスト, y: 銅（レジスト下）, z: 表面処理, w: 基材
    uvec4 boardMats2; // x: シルク, y: 銅, z: 基材
    vec4 maskParams;  // x: 銅の上のレジストの厚み, y: 基材の上, z: 銅の縁の上, w: 縁から中央への変化の距離（mm）
    vec4 maskParams2; // x: 銅の壁ぎわのたまり, y: その距離, z: 銅の厚み, w: 1 なら層テクスチャは距離場
    vec4 spectral;    // x: 1 なら分光モード（累積は XYZ）, y: 1 / ∫ȳ dλ
    vec4 rgbToXyz0;   // リニア sRGB → XYZ の列
    vec4 rgbToXyz1;
    vec4 rgbToXyz2;
    vec4 lightParams; // x: 環境光を選ぶ確率, y: 発光三角形の Σ(輝度 × 面積), z: シーンを囲む球の半径, w: 発光三角形の数
    vec4 sceneCenter;
    vec4 vcmParams;   // x: 近傍の半径の初期値（mm）, y: α, z: 光源側の経路の数, w: 1 なら merging を使う
} F;

layout(set = 0, binding = 15, std430) readonly buffer Instances { InstanceInfo instanceInfos[]; };
// RGB → スペクトルの表（Jakob & Hanika 2019）：64 x 64 x (3 x 64) の係数と、明るさの格子
layout(set = 0, binding = 16, std430) readonly buffer Rgb2Spec { vec4 rgb2specData[]; };
layout(set = 0, binding = 17, std430) readonly buffer Rgb2SpecScale { float rgb2specScale[]; };

layout(push_constant) uniform Push {
    uint sampleIndex;
    uint seed;
} pc;

#endif
