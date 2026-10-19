#pragma once
// 積分器で共通の部品：計算シェーダのパイプラインと記述子の組、シーンの資源の記述子、フレームの定数
#include <string>
#include <vector>

#include "render/gpu_types.hpp"
#include "render/integrator.hpp"

namespace krbr::render {

// 記述子 1 つ分（バッファ・画像・加速構造のどれか）
struct KernelBinding {
    uint32_t binding = 0;
    VkDescriptorType type = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER;
    VkDescriptorBufferInfo buffer{};
    VkDescriptorImageInfo image{};
    const VkAccelerationStructureKHR* tlas = nullptr;
};

KernelBinding bufferBinding(uint32_t binding, const vk::Buffer& b,
                            VkDescriptorType type = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER);

// 全積分器で共通の記述子（0〜19：シーン・環境マップ・出力先・定数・光源）
std::vector<KernelBinding> sceneBindings(const GpuScene& sc, const RenderTargets& targets, const vk::Buffer& frameUbo);

// フレームの定数（カメラ・環境光・光源の選び方など）
FrameUBO makeFrameUbo(const GpuScene& sc, const RenderTargets& targets, const FrameParams& fp);

// 計算シェーダ 1 本（記述子の組は 1 つ、プッシュ定数は PushConstants）
class ComputeKernel {
public:
    ~ComputeKernel() { destroy(); }
    void create(vk::Context& ctx, const std::string& spvName, const std::vector<KernelBinding>& bindings);
    void dispatch(VkCommandBuffer cmd, const PushConstants& pc, uint32_t width, uint32_t height) const;
    void destroy();

private:
    vk::Context* ctx_ = nullptr;
    VkDescriptorSetLayout setLayout_ = VK_NULL_HANDLE;
    VkDescriptorPool pool_ = VK_NULL_HANDLE;
    VkDescriptorSet set_ = VK_NULL_HANDLE;
    VkPipelineLayout layout_ = VK_NULL_HANDLE;
    VkPipeline pipeline_ = VK_NULL_HANDLE;
};

// 計算シェーダの書き込みを、次の計算シェーダ・転送が読めるようにする
void computeBarrier(VkCommandBuffer cmd);

}  // namespace krbr::render
