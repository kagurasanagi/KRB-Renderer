#include "render/denoiser.hpp"

#include <chrono>

#include "core/log.hpp"
#include "core/platform.hpp"

#ifdef KRBR_HAVE_OIDN
#include <OpenImageDenoise/oidn.h>
#endif
#ifdef _WIN32
#include <windows.h>
#include <vulkan/vulkan_win32.h>
#endif

namespace krbr::render {

#ifdef KRBR_HAVE_OIDN

namespace {

const char* qualityName(DenoiseQuality q) {
    switch (q) {
        case DenoiseQuality::Fast: return "fast";
        case DenoiseQuality::Balanced: return "balanced";
        default: return "high";
    }
}

int oidnQuality(DenoiseQuality q) {
    switch (q) {
        case DenoiseQuality::Fast: return OIDN_QUALITY_FAST;
        case DenoiseQuality::Balanced: return OIDN_QUALITY_BALANCED;
        default: return OIDN_QUALITY_HIGH;
    }
}

bool oidnOk(OIDNDevice dev, const char* what) {
    const char* msg = nullptr;
    if (oidnGetDeviceError(dev, &msg) != OIDN_ERROR_NONE) {
        log::warn("OIDN {}: {}", what, msg ? msg : "error");
        return false;
    }
    return true;
}

}  // namespace

struct Denoiser::Impl {
    vk::Context* ctx = nullptr;
    RenderTargets targets;
    uint32_t w = 0, h = 0;
    bool shared = false;  // GPU と外部メモリで共有
    DenoiseQuality quality = DenoiseQuality::High;
    bool ok = false;
    std::string name;

    struct Buf {
        VkBuffer buffer = VK_NULL_HANDLE;  // 共有の場合
        VkDeviceMemory memory = VK_NULL_HANDLE;
        HANDLE handle = nullptr;
        vk::Buffer host;  // CPU デバイスの場合
        OIDNBuffer oidn = nullptr;
        VkBuffer vkBuffer() const { return buffer ? buffer : host.buffer; }
    };
    Buf color, albedo, normal, out;
    OIDNDevice device = nullptr;
    OIDNFilter filter = nullptr;
    vk::Image outImage;
    VkDescriptorSetLayout setLayout = VK_NULL_HANDLE;
    VkDescriptorPool pool = VK_NULL_HANDLE;
    VkDescriptorSet set = VK_NULL_HANDLE;
    VkPipelineLayout layout = VK_NULL_HANDLE;
    VkPipeline pipeline = VK_NULL_HANDLE;

    bool createShared(Buf& b, VkDeviceSize size) {
        VkDevice dev = ctx->device();
        VkExternalMemoryBufferCreateInfo ext{VK_STRUCTURE_TYPE_EXTERNAL_MEMORY_BUFFER_CREATE_INFO};
        ext.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT;
        VkBufferCreateInfo bci{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO};
        bci.pNext = &ext;
        bci.size = size;
        bci.usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT;
        if (vkCreateBuffer(dev, &bci, nullptr, &b.buffer) != VK_SUCCESS) return false;
        VkMemoryRequirements req;
        vkGetBufferMemoryRequirements(dev, b.buffer, &req);
        VkPhysicalDeviceMemoryProperties mp;
        vkGetPhysicalDeviceMemoryProperties(ctx->physical(), &mp);
        uint32_t type = UINT32_MAX;
        for (uint32_t i = 0; i < mp.memoryTypeCount; ++i)
            if ((req.memoryTypeBits & (1u << i)) && (mp.memoryTypes[i].propertyFlags & VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)) {
                type = i;
                break;
            }
        if (type == UINT32_MAX) return false;
        VkMemoryDedicatedAllocateInfo ded{VK_STRUCTURE_TYPE_MEMORY_DEDICATED_ALLOCATE_INFO};
        ded.buffer = b.buffer;
        VkExportMemoryAllocateInfo exp{VK_STRUCTURE_TYPE_EXPORT_MEMORY_ALLOCATE_INFO};
        exp.pNext = &ded;
        exp.handleTypes = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT;
        VkMemoryAllocateInfo mai{VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO};
        mai.pNext = &exp;
        mai.allocationSize = req.size;
        mai.memoryTypeIndex = type;
        if (vkAllocateMemory(dev, &mai, nullptr, &b.memory) != VK_SUCCESS) return false;
        if (vkBindBufferMemory(dev, b.buffer, b.memory, 0) != VK_SUCCESS) return false;
        VkMemoryGetWin32HandleInfoKHR gi{VK_STRUCTURE_TYPE_MEMORY_GET_WIN32_HANDLE_INFO_KHR};
        gi.memory = b.memory;
        gi.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_OPAQUE_WIN32_BIT;
        // volk のビルド設定によらず使えるよう、関数は直接取得する
        const auto getHandle =
            reinterpret_cast<PFN_vkGetMemoryWin32HandleKHR>(vkGetDeviceProcAddr(dev, "vkGetMemoryWin32HandleKHR"));
        if (!getHandle || getHandle(dev, &gi, &b.handle) != VK_SUCCESS) return false;
        b.oidn = oidnNewSharedBufferFromWin32Handle(device, OIDN_EXTERNAL_MEMORY_TYPE_FLAG_OPAQUE_WIN32, b.handle,
                                                    nullptr, req.size);
        return b.oidn && oidnOk(device, "shared buffer");
    }

    bool createHost(Buf& b, VkDeviceSize size) {
        b.host = ctx->createBuffer(size,
                                   VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_SRC_BIT |
                                       VK_BUFFER_USAGE_TRANSFER_DST_BIT,
                                   true, true);
        b.oidn = oidnNewSharedBuffer(device, b.host.mapped, size);
        return b.oidn && oidnOk(device, "host buffer");
    }

    void releaseBuf(Buf& b) {
        if (b.oidn) oidnReleaseBuffer(b.oidn);
        if (b.handle) CloseHandle(b.handle);
        if (b.buffer) vkDestroyBuffer(ctx->device(), b.buffer, nullptr);
        if (b.memory) vkFreeMemory(ctx->device(), b.memory, nullptr);
        if (b.host.buffer) ctx->destroyBuffer(b.host);
        b = Buf{};
    }

    bool createDevice() {
        if (ctx->externalMemoryEnabled()) {
            device = oidnNewDeviceByUUID(ctx->info().deviceUUID);
            if (device) {
                oidnCommitDevice(device);
                const int types = oidnGetDeviceInt(device, "externalMemoryTypes");
                if (oidnOk(device, "GPU device") && (types & OIDN_EXTERNAL_MEMORY_TYPE_FLAG_OPAQUE_WIN32)) {
                    shared = true;
                    const int t = oidnGetDeviceInt(device, "type");
                    name = t == OIDN_DEVICE_TYPE_CUDA ? "CUDA" : t == OIDN_DEVICE_TYPE_HIP ? "HIP" : "GPU";
                    return true;
                }
                oidnReleaseDevice(device);
                device = nullptr;
            }
        }
        device = oidnNewDevice(OIDN_DEVICE_TYPE_CPU);
        if (!device) return false;
        oidnCommitDevice(device);
        name = "CPU";
        return oidnOk(device, "CPU device");
    }

    bool createPipeline() {
        VkDevice dev = ctx->device();
        VkDescriptorSetLayoutBinding lb[7];
        for (uint32_t i = 0; i < 7; ++i)
            lb[i] = {i, i < 3 ? VK_DESCRIPTOR_TYPE_STORAGE_IMAGE : VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 1,
                     VK_SHADER_STAGE_COMPUTE_BIT, nullptr};
        VkDescriptorSetLayoutCreateInfo lci{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_LAYOUT_CREATE_INFO};
        lci.bindingCount = 7;
        lci.pBindings = lb;
        vk::check(vkCreateDescriptorSetLayout(dev, &lci, nullptr, &setLayout), "vkCreateDescriptorSetLayout");
        const VkDescriptorPoolSize ps[] = {{VK_DESCRIPTOR_TYPE_STORAGE_IMAGE, 3}, {VK_DESCRIPTOR_TYPE_STORAGE_BUFFER, 4}};
        VkDescriptorPoolCreateInfo pci{VK_STRUCTURE_TYPE_DESCRIPTOR_POOL_CREATE_INFO};
        pci.maxSets = 1;
        pci.poolSizeCount = 2;
        pci.pPoolSizes = ps;
        vk::check(vkCreateDescriptorPool(dev, &pci, nullptr, &pool), "vkCreateDescriptorPool");
        VkDescriptorSetAllocateInfo ai{VK_STRUCTURE_TYPE_DESCRIPTOR_SET_ALLOCATE_INFO};
        ai.descriptorPool = pool;
        ai.descriptorSetCount = 1;
        ai.pSetLayouts = &setLayout;
        vk::check(vkAllocateDescriptorSets(dev, &ai, &set), "vkAllocateDescriptorSets");
        VkDescriptorImageInfo imgs[3] = {{VK_NULL_HANDLE, targets.color.view, VK_IMAGE_LAYOUT_GENERAL},
                                         {VK_NULL_HANDLE, targets.albedo.view, VK_IMAGE_LAYOUT_GENERAL},
                                         {VK_NULL_HANDLE, targets.normal.view, VK_IMAGE_LAYOUT_GENERAL}};
        VkDescriptorBufferInfo bufs[4] = {{color.vkBuffer(), 0, VK_WHOLE_SIZE},
                                          {albedo.vkBuffer(), 0, VK_WHOLE_SIZE},
                                          {normal.vkBuffer(), 0, VK_WHOLE_SIZE},
                                          {out.vkBuffer(), 0, VK_WHOLE_SIZE}};
        VkWriteDescriptorSet wr[7];
        for (uint32_t i = 0; i < 7; ++i) {
            wr[i] = {VK_STRUCTURE_TYPE_WRITE_DESCRIPTOR_SET};
            wr[i].dstSet = set;
            wr[i].dstBinding = i;
            wr[i].descriptorCount = 1;
            if (i < 3) {
                wr[i].descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_IMAGE;
                wr[i].pImageInfo = &imgs[i];
            } else {
                wr[i].descriptorType = VK_DESCRIPTOR_TYPE_STORAGE_BUFFER;
                wr[i].pBufferInfo = &bufs[i - 3];
            }
        }
        vkUpdateDescriptorSets(dev, 7, wr, 0, nullptr);
        VkPushConstantRange pcr{VK_SHADER_STAGE_COMPUTE_BIT, 0, 8};
        VkPipelineLayoutCreateInfo plci{VK_STRUCTURE_TYPE_PIPELINE_LAYOUT_CREATE_INFO};
        plci.setLayoutCount = 1;
        plci.pSetLayouts = &setLayout;
        plci.pushConstantRangeCount = 1;
        plci.pPushConstantRanges = &pcr;
        vk::check(vkCreatePipelineLayout(dev, &plci, nullptr, &layout), "vkCreatePipelineLayout");
        auto code = platform::readBinaryFile(platform::exeDir() / "shaders" / "denoise_prep.comp.spv");
        if (!code) return false;
        VkShaderModuleCreateInfo smci{VK_STRUCTURE_TYPE_SHADER_MODULE_CREATE_INFO};
        smci.codeSize = code->size();
        smci.pCode = reinterpret_cast<const uint32_t*>(code->data());
        VkShaderModule sm;
        vk::check(vkCreateShaderModule(dev, &smci, nullptr, &sm), "vkCreateShaderModule");
        VkComputePipelineCreateInfo cci{VK_STRUCTURE_TYPE_COMPUTE_PIPELINE_CREATE_INFO};
        cci.stage = {VK_STRUCTURE_TYPE_PIPELINE_SHADER_STAGE_CREATE_INFO, nullptr, 0, VK_SHADER_STAGE_COMPUTE_BIT, sm,
                     "main", nullptr};
        cci.layout = layout;
        const VkResult r = vkCreateComputePipelines(dev, VK_NULL_HANDLE, 1, &cci, nullptr, &pipeline);
        vkDestroyShaderModule(dev, sm, nullptr);
        return r == VK_SUCCESS;
    }

    void destroy() {
        if (!ctx) return;
        VkDevice dev = ctx->device();
        vkDeviceWaitIdle(dev);
        if (filter) oidnReleaseFilter(filter);
        filter = nullptr;
        for (Buf* b : {&color, &albedo, &normal, &out}) releaseBuf(*b);
        if (device) oidnReleaseDevice(device);
        device = nullptr;
        if (pipeline) vkDestroyPipeline(dev, pipeline, nullptr);
        if (layout) vkDestroyPipelineLayout(dev, layout, nullptr);
        if (pool) vkDestroyDescriptorPool(dev, pool, nullptr);
        if (setLayout) vkDestroyDescriptorSetLayout(dev, setLayout, nullptr);
        pipeline = VK_NULL_HANDLE;
        layout = VK_NULL_HANDLE;
        pool = VK_NULL_HANDLE;
        setLayout = VK_NULL_HANDLE;
        ctx->destroyImage(outImage);
        ok = false;
        ctx = nullptr;
    }
};

Denoiser::Denoiser() : impl_(std::make_unique<Impl>()) {}
Denoiser::~Denoiser() { destroy(); }
bool Denoiser::compiledIn() { return true; }
bool Denoiser::ready() const { return impl_->ok; }
const std::string& Denoiser::deviceName() const { return impl_->name; }
const vk::Image& Denoiser::output() const { return impl_->outImage; }

bool Denoiser::init(vk::Context& ctx, const RenderTargets& targets, DenoiseQuality quality) {
    destroy();
    auto& I = *impl_;
    I.ctx = &ctx;
    I.targets = targets;
    I.w = targets.width;
    I.h = targets.height;
    const VkDeviceSize size = static_cast<VkDeviceSize>(I.w) * I.h * 16;
    try {
        if (!I.createDevice()) {
            log::warn("OIDN: no device");
            destroy();
            return false;
        }
        for (Impl::Buf* b : {&I.color, &I.albedo, &I.normal, &I.out}) {
            if (I.shared ? !I.createShared(*b, size) : !I.createHost(*b, size)) {
                log::warn("OIDN: cannot create buffers");
                destroy();
                return false;
            }
        }
        I.filter = oidnNewFilter(I.device, "RT");
        const size_t rowStride = static_cast<size_t>(I.w) * 16;
        oidnSetFilterImage(I.filter, "color", I.color.oidn, OIDN_FORMAT_FLOAT3, I.w, I.h, 0, 16, rowStride);
        oidnSetFilterImage(I.filter, "albedo", I.albedo.oidn, OIDN_FORMAT_FLOAT3, I.w, I.h, 0, 16, rowStride);
        oidnSetFilterImage(I.filter, "normal", I.normal.oidn, OIDN_FORMAT_FLOAT3, I.w, I.h, 0, 16, rowStride);
        oidnSetFilterImage(I.filter, "output", I.out.oidn, OIDN_FORMAT_FLOAT3, I.w, I.h, 0, 16, rowStride);
        oidnSetFilterBool(I.filter, "hdr", true);
        oidnSetFilterInt(I.filter, "quality", oidnQuality(quality));
        I.quality = quality;
        oidnCommitFilter(I.filter);
        if (!oidnOk(I.device, "filter")) {
            destroy();
            return false;
        }
        I.outImage = ctx.createImage2D(I.w, I.h, 1, VK_FORMAT_R32G32B32A32_SFLOAT,
                                       VK_IMAGE_USAGE_STORAGE_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT);
        ctx.submitAndWait([&](VkCommandBuffer cmd) {
            vk::imageBarrier(cmd, I.outImage.image, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_GENERAL,
                             VK_PIPELINE_STAGE_2_NONE, 0, VK_PIPELINE_STAGE_2_COPY_BIT, VK_ACCESS_2_TRANSFER_WRITE_BIT);
        });
        if (!I.createPipeline()) {
            log::warn("OIDN: cannot create the preparation pipeline");
            destroy();
            return false;
        }
    } catch (const std::exception& e) {
        log::warn("OIDN: {}", e.what());
        destroy();
        return false;
    }
    I.ok = true;
    log::info("denoiser: OIDN on {} ({}), {}x{}, quality {}", I.name, I.shared ? "shared memory" : "host memory", I.w,
              I.h, qualityName(quality));
    return true;
}

void Denoiser::setQuality(DenoiseQuality quality) {
    auto& I = *impl_;
    if (!I.ok || I.quality == quality) return;
    oidnSetFilterInt(I.filter, "quality", oidnQuality(quality));
    oidnCommitFilter(I.filter);
    oidnOk(I.device, "quality");
    I.quality = quality;
}

void Denoiser::recordPrepare(VkCommandBuffer cmd) {
    auto& I = *impl_;
    vkCmdBindPipeline(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, I.pipeline);
    vkCmdBindDescriptorSets(cmd, VK_PIPELINE_BIND_POINT_COMPUTE, I.layout, 0, 1, &I.set, 0, nullptr);
    const uint32_t pc[2] = {I.w, I.h};
    vkCmdPushConstants(cmd, I.layout, VK_SHADER_STAGE_COMPUTE_BIT, 0, sizeof(pc), pc);
    vkCmdDispatch(cmd, (I.w + 7) / 8, (I.h + 7) / 8, 1);
    // 書き込みを、完了の待機後に OIDN（外部・CPU）から読めるようにする
    vk::memoryBarrier(cmd, VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT, VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT,
                      VK_PIPELINE_STAGE_2_HOST_BIT, VK_ACCESS_2_HOST_READ_BIT);
}

bool Denoiser::execute(double* ms) {
    auto& I = *impl_;
    const auto t0 = std::chrono::steady_clock::now();
    if (!I.shared)
        for (Impl::Buf* b : {&I.color, &I.albedo, &I.normal, &I.out})
            vmaInvalidateAllocation(I.ctx->allocator(), b->host.alloc, 0, VK_WHOLE_SIZE);
    oidnExecuteFilter(I.filter);  // 終わるまで待つ
    const bool ok = oidnOk(I.device, "execute");
    if (!I.shared) vmaFlushAllocation(I.ctx->allocator(), I.out.host.alloc, 0, VK_WHOLE_SIZE);
    if (ms) *ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t0).count();
    return ok;
}

void Denoiser::recordOutput(VkCommandBuffer cmd) {
    auto& I = *impl_;
    vk::memoryBarrier(cmd, VK_PIPELINE_STAGE_2_HOST_BIT, VK_ACCESS_2_HOST_WRITE_BIT, VK_PIPELINE_STAGE_2_COPY_BIT,
                      VK_ACCESS_2_TRANSFER_READ_BIT);
    VkBufferImageCopy region{};
    region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
    region.imageExtent = {I.w, I.h, 1};
    vkCmdCopyBufferToImage(cmd, I.out.vkBuffer(), I.outImage.image, VK_IMAGE_LAYOUT_GENERAL, 1, &region);
    vk::memoryBarrier(cmd, VK_PIPELINE_STAGE_2_COPY_BIT, VK_ACCESS_2_TRANSFER_WRITE_BIT,
                      VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT, VK_ACCESS_2_SHADER_STORAGE_READ_BIT);
}

void Denoiser::destroy() {
    if (impl_) impl_->destroy();
}

#else  // KRBR_HAVE_OIDN

struct Denoiser::Impl {
    vk::Image empty;
    std::string name = "none";
};
Denoiser::Denoiser() : impl_(std::make_unique<Impl>()) {}
Denoiser::~Denoiser() = default;
bool Denoiser::compiledIn() { return false; }
bool Denoiser::init(vk::Context&, const RenderTargets&, DenoiseQuality) { return false; }
bool Denoiser::ready() const { return false; }
void Denoiser::setQuality(DenoiseQuality) {}
const std::string& Denoiser::deviceName() const { return impl_->name; }
void Denoiser::recordPrepare(VkCommandBuffer) {}
bool Denoiser::execute(double*) { return false; }
void Denoiser::recordOutput(VkCommandBuffer) {}
const vk::Image& Denoiser::output() const { return impl_->empty; }
void Denoiser::destroy() {}

#endif

}  // namespace krbr::render
