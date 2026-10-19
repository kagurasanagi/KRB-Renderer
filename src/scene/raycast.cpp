#include "scene/raycast.hpp"

#include <cmath>

namespace krbr::scene {

std::optional<float> raycast(const Scene& s, glm::vec3 origin, glm::vec3 dir) {
    float best = 1e30f;
    for (const auto& in : s.instances) {
        // オブジェクト座標で交差を求める（t は変換しても同じ値になる）
        const glm::mat4 inv = glm::inverse(in.xform);
        const glm::vec3 o = glm::vec3(inv * glm::vec4(origin, 1.0f));
        const glm::vec3 d = glm::vec3(inv * glm::vec4(dir, 0.0f));
        const auto& m = s.meshes[in.mesh].mesh;
        for (size_t t = 0; t + 2 < m.indices.size(); t += 3) {
            const glm::vec3 a = m.positions[m.indices[t]], b = m.positions[m.indices[t + 1]],
                            c = m.positions[m.indices[t + 2]];
            // Möller–Trumbore
            const glm::vec3 e1 = b - a, e2 = c - a;
            const glm::vec3 p = glm::cross(d, e2);
            const float det = glm::dot(e1, p);
            if (std::abs(det) < 1e-12f) continue;
            const float inv_det = 1.0f / det;
            const glm::vec3 tv = o - a;
            const float u = glm::dot(tv, p) * inv_det;
            if (u < 0.0f || u > 1.0f) continue;
            const glm::vec3 q = glm::cross(tv, e1);
            const float v = glm::dot(d, q) * inv_det;
            if (v < 0.0f || u + v > 1.0f) continue;
            const float tt = glm::dot(e2, q) * inv_det;
            if (tt > 1e-4f && tt < best) best = tt;
        }
    }
    if (best >= 1e30f) return std::nullopt;
    return best;
}

}  // namespace krbr::scene
