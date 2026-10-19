#pragma once
// 積分器のインターフェース。パストレーサー（今回）と、将来の BDPT / VCM はこれを実装する。
// レンダラ（renderer.cpp）は積分器の中身を知らず、1 サンプルずつ記録・投入して完了を待つだけ。
#include <memory>
#include <string>

#include "render/gpu_scene.hpp"
#include "scene/camera.hpp"

namespace krbr::render {

// 積分器が書き込む先（全積分器で共通の出力の約束：RGBA32F の累積値、A にサンプル数）
struct RenderTargets {
    vk::Image color;   // 放射輝度の累積
    vk::Image albedo;  // 最初の交点のアルベド（ノイズ除去用）
    vk::Image normal;  // 最初の交点の法線（ワールド）
    uint32_t width = 0, height = 0;
};

struct FrameParams {
    scene::Camera camera;
    float envIntensity = 1.0f;
    float envRotationRad = 0.0f;
    float radianceClamp = 0.0f;  // 0 で無効
    uint32_t maxDepth = 8;
    uint32_t rrStartDepth = 3;
    uint32_t seed = 0;
    bool spectral = false;  // 分光モード（代表波長方式。累積は CIE XYZ）
};

class Integrator {
public:
    virtual ~Integrator() = default;
    virtual const char* name() const = 0;
    virtual void setup(vk::Context& ctx, const GpuScene& scene, const RenderTargets& targets,
                       const FrameParams& params) = 0;
    // カメラ・照明などを更新する（GPU がこの積分器の処理を実行していないときに呼ぶこと）
    virtual void updateFrame(const FrameParams& params) = 0;
    // sampleIndex 番目のサンプルを cmd に記録する（0 番目で累積を初期化）
    virtual void recordSample(VkCommandBuffer cmd, uint32_t sampleIndex) = 0;
    virtual void destroy() = 0;
};

// "pt"（パストレーサー）。将来 "bdpt", "vcm" を追加する
std::unique_ptr<Integrator> createIntegrator(const std::string& name);

}  // namespace krbr::render
