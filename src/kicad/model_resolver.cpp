#include "kicad/model_resolver.hpp"

#include <algorithm>
#include <nlohmann/json.hpp>
#include <regex>

#include "core/log.hpp"
#include "core/platform.hpp"

namespace krbr::kicad {

namespace {

std::string lower(std::string s) {
    std::transform(s.begin(), s.end(), s.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return s;
}

bool isDir(const fs::path& p) {
    std::error_code ec;
    return fs::is_directory(p, ec);
}

bool isFile(const fs::path& p) {
    std::error_code ec;
    return fs::is_regular_file(p, ec);
}

// KiCad のインストール先を探して KICADn_3DMODEL_DIR の既定値を作る
void detectInstallVars(std::map<std::string, std::string>& vars) {
    std::vector<fs::path> roots;
#ifdef _WIN32
    for (const char* env : {"ProgramFiles", "ProgramW6432"})
        if (auto pf = platform::getEnv(env)) roots.push_back(platform::u8path(*pf) / "KiCad");
#else
    roots = {"/usr/share/kicad", "/usr/local/share/kicad"};
#endif
    int bestMajor = -1;
    std::string best;
    for (const auto& root : roots) {
        if (!isDir(root)) continue;
        std::error_code ec;
        for (const auto& e : fs::directory_iterator(root, ec)) {
            if (!e.is_directory()) continue;
            const std::string ver = platform::pathToUtf8(e.path().filename());
            const int major = std::atoi(ver.c_str());
            const fs::path models = e.path() / "share" / "kicad" / "3dmodels";
            if (major <= 0 || !isDir(models)) continue;
            const std::string var = "KICAD" + std::to_string(major) + "_3DMODEL_DIR";
            if (!vars.count(var)) vars[var] = platform::pathToUtf8(models);
            if (major > bestMajor) {
                bestMajor = major;
                best = platform::pathToUtf8(models);
            }
        }
    }
    if (bestMajor > 0) {
        // 古い版の変数や KISYS3DMOD も、見つからなければ最新のライブラリを指す
        for (int v = 5; v <= bestMajor; ++v) {
            const std::string var = "KICAD" + std::to_string(v) + "_3DMODEL_DIR";
            if (!vars.count(var)) vars[var] = best;
        }
        if (!vars.count("KISYS3DMOD")) vars["KISYS3DMOD"] = best;
    }
}

// %APPDATA%/kicad/<ver>/kicad_common.json の environment.vars を読む（読み取りのみ）
void readKicadCommon(std::map<std::string, std::string>& vars) {
    fs::path base;
#ifdef _WIN32
    if (auto a = platform::getEnv("APPDATA")) base = platform::u8path(*a) / "kicad";
#else
    if (auto h = platform::getEnv("HOME")) base = platform::u8path(*h) / ".config" / "kicad";
#endif
    if (base.empty() || !isDir(base)) return;
    std::vector<fs::path> files;
    std::error_code ec;
    for (const auto& e : fs::directory_iterator(base, ec))
        if (e.is_directory() && isFile(e.path() / "kicad_common.json")) files.push_back(e.path() / "kicad_common.json");
    std::sort(files.begin(), files.end());  // 新しい版を後に読んで優先
    for (const auto& f : files) {
        auto text = platform::readTextFile(f);
        if (!text) continue;
        try {
            auto j = nlohmann::json::parse(*text);
            const auto& v = j["environment"]["vars"];
            if (!v.is_object()) continue;
            for (auto it = v.begin(); it != v.end(); ++it)
                if (it.value().is_string()) vars[it.key()] = it.value().get<std::string>();
        } catch (const std::exception& e) {
            log::debug("ignoring {}: {}", platform::pathToUtf8(f), e.what());
        }
    }
}

}  // namespace

ModelResolver::ModelResolver(const fs::path& projectDir, const std::map<std::string, std::string>& extraVars,
                             const std::vector<fs::path>& searchDirs)
    : projectDir_(projectDir), searchDirs_(searchDirs) {
    readKicadCommon(vars_);
    detectInstallVars(vars_);
    for (const char* name : {"KISYS3DMOD", "KICAD6_3DMODEL_DIR", "KICAD7_3DMODEL_DIR", "KICAD8_3DMODEL_DIR",
                             "KICAD9_3DMODEL_DIR", "KICAD10_3DMODEL_DIR", "KICAD_USER_3DMODEL_DIR"})
        if (auto v = platform::getEnv(name)) vars_[name] = *v;
    for (const auto& [k, v] : extraVars) vars_[k] = v;
    vars_["KIPRJMOD"] = platform::pathToUtf8(projectDir_);
}

std::string ModelResolver::expand(const std::string& s, std::string* undefinedVar) const {
    static const std::regex re(R"(\$\{([^}]+)\}|\$\(([^)]+)\))");
    std::string out;
    auto begin = std::sregex_iterator(s.begin(), s.end(), re);
    size_t last = 0;
    for (auto it = begin; it != std::sregex_iterator(); ++it) {
        const auto& m = *it;
        out.append(s, last, m.position() - last);
        const std::string name = m[1].matched ? m[1].str() : m[2].str();
        auto v = vars_.find(name);
        if (v != vars_.end()) {
            out += v->second;
        } else {
            if (auto e = platform::getEnv(name)) {
                out += *e;
            } else {
                if (undefinedVar && undefinedVar->empty()) *undefinedVar = name;
                out += m.str();
            }
        }
        last = m.position() + m.length();
    }
    out.append(s, last, std::string::npos);
    return out;
}

ResolvedModel ModelResolver::resolve(const std::string& spec) const {
    ResolvedModel r;
    std::string undefinedVar;
    r.expanded = expand(spec, &undefinedVar);
    fs::path p = platform::u8path(r.expanded);
    if (p.is_relative()) p = projectDir_ / p;
    p = p.lexically_normal();

    const std::string ext = lower(platform::pathToUtf8(p.extension()));
    const bool isVrml = ext == ".wrl" || ext == ".wrz";
    // 候補：STEP を優先（VRML は読み込み器が無いので、同名の STEP があれば差し替える）
    std::vector<fs::path> candidates;
    auto withExt = [&](const fs::path& base, std::initializer_list<const char*> exts) {
        for (const char* e : exts) {
            fs::path q = base;
            q.replace_extension(e);
            candidates.push_back(q);
        }
    };
    if (isVrml) {
        withExt(p, {".step", ".stp", ".STEP", ".STP"});
        candidates.push_back(p);
    } else {
        candidates.push_back(p);
        withExt(p, {".step", ".stp", ".STEP", ".STP"});
    }
    for (const auto& c : candidates) {
        if (isFile(c)) {
            r.found = true;
            r.path = c;
            r.substituted = c != p;
            return r;
        }
    }
    // 追加の検索フォルダをファイル名で探す（"Lib.3dshapes/name.step" の形も試す）
    const fs::path fname = p.filename();
    const fs::path libAndName = p.parent_path().filename() / fname;
    for (const auto& dir : searchDirs_) {
        for (const auto& rel : {libAndName, fname}) {
            fs::path q = dir / rel;
            for (const char* e : {"", ".step", ".stp"}) {
                fs::path c = q;
                if (*e) c.replace_extension(e);
                if (isFile(c)) {
                    r.found = true;
                    r.path = c;
                    r.substituted = true;
                    return r;
                }
            }
        }
    }
    if (!undefinedVar.empty())
        r.reason = "undefined variable ${" + undefinedVar + "}";
    else
        r.reason = "file not found";
    r.path = p;
    return r;
}

}  // namespace krbr::kicad
