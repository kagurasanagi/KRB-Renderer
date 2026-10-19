#pragma once
// シェーダ（shaders/common.glsl）と一致させる GPU 用の構造体
#include <glm/glm.hpp>

#include <cstdint>

#include "scene/material.hpp"
#include "scene/spectrum.hpp"

namespace krbr::render {

struct GpuVertex {
    glm::vec3 pos;
    float pad0;
    glm::vec3 nrm;
    float pad1;
};
static_assert(sizeof(GpuVertex) == 32);

struct GpuMeshInfo {
    uint32_t vertexOffset, indexOffset, triOffset, pad;
};

struct GpuMaterial {
    glm::vec4 baseColor;  // rgb base_color, a base_weight
    glm::vec4 specColor;  // rgb specular_color, a specular_weight
    glm::vec4 params0;    // base_metalness, specular_roughness, specular_ior, coat_weight
    glm::vec4 coatColor;  // rgb coat_color, a coat_roughness
    glm::vec4 params1;    // coat_ior, kind, board side / masked base, masked mask
    glm::vec4 params2;    // bump pattern, bump strength, bump scale (mm), roughness variation
    glm::vec4 emission;   // rgb: 放射輝度（emission_color × emission_luminance × kEmissionScale）, a: geometry_opacity
    glm::vec4 transmission;  // transmission_weight, dispersion abbe number, dispersion scale, thin_walled
    glm::vec4 transmission2;  // rgb: transmission_color, a: transmission_depth（mm）
    glm::vec4 subsurface;     // rgb: subsurface_color（radius だけ進んだときの透過率）, a: subsurface_radius（mm）
    glm::vec4 params3;        // x: subsurface_weight, y, z: Cauchy の A, B（n(λ) = A + B/λ²、λ は nm）, w: 1 なら分散あり
    // 分光モード用：各色の RGB → スペクトルの係数（シグモイド多項式）。発光の w は倍率
    glm::vec4 specBase, specSpecular, specCoat, specTrans, specSubsurface, specEmission;
};
static_assert(sizeof(GpuMaterial) == 272);

// 発光の単位：放射輝度 1.0 を 1000 cd/m² とみなす（HDR の画素値と同じ尺度）
constexpr float kEmissionScale = 1.0f / 1000.0f;

struct GpuInstanceInfo {
    uint32_t mesh, materialBase, pad0, pad1;
    glm::vec4 w2o[3];  // ワールド → オブジェクトの 3x3（列。光源側の頂点で交点を作り直すため）
};

// 発光する三角形（ワールド座標）。光源として直接サンプリングする
struct GpuEmitter {
    glm::vec4 p0;  // w: 材質の番号（float に詰めたビット）
    glm::vec4 p1;  // w: 面積（mm²）
    glm::vec4 p2;
};

// spec: RGB → スペクトルの表（nullptr なら分光用の係数は 0 のまま）
inline GpuMaterial toGpu(const scene::Material& m, const spectrum::Rgb2SpecTable* spec) {
    GpuMaterial g;
    g.baseColor = {m.base_color, m.base_weight};
    g.specColor = {m.specular_color, m.specular_weight};
    g.params0 = {m.base_metalness, m.specular_roughness, m.specular_ior, m.coat_weight};
    g.coatColor = {m.coat_color, m.coat_roughness};
    const bool masked = m.kind == scene::MaterialKind::Masked;
    g.params1 = {m.coat_ior, static_cast<float>(static_cast<uint32_t>(m.kind)),
                 static_cast<float>(masked ? m.maskedBase : m.boardSide),
                 static_cast<float>(masked ? m.maskedMask : 0u)};
    g.params2 = {static_cast<float>(static_cast<uint32_t>(m.bump_pattern)), m.bump_strength, m.bump_scale,
                 m.roughness_variation};
    g.emission = {m.emission_color * (m.emission_luminance * kEmissionScale), m.geometry_opacity};
    g.transmission = {m.transmission_weight, m.transmission_dispersion_abbe_number, m.transmission_dispersion_scale,
                      m.geometry_thin_walled ? 1.0f : 0.0f};
    g.transmission2 = {m.transmission_color, m.transmission_depth};
    g.subsurface = {m.subsurface_color, m.subsurface_radius};
    // 分散：nF, nC から Cauchy の式の係数を求める（Sellmeier の指定があってもこの近似で扱う）
    const float nF = m.iorAt(486.1f), nC = m.iorAt(656.3f), nd = m.iorAt(587.6f);
    const bool dispersive = std::abs(nF - nC) > 1e-6f;
    const float B = dispersive ? (nF - nC) / (1.0f / (486.1f * 486.1f) - 1.0f / (656.3f * 656.3f)) : 0.0f;
    const float A = nd - B / (587.6f * 587.6f);
    g.params3 = {m.subsurface_weight, A, B, dispersive ? 1.0f : 0.0f};
    if (spec) {
        auto coef = [&](glm::vec3 c) { return glm::vec4(spec->lookup(c), 0.0f); };
        g.specBase = coef(m.base_color);
        g.specSpecular = coef(m.specular_color);
        g.specCoat = coef(m.coat_color);
        g.specTrans = coef(m.transmission_color);
        g.specSubsurface = coef(m.subsurface_color);
        g.specEmission = spec->unbounded(glm::vec3(g.emission));
    } else {
        g.specBase = g.specSpecular = g.specCoat = g.specTrans = g.specSubsurface = g.specEmission = glm::vec4(0.0f);
    }
    return g;
}

struct FrameUBO {
    glm::vec4 camPos;
    glm::vec4 camRight;
    glm::vec4 camUp;
    glm::vec4 camForward;
    glm::vec4 boardMap;
    glm::vec4 envParams;
    glm::uvec4 dims;
    glm::uvec4 counts;
    glm::uvec4 boardMats;
    glm::uvec4 boardMats2;
    glm::vec4 maskParams;   // x: 銅の上の厚み, y: 基材の上の厚み, z: 縁の厚み, w: 縁からの変化の距離（mm）
    glm::vec4 maskParams2;  // x: 壁ぎわのたまり, y: その距離, z: 銅の厚み, w: 1 なら距離場が使える
    glm::vec4 spectral;     // x: 1 なら分光モード（累積は XYZ）, y: 1 / ∫ȳ dλ
    glm::vec4 rgbToXyz[3];  // リニア sRGB → XYZ（列ごと。アルベドを XYZ で書くため）
    glm::vec4 lightParams;  // x: 環境光を選ぶ確率, y: 発光三角形の Σ(輝度 × 面積), z: シーンを囲む球の半径, w: 発光三角形の数
    glm::vec4 sceneCenter;  // xyz: シーンを囲む球の中心
    glm::vec4 vcmParams;    // x: 近傍の半径（初期値、mm）, y: 半径を縮める α, z: 光源側の経路の数, w: 1 なら merging を使う
    // レンズ（shaders/camera.glsl）
    glm::vec4 lens0;  // x: 種類（0 ピンホール, 1 薄レンズ, 2 実レンズ）, y: 絞りの半径（mm）, z: ピントの距離（mm）, w: 羽根の枚数
    glm::vec4 lens1;  // x: 羽根の回転（rad）, y: 丸み, z: 口径食の前の開口までの距離（0 で無効）, w: 前の開口の半径
    glm::vec4 lens2;  // x: 1 なら cos⁴θ の周辺減光, y: 倍率色収差の係数, z: レンズからフィルムまでの距離 v, w: 焦点距離（d 線）
    glm::vec4 lens3;  // x: レンズの nd, y: (nd − 1) / アッベ数（0 で軸上色収差なし）, z: 正規化の係数（実レンズ）, w: 実レンズの面の数
    glm::vec4 lens4;  // 実レンズ：x, y: フィルムの半分の幅・高さ（mm）, z: フィルムの z, w: 後ろの開口の半径
    glm::vec4 lensSurfaces[16];  // 実レンズの面：x: 曲率半径, y: 頂点の z, z: フィルム側の屈折率, w: 有効半径
};

struct PushConstants {
    uint32_t sampleIndex;
    uint32_t seed;
};

}  // namespace krbr::render
