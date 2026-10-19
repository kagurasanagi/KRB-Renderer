#pragma once
// krbr material set / show / list
#include <string>
#include <vector>

#include "scene/scene.hpp"

namespace krbr::app {

struct MaterialSetOptions {
    std::string pcb;
    std::string ref;       // 部品番号（--ref）
    std::string model;     // 3D モデル（--model）：展開前のパス、ファイル名、またはファイルのパス
    std::string part;      // 部位のキー（--part）。空なら全部位
    std::string material;  // 材質ライブラリの名前
    bool global = false;   // --model のとき全体設定に書く
};

// 対応表を書き換える。成功したら書いたファイルを written に入れる
bool setMaterial(const MaterialSetOptions& o, std::string& err, std::string* written = nullptr);

// 基板の部品・部位ごとの材質（形状は作らず、3D モデルの部位と対応表だけを見る）
std::vector<scene::MaterialAssignment> computeAssignments(const std::string& pcb, std::string& err);

int cmdMaterialSet(const MaterialSetOptions& o);
int cmdMaterialShow(const std::string& pcb, const std::string& refFilter);
int cmdMaterialList();

}  // namespace krbr::app
