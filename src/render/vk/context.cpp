#include "render/vk/context.hpp"

#include <algorithm>
#include <cstring>
#include <format>

#include "core/log.hpp"

namespace krbr::vk {

VkError::VkError(const std::string& what, VkResult r)
    : std::runtime_error(std::format("{} failed (VkResult {})", what, static_cast<int>(r))), result(r) {}

void check(VkResult r, const char* what) {
    if (r != VK_SUCCESS) throw VkError(what, r);
}

namespace {

bool g_volkReady = false;

void ensureVolk() {
    if (g_volkReady) return;
    check(volkInitialize(), "volkInitialize (Vulkan loader not found)");
    g_volkReady = true;
}

std::vector<VkExtensionProperties> deviceExtensions(VkPhysicalDevice pd) {
    uint32_t n = 0;
    vkEnumerateDeviceExtensionProperties(pd, nullptr, &n, nullptr);
    std::vector<VkExtensionProperties> v(n);
    vkEnumerateDeviceExtensionProperties(pd, nullptr, &n, v.data());
    return v;
}

bool hasExt(const std::vector<VkExtensionProperties>& exts, const char* name) {
    return std::any_of(exts.begin(), exts.end(),
                       [&](const VkExtensionProperties& e) { return std::strcmp(e.extensionName, name) == 0; });
}

DeviceInfo queryInfo(VkPhysicalDevice pd) {
    VkPhysicalDeviceProperties props;
    vkGetPhysicalDeviceProperties(pd, &props);
    DeviceInfo info;
    info.name = props.deviceName;
    info.apiVersion = props.apiVersion;
    info.driverVersion = props.driverVersion;
    info.discrete = props.deviceType == VK_PHYSICAL_DEVICE_TYPE_DISCRETE_GPU;
    const auto exts = deviceExtensions(pd);
    VkPhysicalDeviceRayQueryFeaturesKHR rq{VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_RAY_QUERY_FEATURES_KHR};
    VkPhysicalDeviceAccelerationStructureFeaturesKHR as{
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ACCELERATION_STRUCTURE_FEATURES_KHR};
    as.pNext = &rq;
    VkPhysicalDeviceFeatures2 f2{VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2};
    f2.pNext = &as;
    vkGetPhysicalDeviceFeatures2(pd, &f2);
    info.rayQuery = hasExt(exts, VK_KHR_RAY_QUERY_EXTENSION_NAME) && rq.rayQuery;
    info.accelerationStructure =
        hasExt(exts, VK_KHR_ACCELERATION_STRUCTURE_EXTENSION_NAME) && as.accelerationStructure;
    info.rayTracingPipeline = hasExt(exts, VK_KHR_RAY_TRACING_PIPELINE_EXTENSION_NAME);
    info.globalPriority =
        hasExt(exts, VK_KHR_GLOBAL_PRIORITY_EXTENSION_NAME) || hasExt(exts, VK_EXT_GLOBAL_PRIORITY_EXTENSION_NAME);
    VkPhysicalDeviceMemoryProperties mem;
    vkGetPhysicalDeviceMemoryProperties(pd, &mem);
    for (uint32_t i = 0; i < mem.memoryHeapCount; ++i)
        if (mem.memoryHeaps[i].flags & VK_MEMORY_HEAP_DEVICE_LOCAL_BIT)
            info.deviceLocalBytes = std::max<uint64_t>(info.deviceLocalBytes, mem.memoryHeaps[i].size);
    return info;
}

VkInstance createInstance(bool validation, VkDebugUtilsMessengerEXT* messenger,
                          const std::vector<const char*>& extraExts = {});

VKAPI_ATTR VkBool32 VKAPI_CALL debugCallback(VkDebugUtilsMessageSeverityFlagBitsEXT severity,
                                             VkDebugUtilsMessageTypeFlagsEXT, const VkDebugUtilsMessengerCallbackDataEXT* data,
                                             void*) {
    if (severity >= VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT)
        log::error("[vulkan] {}", data->pMessage);
    else if (severity >= VK_DEBUG_UTILS_MESSAGE_SEVERITY_WARNING_BIT_EXT)
        log::warn("[vulkan] {}", data->pMessage);
    return VK_FALSE;
}

VkInstance createInstance(bool validation, VkDebugUtilsMessengerEXT* messenger,
                          const std::vector<const char*>& extraExts) {
    ensureVolk();
    VkApplicationInfo app{VK_STRUCTURE_TYPE_APPLICATION_INFO};
    app.pApplicationName = "krbr";
    app.applicationVersion = VK_MAKE_VERSION(0, 1, 0);
    app.pEngineName = "krbr";
    app.apiVersion = VK_API_VERSION_1_3;

    std::vector<const char*> layers;
    // サーフェス拡張は、画面表示を頼まれたとき（extraExts）以外は含めない
    std::vector<const char*> exts(extraExts.begin(), extraExts.end());
    if (validation) {
        uint32_t n = 0;
        vkEnumerateInstanceLayerProperties(&n, nullptr);
        std::vector<VkLayerProperties> avail(n);
        vkEnumerateInstanceLayerProperties(&n, avail.data());
        const bool has = std::any_of(avail.begin(), avail.end(), [](const VkLayerProperties& l) {
            return std::strcmp(l.layerName, "VK_LAYER_KHRONOS_validation") == 0;
        });
        if (has) {
            layers.push_back("VK_LAYER_KHRONOS_validation");
            exts.push_back(VK_EXT_DEBUG_UTILS_EXTENSION_NAME);
        } else {
            log::warn("validation layer not available");
            validation = false;
        }
    }
    VkInstanceCreateInfo ci{VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO};
    ci.pApplicationInfo = &app;
    ci.enabledLayerCount = static_cast<uint32_t>(layers.size());
    ci.ppEnabledLayerNames = layers.data();
    ci.enabledExtensionCount = static_cast<uint32_t>(exts.size());
    ci.ppEnabledExtensionNames = exts.data();
    VkInstance inst = VK_NULL_HANDLE;
    check(vkCreateInstance(&ci, nullptr, &inst), "vkCreateInstance");
    volkLoadInstance(inst);
    if (validation && messenger) {
        VkDebugUtilsMessengerCreateInfoEXT mci{VK_STRUCTURE_TYPE_DEBUG_UTILS_MESSENGER_CREATE_INFO_EXT};
        mci.messageSeverity =
            VK_DEBUG_UTILS_MESSAGE_SEVERITY_WARNING_BIT_EXT | VK_DEBUG_UTILS_MESSAGE_SEVERITY_ERROR_BIT_EXT;
        mci.messageType = VK_DEBUG_UTILS_MESSAGE_TYPE_GENERAL_BIT_EXT | VK_DEBUG_UTILS_MESSAGE_TYPE_VALIDATION_BIT_EXT |
                          VK_DEBUG_UTILS_MESSAGE_TYPE_PERFORMANCE_BIT_EXT;
        mci.pfnUserCallback = debugCallback;
        vkCreateDebugUtilsMessengerEXT(inst, &mci, nullptr, messenger);
    }
    return inst;
}

}  // namespace

std::vector<DeviceInfo> Context::enumerate() {
    VkInstance inst = createInstance(false, nullptr);
    uint32_t n = 0;
    vkEnumeratePhysicalDevices(inst, &n, nullptr);
    std::vector<VkPhysicalDevice> pds(n);
    vkEnumeratePhysicalDevices(inst, &n, pds.data());
    std::vector<DeviceInfo> out;
    for (auto pd : pds) out.push_back(queryInfo(pd));
    vkDestroyInstance(inst, nullptr);
    return out;
}

Context::~Context() { destroy(); }

void Context::init(const ContextOptions& opts) {
    instance_ = createInstance(opts.validation, &messenger_, opts.instanceExtensions);
    if (opts.createSurface) {
        surface_ = opts.createSurface(instance_);
        if (!surface_) throw std::runtime_error("window surface creation failed");
    }

    uint32_t n = 0;
    vkEnumeratePhysicalDevices(instance_, &n, nullptr);
    std::vector<VkPhysicalDevice> pds(n);
    vkEnumeratePhysicalDevices(instance_, &n, pds.data());
    int bestScore = -1;
    for (auto pd : pds) {
        DeviceInfo di = queryInfo(pd);
        if (!di.rayQuery || !di.accelerationStructure || di.apiVersion < VK_API_VERSION_1_3) continue;
        const int score = (di.discrete ? 1000 : 0) + static_cast<int>(di.deviceLocalBytes >> 30);
        if (score > bestScore) {
            bestScore = score;
            physical_ = pd;
            info_ = di;
        }
    }
    if (!physical_) throw std::runtime_error("no Vulkan 1.3 device with VK_KHR_ray_query + acceleration_structure");

    // キュー：グラフィックスを持たない計算専用ファミリーを優先（デスクトップ描画と競合しにくい）
    uint32_t qn = 0;
    vkGetPhysicalDeviceQueueFamilyProperties(physical_, &qn, nullptr);
    std::vector<VkQueueFamilyProperties> qprops(qn);
    vkGetPhysicalDeviceQueueFamilyProperties(physical_, &qn, qprops.data());
    int computeOnly = -1, anyCompute = -1, present = -1;
    for (uint32_t i = 0; i < qn; ++i) {
        if (!(qprops[i].queueFlags & VK_QUEUE_COMPUTE_BIT)) continue;
        if (anyCompute < 0) anyCompute = static_cast<int>(i);
        if (!(qprops[i].queueFlags & VK_QUEUE_GRAPHICS_BIT) && computeOnly < 0) computeOnly = static_cast<int>(i);
        if (surface_ && present < 0) {
            VkBool32 ok = VK_FALSE;
            vkGetPhysicalDeviceSurfaceSupportKHR(physical_, i, surface_, &ok);
            if (ok) present = static_cast<int>(i);
        }
    }
    if (surface_) {
        // 画面表示：計算と表示の両方ができるファミリー（計算専用キューは表示できない）
        if (present < 0) throw std::runtime_error("no queue family can present to the window");
        queueFamily_ = static_cast<uint32_t>(present);
        computeOnly = -1;
    } else {
        queueFamily_ = static_cast<uint32_t>(computeOnly >= 0 ? computeOnly : anyCompute);
    }

    const auto exts = deviceExtensions(physical_);
    std::vector<const char*> devExts = {VK_KHR_ACCELERATION_STRUCTURE_EXTENSION_NAME, VK_KHR_RAY_QUERY_EXTENSION_NAME,
                                        VK_KHR_DEFERRED_HOST_OPERATIONS_EXTENSION_NAME};
    if (surface_) devExts.push_back(VK_KHR_SWAPCHAIN_EXTENSION_NAME);
    const char* prioExt = nullptr;
    if (opts.lowQueuePriority) {
        if (hasExt(exts, VK_KHR_GLOBAL_PRIORITY_EXTENSION_NAME))
            prioExt = VK_KHR_GLOBAL_PRIORITY_EXTENSION_NAME;
        else if (hasExt(exts, VK_EXT_GLOBAL_PRIORITY_EXTENSION_NAME))
            prioExt = VK_EXT_GLOBAL_PRIORITY_EXTENSION_NAME;
    }

    VkPhysicalDeviceVulkan12Features f12{VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_2_FEATURES};
    VkPhysicalDeviceVulkan13Features f13{VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_VULKAN_1_3_FEATURES};
    VkPhysicalDeviceAccelerationStructureFeaturesKHR fas{
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ACCELERATION_STRUCTURE_FEATURES_KHR};
    VkPhysicalDeviceRayQueryFeaturesKHR frq{VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_RAY_QUERY_FEATURES_KHR};
    VkPhysicalDeviceFeatures2 f2{VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_FEATURES_2};
    f2.features.shaderInt64 = VK_TRUE;
    f12.bufferDeviceAddress = VK_TRUE;
    f12.descriptorIndexing = VK_TRUE;
    f12.runtimeDescriptorArray = VK_TRUE;
    f12.shaderSampledImageArrayNonUniformIndexing = VK_TRUE;
    f12.scalarBlockLayout = VK_TRUE;
    f13.synchronization2 = VK_TRUE;
    f13.maintenance4 = VK_TRUE;
    f13.dynamicRendering = VK_TRUE;  // ビューアの操作パネル（ImGui）の描画用
    fas.accelerationStructure = VK_TRUE;
    frq.rayQuery = VK_TRUE;
    f2.pNext = &f12;
    f12.pNext = &f13;
    f13.pNext = &fas;
    fas.pNext = &frq;

    const float prio = 0.5f;
    VkDeviceQueueGlobalPriorityCreateInfoKHR gp{VK_STRUCTURE_TYPE_DEVICE_QUEUE_GLOBAL_PRIORITY_CREATE_INFO_KHR};
    gp.globalPriority = VK_QUEUE_GLOBAL_PRIORITY_LOW_KHR;
    VkDeviceQueueCreateInfo qci{VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO};
    qci.queueFamilyIndex = queueFamily_;
    qci.queueCount = 1;
    qci.pQueuePriorities = &prio;

    auto tryCreate = [&](bool withPrio) {
        std::vector<const char*> e = devExts;
        qci.pNext = nullptr;
        if (withPrio && prioExt) {
            e.push_back(prioExt);
            qci.pNext = &gp;
        }
        VkDeviceCreateInfo dci{VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO};
        dci.pNext = &f2;
        dci.queueCreateInfoCount = 1;
        dci.pQueueCreateInfos = &qci;
        dci.enabledExtensionCount = static_cast<uint32_t>(e.size());
        dci.ppEnabledExtensionNames = e.data();
        return vkCreateDevice(physical_, &dci, nullptr, &device_);
    };
    VkResult r = tryCreate(true);
    bool lowPrio = prioExt != nullptr && r == VK_SUCCESS;
    if (r != VK_SUCCESS && prioExt) {
        log::debug("device creation with low global priority failed ({}), retrying", static_cast<int>(r));
        r = tryCreate(false);
        lowPrio = false;
    }
    check(r, "vkCreateDevice");
    volkLoadDevice(device_);
    vkGetDeviceQueue(device_, queueFamily_, 0, &queue_);

    VkPhysicalDeviceProperties2 p2{VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2};
    p2.pNext = &asProps_;
    vkGetPhysicalDeviceProperties2(physical_, &p2);

    VmaVulkanFunctions fns{};
    fns.vkGetInstanceProcAddr = vkGetInstanceProcAddr;
    fns.vkGetDeviceProcAddr = vkGetDeviceProcAddr;
    VmaAllocatorCreateInfo aci{};
    aci.flags = VMA_ALLOCATOR_CREATE_BUFFER_DEVICE_ADDRESS_BIT;
    aci.vulkanApiVersion = VK_API_VERSION_1_3;
    aci.physicalDevice = physical_;
    aci.device = device_;
    aci.instance = instance_;
    aci.pVulkanFunctions = &fns;
    check(vmaCreateAllocator(&aci, &allocator_), "vmaCreateAllocator");

    VkCommandPoolCreateInfo pci{VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO};
    pci.flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT;
    pci.queueFamilyIndex = queueFamily_;
    check(vkCreateCommandPool(device_, &pci, nullptr, &pool_), "vkCreateCommandPool");
    VkFenceCreateInfo fci{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    check(vkCreateFence(device_, &fci, nullptr, &fence_), "vkCreateFence");

    log::info("GPU: {} (Vulkan {}.{}.{}), queue family {}{}{}", info_.name, VK_API_VERSION_MAJOR(info_.apiVersion),
              VK_API_VERSION_MINOR(info_.apiVersion), VK_API_VERSION_PATCH(info_.apiVersion), queueFamily_,
              computeOnly >= 0 ? " (compute-only)" : "", lowPrio ? ", low global priority" : "");
}

void Context::destroy() {
    if (device_) {
        vkDeviceWaitIdle(device_);
        if (fence_) vkDestroyFence(device_, fence_, nullptr);
        if (pool_) vkDestroyCommandPool(device_, pool_, nullptr);
        if (allocator_) vmaDestroyAllocator(allocator_);
        vkDestroyDevice(device_, nullptr);
    }
    if (surface_) vkDestroySurfaceKHR(instance_, surface_, nullptr);
    surface_ = VK_NULL_HANDLE;
    if (messenger_) vkDestroyDebugUtilsMessengerEXT(instance_, messenger_, nullptr);
    if (instance_) vkDestroyInstance(instance_, nullptr);
    fence_ = VK_NULL_HANDLE;
    pool_ = VK_NULL_HANDLE;
    allocator_ = nullptr;
    device_ = VK_NULL_HANDLE;
    messenger_ = VK_NULL_HANDLE;
    instance_ = VK_NULL_HANDLE;
}

Buffer Context::createBuffer(VkDeviceSize size, VkBufferUsageFlags usage, bool hostVisible, bool hostReadback) {
    Buffer b;
    b.size = size;
    VkBufferCreateInfo bci{VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO};
    bci.size = std::max<VkDeviceSize>(size, 16);
    bci.usage = usage | VK_BUFFER_USAGE_SHADER_DEVICE_ADDRESS_BIT;
    VmaAllocationCreateInfo aci{};
    aci.usage = VMA_MEMORY_USAGE_AUTO;
    if (hostVisible) {
        aci.flags = VMA_ALLOCATION_CREATE_MAPPED_BIT | (hostReadback ? VMA_ALLOCATION_CREATE_HOST_ACCESS_RANDOM_BIT
                                                                     : VMA_ALLOCATION_CREATE_HOST_ACCESS_SEQUENTIAL_WRITE_BIT);
        if (hostReadback) aci.usage = VMA_MEMORY_USAGE_AUTO_PREFER_HOST;
    }
    VmaAllocationInfo ai{};
    check(vmaCreateBuffer(allocator_, &bci, &aci, &b.buffer, &b.alloc, &ai), "vmaCreateBuffer");
    b.mapped = ai.pMappedData;
    VkBufferDeviceAddressInfo dai{VK_STRUCTURE_TYPE_BUFFER_DEVICE_ADDRESS_INFO};
    dai.buffer = b.buffer;
    b.address = vkGetBufferDeviceAddress(device_, &dai);
    return b;
}

void Context::destroyBuffer(Buffer& b) {
    if (b.buffer) vmaDestroyBuffer(allocator_, b.buffer, b.alloc);
    b = Buffer{};
}

Buffer Context::createDeviceBuffer(const void* data, VkDeviceSize size, VkBufferUsageFlags usage) {
    Buffer b = createBuffer(size, usage | VK_BUFFER_USAGE_TRANSFER_DST_BIT, false);
    if (data && size) upload(b, data, size);
    return b;
}

void Context::upload(Buffer& dst, const void* data, VkDeviceSize size, VkDeviceSize dstOffset) {
    if (!size) return;
    Buffer staging = createBuffer(size, VK_BUFFER_USAGE_TRANSFER_SRC_BIT, true);
    std::memcpy(staging.mapped, data, size);
    vmaFlushAllocation(allocator_, staging.alloc, 0, VK_WHOLE_SIZE);
    submitAndWait([&](VkCommandBuffer cmd) {
        VkBufferCopy region{0, dstOffset, size};
        vkCmdCopyBuffer(cmd, staging.buffer, dst.buffer, 1, &region);
    });
    destroyBuffer(staging);
}

Image Context::createImage2D(uint32_t w, uint32_t h, uint32_t layers, VkFormat fmt, VkImageUsageFlags usage) {
    Image img;
    img.width = w;
    img.height = h;
    img.layers = layers;
    img.format = fmt;
    VkImageCreateInfo ici{VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO};
    ici.imageType = VK_IMAGE_TYPE_2D;
    ici.format = fmt;
    ici.extent = {w, h, 1};
    ici.mipLevels = 1;
    ici.arrayLayers = layers;
    ici.samples = VK_SAMPLE_COUNT_1_BIT;
    ici.tiling = VK_IMAGE_TILING_OPTIMAL;
    ici.usage = usage;
    ici.initialLayout = VK_IMAGE_LAYOUT_UNDEFINED;
    VmaAllocationCreateInfo aci{};
    aci.usage = VMA_MEMORY_USAGE_AUTO_PREFER_DEVICE;
    check(vmaCreateImage(allocator_, &ici, &aci, &img.image, &img.alloc, nullptr), "vmaCreateImage");
    VkImageViewCreateInfo vci{VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO};
    vci.image = img.image;
    vci.viewType = layers > 1 ? VK_IMAGE_VIEW_TYPE_2D_ARRAY : VK_IMAGE_VIEW_TYPE_2D;
    vci.format = fmt;
    vci.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, layers};
    check(vkCreateImageView(device_, &vci, nullptr, &img.view), "vkCreateImageView");
    return img;
}

void Context::destroyImage(Image& img) {
    if (img.view) vkDestroyImageView(device_, img.view, nullptr);
    if (img.image) vmaDestroyImage(allocator_, img.image, img.alloc);
    img = Image{};
}

void Context::uploadImage(Image& img, const void* data, VkDeviceSize size, VkImageLayout finalLayout) {
    Buffer staging = createBuffer(size, VK_BUFFER_USAGE_TRANSFER_SRC_BIT, true);
    std::memcpy(staging.mapped, data, size);
    vmaFlushAllocation(allocator_, staging.alloc, 0, VK_WHOLE_SIZE);
    submitAndWait([&](VkCommandBuffer cmd) {
        imageBarrier(cmd, img.image, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                     VK_PIPELINE_STAGE_2_NONE, 0, VK_PIPELINE_STAGE_2_COPY_BIT, VK_ACCESS_2_TRANSFER_WRITE_BIT,
                     img.layers);
        VkBufferImageCopy region{};
        region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, img.layers};
        region.imageExtent = {img.width, img.height, 1};
        vkCmdCopyBufferToImage(cmd, staging.buffer, img.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &region);
        imageBarrier(cmd, img.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, finalLayout, VK_PIPELINE_STAGE_2_COPY_BIT,
                     VK_ACCESS_2_TRANSFER_WRITE_BIT, VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT,
                     VK_ACCESS_2_SHADER_SAMPLED_READ_BIT | VK_ACCESS_2_SHADER_STORAGE_READ_BIT, img.layers);
    });
    destroyBuffer(staging);
}

void Context::submitAndWait(const std::function<void(VkCommandBuffer)>& record) {
    VkCommandBufferAllocateInfo ai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO};
    ai.commandPool = pool_;
    ai.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
    ai.commandBufferCount = 1;
    VkCommandBuffer cmd;
    check(vkAllocateCommandBuffers(device_, &ai, &cmd), "vkAllocateCommandBuffers");
    VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
    bi.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
    vkBeginCommandBuffer(cmd, &bi);
    record(cmd);
    check(vkEndCommandBuffer(cmd), "vkEndCommandBuffer");
    VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO};
    si.commandBufferCount = 1;
    si.pCommandBuffers = &cmd;
    check(vkResetFences(device_, 1, &fence_), "vkResetFences");
    check(vkQueueSubmit(queue_, 1, &si, fence_), "vkQueueSubmit");
    check(vkWaitForFences(device_, 1, &fence_, VK_TRUE, UINT64_MAX), "vkWaitForFences");
    vkFreeCommandBuffers(device_, pool_, 1, &cmd);
}

void imageBarrier(VkCommandBuffer cmd, VkImage image, VkImageLayout oldLayout, VkImageLayout newLayout,
                  VkPipelineStageFlags2 srcStage, VkAccessFlags2 srcAccess, VkPipelineStageFlags2 dstStage,
                  VkAccessFlags2 dstAccess, uint32_t layers) {
    VkImageMemoryBarrier2 b{VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER_2};
    b.srcStageMask = srcStage;
    b.srcAccessMask = srcAccess;
    b.dstStageMask = dstStage;
    b.dstAccessMask = dstAccess;
    b.oldLayout = oldLayout;
    b.newLayout = newLayout;
    b.srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    b.dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED;
    b.image = image;
    b.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, layers};
    VkDependencyInfo di{VK_STRUCTURE_TYPE_DEPENDENCY_INFO};
    di.imageMemoryBarrierCount = 1;
    di.pImageMemoryBarriers = &b;
    vkCmdPipelineBarrier2(cmd, &di);
}

void memoryBarrier(VkCommandBuffer cmd, VkPipelineStageFlags2 srcStage, VkAccessFlags2 srcAccess,
                   VkPipelineStageFlags2 dstStage, VkAccessFlags2 dstAccess) {
    VkMemoryBarrier2 b{VK_STRUCTURE_TYPE_MEMORY_BARRIER_2};
    b.srcStageMask = srcStage;
    b.srcAccessMask = srcAccess;
    b.dstStageMask = dstStage;
    b.dstAccessMask = dstAccess;
    VkDependencyInfo di{VK_STRUCTURE_TYPE_DEPENDENCY_INFO};
    di.memoryBarrierCount = 1;
    di.pMemoryBarriers = &b;
    vkCmdPipelineBarrier2(cmd, &di);
}

}  // namespace krbr::vk
