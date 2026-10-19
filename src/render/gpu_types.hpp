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
    glm::vec4 params1;    // coat_ior, kind, board side, -
};
static_assert(sizeof(GpuMaterial) == 80);

inline GpuMaterial toGpu(const scene::Material& m) {
    GpuMaterial g;
    g.baseColor = {m.base_color, m.base_weight};
    g.specColor = {m.specular_color, m.specular_weight};
    g.params0 = {m.base_metalness, m.specular_roughness, m.specular_ior, m.coat_weight};
    g.coatColor = {m.coat_color, m.coat_roughness};
    g.params1 = {m.coat_ior, static_cast<float>(static_cast<uint32_t>(m.kind)), static_cast<float>(m.boardSide), 0.0f};
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
};

struct PushConstants {
    uint32_t sampleIndex;
    uint32_t seed;
};

}  // namespace krbr::render
