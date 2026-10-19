#pragma once
// シェーダ（shaders/common.glsl）と一致させる GPU 用の構造体
#include <glm/glm.hpp>

#include <cstdint>

#include "scene/material.hpp"

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
};
static_assert(sizeof(GpuMaterial) == 144);

// 発光の単位：放射輝度 1.0 を 1000 cd/m² とみなす（HDR の画素値と同じ尺度）
constexpr float kEmissionScale = 1.0f / 1000.0f;

struct GpuInstanceInfo {
    uint32_t mesh, materialBase, pad0, pad1;
};

inline GpuMaterial toGpu(const scene::Material& m) {
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
};

struct PushConstants {
    uint32_t sampleIndex;
    uint32_t seed;
};

}  // namespace krbr::render
