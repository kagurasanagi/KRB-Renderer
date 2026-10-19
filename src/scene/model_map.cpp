#include "scene/model_map.hpp"

#include <algorithm>
#include <cctype>
#include <toml++/toml.hpp>

#include "core/log.hpp"
#include "core/platform.hpp"

namespace krbr::scene {

namespace {

const char* kDefaultToml = R"(# krbr: 部品の 3D モデルの材質の対応表（全体設定）
# パラメータ名は OpenPBR Surface に合わせています。base_color を省略するとモデルの色を使います。
# bump_pattern（"noise" / "weave" / "none"）, bump_strength（傾き）, bump_scale（mm）, roughness_variation は
# krbr 独自の、表面の微細な凹凸と粗さのむらです。
# [[rules]] は上から順に評価され、最初に一致したものが使われます。
#   model   : モデルのファイル名に対するワイルドカード（例 "LED_*"）
#   hue_min / hue_max (度), sat_min / sat_max, val_min / val_max : 面の色（sRGB の HSV）の条件

[presets.metal_tin]        # 錫めっきのリード・端子
base_metalness = 1.0
specular_roughness = 0.28
bump_pattern = "noise"
bump_strength = 0.012
bump_scale = 0.08
roughness_variation = 0.2

[presets.metal_gold]       # 金めっきの端子
base_metalness = 1.0
specular_roughness = 0.22
bump_pattern = "noise"
bump_strength = 0.01
bump_scale = 0.08
roughness_variation = 0.2

[presets.plastic]          # 樹脂・セラミックのボディ
base_metalness = 0.0
specular_roughness = 0.45
specular_ior = 1.5
bump_pattern = "noise"
bump_strength = 0.012
bump_scale = 0.3
roughness_variation = 0.15

[presets.glossy_plastic]   # 白い樹脂など、つやのあるもの
base_metalness = 0.0
specular_roughness = 0.3
specular_ior = 1.5
bump_pattern = "noise"
bump_strength = 0.01
bump_scale = 0.3
roughness_variation = 0.15

[[rules]]   # わずかに黄みのある灰色（KiCad 標準モデルの "metal grey pins" など）
preset = "metal_tin"
sat_max = 0.12
val_min = 0.55
val_max = 0.88

[[rules]]   # 金色
preset = "metal_gold"
hue_min = 30.0
hue_max = 55.0
sat_min = 0.3
val_min = 0.6

[[rules]]   # ほぼ白
preset = "glossy_plastic"
sat_max = 0.05
val_min = 0.88

[[rules]]
preset = "plastic"
)";

void rgbToHsv(glm::vec3 c, float& h, float& s, float& v) {
    const float mx = std::max({c.r, c.g, c.b}), mn = std::min({c.r, c.g, c.b});
    v = mx;
    s = mx > 0 ? (mx - mn) / mx : 0;
    if (mx == mn) {
        h = 0;
        return;
    }
    const float d = mx - mn;
    if (mx == c.r) h = 60.0f * std::fmod((c.g - c.b) / d, 6.0f);
    else if (mx == c.g) h = 60.0f * ((c.b - c.r) / d + 2.0f);
    else h = 60.0f * ((c.r - c.g) / d + 4.0f);
    if (h < 0) h += 360.0f;
}

void readMaterial(const toml::table& t, Material& m, bool& hasBaseColor) {
    auto f = [&](const char* key, float& dst) {
        if (auto v = t[key].value<double>()) dst = static_cast<float>(*v);
    };
    auto c = [&](const char* key, glm::vec3& dst) -> bool {
        if (const toml::array* a = t[key].as_array(); a && a->size() == 3) {
            for (int i = 0; i < 3; ++i) dst[i] = static_cast<float>((*a)[i].value_or(0.0));
            return true;
        }
        return false;
    };
    hasBaseColor = c("base_color", m.base_color);
    f("base_weight", m.base_weight);
    f("base_metalness", m.base_metalness);
    c("specular_color", m.specular_color);
    f("specular_weight", m.specular_weight);
    f("specular_roughness", m.specular_roughness);
    f("specular_ior", m.specular_ior);
    f("coat_weight", m.coat_weight);
    c("coat_color", m.coat_color);
    f("coat_roughness", m.coat_roughness);
    f("coat_ior", m.coat_ior);
    // krbr 独自：表面の微細な凹凸
    if (auto v = t["bump_pattern"].value<std::string>())
        m.bump_pattern = *v == "noise" ? BumpPattern::Noise : *v == "weave" ? BumpPattern::Weave : BumpPattern::None;
    f("bump_strength", m.bump_strength);
    f("bump_scale", m.bump_scale);
    f("roughness_variation", m.roughness_variation);
}

bool parseInto(const std::string& text, const std::string& source, std::map<std::string, MaterialPreset>& presets,
               std::vector<MapRule>& rules, std::string& err) {
    toml::table root;
    try {
        root = toml::parse(text, source);
    } catch (const toml::parse_error& e) {
        err = std::string(e.description());
        return false;
    }
    if (const toml::table* ps = root["presets"].as_table()) {
        for (const auto& [name, node] : *ps) {
            const toml::table* t = node.as_table();
            if (!t) continue;
            // 同じ名前があれば、それに書かれた項目だけを上書きする
            const std::string key(name.str());
            MaterialPreset p = presets.count(key) ? presets[key] : MaterialPreset{};
            p.material.name = key;
            bool hasBase = false;
            readMaterial(*t, p.material, hasBase);
            if (hasBase) p.useModelColor = false;
            presets[key] = p;
        }
    }
    if (const toml::array* rs = root["rules"].as_array()) {
        std::vector<MapRule> parsed;
        for (const auto& node : *rs) {
            const toml::table* t = node.as_table();
            if (!t) continue;
            MapRule r;
            r.model = (*t)["model"].value_or(std::string{});
            r.preset = (*t)["preset"].value_or(std::string{"plastic"});
            auto opt = [&](const char* key) -> std::optional<float> {
                if (auto v = (*t)[key].value<double>()) return static_cast<float>(*v);
                return std::nullopt;
            };
            r.hueMin = opt("hue_min");
            r.hueMax = opt("hue_max");
            r.satMin = opt("sat_min");
            r.satMax = opt("sat_max");
            r.valMin = opt("val_min");
            r.valMax = opt("val_max");
            parsed.push_back(std::move(r));
        }
        // ファイルの規則を既定より先に評価する
        rules.insert(rules.begin(), parsed.begin(), parsed.end());
    }
    return true;
}

}  // namespace

bool globMatch(std::string_view p, std::string_view s) {
    size_t pi = 0, si = 0, star = std::string_view::npos, mark = 0;
    auto eq = [](char a, char b) { return std::tolower(static_cast<unsigned char>(a)) == std::tolower(static_cast<unsigned char>(b)); };
    while (si < s.size()) {
        if (pi < p.size() && (p[pi] == '?' || eq(p[pi], s[si]))) {
            ++pi;
            ++si;
        } else if (pi < p.size() && p[pi] == '*') {
            star = pi++;
            mark = si;
        } else if (star != std::string_view::npos) {
            pi = star + 1;
            si = ++mark;
        } else {
            return false;
        }
    }
    while (pi < p.size() && p[pi] == '*') ++pi;
    return pi == p.size();
}

std::string ModelMap::defaultToml() { return kDefaultToml; }

ModelMap ModelMap::defaults() {
    ModelMap m;
    std::string err;
    if (!parseInto(kDefaultToml, "builtin", m.presets_, m.rules_, err)) log::error("builtin model map: {}", err);
    return m;
}

bool ModelMap::load(const std::filesystem::path& p, std::string& err) {
    auto text = platform::readTextFile(p);
    if (!text) {
        err = "cannot read";
        return false;
    }
    return parseInto(*text, platform::pathToUtf8(p), presets_, rules_, err);
}

Material ModelMap::resolve(const std::string& modelFile, glm::vec4 linearColor) const {
    const glm::vec3 srgb = linearToSrgb(glm::vec3(linearColor));
    float h, s, v;
    rgbToHsv(srgb, h, s, v);
    for (const auto& r : rules_) {
        if (!r.model.empty() && !globMatch(r.model, modelFile)) continue;
        if (r.hueMin && h < *r.hueMin) continue;
        if (r.hueMax && h > *r.hueMax) continue;
        if (r.satMin && s < *r.satMin) continue;
        if (r.satMax && s > *r.satMax) continue;
        if (r.valMin && v < *r.valMin) continue;
        if (r.valMax && v > *r.valMax) continue;
        auto it = presets_.find(r.preset);
        if (it == presets_.end()) continue;
        Material m = it->second.material;
        if (it->second.useModelColor) m.base_color = glm::vec3(linearColor);
        return m;
    }
    Material m;
    m.name = "default";
    m.base_color = glm::vec3(linearColor);
    return m;
}

}  // namespace krbr::scene
