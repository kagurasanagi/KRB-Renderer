#pragma once
// 部品の 3D モデルへの材質の割り当て。
//   全体設定  model_map.toml：3D モデルごとの設定（他のプロジェクトと共有）と、色・名前の規則
//   プロジェクト overrides.toml：部品番号ごと・3D モデルごとの上書き
// 解決順序（優先度の高い順）：
//   1. 部品番号ごとの上書き（プロジェクト）  2. 3D モデルごとの上書き（プロジェクト）
//   3. 3D モデルごとの設定（全体）           4. 色・名前の規則（全体 → 組み込み）  5. 既定値
// 3D モデルは、展開前のパスとファイル内容のハッシュのどちらかが一致すれば同じとみなす。
// 部位は ModelPart::keys()（XDE の名前、"kicad:<色名>"、"#rrggbb"）で照合し、"*" は全部位に一致する。
#include <filesystem>
#include <map>
#include <optional>
#include <string>
#include <vector>

#include "model/model_mesh.hpp"
#include "scene/material_library.hpp"

namespace krbr::scene {

// 材質の割り当て：部位ごとの指定（parts）と、それ以外の部位すべて（material）
struct PartAssignment {
    std::string material;
    std::map<std::string, std::string> parts;
    bool empty() const { return material.empty() && parts.empty(); }
    // 部位に一致する材質名（無ければ空）
    std::string lookup(const model::ModelPart* part) const;
};

struct ModelEntry {
    std::string path;  // 展開前のパス（.kicad_pcb の (model "...") に書かれたとおり）
    std::string hash;  // ファイル内容の XXH64（16 進）
    PartAssignment assign;
    bool matches(const std::string& spec, const std::string& fileHash) const;
};

struct MaterialRule {
    std::string part;   // 部位のキーに対するワイルドカード（空なら全て）
    std::string model;  // モデルのファイル名に対するワイルドカード（空なら全て）
    std::optional<float> hueMin, hueMax, satMin, satMax, valMin, valMax;  // 面の色（sRGB の HSV、色相は度）
    std::string material;
    bool matches(const std::string& modelFile, const model::ModelPart* part) const;
};

// 全体設定 model_map.toml
struct GlobalModelMap {
    std::vector<ModelEntry> models;
    std::vector<MaterialRule> rules;
    bool load(const std::filesystem::path& p, std::string& err);
    bool save(const std::filesystem::path& p, std::string& err) const;
    ModelEntry& upsert(const std::string& path, const std::string& hash);
};

// プロジェクト overrides.toml
struct ProjectOverrides {
    std::map<std::string, PartAssignment> refs;
    std::vector<ModelEntry> models;
    bool load(const std::filesystem::path& p, std::string& err);
    bool save(const std::filesystem::path& p, std::string& err) const;
    ModelEntry& upsert(const std::string& path, const std::string& hash);
};

const std::vector<MaterialRule>& builtinRules();

struct MaterialQuery {
    std::string ref;        // 部品番号
    std::string modelSpec;  // 展開前のパス
    std::string modelHash;  // ファイル内容のハッシュ
    std::string modelFile;  // ファイル名（規則の照合用）
    const model::ModelPart* part = nullptr;
};

struct ResolvedMaterial {
    Material material;
    std::string name;    // 材質ライブラリの名前
    std::string source;  // どの段階で決まったか（"ref U2 (project)" など）
};

class MaterialResolver {
public:
    MaterialResolver(const MaterialLibrary& lib, const ProjectOverrides& project, const GlobalModelMap& global);
    ResolvedMaterial resolve(const MaterialQuery& q) const;

private:
    // 名前が材質ライブラリにあれば材質を作る（base_color をモデルの色にする処理も行う）
    std::optional<ResolvedMaterial> make(const std::string& name, const std::string& source,
                                         const MaterialQuery& q) const;
    const MaterialLibrary& lib_;
    const ProjectOverrides& project_;
    const GlobalModelMap& global_;
};

bool globMatch(std::string_view pattern, std::string_view text);

}  // namespace krbr::scene
