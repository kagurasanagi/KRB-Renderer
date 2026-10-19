// 白い炉テスト（white furnace test）：一様な白い環境光（放射輝度 1）の中に球を置き、オフスクリーンで描く。
// 吸収のない材質なら、球の画素の値は 1 になる。1 を超えたらエネルギーが増えている（誤り）。
// 単一散乱のマイクロファセット（多重散乱の補償なし）や、層の重ね方の近似による損失は、許容範囲として記録する。
#include <doctest/doctest.h>

#include <cmath>
#include <cstdio>
#include <numbers>

#include "render/gpu_scene.hpp"
#include "render/renderer.hpp"
#include "scene/camera.hpp"

using namespace krbr;

namespace {

geom::Mesh uvSphere(float r, int segU, int segV) {
    geom::Mesh m;
    for (int j = 0; j <= segV; ++j) {
        const double th = std::numbers::pi * j / segV;
        for (int i = 0; i <= segU; ++i) {
            const double ph = 2 * std::numbers::pi * i / segU;
            const glm::vec3 n{static_cast<float>(std::sin(th) * std::cos(ph)), static_cast<float>(std::cos(th)),
                              static_cast<float>(std::sin(th) * std::sin(ph))};
            m.addVertex(n * r, n);
        }
    }
    for (int j = 0; j < segV; ++j)
        for (int i = 0; i < segU; ++i) {
            const uint32_t a = j * (segU + 1) + i, b = a + 1, c = a + segU + 1, d = c + 1;
            m.addTriangle(a, c, b, 0);
            m.addTriangle(b, c, d, 0);
        }
    return m;
}

struct FurnaceResult {
    double mean = 0, maxPixel = 0;
    glm::dvec3 meanRgb{0.0};
};

FurnaceResult furnace(vk::Context& ctx, const scene::Material& mat, uint32_t spp, bool spectral = false) {
    scene::Scene s;
    s.materials.push_back(mat);
    s.meshes.push_back({"sphere", uvSphere(10.0f, 96, 48)});
    s.instances.push_back({0, glm::mat4(1.0f), 0});
    s.layers.grid.width = s.layers.grid.height = 1;
    s.layers.grid.pxPerMm = 1.0;
    s.layers.rgba[0] = s.layers.rgba[1] = {0, 0, 0, 255};

    io::ImageF env;
    env.width = 64;
    env.height = 32;
    env.rgba.assign(64 * 32 * 4, 1.0f);
    const auto dist = render::buildEnvDistribution(env);

    render::GpuScene gpu;
    gpu.build(ctx, s, env, dist);
    render::RenderSettings rs;
    rs.width = rs.height = 96;
    rs.spp = spp;
    rs.frame.camera = scene::orbitCamera({0, 0, 0}, 60.0f, 20.0, 30.0, 25.0, 1.0f);
    rs.frame.maxDepth = 64;
    rs.frame.radianceClamp = 0.0f;  // 偏りのない結果で比べる
    rs.frame.spectral = spectral;
    const auto out = render::renderOffscreen(ctx, gpu, rs);
    gpu.destroy();

    FurnaceResult r;
    size_t n = 0;
    for (size_t i = 0; i < out.normal.rgba.size(); i += 4) {
        const float nl = out.normal.rgba[i] * out.normal.rgba[i] + out.normal.rgba[i + 1] * out.normal.rgba[i + 1] +
                         out.normal.rgba[i + 2] * out.normal.rgba[i + 2];
        if (nl < 0.25f) continue;  // 背景（環境マップが直接見えている画素）
        const double v = (out.color.rgba[i] + out.color.rgba[i + 1] + out.color.rgba[i + 2]) / 3.0;
        r.mean += v;
        r.meanRgb += glm::dvec3(out.color.rgba[i], out.color.rgba[i + 1], out.color.rgba[i + 2]);
        r.maxPixel = std::max(r.maxPixel, v);
        ++n;
    }
    if (n) {
        r.mean /= static_cast<double>(n);
        r.meanRgb /= static_cast<double>(n);
    }
    return r;
}

}  // namespace

TEST_CASE("white furnace test") {
    vk::Context ctx;
    ctx.init({});

    struct Case {
        const char* name;
        scene::Material m;
        double lo, hi;  // 平均の許容範囲（1 を超えないこと。下側は既知の損失）
        bool spectral;
    };
    std::vector<Case> cases;
    auto add = [&](const char* name, auto setup, double lo, double hi, bool spectral = false) {
        scene::Material m;
        m.base_color = glm::vec3(1.0f);
        setup(m);
        cases.push_back({name, m, lo, hi, spectral});
    };
    // 鏡面なしの白い拡散：ちょうど 1
    add("lambert", [](scene::Material& m) { m.specular_weight = 0.0f; }, 0.985, 1.015);
    // 完全な鏡（F0 = 1）：ちょうど 1
    add("mirror", [](scene::Material& m) {
        m.base_metalness = 1.0f;
        m.specular_roughness = 0.0f;
    }, 0.985, 1.015);
    // 粗い金属：多重散乱の補償がないので 1 より小さい（フェーズ8で補償する）
    add("metal_r0.3", [](scene::Material& m) {
        m.base_metalness = 1.0f;
        m.specular_roughness = 0.3f;
    }, 0.90, 1.015);
    // 粗さ 1（α = 1）の単一散乱 GGX（F = 1）の半球平均アルベドは数値積分で 0.409。それと一致すること
    add("metal_r1.0", [](scene::Material& m) {
        m.base_metalness = 1.0f;
        m.specular_roughness = 1.0f;
    }, 0.39, 0.43);
    // 拡散の透過（基材の透光性の仕組み）：吸収のない白なら、入って出ても 1
    add("translucent", [](scene::Material& m) {
        m.specular_weight = 0.0f;
        m.subsurface_weight = 0.3f;
        m.subsurface_color = glm::vec3(1.0f);
        m.subsurface_radius = 1000.0f;
    }, 0.985, 1.015);
    // 透明な誘電体（ガラス）：反射と屈折を合わせて 1。中では屈折率の 2 乗の分だけ放射輝度が変わり、出るときに戻る
    add("glass", [](scene::Material& m) {
        m.specular_roughness = 0.0f;
        m.transmission_weight = 1.0f;
    }, 0.985, 1.015);
    // 粗いガラス：単一散乱のマイクロファセットなので少し損失がある
    add("glass_r0.3", [](scene::Material& m) {
        m.specular_roughness = 0.3f;
        m.transmission_weight = 1.0f;
    }, 0.85, 1.015);
    // 白い誘電体（鏡面＋拡散）：層の重ね方の近似で少し損失がある
    add("dielectric_r0.3", [](scene::Material&) {}, 0.80, 1.015);
    // コートつきの白い拡散
    add("coated_lambert", [](scene::Material& m) {
        m.specular_weight = 0.0f;
        m.coat_weight = 1.0f;
        m.coat_roughness = 0.1f;
    }, 0.75, 1.015);

    // 分光モード：白い拡散と、分散のあるガラス（代表波長以外を打ち切っても、偏りなく 1）
    add("lambert (spec)", [](scene::Material& m) { m.specular_weight = 0.0f; }, 0.985, 1.015, true);
    add("acrylic (spec)", [](scene::Material& m) {
        m.specular_roughness = 0.0f;
        m.specular_ior = 1.491f;
        m.transmission_weight = 1.0f;
        m.transmission_dispersion_scale = 1.0f;
        m.transmission_dispersion_abbe_number = 57.0f;
    }, 0.98, 1.02, true);
    add("coated (spec)", [](scene::Material& m) {
        m.specular_weight = 0.0f;
        m.coat_weight = 1.0f;
        m.coat_roughness = 0.1f;
    }, 0.75, 1.015, true);

    std::printf("white furnace test (mean radiance on the sphere, ideal = 1):\n");
    for (const auto& c : cases) {
        const FurnaceResult r = furnace(ctx, c.m, 256, c.spectral);
        std::printf("  %-16s mean %.4f  (allowed %.3f .. %.3f)\n", c.name, r.mean, c.lo, c.hi);
        CHECK_MESSAGE(r.mean >= c.lo, c.name << " mean " << r.mean);
        CHECK_MESSAGE(r.mean <= c.hi, c.name << " gains energy: mean " << r.mean);
    }
}

// 分光モードでも、色のついた拡散は RGB モードと同じ色になる（RGB → スペクトル → XYZ → RGB の往復）
TEST_CASE("spectral color constancy in a white furnace") {
    vk::Context ctx;
    ctx.init({});
    const glm::vec3 colors[] = {{0.8f, 0.3f, 0.1f}, {0.1f, 0.4f, 0.8f}, {0.2f, 0.7f, 0.25f}};
    for (const glm::vec3 c : colors) {
        scene::Material m;
        m.specular_weight = 0.0f;
        m.base_color = c;
        const FurnaceResult rgb = furnace(ctx, m, 128, false);
        const FurnaceResult spec = furnace(ctx, m, 128, true);
        std::printf("  base %.2f %.2f %.2f: RGB %.4f %.4f %.4f, spectral %.4f %.4f %.4f\n", c.r, c.g, c.b, rgb.meanRgb.r,
                    rgb.meanRgb.g, rgb.meanRgb.b, spec.meanRgb.r, spec.meanRgb.g, spec.meanRgb.b);
        for (int k = 0; k < 3; ++k)
            CHECK_MESSAGE(std::abs(spec.meanRgb[k] - rgb.meanRgb[k]) < 0.02 + 0.03 * rgb.meanRgb[k],
                          "channel " << k << ": spectral " << spec.meanRgb[k] << " vs RGB " << rgb.meanRgb[k]);
    }
}
