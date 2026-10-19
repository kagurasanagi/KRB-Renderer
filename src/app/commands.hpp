#pragma once
#include <optional>
#include <string>

namespace krbr::app {

struct RenderOptions {
    std::string pcb;
    std::string hdr;
    std::string out = "out/board.png";
    std::optional<int> spp, width, height, maxDepth;
    std::optional<double> elevation, azimuth, fov;
    std::optional<float> exposure, envIntensity, envRotation, radianceClamp;
    std::string integrator;
    unsigned seed = 0;
    bool init = false;
    bool noCache = false;
    bool noKicadCli = false;
    bool validate = false;
    bool normalPriority = false;
    bool debugLayers = false;
};

int cmdProbe();
int cmdInfo(const std::string& pcb, bool resolveModels);
int cmdRender(const RenderOptions& o);

}  // namespace krbr::app
