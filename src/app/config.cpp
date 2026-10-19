#include "app/config.hpp"

#include <toml++/toml.hpp>

#include "core/log.hpp"
#include "core/platform.hpp"
#include "scene/model_map.hpp"

namespace krbr::app {

namespace {

const char* kBoardToml = R"(# krbr: 基板の設定（このファイルが無ければ既定値を使います）
# 色は sRGB の [r, g, b]（0..1）

[board]
finish = "auto"              # auto（stackup の copper_finish に従う）/ ENIG / HASL / OSP
# mask_color = [0.05, 0.42, 0.16]   # 省略時は stackup の色、それも無ければ緑
mask_roughness = 0.18
mask_scatter = 0.3           # レジストの顔料自身が光を散らす割合
silk_color = [0.93, 0.93, 0.90]
fr4_color = [0.62, 0.58, 0.42]
clip_silk_by_mask = true     # レジストの開口部ではシルクを印刷しない
texture_px_per_mm = 50       # 層テクスチャの解像度（50 = 20 µm/px）
include_dnp = true

[relief]                     # 銅箔・レジスト・シルクの厚みを形状として作る（段差）
enabled = true
# copper_um = 35             # 省略時は stackup の銅の厚み
mask_on_copper_um = 12
mask_on_substrate_um = 25    # 銅の間を埋めるので、銅の上より厚い
silk_um = 15

[surface]
bump_intensity = 0.6         # 表面の微細な凹凸（ゆず肌・織り目・粒状感）の強さの倍率。0 で無効

[placeholder]                # 3D モデルが見つからない部品の代替の箱
height_mm = 1.0
color = [0.30, 0.36, 0.50]

[paths]
# 3D モデルのパスの変数を追加・上書きします
# vars = { KICAD10_3DMODEL_DIR = "C:/Program Files/KiCad/10.0/share/kicad/3dmodels" }
# 見つからないモデルをファイル名で探す追加フォルダ
# model_search_dirs = ["D:/kicad/3dmodels"]

[kicad_cli]
# 線フォントの文字（render_cache の無い文字）を kicad-cli の SVG 出力から取り込む
use_for_text = true
# path = "C:/Program Files/KiCad/10.0/bin/kicad-cli.exe"
)";

const char* kEnvironmentToml = R"(# krbr: 照明・カメラ・レンダリングの設定（コマンドラインの指定が優先されます）

[environment]
# hdr = "../hdr/studio.exr"   # このファイルからの相対パスも可
intensity = 1.0
rotation_deg = 0.0

[camera]
elevation_deg = 35.0         # 基板面からの仰角
azimuth_deg = 45.0           # 0 で基板の手前（下辺側）から、正の値で右回り
fov_deg = 30.0               # 縦の画角
margin = 0.04                # 画面端の余白（割合）

[render]
width = 1920
height = 1080
spp = 64                     # 1 画素あたりのサンプル数（作業中の PC 向けに控えめ）
max_depth = 8
exposure_ev = 0.7
radiance_clamp = 10.0        # 間接光の放射輝度の上限（ホタル対策）。0 で無効（偏りのない結果）
integrator = "pt"            # 今回はパストレーサーのみ
)";

glm::vec3 readColor(const toml::node_view<const toml::node>& n, glm::vec3 def, bool* set = nullptr) {
    if (const toml::array* a = n.as_array(); a && a->size() == 3) {
        if (set) *set = true;
        return {static_cast<float>((*a)[0].value_or(0.0)), static_cast<float>((*a)[1].value_or(0.0)),
                static_cast<float>((*a)[2].value_or(0.0))};
    }
    return def;
}

template <class T>
void readNum(const toml::node_view<const toml::node>& n, T& dst) {
    if (auto v = n.value<double>()) dst = static_cast<T>(*v);
}

bool parseFile(const fs::path& p, toml::table& out) {
    auto text = platform::readTextFile(p);
    if (!text) return false;
    try {
        out = toml::parse(*text, platform::pathToUtf8(p));
        return true;
    } catch (const toml::parse_error& e) {
        log::warn("{}: {} (line {}) - ignored", platform::pathToUtf8(p), e.description(), e.source().begin.line);
        return false;
    }
}

}  // namespace

fs::path projectConfigDir(const fs::path& pcbFile) {
    const fs::path abs = fs::absolute(pcbFile);
    return abs.parent_path() / (platform::pathToUtf8(abs.stem()) + ".krbr");
}

std::string defaultBoardToml() { return kBoardToml; }
std::string defaultEnvironmentToml() { return kEnvironmentToml; }

ProjectConfig loadProjectConfig(const fs::path& pcbFile) {
    ProjectConfig c;
    const fs::path dir = projectConfigDir(pcbFile);
    toml::table t;
    if (parseFile(dir / "board.toml", t)) {
        c.loadedFiles.push_back(dir / "board.toml");
        const toml::node_view<const toml::node> root{t};
        auto b = root["board"];
        auto& o = c.scene;
        o.finish = b["finish"].value_or(o.finish);
        o.maskColor = readColor(b["mask_color"], o.maskColor, &o.maskColorSet);
        readNum(b["mask_roughness"], o.maskRoughness);
        readNum(b["mask_scatter"], o.maskScatter);
        o.silkColor = readColor(b["silk_color"], o.silkColor);
        o.fr4Color = readColor(b["fr4_color"], o.fr4Color);
        o.layerArt.clipSilkByMask = b["clip_silk_by_mask"].value_or(o.layerArt.clipSilkByMask);
        readNum(b["texture_px_per_mm"], o.layerArt.pxPerMm);
        o.includeDnp = b["include_dnp"].value_or(o.includeDnp);
        auto rel = root["relief"];
        o.relief = rel["enabled"].value_or(o.relief);
        if (auto v = rel["copper_um"].value<double>()) {
            o.reliefParams.copper = *v / 1000.0;
            o.reliefCopperFromStackup = false;
        }
        if (auto v = rel["mask_on_copper_um"].value<double>()) o.reliefParams.maskOnCopper = *v / 1000.0;
        if (auto v = rel["mask_on_substrate_um"].value<double>()) o.reliefParams.maskOnSubstrate = *v / 1000.0;
        if (auto v = rel["silk_um"].value<double>()) o.reliefParams.silk = *v / 1000.0;
        readNum(root["surface"]["bump_intensity"], o.bumpIntensity);
        auto ph = root["placeholder"];
        readNum(ph["height_mm"], o.placeholderHeight);
        o.placeholderColor = readColor(ph["color"], o.placeholderColor);
        auto paths = root["paths"];
        if (const toml::table* vars = paths["vars"].as_table())
            for (const auto& [k, v] : *vars)
                if (auto s = v.value<std::string>()) c.pathVars[std::string(k.str())] = *s;
        if (const toml::array* dirs = paths["model_search_dirs"].as_array())
            for (const auto& d : *dirs)
                if (auto s = d.value<std::string>()) {
                    fs::path p = platform::u8path(*s);
                    if (p.is_relative()) p = dir.parent_path() / p;
                    c.modelSearchDirs.push_back(p);
                }
        auto cli = root["kicad_cli"];
        c.useKicadCliText = cli["use_for_text"].value_or(c.useKicadCliText);
        c.kicadCli = cli["path"].value_or(c.kicadCli);
    }
    toml::table e;
    if (parseFile(dir / "environment.toml", e)) {
        c.loadedFiles.push_back(dir / "environment.toml");
        const toml::node_view<const toml::node> root{e};
        auto env = root["environment"];
        if (auto h = env["hdr"].value<std::string>()) {
            fs::path p = platform::u8path(*h);
            if (p.is_relative()) p = dir / p;
            c.hdr = platform::pathToUtf8(p.lexically_normal());
        }
        readNum(env["intensity"], c.envIntensity);
        readNum(env["rotation_deg"], c.envRotationDeg);
        auto cam = root["camera"];
        readNum(cam["elevation_deg"], c.elevationDeg);
        readNum(cam["azimuth_deg"], c.azimuthDeg);
        readNum(cam["fov_deg"], c.fovDeg);
        readNum(cam["margin"], c.margin);
        auto r = root["render"];
        readNum(r["width"], c.width);
        readNum(r["height"], c.height);
        readNum(r["spp"], c.spp);
        readNum(r["max_depth"], c.maxDepth);
        readNum(r["exposure_ev"], c.exposureEv);
        readNum(r["radiance_clamp"], c.radianceClamp);
        c.integrator = r["integrator"].value_or(c.integrator);
    }
    for (const auto& f : c.loadedFiles) log::info("config: {}", platform::pathToUtf8(f));
    if (c.loadedFiles.empty()) log::info("config: no {} found, using defaults", platform::pathToUtf8(dir.filename()));
    return c;
}

void writeDefaultConfigs(const fs::path& pcbFile) {
    const fs::path dir = projectConfigDir(pcbFile);
    auto writeIfMissing = [](const fs::path& p, const std::string& text) {
        std::error_code ec;
        if (fs::exists(p, ec)) {
            log::info("--init: keeping existing {}", platform::pathToUtf8(p));
            return;
        }
        if (platform::writeTextFile(p, text))
            log::info("--init: wrote {}", platform::pathToUtf8(p));
        else
            log::warn("--init: cannot write {}", platform::pathToUtf8(p));
    };
    writeIfMissing(dir / "board.toml", kBoardToml);
    writeIfMissing(dir / "environment.toml", kEnvironmentToml);
    writeIfMissing(platform::configDir() / "model_map.toml", scene::ModelMap::defaultToml());
}

}  // namespace krbr::app
