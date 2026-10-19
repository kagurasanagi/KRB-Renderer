#include "render/kernel.hpp"

#include <algorithm>
#include <map>
#include <mutex>
#include <numbers>
#include <stdexcept>

#include "core/log.hpp"
#include "core/platform.hpp"
#include "scene/spectrum.hpp"

namespace krbr::render {

KernelBinding bufferBinding(uint32_t binding, const vk::Buffer& b, VkDescriptorType type) {
    KernelBinding k;
    k.binding = binding;
    k.type = type;
    k.buffer = {b.buffer, 0, VK_WHOLE_SIZE};
    return k;
}

std::vector<KernelBinding> sceneBindings(const GpuScene& sc, const RenderTargets& targets, const vk::Buffer& frameUbo) {
    std::vector<KernelBinding> b;
    KernelBinding as;
    as.binding = 0;
    as.type = VK_DESCRIPTOR_TYPE_ACCELERATION_STRUCTURE_KHR;
    as.tlas = &sc.tlas;
    b.push_back(as);
    b.push_back(bufferBinding(1, sc.vertices));
    b.push_back(bufferBinding(2, sc.indices));
    b.push_back(bufferBinding(3, sc.triMats));
    b.push_back(bufferBinding(4, sc.meshInfos));
    b.push_back(bufferBinding(5, sc.materials));
    KernelBinding env;
    env.binding = 6;
    env.type = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER;
    env.image = {sc.envSampler, sc.envImage.view, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
    b.push_back(env);
    b.push_back(bufferBinding(7, sc.envMarginal));
    b.push_back(bufferBinding(8, sc.envConditional));
    b.push_back(bufferBinding(9, sc.envPdf));
    KernelBinding board;
    board.binding = 10;
    board.type = VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER;
    board.image = {sc.boardSampler, sc.boardImage.view, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL};
    b.push_back(board);
    const vk::Image* imgs[3] = {&targets.color, &targets.albedo, &targets.normal};
    for (uint32_t i = 0; i < 3; ++i) {
        KernelBinding t;
        t.binding = 11 + i;
        t.type = VK_DESCRIPTOR_TYPE_STORAGE_IMAGE;
        t.image = {VK_NULL_HANDLE, imgs[i]->view, VK_IMAGE_LAYOUT_GENERAL};
        b.push_back(t);
    }
    b.push_back(bufferBinding(14, frameUbo, VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER));
    b.push_back(bufferBinding(15, sc.instanceInfos));
    b.push_back(bufferBinding(16, sc.rgb2spec));
    b.push_back(bufferBinding(17, sc.rgb2specScale));
    b.push_back(bufferBinding(18, sc.emitters));
    b.push_back(bufferBinding(19, sc.emitterCdf));
    return b;
}

FrameUBO makeFrameUbo(const GpuScene& sc, const RenderTargets& targets, const FrameParams& fp) {
    FrameUBO u{};
    const auto& cam = fp.camera;
    u.camPos = {cam.position, 0};
    u.camRight = {cam.right * (cam.tanHalfFovY * cam.aspect), 0};
    u.camUp = {cam.up * cam.tanHalfFovY, 0};
    u.camForward = {cam.forward, 0};
    u.boardMap = sc.boardMap;
    u.envParams = {fp.envIntensity, fp.envRotationRad, fp.radianceClamp, 0};
    u.dims = {targets.width, targets.height, sc.envDistW, sc.envDistH};
    u.counts = {fp.maxDepth, fp.rrStartDepth, 0, 0};
    u.boardMats = {sc.boardMats.mask, sc.boardMats.copper, sc.boardMats.finish, sc.boardMats.fr4};
    u.boardMats2 = {sc.boardMats.silk, sc.boardMats.copper, sc.boardMats.fr4, 0};
    u.maskParams = sc.maskParams;
    u.maskParams2 = sc.maskParams2;
    u.spectral = {fp.spectral ? 1.0f : 0.0f, 1.0f / spectrum::cmfYIntegral(), 0, 0};
    const glm::mat3 toXyz = spectrum::linearSrgbToXyz();
    for (int i = 0; i < 3; ++i) u.rgbToXyz[i] = {toXyz[i], 0.0f};

    // 光源の選び方：シーンに届く放射束の見積もりに比例させる（どちらも 10% 以上は選ぶ）
    constexpr double pi = std::numbers::pi;
    const double R = sc.sceneRadius;
    const double envPower = pi * pi * R * R * sc.envAverageLuminance * fp.envIntensity;
    const double triPower = pi * sc.emitterPower;
    float pEnv = 1.0f;
    if (sc.emitterCount > 0) {
        pEnv = envPower > 0 ? static_cast<float>(std::clamp(envPower / (envPower + triPower), 0.1, 0.9)) : 0.0f;
    }
    u.lightParams = {pEnv, sc.emitterPower, sc.sceneRadius, static_cast<float>(sc.emitterCount)};
    u.sceneCenter = {sc.sceneCenter, 0.0f};
    const float radius = fp.vcmRadius > 0 ? fp.vcmRadius : 0.003f * sc.sceneRadius;
    u.vcmParams = {radius, fp.vcmAlpha, static_cast<float>(targets.width) * targets.height, 0.0f};
    fillLens(u, fp);
    return u;
}

namespace {
// 実レンズのデータは、ファイル名ごとに一度だけ読む
const scene::LensSystem& lensSystemFor(const std::string& file) {
    static std::mutex m;
    static std::map<std::string, scene::LensSystem> cache;
    std::lock_guard lock(m);
    auto it = cache.find(file);
    if (it != cache.end()) return it->second;
    scene::LensSystem sys = scene::LensSystem::doubleGauss50();
    if (!file.empty()) {
        std::string err;
        if (!scene::LensSystem::load(platform::u8path(file), sys, err)) {
            log::error("lens: {}; using {}", err, scene::LensSystem::doubleGauss50().name);
            sys = scene::LensSystem::doubleGauss50();
        }
    }
    return cache.emplace(file, std::move(sys)).first->second;
}
}  // namespace

void fillLens(FrameUBO& u, const FrameParams& fp) {
    const scene::LensParams& L = fp.lens;
    const auto& cam = fp.camera;
    const float focus = L.focusDistanceMm > 0 ? L.focusDistanceMm : cam.distance;
    const float f = std::max(L.focalLengthMm, 1.0f);
    const float a = 0.5f * f / std::max(L.fNumber, 0.5f);
    u.lens0 = {static_cast<float>(L.model), a, focus, static_cast<float>(L.blades >= 3 ? L.blades : 0)};
    // 口径食：画面の隅で、前の開口（半径 a）が絞りに対して直径 × opticalVignetting だけずれる距離に置く
    const float tanCorner = cam.tanHalfFovY * std::sqrt(1.0f + cam.aspect * cam.aspect);
    const float D = L.opticalVignetting > 0 ? L.opticalVignetting * 2.0f * a / std::max(tanCorner, 1e-4f) : 0.0f;
    u.lens1 = {glm::radians(L.bladeRotationDeg), std::clamp(L.bladeRoundness, 0.0f, 1.0f), D, a};
    const float v = focus > f * 1.0001f ? 1.0f / (1.0f / f - 1.0f / focus) : 1e9f;
    u.lens2 = {L.naturalVignetting ? 1.0f : 0.0f, L.lateralCA, v, f};
    constexpr float nd = 1.5168f;  // BK7 相当
    u.lens3 = {nd, L.abbeNumber > 0 ? (nd - 1.0f) / L.abbeNumber : 0.0f, 0.0f, 0.0f};
    if (L.model == scene::LensModel::Realistic) {
        const scene::LensSystem& sys = lensSystemFor(L.lensFile);
        const float frontToFocus = std::max(focus, 1.0f);
        const float filmDist = sys.filmDistanceFor(frontToFocus);
        const float fe = sys.effectiveFocalLength();
        const auto zs = sys.vertexZ();
        const size_t n = std::min<size_t>(sys.surfaces.size(), 16);
        for (size_t i = 0; i < n; ++i)
            u.lensSurfaces[i] = {sys.surfaces[i].radius, zs[i], sys.surfaces[i].ior, 0.5f * sys.surfaces[i].aperture};
        // フィルムの大きさは、ピントの距離での倍率で設定の画角になるように（近くにピントを合わせても構図が変わらない）
        const float halfH = fe * cam.tanHalfFovY * (frontToFocus > fe * 1.01f ? frontToFocus / (frontToFocus - fe) : 1.0f);
        u.lens3.z = 1.0f / std::max(sys.centerWeight(filmDist), 1e-12f);
        u.lens3.w = static_cast<float>(n);
        u.lens4 = {halfH * cam.aspect, halfH, zs[n - 1] - filmDist, 0.5f * sys.surfaces[n - 1].aperture};
    }
}

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
}  // namespace

void ComputeKernel::create(vk::Context& ctx, const std::string& spvName, const std::vector<KernelBinding>& bindings) {
    destroy();
    ctx_ = &ctx;
    VkDevice dev = ctx.device();
    std::vector<VkDescriptorSetLayoutBinding> lb;
    std::map<VkDescriptorType, uint32_t> counts;
    for (const auto& b : bindings) {
        lb.push_back({b.binding, b.type, 1, VK_SHADER_STAGE_COMPUTE_BIT, nullptr});
        ++counts[b.type];
    }
    VkDescriptorSetLayoutCreateInfo lci{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO};
    lci.bindingCount = static_cast<uint32_t>(lb.size());
    lci.pBindings = lb.data();
    vk::check(vkCreateDescriptorSetLayout(dev, &lci, nullptr, &setLayout_), "vkCreateDescriptorSetLayout");

    std::vector<VkDescriptorPoolSize> sizes;
    for (auto [t, n] : counts) sizes.push_back({t, n});
    VkDescriptorPoolCreateInfo pci{VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO};
    pci.maxSets = 1;
    pci.poolSizeCount = static_cast<uint32_t>(sizes.size());
    pci.pPoolSizes = sizes.data();
    vk::check(vkCreateDescriptorPool(dev, &pci, nullptr, &pool_), "vkCreateDescriptorPool");
    VkDescriptorSetAllocateInfo ai{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO};
    ai.descriptorPool = pool_;
    ai.descriptorSetCount = 1;
    ai.pSetLayouts = &setLayout_;
    vk::check(vkAllocateDescriptorSets(dev, &ai, &set_), "vkAllocateDescriptorSets");

    std::vector<VkWriteDescriptorSet> w(bindings.size());
    std::vector<VkWriteDescriptorSetAccelerationStructureKHR> asw(bindings.size());
    for (size_t i = 0; i < bindings.size(); ++i) {
        const auto& b = bindings[i];
        w[i] = {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET};
        w[i].dstSet = set_;
        w[i].dstBinding = b.binding;
        w[i].descriptorCount = 1;
        w[i].descriptorType = b.type;
        if (b.type == VK_DESCRIPTOR_TYPE_ACCELERATION_STRUCTURE_KHR) {
            asw[i] = {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET_ACCELERATION_STRUCTURE_KHR};
            asw[i].accelerationStructureCount = 1;
            asw[i].pAccelerationStructures = b.tlas;
            w[i].pNext = &asw[i];
        } else if (b.type == VK_DESCRIPTOR_TYPE_STORAGE_IMAGE || b.type == VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER) {
            w[i].pImageInfo = &b.image;
        } else {
            w[i].pBufferInfo = &b.buffer;
        }
    }
    vkUpdateDescriptorSets(dev, static_cast<uint32_t>(w.size()), w.data(), 0, nullptr);

    VkPushConstantRange pcr{VK_SHADER_STAGE_COMPUTE_BIT, 0, sizeof(PushConstants)};
    VkPipelineLayoutCreateInfo plci{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO};
    plci.setLayoutCount = 1;
    plci.pSetLayouts = &setLayout_;
    plci.pushConstantRangeCount = 1;
    plci.pPushConstantRanges = &pcr;
    vk::check(vkCreatePipelineLayout(dev, &plci, nullptr, &layout_), "vkCreatePipelineLayout");
    VkShaderModule sm = loadShader(dev, platform::exeDir() / "shaders" / spvName);
    VkComputePipelineCreateInfo cci{VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO};
    cci.stage = {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, nullptr, 0, VK_SHADER_STAGE_COMPUTE_BIT, sm, "main",
                 nullptr};
    cci.layout = layout_;
    const VkResult r = vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &cci, nullptr, &pipeline_);
    vkDestroyShaderModule(dev, sm, nullptr);
    vk::check(r, "vkCreateComputePipelines");
}

void ComputeKernel::dispatch(VkCommandBuffer cmd, const PushConstants& pc, uint32_t width, uint32_t height) const {
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, pipeline_);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, layout_, 0, 1, &set_, 0, nullptr);
    vkCmdPushConstants(cmd, layout_, VK_SHADER_STAGE_COMPUTE_BIT, 0, sizeof(pc), &pc);
    vkCmdDispatch(cmd, (width + 7) / 8, (height + 7) / 8, 1);
}

void ComputeKernel::destroy() {
    if (!ctx_) return;
    VkDevice dev = ctx_->device();
    if (pipeline_) vkDestroyPipeline(dev, pipeline_, nullptr);
    if (layout_) vkDestroyPipelineLayout(dev, layout_, nullptr);
    if (pool_) vkDestroyDescriptorPool(dev, pool_, nullptr);
    if (setLayout_) vkDestroyDescriptorSetLayout(dev, setLayout_, nullptr);
    pipeline_ = VK_NULL_HANDLE;
    layout_ = VK_NULL_HANDLE;
    pool_ = VK_NULL_HANDLE;
    setLayout_ = VK_NULL_HANDLE;
    ctx_ = nullptr;
}

void computeBarrier(VkCommandBuffer cmd) {
    vk::memoryBarrier(cmd, VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT | VK_PIPELINE_STAGE_2_TRANSFER_BIT,
                      VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT | VK_ACCESS_2_TRANSFER_WRITE_BIT,
                      VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT | VK_PIPELINE_STAGE_2_COPY_BIT,
                      VK_ACCESS_2_SHADER_STORAGE_READ_BIT | VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT |
                          VK_ACCESS_2_TRANSFER_READ_BIT);
}

}  // namespace krbr::render
