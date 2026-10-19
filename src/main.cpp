// krbr: KiCad の基板を物理ベースでオフスクリーン描画するコマンドラインツール
#include <CLI/CLI.hpp>

#include "app/commands.hpp"
#include "core/log.hpp"

int main(int argc, char** argv) {
    using namespace krbr;
    CLI::App app{"krbr - physically based renderer for KiCad boards (offscreen, Vulkan ray tracing)"};
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
    auto* render = app.add_subcommand("render", "render a board to PNG (tone mapped) and EXR (linear)");
    render->add_option("pcb", ro.pcb, ".kicad_pcb file")->required()->check(CLI::ExistingFile);
    render->add_option("--hdr", ro.hdr, "equirectangular environment map (.hdr / .exr)");
    render->add_option("--out,-o", ro.out, "output PNG path (EXR and AOVs are written next to it)")
        ->capture_default_str();
    render->add_option("--spp", ro.spp, "samples per pixel (default 64)");
    render->add_option("--width", ro.width, "image width");
    render->add_option("--height", ro.height, "image height");
    render->add_option("--max-depth", ro.maxDepth, "maximum path depth");
    render->add_option("--elevation", ro.elevation, "camera elevation above the board (deg, default 35)");
    render->add_option("--azimuth", ro.azimuth, "camera azimuth (deg, default 45)");
    render->add_option("--fov", ro.fov, "vertical field of view (deg, default 30)");
    render->add_option("--exposure", ro.exposure, "exposure for the PNG (EV)");
    render->add_option("--env-intensity", ro.envIntensity, "environment intensity multiplier");
    render->add_option("--env-rotation", ro.envRotation, "environment rotation around the vertical axis (deg)");
    render->add_option("--clamp", ro.radianceClamp, "clamp per-sample radiance (0 = off)");
    render->add_option("--integrator", ro.integrator, "integrator (pt)");
    render->add_option("--seed", ro.seed, "random seed");
    render->add_flag("--init", ro.init, "write default config files (<project>.krbr/*.toml) if missing");
    render->add_flag("--no-cache", ro.noCache, "do not read or write the mesh cache");
    render->add_flag("--no-kicad-cli", ro.noKicadCli, "do not call kicad-cli for stroke-font text");
    render->add_flag("--validate", ro.validate, "enable Vulkan validation layer");
    render->add_flag("--normal-priority", ro.normalPriority, "do not lower process priority");
    render->add_flag("--debug-layers", ro.debugLayers, "also write the rasterized layer textures as PNG");

    CLI11_PARSE(app, argc, argv);
    if (verbose) log::setLevel(log::Level::Debug);

    if (*probe) return app::cmdProbe();
    if (*info) return app::cmdInfo(infoPcb, infoModels);
    if (*render) return app::cmdRender(ro);
    return 1;
}
