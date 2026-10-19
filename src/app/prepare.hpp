#pragma once
// render / view 共通：基板の読み込み、設定、シーン構築、環境マップの読み込み
#include <filesystem>
#include <future>
#include <memory>

#include "app/commands.hpp"
#include "app/config.hpp"
#include "app/kicad_cli.hpp"
#include "io/image_io.hpp"
#include "kicad/board.hpp"
#include "kicad/model_resolver.hpp"
#include "model/mesh_cache.hpp"
#include "render/env_map.hpp"
#include "render/integrator.hpp"
#include "scene/camera.hpp"
#include "scene/scene.hpp"

namespace krbr::app {

// 材質ライブラリ（組み込み＋全体設定 materials/*.toml）、全体の対応表、プロジェクトの上書き
struct MaterialSetup {
    scene::MaterialLibrary library;
    scene::GlobalModelMap global;
    scene::ProjectOverrides project;
    std::filesystem::path globalFile, projectFile;
};
MaterialSetup loadMaterialSetup(const std::filesystem::path& pcbFile);

struct ReloadStats {
    kicad::BoardDiff diff;
    double parseMs = 0, buildMs = 0;
    int tilesRebuilt = 0;
    std::string error;
};

// 基板・設定・シーンと、作り直しに使うキャッシュ（凹凸のタイル、3D モデル、材質）をまとめて持つ
struct PreparedScene {
    kicad::Board board;
    ProjectConfig cfg;
    SilkPlot silk;
    scene::Scene scene;
    io::ImageF env;
    render::EnvDistribution dist;

    RenderOptions options;
    MaterialSetup materials;
    std::unique_ptr<kicad::ModelResolver> resolver;
    std::unique_ptr<model::ModelLibrary> library;
    scene::ReliefTileCache tiles;
    std::optional<std::filesystem::path> kicadCli;
    std::future<SilkPlot> silkJob;

    // いまの基板からシーンを作る（凹凸は無効なタイルだけ作り直す）。asyncModels なら未分割の STEP は箱で仮表示
    std::unique_ptr<scene::Scene> buildScene(bool asyncModels);
    // ファイルを読み直し、差分の範囲だけ作り直したシーンを返す（読めなければ nullptr）
    std::unique_ptr<scene::Scene> reload(ReloadStats& stats);
    // 裏で描き直していた kicad-cli のシルクが届いていれば取り込む（true ならシーンを作り直すこと）
    bool takeFinishedSilk();
};



// 失敗したらエラーをログに出して nullptr。debugLayersDir が空でなければ層テクスチャの PNG も書く
std::unique_ptr<PreparedScene> prepareScene(const RenderOptions& o, const std::filesystem::path& debugLayersDir = {},
                                            const std::string& debugStem = {});

std::optional<kicad::Board> loadBoardOrReport(const std::string& pcb);

scene::CameraParams cameraParams(const ProjectConfig& cfg, int width, int height);
// 自動の構図に --look-at / --zoom を反映したカメラ
scene::Camera initialCamera(const PreparedScene& prep, const RenderOptions& o, int width, int height);
render::FrameParams frameParams(const ProjectConfig& cfg, const scene::Camera& cam, unsigned seed);

}  // namespace krbr::app
