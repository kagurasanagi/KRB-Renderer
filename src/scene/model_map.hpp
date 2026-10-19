#pragma once
// 部品の 3D モデルの色・名前から材質を決める対応表（全体設定 model_map.toml）
#include <filesystem>
#include <map>
#include <optional>
#include <string>
#include <vector>

#include "scene/material.hpp"

namespace krbr::scene {

struct MaterialPreset {
    Material material;
    bool useModelColor = true;  // base_color を指定しなければモデルの色を使う
};

struct MapRule {
    std::string model;  // モデルのファイル名に対するワイルドカード（* と ?）。空なら全て
    std::optional<float> hueMin, hueMax;  // 色相（度、sRGB の HSV）
    std::optional<float> satMin, satMax;
    std::optional<float> valMin, valMax;
    std::string preset;
};

class ModelMap {
public:
    static ModelMap defaults();
    // 既定の内容を書いた TOML（--init で書き出す）
    static std::string defaultToml();
    // ファイルを読み、既定に上書き・追加する。失敗時は err
    bool load(const std::filesystem::path& p, std::string& err);

    // モデルのファイル名と面の色（リニア RGB）から材質を作る
    Material resolve(const std::string& modelFile, glm::vec4 linearColor) const;

private:
    std::map<std::string, MaterialPreset> presets_;
    std::vector<MapRule> rules_;  // 先頭から順に評価し、最初に一致したものを使う
};

bool globMatch(std::string_view pattern, std::string_view text);

}  // namespace krbr::scene
