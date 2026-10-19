#pragma once
// プロジェクト設定（<プロジェクト名>.krbr/board.toml, environment.toml）
#include <filesystem>
#include <map>
#include <string>
#include <vector>

#include "scene/scene.hpp"

namespace krbr::app {

namespace fs = std::filesystem;

struct ProjectConfig {
    // board.toml
    scene::SceneOptions scene;
    std::map<std::string, std::string> pathVars;  // ${VAR} の追加・上書き
    std::vector<fs::path> modelSearchDirs;
    bool useKicadCliText = true;
    std::string kicadCli;  // 空なら自動検出

    // environment.toml
    std::string hdr;
    float envIntensity = 1.0f;
    float envRotationDeg = 0.0f;
    double elevationDeg = 35.0, azimuthDeg = 45.0, fovDeg = 30.0, margin = 0.04;
    int width = 1920, height = 1080;
    int spp = 64;
    int maxDepth = 8;
    float exposureEv = 0.7f;
    float radianceClamp = 10.0f;  // 間接光だけに効く上限（ホタル対策）
    std::string integrator = "pt";

    std::vector<fs::path> loadedFiles;
};

fs::path projectConfigDir(const fs::path& pcbFile);
// 既定値に、見つかった設定ファイルの内容を重ねる。失敗したファイルは警告して無視
ProjectConfig loadProjectConfig(const fs::path& pcbFile);
std::string defaultBoardToml();
std::string defaultEnvironmentToml();
// --init：既定の設定ファイルを書き出す（既存のファイルは上書きしない）
void writeDefaultConfigs(const fs::path& pcbFile);

}  // namespace krbr::app
