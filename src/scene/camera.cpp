#include "scene/camera.hpp"

#include <algorithm>
#include <cmath>

namespace krbr::scene {

Camera frameCamera(const CameraParams& p, const std::vector<glm::vec3>& points) {
    Camera cam;
    const double el = glm::radians(p.elevationDeg), az = glm::radians(p.azimuthDeg);
    // 注視点からカメラへの向き
    const glm::dvec3 toCam{std::cos(el) * std::sin(az), std::sin(el), std::cos(el) * std::cos(az)};
    const glm::dvec3 fwd = -glm::normalize(toCam);
    glm::dvec3 worldUp{0, 1, 0};
    if (std::abs(glm::dot(fwd, worldUp)) > 0.999) worldUp = {0, 0, -1};  // 真上から見る場合
    const glm::dvec3 right = glm::normalize(glm::cross(fwd, worldUp));
    const glm::dvec3 up = glm::cross(right, fwd);

    cam.aspect = static_cast<float>(p.width) / static_cast<float>(std::max(1, p.height));
    const double ty = std::tan(glm::radians(p.fovDeg) / 2);
    const double tx = ty * cam.aspect;
    cam.tanHalfFovY = static_cast<float>(ty);
    const double keep = 1.0 - std::clamp(p.margin, 0.0, 0.45) * 2;

    glm::dvec3 target{0.0};
    if (!points.empty()) {
        glm::dvec3 lo{1e30}, hi{-1e30};
        for (const auto& q : points) {
            lo = glm::min(lo, glm::dvec3(q));
            hi = glm::max(hi, glm::dvec3(q));
        }
        target = (lo + hi) * 0.5;
    }
    double dist = 1.0;
    for (int iter = 0; iter < 5; ++iter) {
        // 全点が入る最小の距離
        dist = 1e-3;
        for (const auto& q : points) {
            const glm::dvec3 d = glm::dvec3(q) - target;
            const double x = glm::dot(d, right), y = glm::dot(d, up), z = glm::dot(d, fwd);
            dist = std::max(dist, std::abs(x) / (tx * keep) - z);
            dist = std::max(dist, std::abs(y) / (ty * keep) - z);
        }
        if (iter == 4) break;  // 最後は距離だけ確定させる
        // 画面上の偏りを打ち消すように注視点をずらす
        double umin = 1e30, umax = -1e30, vmin = 1e30, vmax = -1e30;
        for (const auto& q : points) {
            const glm::dvec3 d = glm::dvec3(q) - target;
            const double depth = glm::dot(d, fwd) + dist;
            if (depth <= 1e-6) continue;
            const double u = glm::dot(d, right) / (depth * tx), v = glm::dot(d, up) / (depth * ty);
            umin = std::min(umin, u);
            umax = std::max(umax, u);
            vmin = std::min(vmin, v);
            vmax = std::max(vmax, v);
        }
        if (umin > umax) break;
        target += right * ((umax + umin) * 0.5 * tx * dist) + up * ((vmax + vmin) * 0.5 * ty * dist);
    }
    cam.position = glm::vec3(target - fwd * dist);
    cam.forward = glm::vec3(fwd);
    cam.right = glm::vec3(right);
    cam.up = glm::vec3(up);
    return cam;
}

}  // namespace krbr::scene
