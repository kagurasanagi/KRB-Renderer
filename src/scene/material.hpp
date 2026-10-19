#pragma once
// 材質。パラメータは OpenPBR Surface（v1.1）に準拠する。krbr 独自の拡張は
// TOML では [material.<名前>.krbr] の下に書く。
#include <glm/glm.hpp>

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <optional>
#include <string>

namespace krbr::scene {

enum class MaterialKind : uint32_t {
    Generic = 0,
    BoardSurface = 1,  // 層テクスチャで材質を決める基板面（凹凸を形状にしない場合）
    Masked = 2,        // レジストの下に別の材質（maskedBase）がある面。レジストは maskedMask
};

enum class BumpPattern : uint32_t {
    None = 0,
    Noise = 1,  // 等方的なノイズ（塗膜のゆず肌・粒状感・樹脂のむら）
    Weave = 2,  // ガラス繊維の織り目（FR4）。bump_scale が織りのピッチ
};

// Sellmeier の分散式 n²(λ) = 1 + Σ B_i λ² / (λ² − C_i)（λ は µm、C_i は µm²）。アッベ数で表せない材質用
struct Sellmeier {
    std::array<float, 3> B{0, 0, 0};
    std::array<float, 3> C{0, 0, 0};
    float ior(float lambdaUm) const {
        const float l2 = lambdaUm * lambdaUm;
        float n2 = 1.0f;
        for (int i = 0; i < 3; ++i) n2 += B[i] * l2 / (l2 - C[i]);
        return std::sqrt(std::max(n2, 1.0f));
    }
};

struct Material {
    std::string name;
    MaterialKind kind = MaterialKind::Generic;
    uint32_t boardSide = 0;                   // BoardSurface 用（0=表, 1=裏）
    uint32_t maskedBase = 0, maskedMask = 0;  // Masked 用（材質の番号）

    // ---- OpenPBR Surface（既定値は仕様のとおり）。色はリニア
    float base_weight = 1.0f;
    glm::vec3 base_color{0.8f};
    float base_diffuse_roughness = 0.0f;
    float base_metalness = 0.0f;

    float specular_weight = 1.0f;
    glm::vec3 specular_color{1.0f};
    float specular_roughness = 0.3f;
    float specular_roughness_anisotropy = 0.0f;
    float specular_ior = 1.5f;  // 分散がある場合は nd（587.6 nm）として扱う

    float transmission_weight = 0.0f;
    glm::vec3 transmission_color{1.0f};
    float transmission_depth = 0.0f;
    glm::vec3 transmission_scatter{0.0f};
    float transmission_scatter_anisotropy = 0.0f;
    float transmission_dispersion_scale = 0.0f;
    float transmission_dispersion_abbe_number = 20.0f;

    float subsurface_weight = 0.0f;
    glm::vec3 subsurface_color{0.8f};
    float subsurface_radius = 1.0f;
    glm::vec3 subsurface_radius_scale{1.0f, 0.5f, 0.25f};
    float subsurface_scatter_anisotropy = 0.0f;

    float fuzz_weight = 0.0f;
    glm::vec3 fuzz_color{1.0f};
    float fuzz_roughness = 0.5f;

    float coat_weight = 0.0f;
    glm::vec3 coat_color{1.0f};
    float coat_roughness = 0.0f;
    float coat_roughness_anisotropy = 0.0f;
    float coat_ior = 1.6f;
    float coat_darkening = 1.0f;

    float thin_film_weight = 0.0f;
    float thin_film_thickness = 0.5f;  // µm
    float thin_film_ior = 1.4f;

    float emission_luminance = 0.0f;  // cd/m²
    glm::vec3 emission_color{1.0f};

    float geometry_opacity = 1.0f;
    bool geometry_thin_walled = false;

    // ---- krbr 独自の拡張（TOML では [krbr] の下）
    bool base_color_from_model = false;  // base_color を 3D モデルの面の色にする（材質ライブラリで base_color 省略時）
    BumpPattern bump_pattern = BumpPattern::None;
    float bump_strength = 0.0f;        // 傾きの大きさ（0.05 で約 3°）
    float bump_scale = 0.2f;           // 凹凸の大きさ（mm）
    float roughness_variation = 0.0f;  // specular_roughness の相対的なむら（0..1）
    std::optional<Sellmeier> sellmeier;  // 指定があればアッベ数より優先

    // 波長 λ（nm）での屈折率。分散がなければ specular_ior
    float iorAt(float lambdaNm) const {
        if (sellmeier) return sellmeier->ior(lambdaNm * 1e-3f);
        if (transmission_dispersion_scale <= 0.0f || transmission_dispersion_abbe_number <= 0.0f) return specular_ior;
        // OpenPBR：アッベ数 V = (nd − 1) / (nF − nC) と Cauchy の式 n(λ) = A + B/λ² から
        constexpr float lF = 486.1f, lC = 656.3f, lD = 587.6f;
        const float V = transmission_dispersion_abbe_number / transmission_dispersion_scale;
        const float B = (specular_ior - 1.0f) / (V * (1.0f / (lF * lF) - 1.0f / (lC * lC)));
        const float A = specular_ior - B / (lD * lD);
        return A + B / (lambdaNm * lambdaNm);
    }
};

// sRGB（表示値）からリニアへ
inline float srgbToLinear(float c) {
    c = std::clamp(c, 0.0f, 1.0f);
    return c <= 0.04045f ? c / 12.92f : std::pow((c + 0.055f) / 1.055f, 2.4f);
}
inline glm::vec3 srgbToLinear(glm::vec3 c) { return {srgbToLinear(c.r), srgbToLinear(c.g), srgbToLinear(c.b)}; }
inline float linearToSrgb(float c) {
    c = std::clamp(c, 0.0f, 1.0f);
    return c <= 0.0031308f ? 12.92f * c : 1.055f * std::pow(c, 1.0f / 2.4f) - 0.055f;
}
inline glm::vec3 linearToSrgb(glm::vec3 c) { return {linearToSrgb(c.r), linearToSrgb(c.g), linearToSrgb(c.b)}; }

}  // namespace krbr::scene
