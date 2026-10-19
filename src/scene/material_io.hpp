#pragma once
// 材質の TOML 表現。パラメータの一覧（OpenPBR の名前）から、読み書きを自動で行う。
#include <toml++/toml.hpp>

#include <string>
#include <vector>

#include "scene/material.hpp"

namespace krbr::scene {

enum class ParamType { Float, Color, Bool };

struct ParamDesc {
    const char* key;
    ParamType type;
    float Material::* f = nullptr;
    glm::vec3 Material::* c = nullptr;
    bool Material::* b = nullptr;
};

// OpenPBR Surface のパラメータの一覧
const std::vector<ParamDesc>& openPbrParams();

// t に書かれた項目だけを m に上書きする。知らない項目の名前を unknown に入れる。
// base_color が無い表を「モデルの色を使う」とみなすかどうかは呼び出し側（材質ライブラリ）が決める
void readMaterial(const toml::table& t, Material& m, std::vector<std::string>* unknown = nullptr);

// 既定値と異なる項目だけを書き出す（[krbr] の拡張を含む）
toml::table writeMaterial(const Material& m);

// 線形補間の色空間の注意：TOML の色はリニア（OpenPBR の流儀）
}  // namespace krbr::scene
