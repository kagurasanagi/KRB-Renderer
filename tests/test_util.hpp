#pragma once
#include <filesystem>
#include <string>

#ifdef _WIN32
#include <windows.h>
#else
#include <cstdlib>
#endif

#include "core/platform.hpp"

namespace krbr::test {

namespace fs = std::filesystem;

inline void setEnv(const char* name, const std::string& value) {
#ifdef _WIN32
    SetEnvironmentVariableW(platform::u8path(name).wstring().c_str(),
                            value.empty() ? nullptr : platform::u8path(value).wstring().c_str());
#else
    if (value.empty()) unsetenv(name); else setenv(name, value.c_str(), 1);
#endif
}

// テスト用の一時フォルダ（終わったら消す）。全体設定の場所もここへ向ける
struct TempWorkspace {
    fs::path dir;
    explicit TempWorkspace(const std::string& name) {
        dir = platform::tempDir() / ("test_" + name);
        std::error_code ec;
        fs::remove_all(dir, ec);
        fs::create_directories(dir / "config", ec);
        setEnv("KRBR_CONFIG_DIR", platform::pathToUtf8(dir / "config"));
    }
    ~TempWorkspace() {
        setEnv("KRBR_CONFIG_DIR", "");
        std::error_code ec;
        fs::remove_all(dir, ec);
    }
    fs::path configDir() const { return dir / "config"; }
};

inline fs::path sourceDir() { return platform::u8path(KRBR_SOURCE_DIR); }
// テストで描く基板と環境マップ（CMake の KRBR_TEST_PCB / KRBR_TEST_HDR）
inline fs::path samplePcb() { return platform::u8path(KRBR_TEST_PCB); }
inline fs::path sampleHdr() { return platform::u8path(KRBR_TEST_HDR); }

}  // namespace krbr::test
