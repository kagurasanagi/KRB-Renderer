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

struct ViewOptions {
    RenderOptions common;      // 基板・HDR・カメラなど（spp は「ここまで積んだら止める」上限）
    int testFrames = 0;        // >0: このフレーム数を描いたら screenshot を保存して終了（開発用）
    std::string screenshot;    // testFrames 用の保存先（表示中の画像そのもの）
    bool hidden = false;       // ウィンドウを表示しない（testFrames 指定時は自動）
    double testOrbitDeg = 0;   // 開発用：毎フレームこの角度だけカメラを回す（カメラ変更の経路の確認）
};

int cmdProbe();
int cmdView(const ViewOptions& o);
int cmdInfo(const std::string& pcb, bool resolveModels);
int cmdRender(const RenderOptions& o);

}  // namespace krbr::app
