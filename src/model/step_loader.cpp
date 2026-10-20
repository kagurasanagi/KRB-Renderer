// STEP 読み込み：変換器 DLL（step\krbr_step.dll、中で OpenCASCADE を使う）を必要になったときだけ読み込んで呼ぶ。
// 本体は OpenCASCADE に依存しない。DLL が無ければ STEP は「読めない」扱い（代替の箱で表示）
#include <algorithm>
#include <atomic>
#include <chrono>
#include <format>
#include <mutex>

#ifdef _WIN32
#include <windows.h>
#endif

#include "core/log.hpp"
#include "core/platform.hpp"
#include "model/mesh_file.hpp"
#include "model/model_mesh.hpp"
#include "step_dll/krbr_step.h"

namespace krbr::model {

namespace {

std::string lowerExt(const std::filesystem::path& p) {
    std::string e = platform::pathToUtf8(p.extension());
    std::transform(e.begin(), e.end(), e.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return e;
}

using ToMeshFileFn = int (*)(const char*, double, double, const char*, char*, size_t);
using VersionFn = int (*)();

// DLL は 1 回だけ読み込む（見つからなければ以後は呼ばない）
struct StepDll {
    ToMeshFileFn toMeshFile = nullptr;
    std::string error;

    static const StepDll& get() {
        static StepDll dll;
        static std::once_flag once;
        std::call_once(once, [] { dll.load(); });
        return dll;
    }

private:
    void load() {
#ifdef _WIN32
        const std::filesystem::path path = platform::exeDir() / "step" / "krbr_step.dll";
        if (!std::filesystem::exists(path)) {
            error = "not found: " + platform::pathToUtf8(path);
            return;
        }
        // DLL と同じフォルダから OpenCASCADE の DLL を探す
        HMODULE h = LoadLibraryExW(path.wstring().c_str(), nullptr,
                                   LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS);
        if (!h) {
            error = std::format("LoadLibrary failed ({}) for {}", GetLastError(), platform::pathToUtf8(path));
            return;
        }
        auto apiVersion = reinterpret_cast<VersionFn>(GetProcAddress(h, "krbr_step_api_version"));
        auto fileVersion = reinterpret_cast<VersionFn>(GetProcAddress(h, "krbr_step_mesh_file_version"));
        auto fn = reinterpret_cast<ToMeshFileFn>(GetProcAddress(h, "krbr_step_to_mesh_file"));
        if (!apiVersion || !fileVersion || !fn) {
            error = "krbr_step.dll does not export the expected functions";
            return;
        }
        if (apiVersion() != KRBR_STEP_API_VERSION || fileVersion() != static_cast<int>(kMeshFileVersion)) {
            error = std::format("krbr_step.dll version mismatch (api {} / mesh file v{}; expected {} / v{})", apiVersion(),
                                fileVersion(), KRBR_STEP_API_VERSION, kMeshFileVersion);
            return;
        }
        toMeshFile = fn;
        log::debug("STEP converter: {}", platform::pathToUtf8(path));
#else
        error = "STEP converter DLL is only built on Windows";
#endif
    }
};

class StepLoader final : public IModelLoader {
public:
    const char* name() const override { return "occt-step"; }
    bool canLoad(const std::filesystem::path& p) const override {
        const std::string e = lowerExt(p);
        return e == ".step" || e == ".stp";
    }
    LoadResult load(const std::filesystem::path& p, const MeshParams& params) const override;
};

LoadResult StepLoader::load(const std::filesystem::path& p, const MeshParams& params) const {
    LoadResult res;
    const auto t0 = std::chrono::steady_clock::now();
    const StepDll& dll = StepDll::get();
    if (!dll.toMeshFile) {
        res.error = "STEP converter unavailable: " + dll.error;
        return res;
    }
    // 変換器は .krbm をファイルに書くので、一時ファイル経由で受け取る
    std::error_code ec;
    std::filesystem::path tmp = platform::tempPath("step");
    tmp += ".krbm";
    char err[512] = {};
    const int rc = dll.toMeshFile(platform::pathToUtf8(p).c_str(), params.linearDeflection, params.angularDeflection,
                                  platform::pathToUtf8(tmp).c_str(), err, sizeof err);
    if (rc != 0) {
        res.error = err[0] ? err : std::format("converter returned {}", rc);
    } else {
        res.mesh = readMeshFile(tmp);
        if (!res.mesh) res.error = "cannot read converter output";
    }
    std::filesystem::remove(tmp, ec);
    res.seconds = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
    return res;
}

}  // namespace

std::unique_ptr<IModelLoader> createStepLoader() { return std::make_unique<StepLoader>(); }

bool stepSupportAvailable(std::string* why) {
    const StepDll& dll = StepDll::get();
    if (why) *why = dll.error;
    return dll.toMeshFile != nullptr;
}

}  // namespace krbr::model
