#pragma once
// ヘッドレス Vulkan コンテキスト。サーフェス／スワップチェーン系の拡張は一切有効にしない。
#include <volk.h>
#include <vk_mem_alloc.h>

#include <cstdint>
#include <functional>
#include <stdexcept>
#include <string>
#include <vector>

namespace krbr::vk {

class VkError : public std::runtime_error {
public:
    VkError(const std::string& what, VkResult r);
    VkResult result;
};

void check(VkResult r, const char* what);

struct Buffer {
    VkBuffer buffer = VK_NULL_HANDLE;
    VmaAllocation alloc = nullptr;
    VkDeviceSize size = 0;
    void* mapped = nullptr;
    VkDeviceAddress address = 0;
};

struct Image {
    VkImage image = VK_NULL_HANDLE;
    VmaAllocation alloc = nullptr;
    VkImageView view = VK_NULL_HANDLE;
    VkFormat format = VK_FORMAT_UNDEFINED;
    uint32_t width = 0, height = 0, layers = 1;
};

struct DeviceInfo {
    std::string name;
    uint32_t apiVersion = 0;
    uint32_t driverVersion = 0;
    bool discrete = false;
    bool rayQuery = false;
    bool accelerationStructure = false;
    bool rayTracingPipeline = false;
    bool globalPriority = false;
    uint64_t deviceLocalBytes = 0;
};

struct ContextOptions {
    bool validation = false;
    bool lowQueuePriority = true;  // VK_KHR_global_priority が使えれば LOW で作る
};

class Context {
public:
    Context() = default;
    ~Context();
    Context(const Context&) = delete;
    Context& operator=(const Context&) = delete;

    // 全物理デバイスの情報を列挙（デバイスは作らない）
    static std::vector<DeviceInfo> enumerate();

    void init(const ContextOptions& opts);
    void destroy();

    VkInstance instance() const { return instance_; }
    VkPhysicalDevice physical() const { return physical_; }
    VkDevice device() const { return device_; }
    VkQueue queue() const { return queue_; }
    uint32_t queueFamily() const { return queueFamily_; }
    VmaAllocator allocator() const { return allocator_; }
    const DeviceInfo& info() const { return info_; }
    const VkPhysicalDeviceAccelerationStructurePropertiesKHR& asProps() const { return asProps_; }

    Buffer createBuffer(VkDeviceSize size, VkBufferUsageFlags usage, bool hostVisible, bool hostReadback = false);
    void destroyBuffer(Buffer& b);
    // device-local バッファを作ってステージング経由で転送
    Buffer createDeviceBuffer(const void* data, VkDeviceSize size, VkBufferUsageFlags usage);
    void upload(Buffer& dst, const void* data, VkDeviceSize size, VkDeviceSize dstOffset = 0);

    Image createImage2D(uint32_t w, uint32_t h, uint32_t layers, VkFormat fmt, VkImageUsageFlags usage);
    void destroyImage(Image& img);
    // 画像全体にデータを転送して最終レイアウトへ遷移
    void uploadImage(Image& img, const void* data, VkDeviceSize size, VkImageLayout finalLayout);

    // 1回きりのコマンドを記録して完了まで待つ
    void submitAndWait(const std::function<void(VkCommandBuffer)>& record);

    VkCommandPool commandPool() const { return pool_; }

private:
    VkInstance instance_ = VK_NULL_HANDLE;
    VkDebugUtilsMessengerEXT messenger_ = VK_NULL_HANDLE;
    VkPhysicalDevice physical_ = VK_NULL_HANDLE;
    VkDevice device_ = VK_NULL_HANDLE;
    VkQueue queue_ = VK_NULL_HANDLE;
    uint32_t queueFamily_ = 0;
    VmaAllocator allocator_ = nullptr;
    VkCommandPool pool_ = VK_NULL_HANDLE;
    VkFence fence_ = VK_NULL_HANDLE;
    DeviceInfo info_;
    VkPhysicalDeviceAccelerationStructurePropertiesKHR asProps_{
        VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_ACCELERATION_STRUCTURE_PROPERTIES_KHR};
};

void imageBarrier(VkCommandBuffer cmd, VkImage image, VkImageLayout oldLayout, VkImageLayout newLayout,
                  VkPipelineStageFlags2 srcStage, VkAccessFlags2 srcAccess, VkPipelineStageFlags2 dstStage,
                  VkAccessFlags2 dstAccess, uint32_t layers = 1);
void memoryBarrier(VkCommandBuffer cmd, VkPipelineStageFlags2 srcStage, VkAccessFlags2 srcAccess,
                   VkPipelineStageFlags2 dstStage, VkAccessFlags2 dstAccess);

}  // namespace krbr::vk
