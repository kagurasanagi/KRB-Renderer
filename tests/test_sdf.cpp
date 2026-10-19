// 銅箔・レジスト開口の縁からの距離場（フェーズ4）
#include <doctest/doctest.h>

#include <cmath>

#include "raster/board_sdf.hpp"

using namespace krbr::raster;

TEST_CASE("signed distance: half plane and a single disc") {
    // x < 10 が内側の半平面。縁は x = 9.5（画素の中心の間）
    const int w = 40, h = 8;
    std::vector<uint8_t> inside(w * h);
    for (int y = 0; y < h; ++y)
        for (int x = 0; x < w; ++x) inside[y * w + x] = x < 10 ? 1 : 0;
    const auto sd = signedDistancePx(inside, w, h);
    CHECK(sd[3 * w + 20] == doctest::Approx(10.5f));
    CHECK(sd[3 * w + 5] == doctest::Approx(-4.5f));
    CHECK(sd[3 * w + 10] == doctest::Approx(0.5f));
    CHECK(sd[3 * w + 9] == doctest::Approx(-0.5f));

    // 中心 (32,32) の 1 画素だけが内側：外側の距離は中心からの距離 − 0.5
    const int n = 64;
    std::vector<uint8_t> dot(n * n, 0);
    dot[32 * n + 32] = 1;
    const auto sd2 = signedDistancePx(dot, n, n);
    CHECK(sd2[32 * n + 32 + 7] == doctest::Approx(6.5f));
    CHECK(sd2[(32 + 3) * n + 32 + 4] == doctest::Approx(4.5f));  // 3-4-5
}

TEST_CASE("encoded SDF: zero at the edge, clamped beyond the range") {
    const int w = 200, h = 4;
    const double pxPerMm = 50.0;
    std::vector<float> cov(w * h);
    for (int y = 0; y < h; ++y)
        for (int x = 0; x < w; ++x) cov[y * w + x] = x < 100 ? 1.0f : 0.0f;
    std::vector<uint8_t> enc;
    encodeSdf(cov, w, h, pxPerMm, enc);
    // 縁のすぐ外側（0.5 画素 = 0.01 mm）
    CHECK(decodeSdfMm(enc[w + 100]) == doctest::Approx(0.01f).epsilon(0.3));
    // 0.1 mm 外側（5 画素）、0.1 mm 内側
    CHECK(decodeSdfMm(enc[w + 105]) == doctest::Approx(0.11f).epsilon(0.05));
    CHECK(decodeSdfMm(enc[w + 94]) == doctest::Approx(-0.11f).epsilon(0.05));
    // 範囲（0.3 mm）より遠い所は上限に張り付く
    CHECK(decodeSdfMm(enc[w + 190]) == doctest::Approx(kSdfRangeMm).epsilon(0.01));
    CHECK(decodeSdfMm(enc[w + 2]) == doctest::Approx(-kSdfRangeMm).epsilon(0.01));
}
