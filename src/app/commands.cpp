#include "app/commands.hpp"

#include "app/prepare.hpp"
#include "app/report.hpp"
#include "core/log.hpp"
#include "core/platform.hpp"
#include "geom/board_mesh.hpp"
#include "kicad/model_resolver.hpp"
#include "render/gpu_scene.hpp"
#include "render/renderer.hpp"

namespace krbr::app {

namespace fs = std::filesystem;

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
    int strokeTexts = 0;
    for (const auto& t : board->strokeTexts)
        if (t.layer == "F.SilkS" || t.layer == "B.SilkS") ++strokeTexts;
    log::info("stroke-font texts on silkscreen (need kicad-cli): {}", strokeTexts);
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
    const fs::path outPng = platform::u8path(o.out);
    const fs::path outDir = outPng.has_parent_path() ? outPng.parent_path() : fs::path(".");
    const std::string stem = platform::pathToUtf8(outPng.stem());
    std::error_code ec;
    fs::create_directories(outDir, ec);

    auto prep = prepareScene(o, o.debugLayers ? outDir : fs::path{}, stem);
    if (!prep) return 1;
    const ProjectConfig& cfg = prep->cfg;

    const scene::Camera cam = initialCamera(*prep, o, cfg.width, cfg.height);
    log::info("camera: elevation {:.1f} deg, azimuth {:.1f} deg, fov {:.1f} deg, position ({:.1f}, {:.1f}, {:.1f}) mm",
              cfg.elevationDeg, cfg.azimuthDeg, cfg.fovDeg, cam.position.x, cam.position.y, cam.position.z);

    render::RenderOutput result;
    try {
        vk::ContextOptions vo;
        vo.validation = o.validate;
        vk::Context ctx;
        ctx.init(vo);
        render::GpuScene gpu;
        gpu.build(ctx, prep->scene, prep->env, prep->dist);
        prep->env.rgba.clear();
        prep->env.rgba.shrink_to_fit();
        render::RenderSettings rs;
        rs.width = static_cast<uint32_t>(cfg.width);
        rs.height = static_cast<uint32_t>(cfg.height);
        rs.spp = static_cast<uint32_t>(cfg.spp);
        rs.integrator = cfg.integrator;
        rs.frame = frameParams(cfg, cam, o.seed);
        result = render::renderOffscreen(ctx, gpu, rs);
        gpu.destroy();
    } catch (const std::exception& e) {
        log::error("render failed: {}", e.what());
        return 1;
    }

    int failures = 0;
    auto report = [&](bool ok, const fs::path& p, const std::string& err) {
        if (ok) {
            log::info("wrote {}", platform::pathToUtf8(p));
        } else {
            log::error("{}: {}", platform::pathToUtf8(p), err);
            ++failures;
        }
    };
    std::string err;
    const fs::path png = outDir / (stem + ".png");
    report(io::savePngTonemapped(png, result.color, {cfg.exposureEv}, err), png, err);
    const fs::path exr = outDir / (stem + ".exr");
    report(io::saveExr(exr, result.color, err), exr, err);
    const fs::path alb = outDir / (stem + "_albedo.exr");
    report(io::saveExr(alb, result.albedo, err), alb, err);
    const fs::path nrm = outDir / (stem + "_normal.exr");
    report(io::saveExr(nrm, result.normal, err), nrm, err);
    writeMissingReport(prep->scene.missing, prep->board.file, outDir);
    return failures ? 1 : 0;
}

}  // namespace krbr::app
