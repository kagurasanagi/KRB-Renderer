#pragma once
// 3D モデル指定（${KICAD10_3DMODEL_DIR}/... など）を実ファイルに解決する。KiCad の設定は読むだけで書き換えない。
#include <filesystem>
#include <map>
#include <string>
#include <vector>

namespace krbr::kicad {

namespace fs = std::filesystem;

struct ResolvedModel {
    bool found = false;
    fs::path path;           // 見つかったファイル
    std::string expanded;    // 変数展開後の文字列
    std::string reason;      // 見つからない理由
    bool substituted = false;  // .wrl -> .step などの差し替えをした
};

class ModelResolver {
public:
    // extraVars: 設定ファイルで与えた変数（最優先）。searchDirs: ファイル名で探す追加フォルダ
    ModelResolver(const fs::path& projectDir, const std::map<std::string, std::string>& extraVars,
                  const std::vector<fs::path>& searchDirs);

    ResolvedModel resolve(const std::string& spec) const;
    const std::map<std::string, std::string>& vars() const { return vars_; }

private:
    std::string expand(const std::string& s, std::string* undefinedVar) const;

    fs::path projectDir_;
    std::map<std::string, std::string> vars_;
    std::vector<fs::path> searchDirs_;
};

}  // namespace krbr::kicad
