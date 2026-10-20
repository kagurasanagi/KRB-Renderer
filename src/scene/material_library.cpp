#include "scene/material_library.hpp"

#include <algorithm>
#include <toml++/toml.hpp>

#include "core/log.hpp"
#include "core/platform.hpp"
#include "scene/material_io.hpp"

namespace krbr::scene {

namespace {

// base_color を書かなければ、3D モデルの面の色を使う
const char* kBuiltinMaterials = R"(# krbr 組み込みの材質（OpenPBR Surface）。色はリニア
# 全体設定の materials/*.toml に同じ名前を書くと、書いた項目だけ上書きされる
# roughness_variation（粗さのむら）は 0.1〜0.8 mm 周期の模様。引きの画では画素と同じくらいの細かさになり
# 砂粒のようなノイズに見えるので、部品ではごく弱くしている（部品の質感は specular_roughness で表す）

[material.metal_tin]           # 錫めっきのリード・端子
base_metalness = 1.0
specular_roughness = 0.28
[material.metal_tin.krbr]
bump_pattern = "noise"
bump_strength = 0.012
bump_scale = 0.08
roughness_variation = 0.03

[material.metal_gold]          # 金めっきの端子
base_metalness = 1.0
specular_roughness = 0.22
[material.metal_gold.krbr]
bump_pattern = "noise"
bump_strength = 0.01
bump_scale = 0.08
roughness_variation = 0.03

[material.plastic]             # 樹脂のボディ
specular_roughness = 0.45
specular_ior = 1.5
[material.plastic.krbr]
bump_pattern = "noise"
bump_strength = 0.012
bump_scale = 0.3
roughness_variation = 0.02

[material.glossy_plastic]      # 白い樹脂など、つやのあるもの
specular_roughness = 0.3
specular_ior = 1.5
[material.glossy_plastic.krbr]
bump_pattern = "noise"
bump_strength = 0.01
bump_scale = 0.3
roughness_variation = 0.02

[material.epoxy_black]         # IC のモールド樹脂（つや消しの黒）
base_color = [0.018, 0.018, 0.018]
specular_roughness = 0.55
specular_ior = 1.55
[material.epoxy_black.krbr]
bump_pattern = "noise"
bump_strength = 0.015
bump_scale = 0.1
roughness_variation = 0.03

[material.ceramic]             # チップ部品のセラミック
specular_roughness = 0.4
specular_ior = 1.6
[material.ceramic.krbr]
bump_pattern = "noise"
bump_strength = 0.015
bump_scale = 0.05
roughness_variation = 0.03

[material.aluminum]            # 電解コンデンサの缶など
base_color = [0.91, 0.92, 0.92]
base_metalness = 1.0
specular_roughness = 0.35
[material.aluminum.krbr]
bump_pattern = "noise"
bump_strength = 0.01
bump_scale = 0.2
roughness_variation = 0.03

[material.acrylic]             # アクリル（PMMA）
base_color = [1.0, 1.0, 1.0]
specular_roughness = 0.0
specular_ior = 1.491
transmission_weight = 1.0
transmission_dispersion_scale = 1.0
transmission_dispersion_abbe_number = 57.0

[material.polycarbonate]       # ポリカーボネート
base_color = [1.0, 1.0, 1.0]
specular_roughness = 0.0
specular_ior = 1.585
transmission_weight = 1.0
transmission_dispersion_scale = 1.0
transmission_dispersion_abbe_number = 30.0
)";

}  // namespace

const char* MaterialLibrary::builtinToml() { return kBuiltinMaterials; }

MaterialLibrary MaterialLibrary::builtin() {
    MaterialLibrary lib;
    std::string err;
    if (!lib.loadText(kBuiltinMaterials, "builtin", err)) log::error("builtin materials: {}", err);
    return lib;
}

MaterialLibrary MaterialLibrary::load(const std::filesystem::path& configDir) {
    MaterialLibrary lib = builtin();
    const auto dir = configDir / "materials";
    std::error_code ec;
    if (!std::filesystem::is_directory(dir, ec)) return lib;
    std::vector<std::filesystem::path> files;
    for (const auto& e : std::filesystem::directory_iterator(dir, ec))
        if (e.is_regular_file() && e.path().extension() == ".toml") files.push_back(e.path());
    std::sort(files.begin(), files.end());
    for (const auto& f : files) {
        std::string err;
        if (lib.loadFile(f, err))
            log::info("materials: {}", platform::pathToUtf8(f));
        else
            log::warn("materials {}: {} - ignored", platform::pathToUtf8(f), err);
    }
    return lib;
}

bool MaterialLibrary::loadFile(const std::filesystem::path& p, std::string& err) {
    auto text = platform::readTextFile(p);
    if (!text) {
        err = "cannot read";
        return false;
    }
    return loadText(*text, platform::pathToUtf8(p), err);
}

bool MaterialLibrary::loadText(const std::string& text, const std::string& source, std::string& err) {
    toml::table root;
    try {
        root = toml::parse(text, source);
    } catch (const toml::parse_error& e) {
        err = std::format("{} (line {})", e.description(), e.source().begin.line);
        return false;
    }
    const toml::table* mats = root["material"].as_table();
    if (!mats) return true;
    for (const auto& [key, node] : *mats) {
        const toml::table* t = node.as_table();
        if (!t) continue;
        const std::string name(key.str());
        auto it = materials_.find(name);
        Material m;
        if (it != materials_.end()) {
            m = it->second;
        } else {
            m.name = name;
            m.base_color_from_model = true;  // base_color が無ければモデルの色
        }
        std::vector<std::string> unknown;
        readMaterial(*t, m, &unknown);
        if (t->contains("base_color")) m.base_color_from_model = false;
        if (const toml::table* k = (*t)["krbr"].as_table())
            if (auto v = (*k)["base_color_from_model"].value<bool>()) m.base_color_from_model = *v;
        for (const auto& u : unknown) log::warn("{}: material '{}': unknown parameter '{}'", source, name, u);
        materials_[name] = m;
        sources_[name] = source;
    }
    return true;
}

const Material* MaterialLibrary::find(const std::string& name) const {
    auto it = materials_.find(name);
    return it == materials_.end() ? nullptr : &it->second;
}

std::vector<std::string> MaterialLibrary::names() const {
    std::vector<std::string> out;
    for (const auto& [k, v] : materials_) out.push_back(k);
    return out;
}

const std::string& MaterialLibrary::sourceOf(const std::string& name) const {
    static const std::string empty;
    auto it = sources_.find(name);
    return it == sources_.end() ? empty : it->second;
}

}  // namespace krbr::scene
