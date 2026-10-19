#pragma once
#include <glm/glm.hpp>

#include <vector>

namespace krbr::scene {

struct CameraParams {
    double elevationDeg = 35.0;  // 基板面からの仰角
    double azimuthDeg = 45.0;    // 0 で基板の手前（KiCad の y が大きい側）から見る。正で +x 側へ回る
    double fovDeg = 30.0;        // 縦の画角
    double margin = 0.04;        // 画面端に残す余白（割合）
    int width = 1920, height = 1080;
};

struct Camera {
    glm::vec3 position{0.0f};
    glm::vec3 forward{0, 0, -1}, right{1, 0, 0}, up{0, 1, 0};
    float tanHalfFovY = 0.27f;
    float aspect = 16.0f / 9.0f;
};

// 指定した点がすべて画面に収まるように距離と注視点を決める
Camera frameCamera(const CameraParams& p, const std::vector<glm::vec3>& points);

}  // namespace krbr::scene
