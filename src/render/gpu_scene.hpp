#pragma once
// シーンを GPU へ転送し、加速構造（BLAS/TLAS）を作る。積分器に依存しない共通の資源
#include <vector>

#include "io/image_io.hpp"
#include "render/env_map.hpp"
#include "render/vk/context.hpp"
#include "scene/scene.hpp"

namespace krbr::render {

class GpuScene {
public:
    ~GpuScene();
    void build(vk::Context& ctx, const scene::Scene& scene, const io::ImageF& env, const EnvDistribution& dist);
    void destroy();

    vk::Buffer vertices, indices, triMats, meshInfos, materials;
    vk::Buffer envMarginal, envConditional, envPdf;
    vk::Image envImage, boardImage;
    VkSampler envSampler = VK_NULL_HANDLE, boardSampler = VK_NULL_HANDLE;
    VkAccelerationStructureKHR tlas = VK_NULL_HANDLE;
    uint32_t envDistW = 0, envDistH = 0;
    // 基板テクスチャの対応（ワールド X, Z の原点と 1/大きさ）
    glm::vec4 boardMap{0.0f};
    scene::BoardMaterialIds boardMats;
    size_t triangleCount = 0;

private:
    vk::Context* ctx_ = nullptr;
    std::vector<vk::Buffer> blasBuffers_;
    std::vector<VkAccelerationStructureKHR> blas_;
    vk::Buffer tlasBuffer_, instanceBuffer_;
};

}  // namespace krbr::render
