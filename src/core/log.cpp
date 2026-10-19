#include "core/log.hpp"

#include <chrono>
#include <cstdio>
#include <mutex>

namespace krbr::log {
namespace {
Level g_level = Level::Info;
std::mutex g_mutex;
const auto g_start = std::chrono::steady_clock::now();

long long nowNs() {
    return std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now().time_since_epoch())
        .count();
}
}  // namespace

void setLevel(Level l) { g_level = l; }
Level level() { return g_level; }

void write(Level l, std::string_view msg) {
    static const char* tags[] = {"debug", "info", "warn", "error"};
    const double t = std::chrono::duration<double>(std::chrono::steady_clock::now() - g_start).count();
    std::lock_guard lock(g_mutex);
    std::fprintf(stderr, "[%8.3f] %-5s %.*s\n", t, tags[static_cast<int>(l)], static_cast<int>(msg.size()),
                 msg.data());
    std::fflush(stderr);
}

ScopedTimer::ScopedTimer(std::string label) : label_(std::move(label)), start_(nowNs()) {}
ScopedTimer::~ScopedTimer() { debug("{}: {:.3f} s", label_, elapsedSec()); }
double ScopedTimer::elapsedSec() const { return (nowNs() - start_) * 1e-9; }

}  // namespace krbr::log
