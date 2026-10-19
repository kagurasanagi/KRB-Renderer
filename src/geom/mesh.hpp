#pragma once
#include <glm/glm.hpp>

#include <cstdint>
#include <limits>
#include <vector>

namespace krbr::geom {

struct Bounds3 {
    glm::vec3 lo{std::numeric_limits<float>::max()};
    glm::vec3 hi{-std::numeric_limits<float>::max()};
    void add(glm::vec3 p) {
        lo = glm::min(lo, p);
        hi = glm::max(hi, p);
    }
    void add(const Bounds3& b) {
        if (b.valid()) {
            add(b.lo);
            add(b.hi);
        }
    }
    bool valid() const { return lo.x <= hi.x; }
    glm::vec3 center() const { return (lo + hi) * 0.5f; }
    glm::vec3 extent() const { return hi - lo; }
};

// 三角形メッシュ。triMaterial の意味は用途ごと（基板なら BoardMat、モデルなら色番号）
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
    Bounds3 bounds() const {
        Bounds3 b;
        for (const auto& p : positions) b.add(p);
        return b;
    }
};

}  // namespace krbr::geom
