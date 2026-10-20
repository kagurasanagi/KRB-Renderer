#include "mesh_file.hpp"

#include <cstring>
#include <fstream>

namespace krbr::model {

namespace {

constexpr char kMagic[4] = {'K', 'R', 'B', 'M'};

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
        const uint32_t hdr[4] = {kMeshFileVersion, static_cast<uint32_t>(m.mesh.positions.size()),
                                 static_cast<uint32_t>(m.mesh.triangleCount()), static_cast<uint32_t>(m.parts.size())};
        f.write(kMagic, 4);
        f.write(reinterpret_cast<const char*>(hdr), sizeof(hdr));
        writeVec(f, m.mesh.positions);
        writeVec(f, m.mesh.normals);
        writeVec(f, m.mesh.indices);
        writeVec(f, m.mesh.triMaterial);
        for (const auto& part : m.parts) {
            const uint32_t len = static_cast<uint32_t>(part.name.size());
            f.write(reinterpret_cast<const char*>(&len), 4);
            f.write(part.name.data(), len);
            const float c[5] = {part.srgb.r, part.srgb.g, part.srgb.b, part.alpha, part.hasColor ? 1.0f : 0.0f};
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
    if (!f.read(reinterpret_cast<char*>(hdr), sizeof(hdr)) || hdr[0] != kMeshFileVersion) return nullptr;
    auto m = std::make_shared<ModelMesh>();
    const size_t nv = hdr[1], nt = hdr[2], nc = hdr[3];
    if (!readVec(f, m->mesh.positions, nv) || !readVec(f, m->mesh.normals, nv) || !readVec(f, m->mesh.indices, nt * 3) ||
        !readVec(f, m->mesh.triMaterial, nt))
        return nullptr;
    m->parts.resize(nc);
    for (auto& part : m->parts) {
        uint32_t len = 0;
        if (!f.read(reinterpret_cast<char*>(&len), 4) || len > 4096) return nullptr;
        part.name.resize(len);
        float c[5];
        if (!f.read(part.name.data(), len) || !f.read(reinterpret_cast<char*>(c), sizeof(c))) return nullptr;
        part.srgb = {c[0], c[1], c[2]};
        part.alpha = c[3];
        part.hasColor = c[4] != 0.0f;
    }
    return m;
}

}  // namespace krbr::model
