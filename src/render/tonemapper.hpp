#pragma once
// 表示用のトーンマッピング（累積画像 -> RGBA8）
#include "render/integrator.hpp"

namespace krbr::render {

enum class DisplayMode : uint32_t { Beauty = 0, Albedo = 1, Normal = 2 };

class Tonemapper {
public:
    ~Tonemapper();
    // 出力（RGBA8、GENERAL レイアウト）を作る。targets が変わったら init し直す
    void init(vk::Context& ctx, const RenderTargets& targets);
    void record(VkCommandBuffer cmd, float exposureEv, DisplayMode mode);
    const vk::Image& output() const { return out_; }
    void destroy();

private:
    vk::Context* ctx_ = nullptr;
    RenderTargets targets_;
    vk::Image out_;
    VkDescriptorSetLayout setLayout_ = VK_NULL_HANDLE;
    VkDescriptorPool pool_ = VK_NULL_HANDLE;
    VkDescriptorSet set_ = VK_NULL_HANDLE;
    VkPipelineLayout layout_ = VK_NULL_HANDLE;
    VkPipeline pipeline_ = VK_NULL_HANDLE;
};

}  // namespace krbr::render
