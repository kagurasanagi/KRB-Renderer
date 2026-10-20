#pragma once
#include <optional>
#include <string>
#include <vector>

namespace krbr::app {

struct RenderOptions {
    std::string pcb;
    std::string hdr;
    std::string out = "out/board.png";
    std::optional<int> spp, width, height, maxDepth;
    std::optional<double> elevation, azimuth, fov;
    std::optional<float> exposure, envIntensity, envRotation, radianceClamp;
    std::vector<double> lookAt;  // 注視点（KiCad の基板座標 X, Y、mm）
    double zoom = 1.0;           // 自動で決めた距離に対する拡大率
    std::string integrator;
    unsigned seed = 0;
    bool init = false;
    bool noCache = false;
    bool noKicadCli = false;
    bool validate = false;
    bool normalPriority = false;
    bool debugLayers = false;
    std::optional<bool> denoise;  // ノイズ除去（watch は既定で有効、view / render は既定で無効）
    std::optional<bool> spectral;  // 分光モード（既定は設定ファイル、無ければ RGB）
    std::string lens;              // pinhole / thin / realistic（空なら設定ファイル）
    std::string maskColor;         // レジストの色：プリセット名か r,g,b（空なら設定ファイル／stackup）
    std::optional<float> fNumber, focalLength, focusDistance;
    std::optional<int> blades;
};

struct ViewOptions {
    RenderOptions common;      // 基板・HDR・カメラなど（spp は「ここまで積んだら止める」上限）
    int testFrames = 0;        // >0: このフレーム数を描いたら screenshot を保存して終了（開発用）
    std::string screenshot;    // testFrames 用の保存先（表示中の画像そのもの）
    bool hidden = false;       // ウィンドウを表示しない（testFrames 指定時は自動）
    double testOrbitDeg = 0;   // 開発用：毎フレームこの角度だけカメラを回す（カメラ変更の経路の確認）
    std::string testEnv;       // 開発用：途中でこの環境マップに切り替える（差し替えの経路の確認）
    bool watch = true;         // .kicad_pcb の保存を見張って自動で読み直す
    bool testReload = false;   // 開発用：途中で保存されたものとして読み直す（作り直しの経路の確認）
};

int cmdProbe();
int cmdMaskList();
int cmdView(const ViewOptions& o);
int cmdInfo(const std::string& pcb, bool resolveModels);
int cmdRender(const RenderOptions& o);

}  // namespace krbr::app
