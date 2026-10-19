#include "app/watch.hpp"

#include <thread>

#include "core/file_watcher.hpp"
#include "core/log.hpp"
#include "core/platform.hpp"
#include "render/gpu_scene.hpp"
#include "render/denoiser.hpp"
#include "render/progressive.hpp"
#include "render/tonemapper.hpp"

namespace krbr::app {

namespace fs = std::filesystem;
using Clock = std::chrono::steady_clock;

namespace {
double msSince(Clock::time_point t) { return std::chrono::duration<double, std::milli>(Clock::now() - t).count(); }
}  // namespace

struct WatchRunner::Impl {
    WatchOptions opts;
    std::unique_ptr<PreparedScene> prep;
    std::unique_ptr<FileWatcher> watcher;
    vk::Context ctx;
    render::GpuScene gpu;
    render::Progressive prog;
    render::Tonemapper tone;
    render::Denoiser denoiser;
    render::Tonemapper toneDenoised;  // ノイズ除去の結果を色の入力にしたもの
    uint32_t spp = 32;
    size_t modelsCompleted = 0;
    render::FrameParams fixedFrame;  // 構図は最初に決めたまま（部品が動いても視点を動かさない）

    ~Impl() {
        toneDenoised.destroy();
        denoiser.destroy();
        tone.destroy();
        prog.destroy();
        gpu.destroy();
        ctx.destroy();
    }

    render::FrameParams frame() const { return fixedFrame; }

    // 描いて PNG を書く（累積は 0 から）
    void renderAndWrite(const fs::path& out, double& renderMs, double& writeMs, double* denoiseMs = nullptr) {
        const auto t0 = Clock::now();
        prog.setFrame(frame());
        // 数サンプルずつまとめて投入する（1 回の投入が長くなりすぎない程度）
        for (uint32_t done = 0; done < spp;) {
            const uint32_t n = std::min<uint32_t>(8, spp - done);
            ctx.submitAndWait([&](VkCommandBuffer cmd) { prog.record(cmd, n); });
            done += n;
        }
        // ノイズ除去（使えれば）
        const bool denoised = denoiser.ready();
        if (denoised) {
            ctx.submitAndWait([&](VkCommandBuffer cmd) { denoiser.recordPrepare(cmd); });
            double ms = 0;
            denoiser.execute(&ms);
            if (denoiseMs) *denoiseMs = ms;
        }
        render::Tonemapper& tm = denoised ? toneDenoised : tone;
        // トーンマッピングは GPU で行い、8bit の画像だけを読み戻す
        const auto& img = tm.output();
        const VkDeviceSize bytes = static_cast<VkDeviceSize>(img.width) * img.height * 4;
        vk::Buffer rb = ctx.createBuffer(bytes, VK_BUFFER_USAGE_TRANSFER_DST_BIT, true, true);
        ctx.submitAndWait([&](VkCommandBuffer cmd) {
            if (denoised) denoiser.recordOutput(cmd);
            tm.record(cmd, prep->cfg.exposureEv, render::DisplayMode::Beauty);
            vk::memoryBarrier(cmd, VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT, VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT,
                              VK_PIPELINE_STAGE_2_COPY_BIT, VK_ACCESS_2_TRANSFER_READ_BIT);
            VkBufferImageCopy region{};
            region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
            region.imageExtent = {img.width, img.height, 1};
            vkCmdCopyImageToBuffer(cmd, img.image, VK_IMAGE_LAYOUT_GENERAL, rb.buffer, 1, &region);
        });
        vmaInvalidateAllocation(ctx.allocator(), rb.alloc, 0, VK_WHOLE_SIZE);
        renderMs = msSince(t0);
        const auto t1 = Clock::now();
        std::string err;
        if (!io::savePngRgba8(out, static_cast<int>(img.width), static_cast<int>(img.height),
                              static_cast<const unsigned char*>(rb.mapped), err, 0))  // 速さを優先（圧縮しない）
            log::error("{}: {}", platform::pathToUtf8(out), err);
        ctx.destroyBuffer(rb);
        writeMs = msSince(t1);
    }
};

WatchRunner::WatchRunner() = default;
WatchRunner::~WatchRunner() = default;

bool WatchRunner::open(const WatchOptions& o) {
    impl_ = std::make_unique<Impl>();
    impl_->opts = o;
    impl_->prep = prepareScene(o.common);
    if (!impl_->prep) return false;
    auto& prep = *impl_->prep;
    impl_->spp = static_cast<uint32_t>(o.common.spp ? *o.common.spp : 32);
    outPng_ = platform::u8path(o.common.out);
    std::error_code ec;
    if (outPng_.has_parent_path()) fs::create_directories(outPng_.parent_path(), ec);
    impl_->fixedFrame = frameParams(prep.cfg, initialCamera(prep, o.common, prep.cfg.width, prep.cfg.height), o.common.seed);
    try {
        vk::ContextOptions co;
        co.validation = o.common.validate;
        impl_->ctx.init(co);
        impl_->gpu.build(impl_->ctx, prep.scene, prep.env, prep.dist, prep.cfg.scene.bumpIntensity);
        prep.env.rgba.clear();
        prep.env.rgba.shrink_to_fit();
        impl_->prog.init(impl_->ctx, impl_->gpu, prep.cfg.integrator, static_cast<uint32_t>(prep.cfg.width),
                         static_cast<uint32_t>(prep.cfg.height), impl_->frame());
        impl_->tone.init(impl_->ctx, impl_->prog.targets());
        if (o.common.denoise.value_or(true) &&
            impl_->denoiser.init(impl_->ctx, impl_->prog.targets(), render::DenoiseQuality::High)) {
            render::RenderTargets t = impl_->prog.targets();
            t.color = impl_->denoiser.output();
            impl_->toneDenoised.init(impl_->ctx, t);
        }
    } catch (const std::exception& e) {
        log::error("watch: {}", e.what());
        return false;
    }
    double r, w;
    impl_->renderAndWrite(outPng_, r, w);
    log::info("watch: wrote {} ({:.0f} ms render, {:.0f} ms write); watching {}", platform::pathToUtf8(outPng_), r, w,
              platform::pathToUtf8(prep.board.file));
    impl_->watcher = std::make_unique<FileWatcher>(prep.board.file);
    impl_->modelsCompleted = prep.library->completed();
    return true;
}

std::optional<WatchUpdate> WatchRunner::waitAndUpdate(std::chrono::milliseconds timeout) {
    auto& I = *impl_;
    auto& prep = *I.prep;
    const auto deadline = Clock::now() + timeout;
    while (Clock::now() < deadline) {
        const bool saved = I.watcher->poll();
        const bool modelsDone = prep.library->completed() != I.modelsCompleted;
        const bool silkDone = prep.takeFinishedSilk();
        if (!saved && !modelsDone && !silkDone) {
            std::this_thread::sleep_for(std::chrono::milliseconds(20));
            continue;
        }
        WatchUpdate u;
        const auto t0 = Clock::now();
        std::unique_ptr<scene::Scene> sc;
        if (saved) {
            sc = prep.reload(u.stats);
            if (!sc) {
                log::warn("watch: cannot read the board ({}); waiting for the next save", u.stats.error);
                continue;
            }
        } else {
            I.modelsCompleted = prep.library->completed();
            sc = prep.buildScene(true);
        }
        u.reloadMs = msSince(t0);
        u.pendingModels = sc->pendingModels;
        const auto t1 = Clock::now();
        prep.scene = std::move(*sc);
        I.gpu.rebuildGeometry(prep.scene, prep.cfg.scene.bumpIntensity);
        I.prog.rebuildIntegrator();
        u.gpuMs = msSince(t1);
        I.renderAndWrite(outPng_, u.renderMs, u.writeMs, &u.denoiseMs);
        if (saved) {
            u.sinceSaveMs = std::chrono::duration<double, std::milli>(std::chrono::system_clock::now() -
                                                                      I.watcher->savedAt()).count();
            log::info("watch: updated {:.0f} ms after save (reload {:.0f} [parse {:.0f}, {} tiles], gpu {:.0f}, "
                      "render {:.0f} [denoise {:.0f}], write {:.0f}); {} added, {} changed, {} removed{}",
                      u.sinceSaveMs, u.reloadMs, u.stats.parseMs, u.stats.tilesRebuilt, u.gpuMs, u.renderMs,
                      u.denoiseMs, u.writeMs, u.stats.diff.added, u.stats.diff.changed, u.stats.diff.removed,
                      u.pendingModels ? std::format(", {} model(s) meshing in background", u.pendingModels) : "");
        } else {
            log::info("watch: updated (background {} finished)", modelsDone ? "model meshing" : "silkscreen plot");
        }
        return u;
    }
    return std::nullopt;
}

int cmdWatch(const WatchOptions& o) {
    if (!o.common.normalPriority) platform::setLowPriority();
    WatchRunner runner;
    if (!runner.open(o)) return 1;
    int updates = 0;
    for (;;) {
        if (runner.waitAndUpdate(std::chrono::milliseconds(1000))) ++updates;
        if (o.maxUpdates > 0 && updates >= o.maxUpdates) break;
    }
    return 0;
}

}  // namespace krbr::app
