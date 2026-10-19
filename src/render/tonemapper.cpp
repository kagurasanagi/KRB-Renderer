#include "render/tonemapper.hpp"

#include <cmath>

#include "core/platform.hpp"

namespace krbr::render {

namespace {
struct TonemapPush {
    float exposureScale;
    uint32_t mode;
    uint32_t width;
    uint32_t height;
};
}  // namespace

Tonemapper::~Tonemapper() { destroy(); }

void Tonemapper::init(vk::Context& ctx, const RenderTargets& targets) {
    destroy();
    ctx_ = &ctx;
    targets_ = targets;
    VkDevice dev = ctx.device();

    out_ = ctx.createImage2D(targets.width, targets.height, 1, VK_FORMAT_R8G8B8A8_UNORM,
                             VK_IMAGE_USAGE_STORAGE_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT);
    ctx.submitAndWait([&](VkCommandBuffer cmd) {
        vk::imageBarrier(cmd, out_.image, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_GENERAL, VK_PIPELINE_STAGE_2_NONE, 0,
                         VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT, VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT);
    });

    VkDescriptorSetLayoutBinding lb[4];
    for (uint32_t i = 0; i < 4; ++i) lb[i] = {i, VK_DESCRIPTOR_TYPE_STORAGE_IMAGE, 1, VK_SHADER_STAGE_COMPUTE_BIT, nullptr};
    VkDescriptorSetLayoutCreateInfo lci{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO};
    lci.bindingCount = 4;
    lci.pBindings = lb;
    vk::check(vkCreateDescriptorSetLayout(dev, &lci, nullptr, &setLayout_), "vkCreateDescriptorSetLayout");
    VkDescriptorPoolSize ps{VK_DESCRIPTOR_TYPE_STORAGE_IMAGE, 4};
    VkDescriptorPoolCreateInfo pci{VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO};
    pci.maxSets = 1;
    pci.poolSizeCount = 1;
    pci.pPoolSizes = &ps;
    vk::check(vkCreateDescriptorPool(dev, &pci, nullptr, &pool_), "vkCreateDescriptorPool");
    VkDescriptorSetAllocateInfo ai{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO};
    ai.descriptorPool = pool_;
    ai.descriptorSetCount = 1;
    ai.pSetLayouts = &setLayout_;
    vk::check(vkAllocateDescriptorSets(dev, &ai, &set_), "vkAllocateDescriptorSets");
    VkDescriptorImageInfo infos[4] = {{VK_NULL_HANDLE, targets.color.view, VK_IMAGE_LAYOUT_GENERAL},
                                      {VK_NULL_HANDLE, targets.albedo.view, VK_IMAGE_LAYOUT_GENERAL},
                                      {VK_NULL_HANDLE, targets.normal.view, VK_IMAGE_LAYOUT_GENERAL},
                                      {VK_NULL_HANDLE, out_.view, VK_IMAGE_LAYOUT_GENERAL}};
    VkWriteDescriptorSet w[4];
    for (uint32_t i = 0; i < 4; ++i) {
        w[i] = {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET};
        w[i].dstSet = set_;
        w[i].dstBinding = i;
        w[i].descriptorCount = 1;
        w[i].descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_IMAGE;
        w[i].pImageInfo = &infos[i];
    }
    vkUpdateDescriptorSets(dev, 4, w, 0, nullptr);

    VkPushConstantRange pcr{VK_SHADER_STAGE_COMPUTE_BIT, 0, sizeof(TonemapPush)};
    VkPipelineLayoutCreateInfo plci{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO};
    plci.setLayoutCount = 1;
    plci.pSetLayouts = &setLayout_;
    plci.pushConstantRangeCount = 1;
    plci.pPushConstantRanges = &pcr;
    vk::check(vkCreatePipelineLayout(dev, &plci, nullptr, &layout_), "vkCreatePipelineLayout");

    auto code = platform::readBinaryFile(platform::exeDir() / "shaders" / "tonemap.comp.spv");
    if (!code) throw std::runtime_error("cannot read tonemap.comp.spv");
    VkShaderModuleCreateInfo smci{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO};
    smci.codeSize = code->size();
    smci.pCode = reinterpret_cast<const uint32_t*>(code->data());
    VkShaderModule sm;
    vk::check(vkCreateShaderModule(dev, &smci, nullptr, &sm), "vkCreateShaderModule");
    VkComputePipelineCreateInfo cci{VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO};
    cci.stage = {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, nullptr, 0, VK_SHADER_STAGE_COMPUTE_BIT, sm, "main",
                 nullptr};
    cci.layout = layout_;
    const VkResult r = vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &cci, nullptr, &pipeline_);
    vkDestroyShaderModule(dev, sm, nullptr);
    vk::check(r, "vkCreateComputePipelines(tonemap)");
}

void Tonemapper::record(VkCommandBuffer cmd, float exposureEv, DisplayMode mode) {
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline_);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, layout_, 0, 1, &set_, 0, nullptr);
    TonemapPush pc{std::exp2(exposureEv), static_cast<uint32_t>(mode), targets_.width, targets_.height};
    vkCmdPushConstants(cmd, layout_, VK_SHADER_STAGE_COMPUTE_BIT, 0, sizeof(pc), &pc);
    vkCmdDispatch(cmd, (targets_.width + 7) / 8, (targets_.height + 7) / 8, 1);
}

void Tonemapper::destroy() {
    if (!ctx_) return;
    VkDevice dev = ctx_->device();
    vkDeviceWaitIdle(dev);
    if (pipeline_) vkDestroyPipeline(dev, pipeline_, nullptr);
    if (layout_) vkDestroyPipelineLayout(dev, layout_, nullptr);
    if (pool_) vkDestroyDescriptorPool(dev, pool_, nullptr);
    if (setLayout_) vkDestroyDescriptorSetLayout(dev, setLayout_, nullptr);
    ctx_->destroyImage(out_);
    pipeline_ = VK_NULL_HANDLE;
    layout_ = VK_NULL_HANDLE;
    pool_ = VK_NULL_HANDLE;
    setLayout_ = VK_NULL_HANDLE;
    ctx_ = nullptr;
}

}  // namespace krbr::render
