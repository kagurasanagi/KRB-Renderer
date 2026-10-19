#pragma once
#include <format>
#include <string>
#include <string_view>

namespace krbr::log {

enum class Level { Debug = 0, Info, Warn, Error };

void setLevel(Level level);
Level level();
void write(Level level, std::string_view msg);

template <class... Args>
void debug(std::format_string<Args...> fmt, Args&&... args) {
    if (level() <= Level::Debug) write(Level::Debug, std::format(fmt, std::forward<Args>(args)...));
}
template <class... Args>
void info(std::format_string<Args...> fmt, Args&&... args) {
    if (level() <= Level::Info) write(Level::Info, std::format(fmt, std::forward<Args>(args)...));
}
template <class... Args>
void warn(std::format_string<Args...> fmt, Args&&... args) {
    write(Level::Warn, std::format(fmt, std::forward<Args>(args)...));
}
template <class... Args>
void error(std::format_string<Args...> fmt, Args&&... args) {
    write(Level::Error, std::format(fmt, std::forward<Args>(args)...));
}

// 処理時間を計測してスコープ終了時にログへ出す
class ScopedTimer {
public:
    explicit ScopedTimer(std::string label);
    ~ScopedTimer();
    double elapsedSec() const;

private:
    std::string label_;
    long long start_;
};

}  // namespace krbr::log
