#pragma once
// 分光レンダリングの土台：CIE 1931 の等色関数（解析的な近似）、色空間の変換行列、
// RGB からスペクトルへの変換表（Jakob & Hanika 2019, "A Low-Dimensional Function Space for Efficient
// Spectral Upsampling"）。白色点は等エネルギー白色（E）とし、表示用の sRGB（D65）へは Bradford で変換する。
#include <glm/glm.hpp>

#include <algorithm>

#include <array>
#include <cmath>
#include <filesystem>
#include <vector>

namespace krbr::spectrum {

// 波長の範囲（nm）。可視域の重点的サンプリング（pbrt-v4 の SampleVisibleWavelengths）もこの範囲で定義されている
constexpr float kLambdaMin = 360.0f;
constexpr float kLambdaMax = 830.0f;

// CIE 1931 2° 等色関数の解析的な近似（Wyman, Sloan & Shirley 2013, 多峰ガウス関数）
glm::vec3 cmf(float lambdaNm);
// ȳ の積分（[kLambdaMin, kLambdaMax]、1 nm 刻み）。放射輝度 1 の平坦なスペクトルが Y = 1 になるよう割る数
float cmfYIntegral();
// 平坦なスペクトル（E）の XYZ（Y = 1 に正規化）
glm::vec3 whiteE();

// XYZ（白色点 E）→ リニア sRGB（D65）と、その逆
glm::mat3 xyzToLinearSrgb();
glm::mat3 linearSrgbToXyz();

// シグモイド多項式 s(λ) = 1/2 + z / (2 √(1+z²))、z = c0 x² + c1 x + c2、x = (λ − min) / (max − min)
inline float sigmoidPoly(const glm::vec3& c, float lambdaNm) {
    const float x = (lambdaNm - kLambdaMin) / (kLambdaMax - kLambdaMin);
    const float z = (c.x * x + c.y) * x + c.z;
    return 0.5f + z / (2.0f * std::sqrt(1.0f + z * z));
}

// RGB（リニア sRGB、0..1）→ シグモイド多項式の係数の表
struct Rgb2SpecTable {
    static constexpr int kRes = 64;
    std::array<float, kRes> scale{};  // z（最大の成分）の非一様な格子
    // data[(((l * kRes + zi) * kRes + yi) * kRes + xi) * 3 + k]
    std::vector<float> data;

    // 表を引いて係数を返す（三重線形補間）
    glm::vec3 lookup(glm::vec3 rgb) const;
    // 0..1 の反射率の RGB をスペクトルにして λ での値
    float eval(glm::vec3 rgb, float lambdaNm) const { return sigmoidPoly(lookup(rgb), lambdaNm); }
    // 上限のない値（発光・環境光）：xyz = 係数、w = 倍率。s(λ) = w · sigmoid(係数, λ)（pbrt の RGBUnboundedSpectrum）
    glm::vec4 unbounded(glm::vec3 rgb) const {
        const float m = std::max({rgb.r, rgb.g, rgb.b});
        if (m <= 0.0f) return {0.0f, 0.0f, 0.0f, 0.0f};
        return {lookup(rgb / (2.0f * m)), 2.0f * m};
    }
};

// 表を作る（数秒）。cacheFile があれば読み、無ければ作って書く
const Rgb2SpecTable& rgb2specTable(const std::filesystem::path& cacheFile);
// キャッシュの既定の場所（<cacheDir>/spectral/rgb2spec_v1.bin）
const Rgb2SpecTable& rgb2specTable();
Rgb2SpecTable buildRgb2SpecTable();
// すでに読み込んだ表（まだなら nullptr）
const Rgb2SpecTable* loadedRgb2SpecTable();

// スペクトル（関数）を XYZ に積分する（検証用）
template <class F>
glm::vec3 integrateXyz(F&& spectrum) {
    glm::vec3 xyz(0.0f);
    for (float l = kLambdaMin; l < kLambdaMax; l += 1.0f) xyz += spectrum(l + 0.5f) * cmf(l + 0.5f);
    return xyz / cmfYIntegral();
}

}  // namespace krbr::spectrum
