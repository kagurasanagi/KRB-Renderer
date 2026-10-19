#include "render/renderer.hpp"

#include <chrono>
#include <cstring>
#include <thread>

#include "core/log.hpp"

namespace krbr::render {

RenderOutput renderOffscreen(vk::Context& ctx, const GpuScene& scene, const RenderSettings& s) {
    RenderOutput out;
    auto integrator = createIntegrator(s.integrator);
    if (!integrator) throw std::runtime_error("unknown integrator: " + s.integrator);

    RenderTargets t;
    t.width = s.width;
    t.height = s.height;
    const VkImageUsageFlags usage = VK_IMAGE_USAGE_STORAGE_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT;
    t.color = ctx.createImage2D(s.width, s.height, 1, VK_FORMAT_R32G32B32A32_SFLOAT, usage);
    t.albedo = ctx.createImage2D(s.width, s.height, 1, VK_FORMAT_R32G32B32A32_SFLOAT, usage);
    t.normal = ctx.createImage2D(s.width, s.height, 1, VK_FORMAT_R32G32B32A32_SFLOAT, usage);
    ctx.submitAndWait([&](VkCommandBuffer cmd) {
        for (VkImage img : {t.color.image, t.albedo.image, t.normal.image})
            vk::imageBarrier(cmd, img, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_GENERAL, VK_PIPELINE_STAGE_2_NONE, 0,
                             VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT,
                             VK_ACCESS_2_SHADER_STORAGE_READ_BIT | VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT);
    });

    integrator->setup(ctx, scene, t, s.frame);
    log::info("rendering {}x{}, {} spp, integrator '{}', max depth {}", s.width, s.height, s.spp, integrator->name(),
              s.frame.maxDepth);

    // 1 サンプルずつ投入して完了を待つ。GPU を長時間占有しないので、作業中の画面描画が止まりにくい
    const auto t0 = std::chrono::steady_clock::now();
    auto lastLog = t0;
    for (uint32_t i = 0; i < s.spp; ++i) {
        ctx.submitAndWait([&](VkCommandBuffer cmd) { integrator->recordSample(cmd, i); });
        std::this_thread::yield();
        const auto now = std::chrono::steady_clock::now();
        if (std::chrono::duration<double>(now - lastLog).count() > 2.0 || i + 1 == s.spp) {
            const double el = std::chrono::duration<double>(now - t0).count();
            const double eta = el / (i + 1) * (s.spp - i - 1);
            log::info("  {}/{} spp, {:.1f} s elapsed, ~{:.1f} s left", i + 1, s.spp, el, eta);
            lastLog = now;
        }
    }
    out.seconds = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();

    // 読み戻し
    const VkDeviceSize bytes = static_cast<VkDeviceSize>(s.width) * s.height * 16;
    vk::Buffer rb = ctx.createBuffer(bytes * 3, VK_BUFFER_USAGE_TRANSFER_DST_BIT, true, true);
    ctx.submitAndWait([&](VkCommandBuffer cmd) {
        VkImage imgs[3] = {t.color.image, t.albedo.image, t.normal.image};
        for (int k = 0; k < 3; ++k) {
            VkBufferImageCopy region{};
            region.bufferOffset = bytes * k;
            region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
            region.imageExtent = {s.width, s.height, 1};
            vkCmdCopyImageToBuffer(cmd, imgs[k], VK_IMAGE_LAYOUT_GENERAL, rb.buffer, 1, &region);
        }
    });
    vmaInvalidateAllocation(ctx.allocator(), rb.alloc, 0, VK_WHOLE_SIZE);
    io::ImageF* dst[3] = {&out.color, &out.albedo, &out.normal};
    const float* src = static_cast<const float*>(rb.mapped);
    const size_t n = static_cast<size_t>(s.width) * s.height;
    for (int k = 0; k < 3; ++k) {
        dst[k]->width = static_cast<int>(s.width);
        dst[k]->height = static_cast<int>(s.height);
        dst[k]->rgba.resize(n * 4);
        const float* p = src + n * 4 * k;
        for (size_t i = 0; i < n; ++i) {
            const float cnt = p[i * 4 + 3];  // A にサンプル数が入っている
            const float inv = cnt > 0 ? 1.0f / cnt : 0.0f;
            for (int c = 0; c < 3; ++c) dst[k]->rgba[i * 4 + c] = p[i * 4 + c] * inv;
            dst[k]->rgba[i * 4 + 3] = 1.0f;
        }
    }
    ctx.destroyBuffer(rb);
    integrator->destroy();
    ctx.destroyImage(t.color);
    ctx.destroyImage(t.albedo);
    ctx.destroyImage(t.normal);
    log::info("render finished in {:.2f} s ({:.1f} ms/spp)", out.seconds, out.seconds * 1000.0 / std::max(1u, s.spp));
    return out;
}

}  // namespace krbr::render
