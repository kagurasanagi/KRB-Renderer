#pragma once
// ファイルの保存を見張る（更新日時とサイズを一定間隔で調べる）。
// 一時ファイルへの書き込み・置き換えの途中で読まないよう、変化が止まってから settle だけ待って知らせる。
#include <chrono>
#include <filesystem>
#include <optional>

namespace krbr {

class FileWatcher {
public:
    using Clock = std::chrono::steady_clock;

    explicit FileWatcher(std::filesystem::path file, std::chrono::milliseconds interval = std::chrono::milliseconds(100),
                         std::chrono::milliseconds settle = std::chrono::milliseconds(200));

    // こまめに呼ぶ。保存が落ち着いたら 1 回だけ true を返す
    bool poll();

    // 最後に知らせた保存の、ファイルの更新時刻（壁時計）。保存からの経過時間の計測に使う
    std::chrono::system_clock::time_point savedAt() const { return savedAt_; }

private:
    struct Stamp {
        std::filesystem::file_time_type time{};
        uintmax_t size = 0;
        bool exists = false;
        bool operator==(const Stamp& o) const { return time == o.time && size == o.size && exists == o.exists; }
    };
    Stamp stamp() const;

    std::filesystem::path file_;
    std::chrono::milliseconds interval_, settle_;
    Clock::time_point lastCheck_{};
    Stamp committed_;
    std::optional<Stamp> candidate_;
    Clock::time_point candidateSince_{};
    std::chrono::system_clock::time_point savedAt_{};
};

}  // namespace krbr
