#include "render/renderer.hpp"

#include <chrono>
#include <thread>

#include "core/log.hpp"
#include "render/denoiser.hpp"
#include "render/progressive.hpp"

namespace krbr::render {

RenderOutput renderOffscreen(vk::Context& ctx, const GpuScene& scene, const RenderSettings& s) {
    RenderOutput out;
    Progressive prog;
    prog.init(ctx, scene, s.integrator, s.width, s.height, s.frame);
    log::info("rendering {}x{}, {} spp, integrator '{}', max depth {}, {}", s.width, s.height, s.spp,
              prog.integratorName(), s.frame.maxDepth, s.frame.spectral ? "spectral" : "RGB");

    // 1 サンプルずつ投入して完了を待つ。GPU を長時間占有しないので、作業中の画面描画が止まりにくい
    const auto t0 = std::chrono::steady_clock::now();
    auto lastLog = t0;
    for (uint32_t i = 0; i < s.spp; ++i) {
        ctx.submitAndWait([&](VkCommandBuffer cmd) { prog.record(cmd, 1); });
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

    if (s.denoise) {
        Denoiser d;
        if (d.init(ctx, prog.targets(), DenoiseQuality::High)) {
            ctx.submitAndWait([&](VkCommandBuffer cmd) { d.recordPrepare(cmd); });
            if (d.execute(&out.denoiseMs)) {
                const auto& img = d.output();
                const VkDeviceSize bytes = static_cast<VkDeviceSize>(img.width) * img.height * 16;
                vk::Buffer rb = ctx.createBuffer(bytes, VK_BUFFER_USAGE_TRANSFER_DST_BIT, true, true);
                ctx.submitAndWait([&](VkCommandBuffer cmd) {
                    d.recordOutput(cmd);
                    vk::memoryBarrier(cmd, VK_PIPELINE_STAGE_2_COPY_BIT, VK_ACCESS_2_TRANSFER_WRITE_BIT,
                                      VK_PIPELINE_STAGE_2_COPY_BIT, VK_ACCESS_2_TRANSFER_READ_BIT);
                    VkBufferImageCopy region{};
                    region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
                    region.imageExtent = {img.width, img.height, 1};
                    vkCmdCopyImageToBuffer(cmd, img.image, VK_IMAGE_LAYOUT_GENERAL, rb.buffer, 1, &region);
                });
                vmaInvalidateAllocation(ctx.allocator(), rb.alloc, 0, VK_WHOLE_SIZE);
                out.denoised.width = static_cast<int>(img.width);
                out.denoised.height = static_cast<int>(img.height);
                const float* p = static_cast<const float*>(rb.mapped);
                out.denoised.rgba.assign(p, p + static_cast<size_t>(img.width) * img.height * 4);
                if (prog.xyzOutput()) xyzToLinearSrgb(out.denoised);  // ノイズ除去は XYZ のままかけている
                ctx.destroyBuffer(rb);
                log::info("denoised in {:.0f} ms", out.denoiseMs);
            }
            d.destroy();
        } else {
            log::warn("denoiser not available; writing the noisy image only");
        }
    }
    AovImages aov = prog.readback();
    out.color = std::move(aov.color);
    out.albedo = std::move(aov.albedo);
    out.normal = std::move(aov.normal);
    prog.destroy();
    log::info("render finished in {:.2f} s ({:.1f} ms/spp)", out.seconds, out.seconds * 1000.0 / std::max(1u, s.spp));
    return out;
}

}  // namespace krbr::render
