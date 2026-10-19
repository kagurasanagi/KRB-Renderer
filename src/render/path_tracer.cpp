// 積分器：パストレーサー（pathtrace.comp）と、BDPT / VCM（vcm_light.comp → vcm_camera.comp）。
// どちらも 1 サンプル（1 回の繰り返し）を 1 回の記録で行う
#include <algorithm>
#include <cstring>
#include <stdexcept>

#include "core/log.hpp"
#include "render/integrator.hpp"
#include "render/kernel.hpp"

namespace krbr::render {

namespace {

// フレームの定数を置くバッファ（カメラ操作で毎フレーム書き換えるので CPU から見えるメモリに置く）
struct FrameBuffer {
    void create(vk::Context& ctx) {
        ctx_ = &ctx;
        ubo = ctx.createBuffer(sizeof(FrameUBO), VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT, true);
    }
    void write(const FrameUBO& u) {
        std::memcpy(ubo.mapped, &u, sizeof(u));
        vmaFlushAllocation(ctx_->allocator(), ubo.alloc, 0, VK_WHOLE_SIZE);
    }
    void destroy() {
        if (ctx_) ctx_->destroyBuffer(ubo);
        ctx_ = nullptr;
    }
    vk::Context* ctx_ = nullptr;
    vk::Buffer ubo;
};

class PathTracer final : public Integrator {
public:
    ~PathTracer() override { destroy(); }
    const char* name() const override { return "pt"; }

    void setup(vk::Context& ctx, const GpuScene& sc, const RenderTargets& targets, const FrameParams& fp) override {
        ctx_ = &ctx;
        scene_ = &sc;
        targets_ = targets;
        frame_.create(ctx);
        updateFrame(fp);
        kernel_.create(ctx, "pathtrace.comp.spv", sceneBindings(sc, targets, frame_.ubo));
    }

    void updateFrame(const FrameParams& fp) override {
        seed_ = fp.seed;
        frame_.write(makeFrameUbo(*scene_, targets_, fp));
    }

    void recordSample(VkCommandBuffer cmd, uint32_t sampleIndex) override {
        kernel_.dispatch(cmd, {sampleIndex, seed_}, targets_.width, targets_.height);
        computeBarrier(cmd);  // 次のサンプルが累積を読む前に書き込みを完了させる
    }

    void destroy() override {
        if (!ctx_) return;
        vkDeviceWaitIdle(ctx_->device());
        kernel_.destroy();
        frame_.destroy();
        ctx_ = nullptr;
    }

private:
    vk::Context* ctx_ = nullptr;
    const GpuScene* scene_ = nullptr;
    RenderTargets targets_;
    uint32_t seed_ = 0;
    FrameBuffer frame_;
    ComputeKernel kernel_;
};

// BDPT（merging なし）と VCM（merging あり）
class Bidirectional final : public Integrator {
public:
    explicit Bidirectional(bool merging) : merging_(merging) {}
    ~Bidirectional() override { destroy(); }
    const char* name() const override { return merging_ ? "vcm" : "bdpt"; }

    void setup(vk::Context& ctx, const GpuScene& sc, const RenderTargets& targets, const FrameParams& fp) override {
        if (!ctx.info().atomicFloatAdd)
            throw std::runtime_error("bdpt / vcm need VK_EXT_shader_atomic_float (shaderBufferFloat32AtomicAdd)");
        ctx_ = &ctx;
        scene_ = &sc;
        targets_ = targets;
        frame_.create(ctx);
        updateFrame(fp);

        // 光源側の頂点：1 本の経路あたり平均 4 個まで、全体で 512 MB まで
        const uint64_t paths = static_cast<uint64_t>(targets.width) * targets.height;
        capacity_ = static_cast<uint32_t>(std::min<uint64_t>(paths * 4, (512ull << 20) / 96));
        const VkBufferUsageFlags usage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT | VK_BUFFER_USAGE_TRANSFER_DST_BIT;
        lightVerts_ = ctx.createBuffer(static_cast<VkDeviceSize>(capacity_) * 96, usage, false);
        pathHeads_ = ctx.createBuffer(paths * 4, usage, false);
        cellHeads_ = ctx.createBuffer((1u << 21) * 4, usage, false);
        counters_ = ctx.createBuffer(16, usage, false);
        splat_ = ctx.createBuffer(paths * 16, usage, false);

        std::vector<KernelBinding> b = sceneBindings(sc, targets, frame_.ubo);
        b.push_back(bufferBinding(20, lightVerts_));
        b.push_back(bufferBinding(21, pathHeads_));
        b.push_back(bufferBinding(22, cellHeads_));
        b.push_back(bufferBinding(23, counters_));
        b.push_back(bufferBinding(24, splat_));
        light_.create(ctx, "vcm_light.comp.spv", b);
        camera_.create(ctx, "vcm_camera.comp.spv", b);
        log::info("{}: up to {} light vertices ({} MB)", name(), capacity_,
                  static_cast<uint64_t>(capacity_) * 96 >> 20);
    }

    void updateFrame(const FrameParams& fp) override {
        seed_ = fp.seed;
        FrameUBO u = makeFrameUbo(*scene_, targets_, fp);
        u.vcmParams.w = merging_ ? 1.0f : 0.0f;
        frame_.write(u);
    }

    void recordSample(VkCommandBuffer cmd, uint32_t sampleIndex) override {
        // 前の繰り返しの頂点・格子・画面への足し込みを空にする
        vkCmdFillBuffer(cmd, pathHeads_.buffer, 0, VK_WHOLE_SIZE, 0xFFFFFFFFu);
        if (merging_) vkCmdFillBuffer(cmd, cellHeads_.buffer, 0, VK_WHOLE_SIZE, 0xFFFFFFFFu);
        vkCmdFillBuffer(cmd, splat_.buffer, 0, VK_WHOLE_SIZE, 0);
        const uint32_t counters[4] = {0, capacity_, 0, 0};
        vkCmdUpdateBuffer(cmd, counters_.buffer, 0, sizeof(counters), counters);
        computeBarrier(cmd);
        light_.dispatch(cmd, {sampleIndex, seed_}, targets_.width, targets_.height);
        computeBarrier(cmd);
        camera_.dispatch(cmd, {sampleIndex, seed_}, targets_.width, targets_.height);
        computeBarrier(cmd);
    }

    void destroy() override {
        if (!ctx_) return;
        vkDeviceWaitIdle(ctx_->device());
        light_.destroy();
        camera_.destroy();
        for (vk::Buffer* b : {&lightVerts_, &pathHeads_, &cellHeads_, &counters_, &splat_}) ctx_->destroyBuffer(*b);
        frame_.destroy();
        ctx_ = nullptr;
    }

private:
    bool merging_;
    vk::Context* ctx_ = nullptr;
    const GpuScene* scene_ = nullptr;
    RenderTargets targets_;
    uint32_t seed_ = 0;
    uint32_t capacity_ = 0;
    FrameBuffer frame_;
    vk::Buffer lightVerts_, pathHeads_, cellHeads_, counters_, splat_;
    ComputeKernel light_, camera_;
};

}  // namespace

std::unique_ptr<Integrator> createIntegrator(const std::string& name) {
    if (name == "pt" || name == "path") return std::make_unique<PathTracer>();
    if (name == "bdpt") return std::make_unique<Bidirectional>(false);
    if (name == "vcm") return std::make_unique<Bidirectional>(true);
    return nullptr;
}

}  // namespace krbr::render
