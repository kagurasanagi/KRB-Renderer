// krbr: KiCad の基板を物理ベースで描画するツール（既定はリアルタイム表示、render でファイル出力）
#include <CLI/CLI.hpp>

#include <string>
#include <vector>

#include "app/commands.hpp"
#include "app/material_cmd.hpp"
#include "app/watch.hpp"
#include "core/log.hpp"

namespace {

// render / view 共通のオプション
void addCommonOptions(CLI::App* c, krbr::app::RenderOptions& ro) {
    c->add_option("pcb", ro.pcb, ".kicad_pcb file")->required()->check(CLI::ExistingFile);
    c->add_option("--hdr", ro.hdr, "equirectangular environment map (.hdr / .exr)");
    c->add_option("--width", ro.width, "image / window width");
    c->add_option("--height", ro.height, "image / window height");
    c->add_option("--max-depth", ro.maxDepth, "maximum path depth");
    c->add_option("--elevation", ro.elevation, "camera elevation above the board (deg, default 35)");
    c->add_option("--azimuth", ro.azimuth, "camera azimuth (deg, default 45)");
    c->add_option("--fov", ro.fov, "vertical field of view (deg, default 30)");
    c->add_option("--look-at", ro.lookAt, "look at this board point (KiCad X Y in mm)")->expected(2)->delimiter(',');
    c->add_option("--zoom", ro.zoom, "zoom factor relative to the automatic framing");
    c->add_option("--exposure", ro.exposure, "exposure (EV)");
    c->add_option("--env-intensity", ro.envIntensity, "environment intensity multiplier");
    c->add_option("--env-rotation", ro.envRotation, "environment rotation around the vertical axis (deg)");
    c->add_option("--clamp", ro.radianceClamp, "clamp indirect radiance per sample to suppress fireflies (default 10, 0 = off)");
    c->add_option("--integrator", ro.integrator, "integrator (pt)");
    c->add_option("--seed", ro.seed, "random seed");
    c->add_flag("--init", ro.init, "write default config files (<project>.krbr/*.toml) if missing");
    c->add_flag("--no-cache", ro.noCache, "do not read or write the mesh cache");
    c->add_flag("--no-kicad-cli", ro.noKicadCli, "do not call kicad-cli for stroke-font text");
    c->add_flag("--validate", ro.validate, "enable Vulkan validation layer");
    c->add_flag("--normal-priority", ro.normalPriority, "do not lower process priority");
}

}  // namespace

int main(int argc, char** argv) {
    using namespace krbr;

    // サブコマンドを省略して `krbr board.kicad_pcb ...` と書いたら view とみなす
    std::vector<std::string> args(argv, argv + argc);
    for (size_t i = 1; i < args.size(); ++i) {
        if (!args[i].empty() && args[i][0] == '-') continue;
        if (args[i] != "probe" && args[i] != "info" && args[i] != "render" && args[i] != "view" && args[i] != "material" && args[i] != "watch")
            args.insert(args.begin() + static_cast<std::ptrdiff_t>(i), "view");
        break;
    }
    std::vector<char*> cargs;
    for (auto& a : args) cargs.push_back(a.data());

    CLI::App app{"krbr - physically based renderer for KiCad boards (Vulkan ray tracing)\n"
                 "  krbr <pcb> --hdr <env>          real-time viewer (window)\n"
                 "  krbr render <pcb> --hdr <env>   offscreen render to PNG / EXR"};
    app.require_subcommand(1);
    bool verbose = false;
    app.add_flag("-v,--verbose", verbose, "debug log");

    auto* probe = app.add_subcommand("probe", "list Vulkan devices and check ray tracing support");

    std::string infoPcb;
    bool infoModels = false;
    auto* info = app.add_subcommand("info", "parse a .kicad_pcb and print a summary");
    info->add_option("pcb", infoPcb, ".kicad_pcb file")->required()->check(CLI::ExistingFile);
    info->add_flag("--models", infoModels, "resolve 3D model paths");

    app::RenderOptions ro;
    auto* render = app.add_subcommand("render", "render offscreen to PNG (tone mapped) and EXR (linear)");
    addCommonOptions(render, ro);
    render->add_option("--out,-o", ro.out, "output PNG path (EXR and AOVs are written next to it)")
        ->capture_default_str();
    render->add_option("--spp", ro.spp, "samples per pixel (default 64)");
    render->add_flag("--debug-layers", ro.debugLayers, "also write the rasterized layer textures as PNG");

    app::ViewOptions vo;
    vo.common.out = "out/view.png";
    auto* view = app.add_subcommand("view", "real-time viewer window (default command)");
    addCommonOptions(view, vo.common);
    view->add_option("--spp", vo.common.spp, "stop accumulating at this many samples per pixel (default 1024)");
    view->add_option("--out,-o", vo.common.out, "base path for images saved with the S key")->capture_default_str();
    view->add_option("--test-frames", vo.testFrames, "development: render N frames in a hidden window, then exit");
    view->add_option("--screenshot", vo.screenshot, "development: save the displayed image (with --test-frames)");
    view->add_flag("--hidden", vo.hidden, "do not show the window");
    view->add_option("--test-orbit", vo.testOrbitDeg, "development: rotate the camera by this many degrees per frame");
    view->add_option("--test-env", vo.testEnv, "development: switch to this environment map during the test");
    bool noWatch = false;
    view->add_flag("--no-watch", noWatch, "do not reload when the board file is saved");
    view->add_flag("--test-reload", vo.testReload, "development: reload the board once during the test");

    app::WatchOptions wo;
    wo.common.out = "out/watch.png";
    auto* watch = app.add_subcommand("watch", "headless: re-render whenever the board is saved (no window)");
    addCommonOptions(watch, wo.common);
    watch->add_option("--out,-o", wo.common.out, "output PNG (rewritten on every save)")->capture_default_str();
    watch->add_option("--spp", wo.common.spp, "samples per pixel per update (default 32)");
    watch->add_option("--max-updates", wo.maxUpdates, "stop after this many updates (0 = run until stopped)");

    // 材質の対応表
    auto* material = app.add_subcommand("material", "material assignment (OpenPBR materials, per model / per reference)");
    material->require_subcommand(1);
    app::MaterialSetOptions mso;
    auto* mset = material->add_subcommand("set", "assign a material to a reference (project) or a 3D model (project / --global)");
    mset->add_option("pcb", mso.pcb, ".kicad_pcb file")->required()->check(CLI::ExistingFile);
    mset->add_option("--ref", mso.ref, "reference designator, e.g. U3 (written to <project>.krbr/overrides.toml)");
    mset->add_option("--model", mso.model, "3D model: path as written in the board, file name, or file path");
    mset->add_option("--part", mso.part, "part key (XDE name, kicad:<color>, #rrggbb); default: all parts");
    mset->add_option("--material", mso.material, "material name (see: krbr material list)")->required();
    mset->add_flag("--global", mso.global, "with --model: write to the global model_map.toml (shared by all projects)");
    std::string showPcb, showRef;
    auto* mshow = material->add_subcommand("show", "show the resolved material of each part and where it came from");
    mshow->add_option("pcb", showPcb, ".kicad_pcb file")->required()->check(CLI::ExistingFile);
    mshow->add_option("--ref", showRef, "only this reference");
    auto* mlist = material->add_subcommand("list", "list materials in the material library");

    try {
        app.parse(static_cast<int>(cargs.size()), cargs.data());
    } catch (const CLI::ParseError& e) {
        return app.exit(e);
    }
    if (verbose) log::setLevel(log::Level::Debug);

    if (*probe) return app::cmdProbe();
    if (*info) return app::cmdInfo(infoPcb, infoModels);
    if (*render) return app::cmdRender(ro);
    if (*view) {
        vo.watch = !noWatch;
        return app::cmdView(vo);
    }
    if (*watch) return app::cmdWatch(wo);
    if (*mset) return app::cmdMaterialSet(mso);
    if (*mshow) return app::cmdMaterialShow(showPcb, showRef);
    if (*mlist) return app::cmdMaterialList();
    return 1;
}
