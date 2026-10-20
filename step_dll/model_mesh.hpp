#pragma once
// 変換器 DLL と .krbm ファイルで使う、3D モデルのメッシュ（Rust 版の krbr-model の ModelMesh と同じ中身）
#include <glm/glm.hpp>

#include <cstdint>
#include <string>
#include <vector>

namespace krbr::model {

// 三角形メッシュ。triMaterial は parts の番号
struct Mesh {
    std::vector<glm::vec3> positions;
    std::vector<glm::vec3> normals;
    std::vector<uint32_t> indices;
    std::vector<uint32_t> triMaterial;

    uint32_t addVertex(glm::vec3 p, glm::vec3 n) {
        positions.push_back(p);
        normals.push_back(n);
        return static_cast<uint32_t>(positions.size() - 1);
    }
    void addTriangle(uint32_t a, uint32_t b, uint32_t c, uint32_t mat) {
        indices.insert(indices.end(), {a, b, c});
        triMaterial.push_back(mat);
    }
    size_t triangleCount() const { return indices.size() / 3; }
};

// モデルの部位（STEP の XDE の名前と面の色の組）。材質はこの単位で割り当てる
struct ModelPart {
    std::string name;  // XDE の名前（部品・サブシェイプ）。無ければ空
    glm::vec3 srgb{0.5f};  // STEP に書かれた色の値（表示用の sRGB）
    float alpha = 1.0f;
    bool hasColor = false;
};

// 3D モデルを分割したメッシュ。座標は mm（モデル自身の座標系、KiCad 3D と同じ Z 上向き）
struct ModelMesh {
    Mesh mesh;
    std::vector<ModelPart> parts;
};

}  // namespace krbr::model
