#include "app/prepare.hpp"

#include <glm/gtc/constants.hpp>

#include "core/log.hpp"
#include "core/platform.hpp"
#include "kicad/model_resolver.hpp"
#include "model/mesh_cache.hpp"
#include "scene/model_map.hpp"

namespace krbr::app {

namespace fs = std::filesystem;

namespace {

int countSilkStrokeTexts(const kicad::Board& b) {
    int n = 0;
    for (const auto& t : b.strokeTexts)
        if (t.layer == "F.SilkS" || t.layer == "B.SilkS") ++n;
    return n;
}

}  // namespace

std::optional<kicad::Board> loadBoardOrReport(const std::string& pcb) {
    const fs::path p = platform::u8path(pcb);
    log::info("loading {}", platform::pathToUtf8(fs::absolute(p)));
    auto r = kicad::loadBoard(p);
    if (!r.board) {
        log::error("{}", r.error);
        return std::nullopt;
    }
    const auto& b = *r.board;
    log::info("board v{}: {} footprints, {} pads, {} tracks, {} vias, {} zone fills, {} graphics", b.version,
              b.footprints.size(), b.pads.size(), b.tracks.size(), b.vias.size(), b.zones.size(), b.graphics.size());
    if (b.unfilledZones) log::warn("{} zone(s) have no fill data (refill zones in KiCad and save)", b.unfilledZones);
    const int st = countSilkStrokeTexts(b);
    if (st) log::debug("{} stroke-font silkscreen text(s)", st);
    return r.board;
}

std::unique_ptr<PreparedScene> prepareScene(const RenderOptions& o, const fs::path& debugLayersDir,
                                            const std::string& debugStem) {
    auto boardOpt = loadBoardOrReport(o.pcb);
    if (!boardOpt) return nullptr;
    auto prep = std::make_unique<PreparedScene>();
    prep->board = std::move(*boardOpt);
    const kicad::Board& board = prep->board;

    if (o.init) writeDefaultConfigs(board.file);
    ProjectConfig& cfg = prep->cfg;
    cfg = loadProjectConfig(board.file);
    if (o.spp) cfg.spp = *o.spp;
    if (o.width) cfg.width = *o.width;
    if (o.height) cfg.height = *o.height;
    if (o.maxDepth) cfg.maxDepth = *o.maxDepth;
    if (o.elevation) cfg.elevationDeg = *o.elevation;
    if (o.azimuth) cfg.azimuthDeg = *o.azimuth;
    if (o.fov) cfg.fovDeg = *o.fov;
    if (o.exposure) cfg.exposureEv = *o.exposure;
    if (o.envIntensity) cfg.envIntensity = *o.envIntensity;
    if (o.envRotation) cfg.envRotationDeg = *o.envRotation;
    if (o.radianceClamp) cfg.radianceClamp = *o.radianceClamp;
    if (!o.integrator.empty()) cfg.integrator = o.integrator;
    if (!o.hdr.empty()) cfg.hdr = o.hdr;
    if (cfg.hdr.empty()) {
        log::error("no environment map: pass --hdr <file.hdr|file.exr> or set [environment] hdr in environment.toml");
        return nullptr;
    }
    if (cfg.width <= 0 || cfg.height <= 0 || cfg.spp <= 0) {
        log::error("invalid size or spp");
        return nullptr;
    }
    std::error_code ec;
    const fs::path cacheDir = platform::cacheDir();

    // 部品の材質の対応表（全体設定）
    scene::ModelMap modelMap = scene::ModelMap::defaults();
    const fs::path mapFile = platform::configDir() / "model_map.toml";
    if (fs::exists(mapFile, ec)) {
        std::string err;
        if (modelMap.load(mapFile, err))
            log::info("model map: {}", platform::pathToUtf8(mapFile));
        else
            log::warn("model map {}: {} - using defaults", platform::pathToUtf8(mapFile), err);
    }

    // 線フォントの文字は kicad-cli の SVG 出力から取り込む
    const int strokeTexts = countSilkStrokeTexts(board);
    if (strokeTexts > 0) {
        if (!cfg.useKicadCliText || o.noKicadCli) {
            log::warn("{} stroke-font silkscreen text(s) will be missing (kicad-cli disabled)", strokeTexts);
        } else if (auto cli = findKicadCli(cfg.kicadCli)) {
            log::ScopedTimer t("kicad-cli");
            prep->silk = plotSilkWithKicadCli(*cli, board, cacheDir, !o.noCache);
            log::info("silkscreen text via kicad-cli ({} stroke texts){}", strokeTexts,
                      prep->silk.fromCache ? " [cache hit]" : "");
        } else {
            log::warn("kicad-cli not found; {} stroke-font silkscreen text(s) will be missing", strokeTexts);
        }
    }
    for (int s = 0; s < 2; ++s)
        if (prep->silk.side[s]) cfg.scene.layerArt.extraSilk[s] = &*prep->silk.side[s];

    kicad::ModelResolver resolver(board.projectDir, cfg.pathVars, cfg.modelSearchDirs);
    model::ModelLibrary library(cacheDir, model::MeshParams{}, !o.noCache);
    log::info("mesh cache: {}{}", platform::pathToUtf8(cacheDir / "meshes"), o.noCache ? " (disabled)" : "");
    prep->scene = scene::buildScene({board, resolver, library, modelMap, cfg.scene});
    const auto& st = library.stats();
    log::info("models: {} loaded+meshed, {} from cache, {} failed", st.loaded, st.cacheHits, st.failed);

    if (!debugLayersDir.empty()) {
        const auto& L = prep->scene.layers;
        for (int s = 0; s < 2; ++s) {
            std::string err;
            const fs::path p = debugLayersDir / (debugStem + (s == 0 ? "_layers_front.png" : "_layers_back.png"));
            if (io::savePngRgba8(p, L.grid.width, L.grid.height, L.rgba[s].data(), err))
                log::info("wrote {}", platform::pathToUtf8(p));
            else
                log::warn("{}", err);
        }
    }

    std::string err;
    if (!io::loadHdrImage(platform::u8path(cfg.hdr), prep->env, err)) {
        log::error("environment map {}: {}", cfg.hdr, err);
        return nullptr;
    }
    log::info("environment: {} ({}x{})", cfg.hdr, prep->env.width, prep->env.height);
    prep->dist = render::buildEnvDistribution(prep->env);
    return prep;
}

scene::CameraParams cameraParams(const ProjectConfig& cfg, int width, int height) {
    scene::CameraParams cp;
    cp.elevationDeg = cfg.elevationDeg;
    cp.azimuthDeg = cfg.azimuthDeg;
    cp.fovDeg = cfg.fovDeg;
    cp.margin = cfg.margin;
    cp.width = width;
    cp.height = height;
    return cp;
}

scene::Camera initialCamera(const PreparedScene& prep, const RenderOptions& o, int width, int height) {
    const ProjectConfig& cfg = prep.cfg;
    scene::Camera cam = scene::frameCamera(cameraParams(cfg, width, height), prep.scene.framingPoints);
    glm::vec3 target = cam.target;
    if (o.lookAt.size() == 2) {
        const auto& b = prep.scene.board;
        target = geom::boardToWorld({o.lookAt[0], o.lookAt[1]}, b.thickness / 2, b.center);
    }
    const float dist = cam.distance / static_cast<float>(std::max(1e-3, o.zoom));
    return scene::orbitCamera(target, dist, cfg.elevationDeg, cfg.azimuthDeg, cfg.fovDeg, cam.aspect);
}

render::FrameParams frameParams(const ProjectConfig& cfg, const scene::Camera& cam, unsigned seed) {
    render::FrameParams f;
    f.camera = cam;
    f.envIntensity = cfg.envIntensity;
    f.envRotationRad = glm::radians(cfg.envRotationDeg);
    f.radianceClamp = cfg.radianceClamp;
    f.maxDepth = static_cast<uint32_t>(std::max(1, cfg.maxDepth));
    f.seed = seed;
    return f;
}

}  // namespace krbr::app
