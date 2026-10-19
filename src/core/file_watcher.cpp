#include "core/file_watcher.hpp"

namespace krbr {

namespace fs = std::filesystem;

FileWatcher::FileWatcher(fs::path file, std::chrono::milliseconds interval, std::chrono::milliseconds settle)
    : file_(std::move(file)), interval_(interval), settle_(settle) {
    committed_ = stamp();
}

FileWatcher::Stamp FileWatcher::stamp() const {
    Stamp s;
    std::error_code ec;
    s.time = fs::last_write_time(file_, ec);
    if (ec) return s;
    s.size = fs::file_size(file_, ec);
    s.exists = !ec;
    return s;
}

bool FileWatcher::poll() {
    const auto now = Clock::now();
    if (now - lastCheck_ < interval_) return false;
    lastCheck_ = now;
    const Stamp s = stamp();
    if (!s.exists) return false;  // 置き換えの途中（いったん消える）
    if (s == committed_) {
        candidate_.reset();
        return false;
    }
    if (!candidate_ || !(*candidate_ == s)) {
        candidate_ = s;  // まだ書き込み中かもしれないので、変化が止まるのを待つ
        candidateSince_ = now;
        return false;
    }
    if (now - candidateSince_ < settle_) return false;
    committed_ = s;
    candidate_.reset();
    savedAt_ = std::chrono::clock_cast<std::chrono::system_clock>(s.time);
    return true;
}

}  // namespace krbr
