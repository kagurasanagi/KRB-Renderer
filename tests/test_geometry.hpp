#pragma once
// テスト用の簡単な形状（外向きの法線・面の向きで三角形を作る）
#include <cmath>
#include <numbers>

#include "geom/mesh.hpp"
#include "io/image_io.hpp"
#include "scene/scene.hpp"

namespace krbr::test {

// 平面の四角形（p0→p1→p2→p3 の順、法線は n の向き）
inline void addQuad(geom::Mesh& m, glm::vec3 p0, glm::vec3 p1, glm::vec3 p2, glm::vec3 p3, glm::vec3 n, uint32_t mat) {
    if (glm::dot(glm::cross(p1 - p0, p2 - p0), n) < 0) {
        std::swap(p1, p3);
    }
    const uint32_t a = m.addVertex(p0, n), b = m.addVertex(p1, n), c = m.addVertex(p2, n), d = m.addVertex(p3, n);
    m.addTriangle(a, b, c, mat);
    m.addTriangle(a, c, d, mat);
}

inline void addBox(geom::Mesh& m, glm::vec3 lo, glm::vec3 hi, uint32_t mat) {
    const glm::vec3 c[8] = {{lo.x, lo.y, lo.z}, {hi.x, lo.y, lo.z}, {hi.x, hi.y, lo.z}, {lo.x, hi.y, lo.z},
                            {lo.x, lo.y, hi.z}, {hi.x, lo.y, hi.z}, {hi.x, hi.y, hi.z}, {lo.x, hi.y, hi.z}};
    addQuad(m, c[0], c[1], c[2], c[3], {0, 0, -1}, mat);
    addQuad(m, c[4], c[5], c[6], c[7], {0, 0, 1}, mat);
    addQuad(m, c[0], c[1], c[5], c[4], {0, -1, 0}, mat);
    addQuad(m, c[3], c[2], c[6], c[7], {0, 1, 0}, mat);
    addQuad(m, c[0], c[3], c[7], c[4], {-1, 0, 0}, mat);
    addQuad(m, c[1], c[2], c[6], c[5], {1, 0, 0}, mat);
}

inline void addSphere(geom::Mesh& m, glm::vec3 center, float r, uint32_t mat, int segU = 96, int segV = 48) {
    const uint32_t base = static_cast<uint32_t>(m.positions.size());
    for (int j = 0; j <= segV; ++j) {
        const double th = std::numbers::pi * j / segV;
        for (int i = 0; i <= segU; ++i) {
            const double ph = 2 * std::numbers::pi * i / segU;
            const glm::vec3 n{static_cast<float>(std::sin(th) * std::cos(ph)), static_cast<float>(std::cos(th)),
                              static_cast<float>(std::sin(th) * std::sin(ph))};
            m.addVertex(center + n * r, n);
        }
    }
    for (int j = 0; j < segV; ++j)
        for (int i = 0; i < segU; ++i) {
            const uint32_t a = base + j * (segU + 1) + i, b = a + 1, c = a + segU + 1, d = c + 1;
            m.addTriangle(a, b, c, mat);
            m.addTriangle(b, d, c, mat);
        }
}

// 1 つのメッシュ・1 つの配置だけのシーン（基板面のテクスチャは使わない）
inline void finishScene(scene::Scene& s, geom::Mesh mesh) {
    s.meshes.push_back({"test_scene", std::move(mesh)});
    s.instances.push_back({0, glm::mat4(1.0f), 0});
    s.layers.grid.width = s.layers.grid.height = 1;
    s.layers.grid.pxPerMm = 1.0;
    s.layers.rgba[0] = s.layers.rgba[1] = {0, 0, 0, 255};
}

// 一様な色の環境マップ
inline io::ImageF uniformEnv(float v) {
    io::ImageF env;
    env.width = 16;
    env.height = 8;
    env.rgba.assign(16 * 8 * 4, v);
    for (size_t i = 3; i < env.rgba.size(); i += 4) env.rgba[i] = 1.0f;
    return env;
}

}  // namespace krbr::test
