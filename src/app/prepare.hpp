#pragma once
// render / view 共通：基板の読み込み、設定、シーン構築、環境マップの読み込み
#include <filesystem>
#include <memory>

#include "app/commands.hpp"
#include "app/config.hpp"
#include "app/kicad_cli.hpp"
#include "io/image_io.hpp"
#include "kicad/board.hpp"
#include "render/env_map.hpp"
#include "render/integrator.hpp"
#include "scene/camera.hpp"
#include "scene/scene.hpp"

namespace krbr::app {

struct PreparedScene {
    kicad::Board board;
    ProjectConfig cfg;
    SilkPlot silk;
    scene::Scene scene;
    io::ImageF env;
    render::EnvDistribution dist;
};

// 失敗したらエラーをログに出して nullptr。debugLayersDir が空でなければ層テクスチャの PNG も書く
std::unique_ptr<PreparedScene> prepareScene(const RenderOptions& o, const std::filesystem::path& debugLayersDir = {},
                                            const std::string& debugStem = {});

std::optional<kicad::Board> loadBoardOrReport(const std::string& pcb);

scene::CameraParams cameraParams(const ProjectConfig& cfg, int width, int height);
render::FrameParams frameParams(const ProjectConfig& cfg, const scene::Camera& cam, unsigned seed);

}  // namespace krbr::app
