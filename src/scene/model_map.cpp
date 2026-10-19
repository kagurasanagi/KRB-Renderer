#include "scene/model_map.hpp"

#include <algorithm>
#include <cctype>
#include <sstream>
#include <toml++/toml.hpp>

#include "core/log.hpp"
#include "core/platform.hpp"

namespace krbr::scene {

namespace {

// 組み込みの規則（全体設定の規則の後に評価する）
const char* kBuiltinRules = R"(
[[rule]]
part = "kicad:metal_grey_pins"
material = "metal_tin"
[[rule]]
part = "kicad:gold_pins"
material = "metal_gold"
[[rule]]
part = "kicad:black_body"
material = "epoxy_black"
[[rule]]
part = "kicad:light_brown_body"
material = "ceramic"
[[rule]]   # わずかに黄みのある灰色（端子の金属）
sat_max = 0.12
val_min = 0.55
val_max = 0.88
material = "metal_tin"
[[rule]]   # 金色
hue_min = 30.0
hue_max = 55.0
sat_min = 0.3
val_min = 0.6
material = "metal_gold"
[[rule]]   # ほぼ白
sat_max = 0.05
val_min = 0.88
material = "glossy_plastic"
[[rule]]
material = "plastic"
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

PartAssignment readAssignment(const toml::table& t) {
    PartAssignment a;
    a.material = t["material"].value_or(std::string{});
    if (const toml::table* parts = t["parts"].as_table())
        for (const auto& [k, v] : *parts)
            if (auto s = v.value<std::string>()) a.parts[std::string(k.str())] = *s;
    return a;
}

void writeAssignment(toml::table& t, const PartAssignment& a) {
    if (!a.material.empty()) t.insert("material", a.material);
    if (!a.parts.empty()) {
        toml::table parts;
        for (const auto& [k, v] : a.parts) parts.insert(k, v);
        t.insert("parts", parts);
    }
}

std::vector<ModelEntry> readModels(const toml::table& root) {
    std::vector<ModelEntry> out;
    if (const toml::array* arr = root["model"].as_array()) {
        for (const auto& n : *arr) {
            const toml::table* t = n.as_table();
            if (!t) continue;
            ModelEntry e;
            e.path = (*t)["path"].value_or(std::string{});
            e.hash = (*t)["hash"].value_or(std::string{});
            e.assign = readAssignment(*t);
            if (!e.path.empty() || !e.hash.empty()) out.push_back(std::move(e));
        }
    }
    return out;
}

toml::array writeModels(const std::vector<ModelEntry>& models) {
    toml::array arr;
    for (const auto& e : models) {
        toml::table t;
        if (!e.path.empty()) t.insert("path", e.path);
        if (!e.hash.empty()) t.insert("hash", e.hash);
        writeAssignment(t, e.assign);
        arr.push_back(t);
    }
    return arr;
}

std::vector<MaterialRule> readRules(const toml::table& root) {
    std::vector<MaterialRule> out;
    if (const toml::array* arr = root["rule"].as_array()) {
        for (const auto& n : *arr) {
            const toml::table* t = n.as_table();
            if (!t) continue;
            MaterialRule r;
            r.part = (*t)["part"].value_or(std::string{});
            r.model = (*t)["model"].value_or(std::string{});
            r.material = (*t)["material"].value_or(std::string{});
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
            if (!r.material.empty()) out.push_back(std::move(r));
        }
    }
    return out;
}

toml::array writeRules(const std::vector<MaterialRule>& rules) {
    toml::array arr;
    for (const auto& r : rules) {
        toml::table t;
        if (!r.part.empty()) t.insert("part", r.part);
        if (!r.model.empty()) t.insert("model", r.model);
        auto put = [&](const char* k, const std::optional<float>& v) {
            if (v) t.insert(k, double(*v));
        };
        put("hue_min", r.hueMin);
        put("hue_max", r.hueMax);
        put("sat_min", r.satMin);
        put("sat_max", r.satMax);
        put("val_min", r.valMin);
        put("val_max", r.valMax);
        t.insert("material", r.material);
        arr.push_back(t);
    }
    return arr;
}

bool parseFile(const std::filesystem::path& p, toml::table& out, std::string& err) {
    auto text = platform::readTextFile(p);
    if (!text) {
        err = "cannot read";
        return false;
    }
    try {
        out = toml::parse(*text, platform::pathToUtf8(p));
        return true;
    } catch (const toml::parse_error& e) {
        err = std::format("{} (line {})", e.description(), e.source().begin.line);
        return false;
    }
}

bool writeFile(const std::filesystem::path& p, const std::string& header, const toml::table& t, std::string& err) {
    std::ostringstream ss;
    ss << header << "\n\n" << toml::toml_formatter(t) << "\n";
    if (!platform::writeTextFile(p, ss.str())) {
        err = "cannot write " + platform::pathToUtf8(p);
        return false;
    }
    return true;
}

ModelEntry& upsertModel(std::vector<ModelEntry>& models, const std::string& path, const std::string& hash) {
    for (auto& e : models)
        if ((!path.empty() && e.path == path) || (!hash.empty() && e.hash == hash)) {
            if (e.path.empty()) e.path = path;
            if (!hash.empty()) e.hash = hash;
            return e;
        }
    models.push_back({path, hash, {}});
    return models.back();
}

}  // namespace

bool globMatch(std::string_view p, std::string_view s) {
    size_t pi = 0, si = 0, star = std::string_view::npos, mark = 0;
    auto eq = [](char a, char b) {
        return std::tolower(static_cast<unsigned char>(a)) == std::tolower(static_cast<unsigned char>(b));
    };
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

std::string PartAssignment::lookup(const model::ModelPart* part) const {
    if (part)
        for (const auto& k : part->keys())
            if (auto it = parts.find(k); it != parts.end()) return it->second;
    if (auto it = parts.find("*"); it != parts.end()) return it->second;
    return material;
}

bool ModelEntry::matches(const std::string& spec, const std::string& fileHash) const {
    return (!path.empty() && path == spec) || (!hash.empty() && !fileHash.empty() && hash == fileHash);
}

bool MaterialRule::matches(const std::string& modelFile, const model::ModelPart* p) const {
    if (!model.empty() && !globMatch(model, modelFile)) return false;
    if (!part.empty()) {
        if (!p) return false;
        bool any = false;
        for (const auto& k : p->keys()) any = any || globMatch(part, k);
        if (!any) return false;
    }
    const bool needsColor = hueMin || hueMax || satMin || satMax || valMin || valMax;
    if (needsColor) {
        if (!p || !p->hasColor) return false;
        float h, s, v;
        rgbToHsv(p->srgb, h, s, v);
        if ((hueMin && h < *hueMin) || (hueMax && h > *hueMax) || (satMin && s < *satMin) || (satMax && s > *satMax) ||
            (valMin && v < *valMin) || (valMax && v > *valMax))
            return false;
    }
    return true;
}

bool GlobalModelMap::load(const std::filesystem::path& p, std::string& err) {
    toml::table root;
    if (!parseFile(p, root, err)) return false;
    models = readModels(root);
    rules = readRules(root);
    return true;
}

bool GlobalModelMap::save(const std::filesystem::path& p, std::string& err) const {
    toml::table root;
    if (!models.empty()) root.insert("model", writeModels(models));
    if (!rules.empty()) root.insert("rule", writeRules(rules));
    return writeFile(p,
                     "# krbr: 3D モデルごとの材質の設定（全体設定。他のプロジェクトと共有）\n"
                     "# [[model]] path（展開前のパス）か hash（ファイル内容の XXH64）が一致したモデルに使う。\n"
                     "#   material: 全部位, parts: 部位ごと（キーは XDE の名前 / \"kicad:<色名>\" / \"#rrggbb\" / \"*\"）\n"
                     "# [[rule]] 色・名前の規則（上から順に評価。組み込みの規則より先）",
                     root, err);
}

ModelEntry& GlobalModelMap::upsert(const std::string& path, const std::string& hash) {
    return upsertModel(models, path, hash);
}

bool ProjectOverrides::load(const std::filesystem::path& p, std::string& err) {
    toml::table root;
    if (!parseFile(p, root, err)) return false;
    refs.clear();
    if (const toml::table* r = root["ref"].as_table())
        for (const auto& [k, v] : *r)
            if (const toml::table* t = v.as_table()) refs[std::string(k.str())] = readAssignment(*t);
    models = readModels(root);
    return true;
}

bool ProjectOverrides::save(const std::filesystem::path& p, std::string& err) const {
    toml::table root;
    if (!refs.empty()) {
        toml::table r;
        for (const auto& [k, v] : refs) {
            toml::table t;
            writeAssignment(t, v);
            r.insert(k, t);
        }
        root.insert("ref", r);
    }
    if (!models.empty()) root.insert("model", writeModels(models));
    return writeFile(p,
                     "# krbr: このプロジェクトの材質の上書き（全体設定より優先）\n"
                     "# [ref.<部品番号>] 部品ごと / [[model]] 3D モデルごと（path か hash で照合）\n"
                     "#   material: 全部位, parts: 部位ごと（キーは XDE の名前 / \"kicad:<色名>\" / \"#rrggbb\" / \"*\"）",
                     root, err);
}

ModelEntry& ProjectOverrides::upsert(const std::string& path, const std::string& hash) {
    return upsertModel(models, path, hash);
}

const std::vector<MaterialRule>& builtinRules() {
    static const std::vector<MaterialRule> rules = [] {
        toml::table root = toml::parse(std::string_view(kBuiltinRules), std::string_view("builtin rules"));
        return readRules(root);
    }();
    return rules;
}

MaterialResolver::MaterialResolver(const MaterialLibrary& lib, const ProjectOverrides& project,
                                   const GlobalModelMap& global)
    : lib_(lib), project_(project), global_(global) {}

std::optional<ResolvedMaterial> MaterialResolver::make(const std::string& name, const std::string& source,
                                                       const MaterialQuery& q) const {
    if (name.empty()) return std::nullopt;
    const Material* m = lib_.find(name);
    if (!m) {
        static std::map<std::string, bool> warned;
        if (!warned[name]) log::warn("material '{}' ({}) is not in the material library; skipped", name, source);
        warned[name] = true;
        return std::nullopt;
    }
    ResolvedMaterial r{*m, name, source};
    if (m->base_color_from_model && q.part && q.part->hasColor) r.material.base_color = q.part->linearColor();
    r.material.name = name;
    return r;
}

ResolvedMaterial MaterialResolver::resolve(const MaterialQuery& q) const {
    // 1. 部品番号ごとの上書き（プロジェクト）
    if (auto it = project_.refs.find(q.ref); !q.ref.empty() && it != project_.refs.end())
        if (auto r = make(it->second.lookup(q.part), "ref " + q.ref + " (project)", q)) return *r;
    // 2. 3D モデルごとの上書き（プロジェクト）
    for (const auto& e : project_.models)
        if (e.matches(q.modelSpec, q.modelHash))
            if (auto r = make(e.assign.lookup(q.part), "model (project)", q)) return *r;
    // 3. 3D モデルごとの設定（全体）
    for (const auto& e : global_.models)
        if (e.matches(q.modelSpec, q.modelHash))
            if (auto r = make(e.assign.lookup(q.part), "model (global)", q)) return *r;
    // 4. 色・名前の規則（全体 → 組み込み）
    for (size_t i = 0; i < global_.rules.size(); ++i)
        if (global_.rules[i].matches(q.modelFile, q.part))
            if (auto r = make(global_.rules[i].material, std::format("rule {} (global)", i + 1), q)) return *r;
    const auto& br = builtinRules();
    for (size_t i = 0; i < br.size(); ++i)
        if (br[i].matches(q.modelFile, q.part))
            if (auto r = make(br[i].material, std::format("rule {} (builtin)", i + 1), q)) return *r;
    // 5. 既定値
    ResolvedMaterial r;
    r.name = "default";
    r.source = "default";
    r.material.name = "default";
    if (q.part && q.part->hasColor) r.material.base_color = q.part->linearColor();
    return r;
}

}  // namespace krbr::scene
