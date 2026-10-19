#pragma once
// 材質。パラメータ名は OpenPBR Surface に合わせる（今回は簡易な部分集合）
#include <glm/glm.hpp>

#include <cmath>
#include <cstdint>
#include <string>

namespace krbr::scene {

enum class MaterialKind : uint32_t {
    Generic = 0,
    BoardSurface = 1,  // 層テクスチャで材質を決める基板面
};

struct Material {
    std::string name;
    MaterialKind kind = MaterialKind::Generic;
    uint32_t boardSide = 0;  // BoardSurface 用（0=表, 1=裏）

    glm::vec3 base_color{0.8f};
    float base_weight = 1.0f;
    float base_metalness = 0.0f;
    glm::vec3 specular_color{1.0f};
    float specular_weight = 1.0f;
    float specular_roughness = 0.3f;
    float specular_ior = 1.5f;
    float coat_weight = 0.0f;
    glm::vec3 coat_color{1.0f};
    float coat_roughness = 0.0f;
    float coat_ior = 1.6f;
};

// sRGB（表示値）からリニアへ
inline float srgbToLinear(float c) {
    c = glm::clamp(c, 0.0f, 1.0f);
    return c <= 0.04045f ? c / 12.92f : std::pow((c + 0.055f) / 1.055f, 2.4f);
}
inline glm::vec3 srgbToLinear(glm::vec3 c) { return {srgbToLinear(c.r), srgbToLinear(c.g), srgbToLinear(c.b)}; }
inline float linearToSrgb(float c) {
    c = glm::clamp(c, 0.0f, 1.0f);
    return c <= 0.0031308f ? 12.92f * c : 1.055f * std::pow(c, 1.0f / 2.4f) - 0.055f;
}
inline glm::vec3 linearToSrgb(glm::vec3 c) { return {linearToSrgb(c.r), linearToSrgb(c.g), linearToSrgb(c.b)}; }

}  // namespace krbr::scene
