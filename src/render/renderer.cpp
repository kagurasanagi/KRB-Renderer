#include "render/renderer.hpp"

#include <chrono>
#include <thread>

#include "core/log.hpp"
#include "render/progressive.hpp"

namespace krbr::render {

RenderOutput renderOffscreen(vk::Context& ctx, const GpuScene& scene, const RenderSettings& s) {
    RenderOutput out;
    Progressive prog;
    prog.init(ctx, scene, s.integrator, s.width, s.height, s.frame);
    log::info("rendering {}x{}, {} spp, integrator '{}', max depth {}", s.width, s.height, s.spp,
              prog.integratorName(), s.frame.maxDepth);

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

    AovImages aov = prog.readback();
    out.color = std::move(aov.color);
    out.albedo = std::move(aov.albedo);
    out.normal = std::move(aov.normal);
    prog.destroy();
    log::info("render finished in {:.2f} s ({:.1f} ms/spp)", out.seconds, out.seconds * 1000.0 / std::max(1u, s.spp));
    return out;
}

}  // namespace krbr::render
