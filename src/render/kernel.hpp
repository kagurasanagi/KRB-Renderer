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
// レンズの値（makeFrameUbo から呼ぶ）
void fillLens(FrameUBO& u, const FrameParams& fp);

// レイを飛ばすカーネル 1 本（記述子の組は 1 つ、プッシュ定数は PushConstants）。
// 画素ごとに 1 スレッドで、GPU によって次のどちらかで作る（シェーダの本文は同じ）：
//   ray query            : 計算シェーダ shaders/<name>.comp.spv
//   ray tracing pipeline : レイ生成シェーダ shaders/<name>.rgen.spv ＋ rt_hit.rchit / rt_miss.rmiss / rt_shadow.rmiss
class ComputeKernel {
public:
    ~ComputeKernel() { destroy(); }
    void create(vk::Context& ctx, const std::string& name, const std::vector<KernelBinding>& bindings);
    void dispatch(VkCommandBuffer cmd, const PushConstants& pc, uint32_t width, uint32_t height) const;
    void destroy();

private:
    void createRayTracingPipeline(const std::string& name);

    vk::Context* ctx_ = nullptr;
    bool rtPipeline_ = false;
    VkDescriptorSetLayout setLayout_ = VK_NULL_HANDLE;
    VkDescriptorPool pool_ = VK_NULL_HANDLE;
    VkDescriptorSet set_ = VK_NULL_HANDLE;
    VkPipelineLayout layout_ = VK_NULL_HANDLE;
    VkPipeline pipeline_ = VK_NULL_HANDLE;
    // シェーダバインディングテーブル（ray tracing pipeline のとき）
    vk::Buffer sbt_;
    VkStridedDeviceAddressRegionKHR sbtRaygen_{}, sbtMiss_{}, sbtHit_{}, sbtCallable_{};
};

// 計算シェーダの書き込みを、次の計算シェーダ・転送が読めるようにする
void computeBarrier(VkCommandBuffer cmd);

}  // namespace krbr::render
