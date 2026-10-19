#include <cmath>
#include <format>

#include "model/model_mesh.hpp"
#include "scene/material.hpp"

namespace krbr::model {

namespace {

// KiCad 標準モデル（kicad-packages3D の生成スクリプト）でよく使われる色。STEP に書かれた値そのもの
struct NamedColor {
    const char* name;
    float r, g, b;
};
constexpr NamedColor kKicadColors[] = {
    {"black_body", 0.148f, 0.145f, 0.145f},       {"metal_grey_pins", 0.824f, 0.820f, 0.781f},
    {"gold_pins", 0.859f, 0.738f, 0.496f},        {"light_brown_body", 0.691f, 0.664f, 0.598f},
    {"white_body", 0.895f, 0.891f, 0.813f},       {"green_body", 0.337f, 0.678f, 0.443f},
    {"blue_body", 0.137f, 0.402f, 0.727f},        {"red_body", 0.700f, 0.100f, 0.050f},
    {"orange_body", 0.809f, 0.426f, 0.148f},
};

}  // namespace

glm::vec3 ModelPart::linearColor() const { return scene::srgbToLinear(srgb); }

std::string ModelPart::hex() const {
    auto b = [](float v) { return static_cast<int>(std::lround(std::clamp(v, 0.0f, 1.0f) * 255.0f)); };
    return std::format("#{:02x}{:02x}{:02x}", b(srgb.r), b(srgb.g), b(srgb.b));
}

std::string ModelPart::kicadColorName() const {
    if (!hasColor) return {};
    for (const auto& c : kKicadColors)
        if (std::abs(srgb.r - c.r) < 0.004f && std::abs(srgb.g - c.g) < 0.004f && std::abs(srgb.b - c.b) < 0.004f)
            return c.name;
    return {};
}

std::vector<std::string> ModelPart::keys() const {
    std::vector<std::string> k;
    if (!name.empty()) k.push_back(name);
    if (auto c = kicadColorName(); !c.empty()) k.push_back("kicad:" + c);
    if (hasColor) k.push_back(hex());
    return k;
}

}  // namespace krbr::model
