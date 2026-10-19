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
    // bumpIntensity: 全材質の bump_strength に掛ける倍率
    void build(vk::Context& ctx, const scene::Scene& scene, const io::ImageF& env, const EnvDistribution& dist,
               float bumpIntensity = 1.0f);
    // シーンの形状・配置・材質を作り直す（環境マップはそのまま。積分器は作り直しが必要）
    void rebuildGeometry(const scene::Scene& scene, float bumpIntensity);
    // 環境マップを差し替える（GPU が待機中に呼ぶこと。積分器は作り直しが必要）
    void setEnvironment(const io::ImageF& env, const EnvDistribution& dist);
    // 材質を送り直す（GPU が待機中に呼ぶこと）
    void updateMaterials(const std::vector<scene::Material>& materials, float bumpIntensity);
    void destroy();

    vk::Buffer vertices, indices, triMats, meshInfos, materials, instanceInfos;
    vk::Buffer envMarginal, envConditional, envPdf;
    vk::Image envImage, boardImage;
    VkSampler envSampler = VK_NULL_HANDLE, boardSampler = VK_NULL_HANDLE;
    VkAccelerationStructureKHR tlas = VK_NULL_HANDLE;
    uint32_t envDistW = 0, envDistH = 0, envWidth = 0, envHeight = 0;
    // 基板テクスチャの対応（ワールド X, Z の原点と 1/大きさ）
    glm::vec4 boardMap{0.0f};
    scene::BoardMaterialIds boardMats;
    glm::vec4 maskParams{0.0f}, maskParams2{0.0f};
    size_t triangleCount = 0;

private:
    void buildGeometry(const scene::Scene& sc, float bumpIntensity);
    void buildBoardTexture(const scene::Scene& sc);
    void destroyGeometry();
    void updateSdfTiles(const scene::Scene& sc);
    bool boardImageIsSdf_ = false;

    vk::Context* ctx_ = nullptr;
    std::vector<vk::Buffer> blasBuffers_;
    std::vector<VkAccelerationStructureKHR> blas_;
    vk::Buffer tlasBuffer_, instanceBuffer_;
};

}  // namespace krbr::render
