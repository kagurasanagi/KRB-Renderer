#include "app/commands.hpp"

#include <glm/gtc/constants.hpp>

#include "app/config.hpp"
#include "app/kicad_cli.hpp"
#include "app/report.hpp"
#include "core/log.hpp"
#include "core/platform.hpp"
#include "io/image_io.hpp"
#include "kicad/board.hpp"
#include "kicad/model_resolver.hpp"
#include "model/mesh_cache.hpp"
#include "render/env_map.hpp"
#include "render/gpu_scene.hpp"
#include "render/renderer.hpp"
#include "scene/camera.hpp"
#include "scene/model_map.hpp"
#include "scene/scene.hpp"

namespace krbr::app {

namespace fs = std::filesystem;

namespace {

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
    return r.board;
}

int countSilkStrokeTexts(const kicad::Board& b) {
    int n = 0;
    for (const auto& t : b.strokeTexts)
        if (t.layer == "F.SilkS" || t.layer == "B.SilkS") ++n;
    return n;
}

}  // namespace

int cmdProbe() {
    try {
        const auto devs = vk::Context::enumerate();
        if (devs.empty()) {
            log::error("no Vulkan devices");
            return 1;
        }
        for (const auto& d : devs) {
            log::info("{}: Vulkan {}.{}.{}, {} MiB, {}", d.name, VK_API_VERSION_MAJOR(d.apiVersion),
                      VK_API_VERSION_MINOR(d.apiVersion), VK_API_VERSION_PATCH(d.apiVersion), d.deviceLocalBytes >> 20,
                      d.discrete ? "discrete" : "integrated/other");
            log::info("  ray_query={} acceleration_structure={} ray_tracing_pipeline={} global_priority={}", d.rayQuery,
                      d.accelerationStructure, d.rayTracingPipeline, d.globalPriority);
        }
        vk::Context ctx;
        ctx.init({});
        log::info("headless device created (no surface / swapchain)");
        return 0;
    } catch (const std::exception& e) {
        log::error("{}", e.what());
        return 1;
    }
}

int cmdInfo(const std::string& pcb, bool resolveModels) {
    auto board = loadBoardOrReport(pcb);
    if (!board) return 1;
    const auto cfg = loadProjectConfig(board->file);
    const auto geo = geom::buildBoardGeometry(*board);
    log::info("stackup: thickness {:.3f} mm, finish '{}', mask '{}'", board->stackup.thickness,
              board->stackup.copperFinish, board->stackup.maskColorFront);
    log::info("stroke-font texts on silkscreen (need kicad-cli): {}", countSilkStrokeTexts(*board));
    if (!resolveModels) return 0;
    kicad::ModelResolver resolver(board->projectDir, cfg.pathVars, cfg.modelSearchDirs);
    int found = 0, missing = 0, noModel = 0;
    for (const auto& fp : board->footprints) {
        if (fp.models.empty()) {
            ++noModel;
            continue;
        }
        for (const auto& m : fp.models) {
            auto r = resolver.resolve(m.path);
            if (r.found) {
                ++found;
                log::info("  {:<6} OK      {}{}", fp.reference, platform::pathToUtf8(r.path),
                          r.substituted ? " (substituted)" : "");
            } else {
                ++missing;
                log::info("  {:<6} MISSING {} [{}]", fp.reference, m.path, r.reason);
            }
        }
    }
    log::info("models: {} found, {} missing, {} footprints without model", found, missing, noModel);
    return 0;
}

int cmdRender(const RenderOptions& o) {
    if (!o.normalPriority) platform::setLowPriority();
    auto boardOpt = loadBoardOrReport(o.pcb);
    if (!boardOpt) return 1;
    const kicad::Board& board = *boardOpt;

    if (o.init) writeDefaultConfigs(board.file);
    ProjectConfig cfg = loadProjectConfig(board.file);
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
        return 1;
    }
    if (cfg.width <= 0 || cfg.height <= 0 || cfg.spp <= 0) {
        log::error("invalid size or spp");
        return 1;
    }

    const fs::path outPng = platform::u8path(o.out);
    const fs::path outDir = outPng.has_parent_path() ? outPng.parent_path() : fs::path(".");
    const std::string stem = platform::pathToUtf8(outPng.stem());
    std::error_code ec;
    fs::create_directories(outDir, ec);
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
    SilkPlot silk;
    const int strokeTexts = countSilkStrokeTexts(board);
    if (strokeTexts > 0) {
        if (!cfg.useKicadCliText || o.noKicadCli) {
            log::warn("{} stroke-font silkscreen text(s) will be missing (kicad-cli disabled)", strokeTexts);
        } else if (auto cli = findKicadCli(cfg.kicadCli)) {
            log::ScopedTimer t("kicad-cli");
            silk = plotSilkWithKicadCli(*cli, board, cacheDir, !o.noCache);
            log::info("silkscreen text via kicad-cli ({} stroke texts){}", strokeTexts,
                      silk.fromCache ? " [cache hit]" : "");
        } else {
            log::warn("kicad-cli not found; {} stroke-font silkscreen text(s) will be missing", strokeTexts);
        }
    }
    for (int s = 0; s < 2; ++s)
        if (silk.side[s]) cfg.scene.layerArt.extraSilk[s] = &*silk.side[s];

    kicad::ModelResolver resolver(board.projectDir, cfg.pathVars, cfg.modelSearchDirs);
    model::ModelLibrary library(cacheDir, model::MeshParams{}, !o.noCache);
    log::info("mesh cache: {}{}", platform::pathToUtf8(cacheDir / "meshes"), o.noCache ? " (disabled)" : "");
    scene::Scene scene = scene::buildScene({board, resolver, library, modelMap, cfg.scene});
    const auto& st = library.stats();
    log::info("models: {} loaded+meshed, {} from cache, {} failed", st.loaded, st.cacheHits, st.failed);

    if (o.debugLayers) {
        for (int s = 0; s < 2; ++s) {
            std::string err;
            const fs::path p = outDir / (stem + (s == 0 ? "_layers_front.png" : "_layers_back.png"));
            if (io::savePngRgba8(p, scene.layers.grid.width, scene.layers.grid.height, scene.layers.rgba[s].data(), err))
                log::info("wrote {}", platform::pathToUtf8(p));
            else
                log::warn("{}", err);
        }
    }

    // 環境マップ
    io::ImageF env;
    {
        std::string err;
        if (!io::loadHdrImage(platform::u8path(cfg.hdr), env, err)) {
            log::error("environment map {}: {}", cfg.hdr, err);
            return 1;
        }
        log::info("environment: {} ({}x{})", cfg.hdr, env.width, env.height);
    }
    const render::EnvDistribution dist = render::buildEnvDistribution(env);

    scene::CameraParams cp;
    cp.elevationDeg = cfg.elevationDeg;
    cp.azimuthDeg = cfg.azimuthDeg;
    cp.fovDeg = cfg.fovDeg;
    cp.margin = cfg.margin;
    cp.width = cfg.width;
    cp.height = cfg.height;
    const scene::Camera cam = scene::frameCamera(cp, scene.framingPoints);
    log::info("camera: elevation {:.1f} deg, azimuth {:.1f} deg, fov {:.1f} deg, position ({:.1f}, {:.1f}, {:.1f}) mm",
              cp.elevationDeg, cp.azimuthDeg, cp.fovDeg, cam.position.x, cam.position.y, cam.position.z);

    render::RenderOutput result;
    try {
        vk::ContextOptions vo;
        vo.validation = o.validate;
        vk::Context ctx;
        ctx.init(vo);
        render::GpuScene gpu;
        gpu.build(ctx, scene, env, dist);
        env.rgba.clear();
        env.rgba.shrink_to_fit();
        render::RenderSettings rs;
        rs.width = static_cast<uint32_t>(cfg.width);
        rs.height = static_cast<uint32_t>(cfg.height);
        rs.spp = static_cast<uint32_t>(cfg.spp);
        rs.integrator = cfg.integrator;
        rs.frame.camera = cam;
        rs.frame.envIntensity = cfg.envIntensity;
        rs.frame.envRotationRad = glm::radians(cfg.envRotationDeg);
        rs.frame.radianceClamp = cfg.radianceClamp;
        rs.frame.maxDepth = static_cast<uint32_t>(std::max(1, cfg.maxDepth));
        rs.frame.seed = o.seed;
        result = render::renderOffscreen(ctx, gpu, rs);
        gpu.destroy();
    } catch (const std::exception& e) {
        log::error("render failed: {}", e.what());
        return 1;
    }

    // 出力
    int failures = 0;
    auto report = [&](bool ok, const fs::path& p, const std::string& err) {
        if (ok) {
            log::info("wrote {}", platform::pathToUtf8(p));
        } else {
            log::error("{}: {}", platform::pathToUtf8(p), err);
            ++failures;
        }
    };
    {
        std::string err;
        const fs::path png = outDir / (stem + ".png");
        report(io::savePngTonemapped(png, result.color, {cfg.exposureEv}, err), png, err);
        const fs::path exr = outDir / (stem + ".exr");
        report(io::saveExr(exr, result.color, err), exr, err);
        const fs::path alb = outDir / (stem + "_albedo.exr");
        report(io::saveExr(alb, result.albedo, err), alb, err);
        const fs::path nrm = outDir / (stem + "_normal.exr");
        report(io::saveExr(nrm, result.normal, err), nrm, err);
    }
    writeMissingReport(scene.missing, board.file, outDir);
    return failures ? 1 : 0;
}

}  // namespace krbr::app
