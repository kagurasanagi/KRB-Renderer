#pragma once
// 銅箔・レジスト開口の縁からの符号付き距離場（SDF）。レジストの厚み t(x) や縁の丸みに使う。
// 値は 8bit：128 が縁、+ が外側（銅・開口の外）、1 単位 = kSdfRangeMm / 127 mm。
#include <cstdint>
#include <vector>

#include "raster/rasterizer.hpp"

namespace krbr::raster {

constexpr double kSdfRangeMm = 0.3;  // これより遠い距離は同じ値にまとめる

// 2 値の画像（inside）から、各画素の中心と反対側の画素の中心との最短距離（画素単位）を求める
// （Felzenszwalb & Huttenlocher の厳密なユークリッド距離変換）。返り値は符号付き（外側が正）
std::vector<float> signedDistancePx(const std::vector<uint8_t>& inside, int width, int height);

// 被覆率（0..1）から符号付き距離を求め、8bit に詰める
void encodeSdf(const std::vector<float>& coverage, int width, int height, double pxPerMm, std::vector<uint8_t>& out);

inline float decodeSdfMm(uint8_t v) { return (static_cast<float>(v) - 128.0f) / 127.0f * static_cast<float>(kSdfRangeMm); }

}  // namespace krbr::raster
