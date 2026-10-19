#include "model/mesh_cache.hpp"

#include <format>

#include "model/mesh_file.hpp"

#include <cstring>
#include <fstream>

#include "core/hash.hpp"
#include "core/log.hpp"
#include "core/platform.hpp"

namespace krbr::model {

ModelLibrary::ModelLibrary(std::filesystem::path cacheDir, MeshParams params, bool useCache)
    : cacheDir_(std::move(cacheDir)), params_(params), useCache_(useCache) {
    loaders_.push_back(createStepLoader());
}

ModelLibrary::~ModelLibrary() {
    {
        std::lock_guard lock(mutex_);
        stop_ = true;
    }
    cv_.notify_all();
    if (worker_.joinable()) worker_.join();
}

const IModelLoader* ModelLibrary::loaderFor(const std::filesystem::path& p) const {
    for (const auto& l : loaders_)
        if (l->canLoad(p)) return l.get();
    return nullptr;
}

// メモかディスクのキャッシュにあれば返す。無ければ cacheFile / fileHash を埋めて nullopt
std::optional<LoadResult> ModelLibrary::lookup(const std::filesystem::path& p, const IModelLoader* loader,
                                               std::filesystem::path& cacheFile, std::string& fileHash) {
    {
        std::lock_guard lock(mutex_);
        if (auto it = memo_.find(p); it != memo_.end()) return it->second;
    }
    auto bytes = platform::readBinaryFile(p);
    if (bytes) fileHash = toHex(xxh64(bytes->data(), bytes->size()));
    if (!useCache_ || fileHash.empty()) return std::nullopt;
    // キーはファイル内容のハッシュ＋分割パラメータ＋読み込み器（パスが変わっても同じ内容なら再利用）
    const std::string paramKey = std::format("{}|{:.6f}|{:.6f}|v{}", loader->name(), params_.linearDeflection,
                                             params_.angularDeflection, kMeshFileVersion);
    const uint64_t ph = xxh64(paramKey.data(), paramKey.size());
    cacheFile = cacheDir_ / "meshes" / (fileHash + "-" + toHex(ph).substr(0, 8) + ".krbm");
    auto m = readMeshFile(cacheFile);
    if (!m) return std::nullopt;
    LoadResult res;
    res.mesh = m;
    res.fileHash = fileHash;
    res.fromCache = true;
    log::info("model cache hit: {} -> {}", platform::pathToUtf8(p.filename()), platform::pathToUtf8(cacheFile.filename()));
    std::lock_guard lock(mutex_);
    ++stats_.cacheHits;
    memo_[p] = res;
    return res;
}

LoadResult ModelLibrary::loadNow(const std::filesystem::path& p, const IModelLoader* loader,
                                 const std::filesystem::path& cacheFile, const std::string& fileHash) {
    LoadResult res = loader->load(p, params_);
    res.fileHash = fileHash;
    if (res.mesh) {
        log::info("model loaded: {} ({} tris, {:.2f} s){}", platform::pathToUtf8(p.filename()),
                  res.mesh->mesh.triangleCount(), res.seconds, cacheFile.empty() ? "" : ", cached");
        if (!cacheFile.empty() && !writeMeshFile(cacheFile, *res.mesh))
            log::warn("cannot write mesh cache {}", platform::pathToUtf8(cacheFile));
    } else {
        log::warn("model load failed: {}: {}", platform::pathToUtf8(p.filename()), res.error);
    }
    std::lock_guard lock(mutex_);
    if (res.mesh) ++stats_.loaded;
    else ++stats_.failed;
    memo_[p] = res;
    return res;
}

LoadResult ModelLibrary::get(const std::filesystem::path& p) {
    const IModelLoader* loader = loaderFor(p);
    if (!loader) {
        LoadResult res;
        res.error = "unsupported model format " + platform::pathToUtf8(p.extension());
        std::lock_guard lock(mutex_);
        ++stats_.failed;
        memo_[p] = res;
        return res;
    }
    std::filesystem::path cacheFile;
    std::string fileHash;
    if (auto r = lookup(p, loader, cacheFile, fileHash)) return *r;
    return loadNow(p, loader, cacheFile, fileHash);
}

LoadResult ModelLibrary::request(const std::filesystem::path& p) {
    const IModelLoader* loader = loaderFor(p);
    if (!loader) return get(p);
    std::filesystem::path cacheFile;
    std::string fileHash;
    if (auto r = lookup(p, loader, cacheFile, fileHash)) return *r;
    // 分割は裏の 1 本のスレッドで順に行う（OCCT の読み込みを並べない）
    std::lock_guard lock(mutex_);
    if (!queued_.count(p)) {
        queued_.insert(p);
        queue_.push_back({p, cacheFile, fileHash});
        if (!worker_.joinable()) worker_ = std::thread([this] { workerLoop(); });
        cv_.notify_one();
    }
    LoadResult res;
    res.pending = true;
    res.fileHash = fileHash;
    return res;
}

void ModelLibrary::workerLoop() {
    for (;;) {
        Job job;
        {
            std::unique_lock lock(mutex_);
            cv_.wait(lock, [&] { return stop_ || !queue_.empty(); });
            if (stop_) return;
            job = queue_.front();
            queue_.pop_front();
        }
        loadNow(job.path, loaderFor(job.path), job.cacheFile, job.fileHash);
        std::lock_guard lock(mutex_);
        queued_.erase(job.path);
        ++completed_;
    }
}

size_t ModelLibrary::completed() const {
    std::lock_guard lock(mutex_);
    return completed_;
}

size_t ModelLibrary::pending() const {
    std::lock_guard lock(mutex_);
    return queued_.size();
}

ModelLibrary::Stats ModelLibrary::stats() const {
    std::lock_guard lock(mutex_);
    return stats_;
}

}  // namespace krbr::model
