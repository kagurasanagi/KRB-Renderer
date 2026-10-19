#pragma once
// オフスクリーンでのレンダリングの進行（サンプルの投入・待機・読み戻し）
#include "io/image_io.hpp"
#include "render/integrator.hpp"

namespace krbr::render {

struct RenderSettings {
    uint32_t width = 1920, height = 1080;
    uint32_t spp = 64;
    std::string integrator = "pt";
    FrameParams frame;
};

struct RenderOutput {
    io::ImageF color, albedo, normal;  // サンプル数で割った値
    double seconds = 0.0;
};

RenderOutput renderOffscreen(vk::Context& ctx, const GpuScene& scene, const RenderSettings& settings);

}  // namespace krbr::render
