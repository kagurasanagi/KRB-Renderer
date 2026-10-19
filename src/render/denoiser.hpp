#pragma once
// ノイズ除去（Intel Open Image Denoise）。補助情報としてアルベドと法線を渡す。
// GPU（CUDA）デバイスが使えれば、Vulkan のバッファを外部メモリとして OIDN と共有する（CPU を経由しない）。
// 使えなければ CPU デバイスで、CPU から見えるバッファを使う。
#include <memory>
#include <string>

#include "render/integrator.hpp"

namespace krbr::render {

enum class DenoiseQuality { Fast, Balanced, High };

class Denoiser {
public:
    Denoiser();
    ~Denoiser();
    static bool compiledIn();

    // targets の大きさで作る（大きさが変わったら作り直すこと）
    bool init(vk::Context& ctx, const RenderTargets& targets, DenoiseQuality quality);
    bool ready() const;
    // 品質を変える（変わるときだけフィルタを作り直す）
    void setQuality(DenoiseQuality quality);
    const std::string& deviceName() const;

    // 累積画像から入力を作るコマンドを記録する（実行後に execute を呼ぶ）
    void recordPrepare(VkCommandBuffer cmd);
    // ノイズ除去を実行する（終わるまで待つ）。失敗したら false
    bool execute(double* ms = nullptr);
    // 結果を output() へ写すコマンドを記録する
    void recordOutput(VkCommandBuffer cmd);
    // 結果（RGBA32F、A = 1）。Tonemapper の色の入力にそのまま使える
    const vk::Image& output() const;
    void destroy();

private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
};

}  // namespace krbr::render
