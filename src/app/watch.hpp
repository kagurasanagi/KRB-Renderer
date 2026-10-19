#pragma once
// ヘッドレスの保存監視（krbr watch）。ウィンドウは開かず、保存のたびに描き直して PNG を書き出す
#include <chrono>
#include <memory>
#include <optional>

#include "app/commands.hpp"
#include "app/prepare.hpp"

namespace krbr::app {

struct WatchOptions {
    RenderOptions common;  // spp は 1 回の更新で積むサンプル数（既定 32）
    int maxUpdates = 0;    // 0 なら止めるまで続ける
};

struct WatchUpdate {
    double sinceSaveMs = 0;  // ファイルの更新時刻から、画像を書き終えるまで
    double reloadMs = 0, gpuMs = 0, renderMs = 0, writeMs = 0;
    ReloadStats stats;
    int pendingModels = 0;
};

class WatchRunner {
public:
    WatchRunner();
    ~WatchRunner();
    bool open(const WatchOptions& o);
    // 保存（または裏で分割していた 3D モデルの完了）を待って描き直す。timeout までに何も無ければ nullopt
    std::optional<WatchUpdate> waitAndUpdate(std::chrono::milliseconds timeout);
    const std::filesystem::path& output() const { return outPng_; }

private:
    struct Impl;
    std::unique_ptr<Impl> impl_;
    std::filesystem::path outPng_;
};

int cmdWatch(const WatchOptions& o);

}  // namespace krbr::app
