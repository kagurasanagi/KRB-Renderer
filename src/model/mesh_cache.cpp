#include "model/mesh_cache.hpp"

#include <cstring>
#include <fstream>

#include "core/hash.hpp"
#include "core/log.hpp"
#include "core/platform.hpp"

namespace krbr::model {

namespace {

constexpr char kMagic[4] = {'K', 'R', 'B', 'M'};
constexpr uint32_t kVersion = 3;  // v2: 部位（XDE の名前と色）, v3: 色を STEP の sRGB 値で保存

template <class T>
void writeVec(std::ofstream& f, const std::vector<T>& v) {
    if (!v.empty()) f.write(reinterpret_cast<const char*>(v.data()), static_cast<std::streamsize>(v.size() * sizeof(T)));
}
template <class T>
bool readVec(std::ifstream& f, std::vector<T>& v, size_t n) {
    v.resize(n);
    if (n == 0) return true;
    return static_cast<bool>(f.read(reinterpret_cast<char*>(v.data()), static_cast<std::streamsize>(n * sizeof(T))));
}

}  // namespace

bool writeMeshFile(const std::filesystem::path& p, const ModelMesh& m) {
    std::error_code ec;
    std::filesystem::create_directories(p.parent_path(), ec);
    const auto tmp = p.string() + ".tmp";
    {
        std::ofstream f(tmp, std::ios::binary);
        if (!f) return false;
        const uint32_t hdr[4] = {kVersion, static_cast<uint32_t>(m.mesh.positions.size()),
                                 static_cast<uint32_t>(m.mesh.triangleCount()), static_cast<uint32_t>(m.parts.size())};
        f.write(kMagic, 4);
        f.write(reinterpret_cast<const char*>(hdr), sizeof(hdr));
        writeVec(f, m.mesh.positions);
        writeVec(f, m.mesh.normals);
        writeVec(f, m.mesh.indices);
        writeVec(f, m.mesh.triMaterial);
        for (const auto& p : m.parts) {
            const uint32_t len = static_cast<uint32_t>(p.name.size());
            f.write(reinterpret_cast<const char*>(&len), 4);
            f.write(p.name.data(), len);
            const float c[5] = {p.srgb.r, p.srgb.g, p.srgb.b, p.alpha, p.hasColor ? 1.0f : 0.0f};
            f.write(reinterpret_cast<const char*>(c), sizeof(c));
        }
        if (!f) return false;
    }
    std::filesystem::rename(tmp, p, ec);  // 書き込み途中のファイルを残さない
    return !ec;
}

std::shared_ptr<ModelMesh> readMeshFile(const std::filesystem::path& p) {
    std::ifstream f(p, std::ios::binary);
    if (!f) return nullptr;
    char magic[4];
    uint32_t hdr[4];
    if (!f.read(magic, 4) || std::memcmp(magic, kMagic, 4) != 0) return nullptr;
    if (!f.read(reinterpret_cast<char*>(hdr), sizeof(hdr)) || hdr[0] != kVersion) return nullptr;
    auto m = std::make_shared<ModelMesh>();
    const size_t nv = hdr[1], nt = hdr[2], nc = hdr[3];
    if (!readVec(f, m->mesh.positions, nv) || !readVec(f, m->mesh.normals, nv) || !readVec(f, m->mesh.indices, nt * 3) ||
        !readVec(f, m->mesh.triMaterial, nt))
        return nullptr;
    m->parts.resize(nc);
    for (auto& p : m->parts) {
        uint32_t len = 0;
        if (!f.read(reinterpret_cast<char*>(&len), 4) || len > 4096) return nullptr;
        p.name.resize(len);
        float c[5];
        if (!f.read(p.name.data(), len) || !f.read(reinterpret_cast<char*>(c), sizeof(c))) return nullptr;
        p.srgb = {c[0], c[1], c[2]};
        p.alpha = c[3];
        p.hasColor = c[4] != 0.0f;
    }
    return m;
}

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
                                             params_.angularDeflection, kVersion);
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
