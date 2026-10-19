// パストレーサー積分器（shaders/pathtrace.comp を 1 サンプル 1 dispatch で回す）
#include <array>
#include <cstring>

#include "core/log.hpp"
#include "core/platform.hpp"
#include "render/gpu_types.hpp"
#include "render/integrator.hpp"

namespace krbr::render {

namespace {

VkShaderModule loadShader(VkDevice dev, const std::filesystem::path& p) {
    auto code = platform::readBinaryFile(p);
    if (!code || code->size() % 4 != 0) throw std::runtime_error("cannot read shader " + platform::pathToUtf8(p));
    VkShaderModuleCreateInfo ci{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO};
    ci.codeSize = code->size();
    ci.pCode = reinterpret_cast<const uint32_t*>(code->data());
    VkShaderModule m;
    vk::check(vkCreateShaderModule(dev, &ci, nullptr, &m), "vkCreateShaderModule");
    return m;
}

class PathTracer final : public Integrator {
public:
    ~PathTracer() override { destroy(); }
    const char* name() const override { return "pt"; }

    void setup(vk::Context& ctx, const GpuScene& sc, const RenderTargets& targets, const FrameParams& fp) override {
        ctx_ = &ctx;
        scene_ = &sc;
        targets_ = targets;
        VkDevice dev = ctx.device();

        // ---- 定数（カメラ操作で毎フレーム書き換えるので CPU から見えるメモリに置く）
        ubo_ = ctx.createBuffer(sizeof(FrameUBO), VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT, true);
        updateFrame(fp);

        // ---- 記述子
        const std::array<std::pair<uint32_t, VkDescriptorType>, 15> bindings = {{
            {0, VK_DESCRIPTOR_TYPE_ACCELERATION_STRUCTURE_KHR},
            {1, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER},
            {2, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER},
            {3, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER},
            {4, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER},
            {5, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER},
            {6, VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER},
            {7, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER},
            {8, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER},
            {9, VK_DESCRIPTOR_TYPE_STORAGE_BUFFER},
            {10, VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER},
            {11, VK_DESCRIPTOR_TYPE_STORAGE_IMAGE},
            {12, VK_DESCRIPTOR_TYPE_STORAGE_IMAGE},
            {13, VK_DESCRIPTOR_TYPE_STORAGE_IMAGE},
            {14, VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER},
        }};
        std::vector<VkDescriptorSetLayoutBinding> lb;
        for (auto [b, t] : bindings) lb.push_back({b, t, 1, VK_SHADER_STAGE_COMPUTE_BIT, nullptr});
        VkDescriptorSetLayoutCreateInfo lci{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO};
        lci.bindingCount = static_cast<uint32_t>(lb.size());
        lci.pBindings = lb.data();
        vk::check(vkCreateDescriptorSetLayout(dev, &lci, nullptr, &setLayout_), "vkCreateDescriptorSetLayout");

        const VkDescriptorPoolSize sizes[] = {{VK_DESCRIPTOR_TYPE_ACCELERATION_STRUCTURE_KHR, 1},
                                              {VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 8},
                                              {VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER, 2},
                                              {VK_DESCRIPTOR_TYPE_STORAGE_IMAGE, 3},
                                              {VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER, 1}};
        VkDescriptorPoolCreateInfo pci{VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO};
        pci.maxSets = 1;
        pci.poolSizeCount = static_cast<uint32_t>(std::size(sizes));
        pci.pPoolSizes = sizes;
        vk::check(vkCreateDescriptorPool(dev, &pci, nullptr, &pool_), "vkCreateDescriptorPool");
        VkDescriptorSetAllocateInfo ai{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO};
        ai.descriptorPool = pool_;
        ai.descriptorSetCount = 1;
        ai.pSetLayouts = &setLayout_;
        vk::check(vkAllocateDescriptorSets(dev, &ai, &set_), "vkAllocateDescriptorSets");

        VkWriteDescriptorSetAccelerationStructureKHR asw{
            VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET_ACCELERATION_STRUCTURE_KHR};
        asw.accelerationStructureCount = 1;
        asw.pAccelerationStructures = &sc.tlas;
        std::vector<VkDescriptorBufferInfo> bufInfos = {
            {sc.vertices.buffer, 0, VK_WHOLE_SIZE},       {sc.indices.buffer, 0, VK_WHOLE_SIZE},
            {sc.triMats.buffer, 0, VK_WHOLE_SIZE},        {sc.meshInfos.buffer, 0, VK_WHOLE_SIZE},
            {sc.materials.buffer, 0, VK_WHOLE_SIZE},      {sc.envMarginal.buffer, 0, VK_WHOLE_SIZE},
            {sc.envConditional.buffer, 0, VK_WHOLE_SIZE}, {sc.envPdf.buffer, 0, VK_WHOLE_SIZE},
            {ubo_.buffer, 0, VK_WHOLE_SIZE}};
        VkDescriptorImageInfo envInfo{sc.envSampler, sc.envImage.view, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
        VkDescriptorImageInfo boardInfo{sc.boardSampler, sc.boardImage.view, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
        VkDescriptorImageInfo targetInfo[3] = {{VK_NULL_HANDLE, targets.color.view, VK_IMAGE_LAYOUT_GENERAL},
                                               {VK_NULL_HANDLE, targets.albedo.view, VK_IMAGE_LAYOUT_GENERAL},
                                               {VK_NULL_HANDLE, targets.normal.view, VK_IMAGE_LAYOUT_GENERAL}};
        std::vector<VkWriteDescriptorSet> w;
        auto write = [&](uint32_t binding, VkDescriptorType type) -> VkWriteDescriptorSet& {
            VkWriteDescriptorSet x{VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET};
            x.dstSet = set_;
            x.dstBinding = binding;
            x.descriptorCount = 1;
            x.descriptorType = type;
            w.push_back(x);
            return w.back();
        };
        write(0, VK_DESCRIPTOR_TYPE_ACCELERATION_STRUCTURE_KHR).pNext = &asw;
        const uint32_t bufBindings[] = {1, 2, 3, 4, 5, 7, 8, 9};
        for (int i = 0; i < 8; ++i) write(bufBindings[i], VK_DESCRIPTOR_TYPE_STORAGE_BUFFER).pBufferInfo = &bufInfos[i];
        write(14, VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER).pBufferInfo = &bufInfos[8];
        write(6, VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER).pImageInfo = &envInfo;
        write(10, VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER).pImageInfo = &boardInfo;
        for (int i = 0; i < 3; ++i) write(11 + i, VK_DESCRIPTOR_TYPE_STORAGE_IMAGE).pImageInfo = &targetInfo[i];
        vkUpdateDescriptorSets(dev, static_cast<uint32_t>(w.size()), w.data(), 0, nullptr);

        // ---- パイプライン
        VkPushConstantRange pcr{VK_SHADER_STAGE_COMPUTE_BIT, 0, sizeof(PushConstants)};
        VkPipelineLayoutCreateInfo plci{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO};
        plci.setLayoutCount = 1;
        plci.pSetLayouts = &setLayout_;
        plci.pushConstantRangeCount = 1;
        plci.pPushConstantRanges = &pcr;
        vk::check(vkCreatePipelineLayout(dev, &plci, nullptr, &layout_), "vkCreatePipelineLayout");
        VkShaderModule sm = loadShader(dev, platform::exeDir() / "shaders" / "pathtrace.comp.spv");
        VkComputePipelineCreateInfo cci{VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO};
        cci.stage = {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, nullptr, 0, VK_SHADER_STAGE_COMPUTE_BIT, sm,
                     "main", nullptr};
        cci.layout = layout_;
        const VkResult r = vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &cci, nullptr, &pipeline_);
        vkDestroyShaderModule(dev, sm, nullptr);
        vk::check(r, "vkCreateComputePipelines");
    }

    void updateFrame(const FrameParams& fp) override {
        seed_ = fp.seed;
        const GpuScene& sc = *scene_;
        FrameUBO u{};
        const auto& cam = fp.camera;
        u.camPos = {cam.position, 0};
        u.camRight = {cam.right * (cam.tanHalfFovY * cam.aspect), 0};
        u.camUp = {cam.up * cam.tanHalfFovY, 0};
        u.camForward = {cam.forward, 0};
        u.boardMap = sc.boardMap;
        u.envParams = {fp.envIntensity, fp.envRotationRad, fp.radianceClamp, 0};
        u.dims = {targets_.width, targets_.height, sc.envDistW, sc.envDistH};
        u.counts = {fp.maxDepth, fp.rrStartDepth, 0, 0};
        u.boardMats = {sc.boardMats.mask, sc.boardMats.copper, sc.boardMats.finish, sc.boardMats.fr4};
        u.boardMats2 = {sc.boardMats.silk, 0, 0, 0};
        std::memcpy(ubo_.mapped, &u, sizeof(u));
        vmaFlushAllocation(ctx_->allocator(), ubo_.alloc, 0, VK_WHOLE_SIZE);
    }

    void recordSample(VkCommandBuffer cmd, uint32_t sampleIndex) override {
        vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline_);
        vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, layout_, 0, 1, &set_, 0, nullptr);
        PushConstants pc{sampleIndex, seed_};
        vkCmdPushConstants(cmd, layout_, VK_SHADER_STAGE_COMPUTE_BIT, 0, sizeof(pc), &pc);
        vkCmdDispatch(cmd, (targets_.width + 7) / 8, (targets_.height + 7) / 8, 1);
        // 次のサンプルが累積を読む前に書き込みを完了させる
        vk::memoryBarrier(cmd, VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT, VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT,
                          VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT | VK_PIPELINE_STAGE_2_COPY_BIT,
                          VK_ACCESS_2_SHADER_STORAGE_READ_BIT | VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT |
                              VK_ACCESS_2_TRANSFER_READ_BIT);
    }

    void destroy() override {
        if (!ctx_) return;
        VkDevice dev = ctx_->device();
        vkDeviceWaitIdle(dev);
        if (pipeline_) vkDestroyPipeline(dev, pipeline_, nullptr);
        if (layout_) vkDestroyPipelineLayout(dev, layout_, nullptr);
        if (pool_) vkDestroyDescriptorPool(dev, pool_, nullptr);
        if (setLayout_) vkDestroyDescriptorSetLayout(dev, setLayout_, nullptr);
        ctx_->destroyBuffer(ubo_);
        pipeline_ = VK_NULL_HANDLE;
        layout_ = VK_NULL_HANDLE;
        pool_ = VK_NULL_HANDLE;
        setLayout_ = VK_NULL_HANDLE;
        ctx_ = nullptr;
    }

private:
    vk::Context* ctx_ = nullptr;
    const GpuScene* scene_ = nullptr;
    RenderTargets targets_;
    uint32_t seed_ = 0;
    vk::Buffer ubo_;
    VkDescriptorSetLayout setLayout_ = VK_NULL_HANDLE;
    VkDescriptorPool pool_ = VK_NULL_HANDLE;
    VkDescriptorSet set_ = VK_NULL_HANDLE;
    VkPipelineLayout layout_ = VK_NULL_HANDLE;
    VkPipeline pipeline_ = VK_NULL_HANDLE;
};

}  // namespace

std::unique_ptr<Integrator> createIntegrator(const std::string& name) {
    if (name == "pt" || name == "path") return std::make_unique<PathTracer>();
    return nullptr;
}

}  // namespace krbr::render
