#include "mesh_file.hpp"

#include <fstream>
#include <string_view>

namespace krbr::model {

namespace {

constexpr char kMagic[4] = {'K', 'R', 'B', 'M'};

template <class T>
void writeVec(std::ofstream& f, const std::vector<T>& v) {
    if (!v.empty()) f.write(reinterpret_cast<const char*>(v.data()), static_cast<std::streamsize>(v.size() * sizeof(T)));
}

// UTF-8 の文字の途中で切らないように、max バイト以内に切り詰める（続きのバイト 10xxxxxx の前では切らない）
std::string_view truncateUtf8(std::string_view s, size_t max) {
    if (s.size() <= max) return s;
    size_t n = max;
    while (n > 0 && (static_cast<unsigned char>(s[n]) & 0xC0) == 0x80) --n;
    return s.substr(0, n);
}

}  // namespace

bool writeMeshFile(const std::filesystem::path& p, const ModelMesh& m) {
    std::error_code ec;
    std::filesystem::create_directories(p.parent_path(), ec);
    // path のまま（Windows では wchar_t のまま）扱う。path::string() は ANSI コードページへの変換で、
    // 表せない文字を含むパスでは例外になる
    std::filesystem::path tmp = p;
    tmp += ".tmp";
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
            const std::string_view name = truncateUtf8(part.name, kMaxPartNameBytes);
            const uint32_t len = static_cast<uint32_t>(name.size());
            f.write(reinterpret_cast<const char*>(&len), 4);
            f.write(name.data(), len);
            const float c[5] = {part.srgb.r, part.srgb.g, part.srgb.b, part.alpha, part.hasColor ? 1.0f : 0.0f};
            f.write(reinterpret_cast<const char*>(c), sizeof(c));
        }
        if (!f) return false;
    }
    std::filesystem::rename(tmp, p, ec);  // 書き込み途中のファイルを残さない
    if (!ec) return true;
    std::filesystem::remove(tmp, ec);
    return false;
}

}  // namespace krbr::model
