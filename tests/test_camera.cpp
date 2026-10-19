// カメラ（フェーズ7）：点光源をぼかしたときのボケの形、画面の端のキャッツアイ、色収差による縁の色のにじみ、
// 周辺減光、薄レンズでの積分器の一致、実レンズの明るさとピント。画像は out/p7_*.png
#include <doctest/doctest.h>

#include <cmath>
#include <cstdio>
#include <filesystem>

#include "io/image_io.hpp"
#include "render/gpu_scene.hpp"
#include "render/renderer.hpp"
#include "test_geometry.hpp"

using namespace krbr;

namespace {

constexpr int W = 640, H = 400;
constexpr float kTanHalf = 0.25f, kAspect = 1.6f, kLightZ = -1000.0f;

scene::Camera testCamera() {
    scene::Camera c;
    c.position = {0, 0, 0};
    c.forward = {0, 0, -1};
    c.right = {1, 0, 0};
    c.up = {0, 1, 0};
    c.tanHalfFovY = kTanHalf;
    c.aspect = kAspect;
    c.distance = 250.0f;
    return c;
}

// 画面の正規化座標（-1..1、y は下向き）に写る、z = kLightZ の面の点
glm::vec3 pointAt(float ndcX, float ndcY) {
    return {ndcX * -kLightZ * kTanHalf * kAspect, -ndcY * -kLightZ * kTanHalf, kLightZ};
}
glm::vec2 pixelOf(float ndcX, float ndcY) { return {(ndcX + 1) * 0.5f * W, (ndcY + 1) * 0.5f * H}; }

struct PointLights {
    std::vector<glm::vec2> ndc;
    float sizeMm = 4.0f;
};

render::RenderOutput renderLights(vk::Context& ctx, const PointLights& pl, const scene::LensParams& lens, bool spectral,
                                  uint32_t spp, const char* integrator = "pt") {
    scene::Scene s;
    scene::Material e;
    e.name = "light";
    e.base_color = glm::vec3(0.0f);
    e.specular_weight = 0.0f;
    e.emission_luminance = 1000.0f;  // 放射輝度 1
    s.materials.push_back(e);
    geom::Mesh m;
    const float h = 0.5f * pl.sizeMm;
    for (glm::vec2 n : pl.ndc) {
        const glm::vec3 c = pointAt(n.x, n.y);
        test::addQuad(m, c + glm::vec3(-h, -h, 0), c + glm::vec3(h, -h, 0), c + glm::vec3(h, h, 0), c + glm::vec3(-h, h, 0),
                      {0, 0, 1}, 0);
    }
    test::finishScene(s, std::move(m));
    const io::ImageF env = test::uniformEnv(0.0f);
    render::GpuScene gpu;
    gpu.build(ctx, s, env, render::buildEnvDistribution(env));
    render::RenderSettings rs;
    rs.width = W;
    rs.height = H;
    rs.spp = spp;
    rs.integrator = integrator;
    rs.frame.camera = testCamera();
    rs.frame.lens = lens;
    rs.frame.spectral = spectral;
    rs.frame.maxDepth = 4;
    rs.frame.radianceClamp = 0.0f;
    auto out = render::renderOffscreen(ctx, gpu, rs);
    gpu.destroy();
    return out;
}

// チャンネル k（3 なら輝度）の値（3x3 の平均でならす）
float valueAt(const io::ImageF& img, int x, int y, int k) {
    float s = 0;
    int n = 0;
    for (int dy = -1; dy <= 1; ++dy)
        for (int dx = -1; dx <= 1; ++dx) {
            const int xx = x + dx, yy = y + dy;
            if (xx < 0 || yy < 0 || xx >= img.width || yy >= img.height) continue;
            const float* p = &img.rgba[(static_cast<size_t>(yy) * img.width + xx) * 4];
            s += k < 3 ? p[k] : (p[0] + p[1] + p[2]) / 3.0f;
            ++n;
        }
    return n ? s / n : 0.0f;
}

struct Bokeh {
    glm::vec2 centroid{0.0f};
    std::vector<float> radius;  // 1° ごとの、重心から縁までの距離
    double varAlong = 0, varAcross = 0;  // 画面中心から重心への向きと、それに直交する向きの分散
    double meanRadius = 0;
};

// center のまわり win 画素の範囲で、明るさの半分を境にボケの形を測る
Bokeh measureBokeh(const io::ImageF& img, glm::vec2 center, int win, int k = 3) {
    Bokeh b;
    float peak = 0;
    for (int y = int(center.y) - win; y <= int(center.y) + win; ++y)
        for (int x = int(center.x) - win; x <= int(center.x) + win; ++x) peak = std::max(peak, valueAt(img, x, y, k));
    // 内側の平均（最大値の 30% を超える画素）を基準にして、その半分を境とする
    double sumIn = 0;
    int nIn = 0;
    for (int y = int(center.y) - win; y <= int(center.y) + win; ++y)
        for (int x = int(center.x) - win; x <= int(center.x) + win; ++x)
            if (const float v = valueAt(img, x, y, k); v > 0.3f * peak) {
                sumIn += v;
                ++nIn;
            }
    const float thr = static_cast<float>(0.5 * sumIn / std::max(nIn, 1));
    double sx = 0, sy = 0;
    int n = 0;
    std::vector<glm::vec2> pts;
    for (int y = int(center.y) - win; y <= int(center.y) + win; ++y)
        for (int x = int(center.x) - win; x <= int(center.x) + win; ++x)
            if (valueAt(img, x, y, k) > thr) {
                sx += x + 0.5;
                sy += y + 0.5;
                ++n;
                pts.push_back({x + 0.5f, y + 0.5f});
            }
    b.centroid = n ? glm::vec2(sx / n, sy / n) : center;
    glm::vec2 along = b.centroid - glm::vec2(W * 0.5f, H * 0.5f);
    along = glm::length(along) > 1 ? glm::normalize(along) : glm::vec2(1, 0);
    const glm::vec2 across(-along.y, along.x);
    for (auto p : pts) {
        const glm::vec2 d = p - b.centroid;
        b.varAlong += glm::dot(d, along) * glm::dot(d, along);
        b.varAcross += glm::dot(d, across) * glm::dot(d, across);
    }
    for (int a = 0; a < 360; ++a) {
        const float th = glm::radians(static_cast<float>(a));
        const glm::vec2 dir(std::cos(th), -std::sin(th));  // 画面の y は下向きなので、角度は上向きを正にする
        float r = 0;
        int miss = 0;
        for (float t = 0; t < win; t += 0.5f) {
            const glm::vec2 p = b.centroid + dir * t;
            if (valueAt(img, int(p.x), int(p.y), k) > thr) {
                r = t;
                miss = 0;
            } else if (++miss > 4) {
                break;
            }
        }
        b.radius.push_back(r);
        b.meanRadius += r / 360.0;
    }
    return b;
}

// 角度 a0 から step ごとの方向の、半径の平均（±3° の平均）
double radiusAt(const Bokeh& b, double a0, double step, int count) {
    double s = 0;
    int n = 0;
    for (int i = 0; i < count; ++i)
        for (int d = -3; d <= 3; ++d) {
            int a = static_cast<int>(std::lround(a0 + step * i)) + d;
            a = ((a % 360) + 360) % 360;
            s += b.radius[a];
            ++n;
        }
    return s / n;
}

std::filesystem::path outDir() {
    const auto p = std::filesystem::path(KRBR_SOURCE_DIR) / "out";
    std::filesystem::create_directories(p);
    return p;
}

scene::LensParams thinLens(float fNumber, float focus) {
    scene::LensParams l;
    l.model = scene::LensModel::Thin;
    l.focalLengthMm = 50.0f;
    l.fNumber = fNumber;
    l.focusDistanceMm = focus;
    return l;
}

}  // namespace

TEST_CASE("thin lens bokeh: aperture blades") {
    vk::Context ctx;
    ctx.init({});
    PointLights pl;
    pl.ndc = {{0, 0}};
    // 絞り F2（半径 12.5 mm）、ピント 250 mm、光源 1000 mm：ボケの直径は約 2a(1/250 − 1/1000) = 0.075（tan）≈ 60 画素
    std::string err;
    struct Case {
        int blades;
        float rotation;
        const char* file;
    };
    for (const Case c : {Case{0, 0.0f, "p7_bokeh_circle.png"}, Case{6, 0.0f, "p7_bokeh_hex.png"},
                         Case{6, 30.0f, "p7_bokeh_hex_rot30.png"}}) {
        scene::LensParams lens = thinLens(2.0f, 250.0f);
        lens.blades = c.blades;
        lens.bladeRotationDeg = c.rotation;
        const auto out = renderLights(ctx, pl, lens, false, 2048);
        io::savePngTonemapped(outDir() / c.file, out.color, {5.0f}, err);
        const Bokeh b = measureBokeh(out.color, pixelOf(0, 0), 60);
        const double rMax = *std::max_element(b.radius.begin(), b.radius.end());
        const double rMin = *std::min_element(b.radius.begin(), b.radius.end());
        // 頂点の方向（回転の角度から 60° ごと）と、辺の中央の方向（さらに 30° ずれる）の半径
        const double rVertex = radiusAt(b, c.rotation, 60, 6), rEdge = radiusAt(b, c.rotation + 30, 60, 6);
        std::printf("bokeh blades %d rot %.0f: mean radius %.1f px, max/min %.3f, vertex/edge %.3f (hexagon 1.155)\n",
                    c.blades, c.rotation, b.meanRadius, rMax / rMin, rVertex / rEdge);
        CHECK(b.meanRadius > 22.0);
        CHECK(b.meanRadius < 40.0);
        if (c.blades == 0) {
            CHECK(rVertex / rEdge == doctest::Approx(1.0).epsilon(0.03));
        } else {
            CHECK(rVertex / rEdge > 1.08);
            CHECK(rVertex / rEdge < 1.20);
        }
    }
}

TEST_CASE("thin lens: cat's eye bokeh near the corner, natural vignetting") {
    vk::Context ctx;
    ctx.init({});
    PointLights pl;
    pl.ndc = {{0.8f, 0.75f}};
    std::string err;
    double ratio[2];
    for (int i = 0; i < 2; ++i) {
        scene::LensParams lens = thinLens(2.0f, 250.0f);
        lens.opticalVignetting = i == 0 ? 0.0f : 0.6f;
        const auto out = renderLights(ctx, pl, lens, false, 2048);
        if (i == 1) io::savePngTonemapped(outDir() / "p7_cats_eye.png", out.color, {5.0f}, err);
        const Bokeh b = measureBokeh(out.color, pixelOf(0.8f, 0.75f), 60);
        ratio[i] = std::sqrt(b.varAlong / b.varAcross);
        std::printf("corner bokeh, optical vignetting %.1f: radial / tangential extent %.3f\n", lens.opticalVignetting,
                    ratio[i]);
    }
    CHECK(ratio[0] == doctest::Approx(1.0).epsilon(0.05));
    CHECK(ratio[1] < 0.8);  // 画面中心の向きにつぶれたレモン形

    // 自然な周辺減光：一様な環境光を見ると、隅は cos⁴θ だけ暗くなる
    scene::Scene s;
    s.materials.push_back({});
    geom::Mesh m;
    test::addQuad(m, {-1, -1, 5000}, {1, -1, 5000}, {1, 1, 5000}, {-1, 1, 5000}, {0, 0, -1}, 0);  // 後ろの小さな板（写らない）
    test::finishScene(s, std::move(m));
    const io::ImageF env = test::uniformEnv(1.0f);
    render::GpuScene gpu;
    gpu.build(ctx, s, env, render::buildEnvDistribution(env));
    render::RenderSettings rs;
    rs.width = W;
    rs.height = H;
    rs.spp = 16;
    rs.frame.camera = testCamera();
    rs.frame.lens.naturalVignetting = true;
    const auto out = render::renderOffscreen(ctx, gpu, rs);
    gpu.destroy();
    const float center = valueAt(out.color, W / 2, H / 2, 3), corner = valueAt(out.color, 1, 1, 3);
    const double tanCorner = kTanHalf * std::sqrt(1.0 + kAspect * kAspect) * (1.0 - 2.0 / H);
    const double expected = std::pow(1.0 / (1.0 + tanCorner * tanCorner), 2.0);
    std::printf("natural vignetting: corner / center %.4f (cos^4 %.4f)\n", corner / center, expected);
    CHECK(corner / center == doctest::Approx(expected).epsilon(0.02));
}

TEST_CASE("thin lens: chromatic aberration in spectral mode") {
    vk::Context ctx;
    ctx.init({});
    std::string err;
    // 軸上色収差：ぼかした点光源のボケの大きさが、青（短い波長でピントが手前）で大きく、赤で小さい
    {
        PointLights pl;
        pl.ndc = {{0, 0}};
        double ratio[2];
        for (int i = 0; i < 2; ++i) {
            scene::LensParams lens = thinLens(2.0f, 250.0f);
            lens.abbeNumber = i == 0 ? 0.0f : 20.0f;
            const auto out = renderLights(ctx, pl, lens, true, 4096);
            if (i == 1) io::savePngTonemapped(outDir() / "p7_axial_ca.png", out.color, {5.0f}, err);
            const Bokeh r = measureBokeh(out.color, pixelOf(0, 0), 70, 0), b = measureBokeh(out.color, pixelOf(0, 0), 70, 2);
            ratio[i] = b.meanRadius / r.meanRadius;
            std::printf("axial CA (Abbe %.0f): bokeh radius blue %.1f px, red %.1f px, ratio %.3f\n", lens.abbeNumber,
                        b.meanRadius, r.meanRadius, ratio[i]);
        }
        CHECK(ratio[0] == doctest::Approx(1.0).epsilon(0.03));
        CHECK(ratio[1] > 1.05);
    }
    // 倍率色収差：ピントの合った点光源（画面の端）で、青の像が赤より外側にずれる
    {
        PointLights pl;
        pl.ndc = {{0.85f, 0.0f}};
        pl.sizeMm = 2.0f;
        double shift[2];
        for (int i = 0; i < 2; ++i) {
            scene::LensParams lens = thinLens(16.0f, 1000.0f);
            lens.lateralCA = i == 0 ? 0.0f : 0.01f;
            const auto out = renderLights(ctx, pl, lens, true, 1024);
            if (i == 1) io::savePngTonemapped(outDir() / "p7_lateral_ca.png", out.color, {5.0f}, err);
            const Bokeh r = measureBokeh(out.color, pixelOf(0.85f, 0), 20, 0), b = measureBokeh(out.color, pixelOf(0.85f, 0), 20, 2);
            shift[i] = b.centroid.x - r.centroid.x;
            std::printf("lateral CA %.3f: blue - red image position %.2f px (expected ~%.2f)\n", lens.lateralCA, shift[i],
                        lens.lateralCA * 0.85 * W / 2);
        }
        CHECK(std::abs(shift[0]) < 0.3);
        CHECK(shift[1] > 1.0);
    }
}

TEST_CASE("thin lens: integrators agree, realistic lens brightness and focus") {
    vk::Context ctx;
    ctx.init({});
    // 薄レンズ（周辺減光・口径食あり）で、光源側からカメラへ直接つなぐ戦略の確率密度が合っていること
    scene::Scene s;
    scene::Material floor;
    floor.base_color = glm::vec3(0.6f);
    floor.specular_weight = 0.0f;
    scene::Material light = floor;
    light.base_color = glm::vec3(0.0f);
    light.emission_luminance = 20000.0f;
    s.materials = {floor, light};
    geom::Mesh m;
    test::addQuad(m, {-200, -60, -600}, {200, -60, -600}, {200, -60, 0}, {-200, -60, 0}, {0, 1, 0}, 0);
    test::addBox(m, {-40, -60, -320}, {0, -20, -280}, 0);
    test::addQuad(m, {-15, 40, -330}, {15, 40, -330}, {15, 40, -300}, {-15, 40, -300}, {0, -1, 0}, 1);
    test::finishScene(s, std::move(m));
    const io::ImageF env = test::uniformEnv(0.02f);
    render::GpuScene gpu;
    gpu.build(ctx, s, env, render::buildEnvDistribution(env));
    scene::LensParams lens = thinLens(4.0f, 260.0f);
    lens.blades = 6;
    lens.naturalVignetting = true;
    lens.opticalVignetting = 0.5f;
    double mean[3];
    const char* names[3] = {"pt", "bdpt", "vcm"};
    for (int i = 0; i < 3; ++i) {
        render::RenderSettings rs;
        rs.width = 160;
        rs.height = 100;
        rs.spp = i == 0 ? 2048 : 1024;
        rs.integrator = names[i];
        rs.frame.camera = testCamera();
        rs.frame.lens = lens;
        rs.frame.maxDepth = 8;
        rs.frame.radianceClamp = 0.0f;
        const auto out = render::renderOffscreen(ctx, gpu, rs);
        double sum = 0;
        for (size_t k = 0; k < out.color.rgba.size(); k += 4) sum += out.color.rgba[k] + out.color.rgba[k + 1] + out.color.rgba[k + 2];
        mean[i] = sum / (out.color.rgba.size() / 4 * 3);
    }
    gpu.destroy();
    std::printf("thin lens agreement: mean pt %.5f, bdpt %.5f, vcm %.5f\n", mean[0], mean[1], mean[2]);
    CHECK(mean[1] / mean[0] == doctest::Approx(1.0).epsilon(0.015));
    CHECK(mean[2] / mean[0] == doctest::Approx(1.0).epsilon(0.025));

    // 実レンズ（内蔵の double Gauss 50mm f/2）：一様な環境光で、画面の中央の明るさが 1（ピンホールと同じ）
    {
        scene::Scene s2;
        s2.materials.push_back({});
        geom::Mesh m2;
        test::addQuad(m2, {-1, -1, 5000}, {1, -1, 5000}, {1, 1, 5000}, {-1, 1, 5000}, {0, 0, -1}, 0);
        test::finishScene(s2, std::move(m2));
        const io::ImageF env2 = test::uniformEnv(1.0f);
        render::GpuScene g2;
        g2.build(ctx, s2, env2, render::buildEnvDistribution(env2));
        render::RenderSettings rs;
        rs.width = W;
        rs.height = H;
        rs.spp = 256;
        rs.frame.camera = testCamera();
        rs.frame.lens.model = scene::LensModel::Realistic;
        rs.frame.lens.focusDistanceMm = 1000.0f;
        const auto out = render::renderOffscreen(ctx, g2, rs);
        g2.destroy();
        float c = 0;
        for (int dy = -4; dy <= 4; ++dy)
            for (int dx = -4; dx <= 4; ++dx) c += valueAt(out.color, W / 2 + dx, H / 2 + dy, 3) / 81.0f;
        const float corner = valueAt(out.color, 2, 2, 3);
        std::printf("realistic lens, uniform environment: center %.4f, corner %.4f\n", c, corner);
        CHECK(c == doctest::Approx(1.0).epsilon(0.05));
        CHECK(corner < c);  // 周辺減光
    }
    // 実レンズのピント：ピントの合った点光源は小さく明るく、外すと広がる
    {
        PointLights pl;
        pl.ndc = {{0, 0}};
        pl.sizeMm = 2.0f;
        double peak[2];
        for (int i = 0; i < 2; ++i) {
            scene::LensParams l;
            l.model = scene::LensModel::Realistic;
            l.focusDistanceMm = i == 0 ? 1000.0f : 300.0f;
            const auto out = renderLights(ctx, pl, l, false, 1024);
            std::string err;
            io::savePngTonemapped(outDir() / (i == 0 ? "p7_real_focused.png" : "p7_real_defocused.png"), out.color, {5.0f},
                                  err);
            peak[i] = 0;
            for (int y = H / 2 - 30; y < H / 2 + 30; ++y)
                for (int x = W / 2 - 30; x < W / 2 + 30; ++x) peak[i] = std::max<double>(peak[i], valueAt(out.color, x, y, 3));
        }
        std::printf("realistic lens: peak in focus %.4f, out of focus %.4f\n", peak[0], peak[1]);
        CHECK(peak[0] > 4.0 * peak[1]);
    }
}

// 完了条件の確認用の画像：多角形のボケ、画面の端のキャッツアイ、縁の色のにじみ（分光モード）。ボケは光が広がって暗いので +5 EV で保存
TEST_CASE("camera showcase image") {
    vk::Context ctx;
    ctx.init({});
    PointLights pl;
    for (int j = 0; j < 3; ++j)
        for (int i = 0; i < 5; ++i) pl.ndc.push_back({-0.8f + 0.4f * i, -0.7f + 0.7f * j});
    pl.sizeMm = 6.0f;
    scene::LensParams lens = thinLens(2.0f, 300.0f);
    lens.blades = 7;
    lens.bladeRotationDeg = 10.0f;
    lens.bladeRoundness = 0.2f;
    lens.naturalVignetting = true;
    lens.opticalVignetting = 0.6f;
    lens.abbeNumber = 25.0f;
    lens.lateralCA = 0.006f;
    const auto out = renderLights(ctx, pl, lens, true, 32768);
    std::string err;
    CHECK(io::savePngTonemapped(outDir() / "p7_showcase.png", out.color, {4.0f}, err));
}

