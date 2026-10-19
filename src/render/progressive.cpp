#include "render/progressive.hpp"

#include <stdexcept>

#include "scene/spectrum.hpp"

namespace krbr::render {

void xyzToLinearSrgb(io::ImageF& img) {
    const glm::mat3 m = spectrum::xyzToLinearSrgb();
    for (size_t i = 0; i + 3 < img.rgba.size(); i += 4) {
        const glm::vec3 c = m * glm::vec3(img.rgba[i], img.rgba[i + 1], img.rgba[i + 2]);
        img.rgba[i] = c.r;
        img.rgba[i + 1] = c.g;
        img.rgba[i + 2] = c.b;
    }
}

Progressive::~Progressive() { destroy(); }

void Progressive::init(vk::Context& ctx, const GpuScene& scene, const std::string& integrator, uint32_t width,
                       uint32_t height, const FrameParams& frame) {
    ctx_ = &ctx;
    scene_ = &scene;
    integratorName_ = integrator;
    frame_ = frame;
    targets_.width = width;
    targets_.height = height;
    createTargets();
}

void Progressive::createTargets() {
    const VkImageUsageFlags usage = VK_IMAGE_USAGE_STORAGE_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT;
    const uint32_t w = targets_.width, h = targets_.height;
    targets_.color = ctx_->createImage2D(w, h, 1, VK_FORMAT_R32G32B32A32_SFLOAT, usage);
    targets_.albedo = ctx_->createImage2D(w, h, 1, VK_FORMAT_R32G32B32A32_SFLOAT, usage);
    targets_.normal = ctx_->createImage2D(w, h, 1, VK_FORMAT_R32G32B32A32_SFLOAT, usage);
    ctx_->submitAndWait([&](VkCommandBuffer cmd) {
        for (VkImage img : {targets_.color.image, targets_.albedo.image, targets_.normal.image})
            vk::imageBarrier(cmd, img, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_GENERAL, VK_PIPELINE_STAGE_2_NONE, 0,
                             VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT,
                             VK_ACCESS_2_SHADER_STORAGE_READ_BIT | VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT);
    });
    integrator_ = createIntegrator(integratorName_);
    if (!integrator_) throw std::runtime_error("unknown integrator: " + integratorName_);
    integrator_->setup(*ctx_, *scene_, targets_, frame_);
    samples_ = 0;
}

void Progressive::destroyTargets() {
    if (!ctx_) return;
    if (integrator_) integrator_->destroy();
    integrator_.reset();
    ctx_->destroyImage(targets_.color);
    ctx_->destroyImage(targets_.albedo);
    ctx_->destroyImage(targets_.normal);
}

void Progressive::resize(uint32_t width, uint32_t height) {
    if (width == targets_.width && height == targets_.height) return;
    vkDeviceWaitIdle(ctx_->device());
    destroyTargets();
    targets_.width = width;
    targets_.height = height;
    createTargets();
}

void Progressive::rebuildIntegrator() {
    vkDeviceWaitIdle(ctx_->device());
    integrator_->destroy();
    integrator_ = createIntegrator(integratorName_);
    if (!integrator_) throw std::runtime_error("unknown integrator: " + integratorName_);
    integrator_->setup(*ctx_, *scene_, targets_, frame_);
    samples_ = 0;
}

void Progressive::setIntegrator(const std::string& name) {
    if (name == integratorName_) return;
    integratorName_ = name;
    rebuildIntegrator();
}

void Progressive::setFrame(const FrameParams& frame) {
    frame_ = frame;
    integrator_->updateFrame(frame_);
    samples_ = 0;
}

void Progressive::record(VkCommandBuffer cmd, uint32_t count) {
    for (uint32_t i = 0; i < count; ++i) integrator_->recordSample(cmd, samples_++);
}

AovImages Progressive::readback() {
    AovImages out;
    const uint32_t w = targets_.width, h = targets_.height;
    const VkDeviceSize bytes = static_cast<VkDeviceSize>(w) * h * 16;
    vk::Buffer rb = ctx_->createBuffer(bytes * 3, VK_BUFFER_USAGE_TRANSFER_DST_BIT, true, true);
    ctx_->submitAndWait([&](VkCommandBuffer cmd) {
        const VkImage imgs[3] = {targets_.color.image, targets_.albedo.image, targets_.normal.image};
        for (int k = 0; k < 3; ++k) {
            VkBufferImageCopy region{};
            region.bufferOffset = bytes * k;
            region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
            region.imageExtent = {w, h, 1};
            vkCmdCopyImageToBuffer(cmd, imgs[k], VK_IMAGE_LAYOUT_GENERAL, rb.buffer, 1, &region);
        }
    });
    vmaInvalidateAllocation(ctx_->allocator(), rb.alloc, 0, VK_WHOLE_SIZE);
    io::ImageF* dst[3] = {&out.color, &out.albedo, &out.normal};
    const float* src = static_cast<const float*>(rb.mapped);
    const size_t n = static_cast<size_t>(w) * h;
    for (int k = 0; k < 3; ++k) {
        dst[k]->width = static_cast<int>(w);
        dst[k]->height = static_cast<int>(h);
        dst[k]->rgba.resize(n * 4);
        const float* p = src + n * 4 * k;
        for (size_t i = 0; i < n; ++i) {
            const float cnt = p[i * 4 + 3];  // A にサンプル数が入っている
            const float inv = cnt > 0 ? 1.0f / cnt : 0.0f;
            for (int c = 0; c < 3; ++c) dst[k]->rgba[i * 4 + c] = p[i * 4 + c] * inv;
            dst[k]->rgba[i * 4 + 3] = 1.0f;
        }
    }
    ctx_->destroyBuffer(rb);
    if (xyzOutput()) {
        xyzToLinearSrgb(out.color);
        xyzToLinearSrgb(out.albedo);
    }
    return out;
}

void Progressive::destroy() {
    if (!ctx_) return;
    vkDeviceWaitIdle(ctx_->device());
    destroyTargets();
    ctx_ = nullptr;
}

}  // namespace krbr::render
