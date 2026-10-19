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

LoadResult ModelLibrary::get(const std::filesystem::path& p) {
    if (auto it = memo_.find(p); it != memo_.end()) return it->second;

    LoadResult res;
    const IModelLoader* loader = nullptr;
    for (const auto& l : loaders_)
        if (l->canLoad(p)) loader = l.get();
    if (!loader) {
        res.error = "unsupported model format " + platform::pathToUtf8(p.extension());
        ++stats_.failed;
        memo_[p] = res;
        return res;
    }

    // キーはファイル内容のハッシュ＋分割パラメータ＋読み込み器（パスが変わっても同じ内容なら再利用）
    std::filesystem::path cacheFile;
    std::string fileHash;
    if (useCache_) {
        log::ScopedTimer t("hash");
        auto bytes = platform::readBinaryFile(p);
        if (bytes) {
            const uint64_t h = xxh64(bytes->data(), bytes->size());
            fileHash = toHex(h);
            const std::string paramKey = std::format("{}|{:.6f}|{:.6f}|v{}", loader->name(), params_.linearDeflection,
                                                     params_.angularDeflection, kVersion);
            const uint64_t ph = xxh64(paramKey.data(), paramKey.size());
            cacheFile = cacheDir_ / "meshes" / (toHex(h) + "-" + toHex(ph).substr(0, 8) + ".krbm");
            if (auto m = readMeshFile(cacheFile)) {
                res.mesh = m;
                res.fileHash = fileHash;
                res.fromCache = true;
                ++stats_.cacheHits;
                log::info("model cache hit: {} -> {}", platform::pathToUtf8(p.filename()),
                          platform::pathToUtf8(cacheFile.filename()));
                memo_[p] = res;
                return res;
            }
        }
    }

    res = loader->load(p, params_);
    if (fileHash.empty())
        if (auto bytes = platform::readBinaryFile(p)) fileHash = toHex(xxh64(bytes->data(), bytes->size()));
    res.fileHash = fileHash;
    if (res.mesh) {
        ++stats_.loaded;
        log::info("model loaded: {} ({} tris, {:.2f} s){}", platform::pathToUtf8(p.filename()),
                  res.mesh->mesh.triangleCount(), res.seconds, cacheFile.empty() ? "" : ", cached");
        if (!cacheFile.empty() && !writeMeshFile(cacheFile, *res.mesh))
            log::warn("cannot write mesh cache {}", platform::pathToUtf8(cacheFile));
    } else {
        ++stats_.failed;
        log::warn("model load failed: {}: {}", platform::pathToUtf8(p.filename()), res.error);
    }
    memo_[p] = res;
    return res;
}

}  // namespace krbr::model
