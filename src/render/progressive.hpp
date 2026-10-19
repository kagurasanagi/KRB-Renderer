#pragma once
// 累積レンダリングの管理（出力先の画像と積分器を持ち、サンプルを積み増す）。
// オフライン描画（renderer.cpp）とリアルタイム表示（app/viewer.cpp）の両方で使う。
#include <memory>

#include "io/image_io.hpp"
#include "render/integrator.hpp"

namespace krbr::render {

struct AovImages {
    io::ImageF color, albedo, normal;  // サンプル数で割った値
};

class Progressive {
public:
    ~Progressive();
    void init(vk::Context& ctx, const GpuScene& scene, const std::string& integrator, uint32_t width,
              uint32_t height, const FrameParams& frame);
    // 解像度を変える（出力先と積分器を作り直し、累積は 0 から）
    void resize(uint32_t width, uint32_t height);
    // カメラなどを変える（累積は 0 から）。GPU がこのクラスの処理を実行していないときに呼ぶこと
    void setFrame(const FrameParams& frame);
    void reset() { samples_ = 0; }

    // count サンプルを cmd に記録する
    void record(VkCommandBuffer cmd, uint32_t count);

    uint32_t samples() const { return samples_; }
    const RenderTargets& targets() const { return targets_; }
    const FrameParams& frame() const { return frame_; }
    const char* integratorName() const { return integrator_ ? integrator_->name() : "?"; }

    // 現在の累積を読み戻す（GPU が待機中に呼ぶこと）
    AovImages readback();
    void destroy();

private:
    void createTargets();
    void destroyTargets();

    vk::Context* ctx_ = nullptr;
    const GpuScene* scene_ = nullptr;
    std::string integratorName_;
    std::unique_ptr<Integrator> integrator_;
    RenderTargets targets_;
    FrameParams frame_;
    uint32_t samples_ = 0;
};

}  // namespace krbr::render
