#pragma once
// 累積レンダリングの管理（出力先の画像と積分器を持ち、サンプルを積み増す）。
// オフライン描画（renderer.cpp）とリアルタイム表示（app/viewer.cpp）の両方で使う。
#include <memory>

#include "io/image_io.hpp"
#include "render/integrator.hpp"

namespace krbr::render {

struct AovImages {
    io::ImageF color, albedo, normal;  // サンプル数で割った値（色とアルベドはリニア sRGB）
};

// CIE XYZ（分光モードの累積）→ リニア sRGB（その場で書き換える）
void xyzToLinearSrgb(io::ImageF& img);

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
    // 積分器を作り直す（シーンの資源、例えば環境マップを差し替えた後）。累積は 0 から
    void rebuildIntegrator();
    // 積分器を切り替える（"pt" / "bdpt" / "vcm"）。累積は 0 から
    void setIntegrator(const std::string& name);

    // count サンプルを cmd に記録する
    void record(VkCommandBuffer cmd, uint32_t count);

    uint32_t samples() const { return samples_; }
    const RenderTargets& targets() const { return targets_; }
    const FrameParams& frame() const { return frame_; }
    // 累積（とノイズ除去の結果）が CIE XYZ か（分光モード）。表示や保存の前に RGB へ変換する
    bool xyzOutput() const { return frame_.spectral; }
    const char* integratorName() const { return integrator_ ? integrator_->name() : "?"; }

    // 現在の累積を読み戻す（GPU が待機中に呼ぶこと）。分光モードでも RGB に変換して返す
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
