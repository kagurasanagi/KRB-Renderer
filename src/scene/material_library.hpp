#pragma once
// 名前付きの材質の一覧。組み込みの材質に、全体設定の materials/*.toml を重ねる（同じ名前は書いた項目だけ上書き）
#include <filesystem>
#include <map>
#include <string>
#include <vector>

#include "scene/material.hpp"

namespace krbr::scene {

class MaterialLibrary {
public:
    // 組み込みの材質と、configDir/materials/*.toml を読む
    static MaterialLibrary load(const std::filesystem::path& configDir);
    static MaterialLibrary builtin();
    static const char* builtinToml();

    bool loadFile(const std::filesystem::path& p, std::string& err);
    bool loadText(const std::string& text, const std::string& source, std::string& err);

    const Material* find(const std::string& name) const;
    std::vector<std::string> names() const;
    const std::string& sourceOf(const std::string& name) const;

private:
    std::map<std::string, Material> materials_;
    std::map<std::string, std::string> sources_;
};

}  // namespace krbr::scene
