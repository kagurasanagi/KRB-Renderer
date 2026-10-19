// 分光の土台（フェーズ5）：等色関数、色空間の変換、RGB → スペクトルの変換表
#include <doctest/doctest.h>

#include <cstdio>

#include "scene/material.hpp"
#include "scene/spectrum.hpp"

using namespace krbr;
using namespace krbr::spectrum;

TEST_CASE("CIE matching functions and the E white point") {
    // ȳ の山は 555 nm 付近でほぼ 1
    CHECK(cmf(555.0f).y == doctest::Approx(1.0f).epsilon(0.01));
    const glm::vec3 w = whiteE();
    CHECK(w.y == doctest::Approx(1.0f).epsilon(1e-4));
    CHECK(w.x == doctest::Approx(1.0f).epsilon(0.02));
    CHECK(w.z == doctest::Approx(1.0f).epsilon(0.02));
    // 平坦なスペクトル（E）は、表示の sRGB で白（1, 1, 1）
    const glm::vec3 rgb = xyzToLinearSrgb() * w;
    CHECK(rgb.r == doctest::Approx(1.0f).epsilon(0.002));
    CHECK(rgb.g == doctest::Approx(1.0f).epsilon(0.002));
    CHECK(rgb.b == doctest::Approx(1.0f).epsilon(0.002));
}

TEST_CASE("RGB -> spectrum table round trip") {
    const Rgb2SpecTable& t = rgb2specTable();
    const glm::vec3 colors[] = {{0.8f, 0.2f, 0.1f}, {0.1f, 0.5f, 0.9f}, {0.5f, 0.5f, 0.5f}, {0.02f, 0.02f, 0.02f},
                                {1.0f, 1.0f, 1.0f}, {0.9f, 0.9f, 0.1f}, {0.05f, 0.3f, 0.08f}, {1.0f, 0.0f, 0.0f},
                                {0.0f, 1.0f, 0.0f}, {0.0f, 0.0f, 1.0f}, {0.33f, 0.07f, 0.61f}, {1.0f, 0.98f, 0.98f}, {0.97f, 1.0f, 0.99f},
                                {0.75f, 0.62f, 0.4f}};
    double worst = 0;
    for (const glm::vec3 c : colors) {
        const glm::vec3 coef = t.lookup(c);
        float lo = 1, hi = 0;
        const glm::vec3 xyz = integrateXyz([&](float l) {
            const float v = sigmoidPoly(coef, l);
            lo = std::min(lo, v);
            hi = std::max(hi, v);
            return v;
        });
        const glm::vec3 back = xyzToLinearSrgb() * xyz;
        const double err = glm::length(back - c);
        worst = std::max(worst, err);
        CHECK_MESSAGE(err < 0.02, "rgb " << c.r << "," << c.g << "," << c.b << " -> " << back.r << "," << back.g << ","
                                         << back.b);
        CHECK(lo >= 0.0f);  // 反射率として物理的に正しい（0..1）
        CHECK(hi <= 1.0f);
    }
    std::printf("rgb -> spectrum round trip: worst error %.4f\n", worst);

    // 上限のない値（環境光）も、倍率をかけて戻る
    const glm::vec3 bright{3.0f, 1.5f, 0.5f};
    const glm::vec4 u = t.unbounded(bright);
    const glm::vec3 back = xyzToLinearSrgb() * integrateXyz([&](float l) { return u.w * sigmoidPoly(glm::vec3(u), l); });
    CHECK(glm::length(back - bright) < 0.05f);
}

TEST_CASE("dispersion from the Abbe number") {
    scene::Material acrylic, pc;
    acrylic.specular_ior = 1.491f;
    acrylic.transmission_dispersion_scale = 1.0f;
    acrylic.transmission_dispersion_abbe_number = 57.0f;
    pc.specular_ior = 1.585f;
    pc.transmission_dispersion_scale = 1.0f;
    pc.transmission_dispersion_abbe_number = 30.0f;
    for (const auto* m : {&acrylic, &pc}) {
        CHECK(m->iorAt(587.6f) == doctest::Approx(m->specular_ior).epsilon(1e-5));
        const float V = (m->iorAt(587.6f) - 1.0f) / (m->iorAt(486.1f) - m->iorAt(656.3f));
        CHECK(V == doctest::Approx(m->transmission_dispersion_abbe_number).epsilon(1e-3));
    }
}
