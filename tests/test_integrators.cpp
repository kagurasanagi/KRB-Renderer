// 積分器の一致テストと、集光模様の収束（フェーズ6の完了条件）
//  - 一致：面光源・相互反射・光沢のあるシーンを PT / BDPT / VCM で描き、平均と画素ごとの差が小さいこと
//  - 集光模様：透明なふた（アクリル板）越しに、ボールレンズが LED の光を床に集める様子を見る。
//    カメラ → 板（鏡面）→ 床（拡散）→ レンズ（鏡面）→ LED の経路は、PT・BDPT では極めて収束しにくく、VCM では収束する。
//    同じ条件で 2 回（乱数の種を変えて）描き、その差（ノイズ）を比べる。画像は out/p6_caustic_*.png
#include <doctest/doctest.h>

#include <chrono>
#include <cmath>
#include <cstdio>
#include <filesystem>

#include "io/image_io.hpp"
#include "render/gpu_scene.hpp"
#include "render/renderer.hpp"
#include "scene/material_library.hpp"
#include "test_geometry.hpp"

using namespace krbr;

namespace {

scene::Material diffuse(const char* name, glm::vec3 c) {
    scene::Material m;
    m.name = name;
    m.base_color = c;
    m.specular_weight = 0.0f;
    return m;
}

scene::Material emitter(const char* name, float luminance) {
    scene::Material m = diffuse(name, glm::vec3(0.0f));
    m.emission_luminance = luminance;
    return m;
}

struct Window {
    int x0 = 0, y0 = 0, x1 = 1 << 30, y1 = 1 << 30;
};

// 4x4 画素ごとの平均を取った画像の、相対的な差（RMS / 平均）。win の範囲だけを比べる
double relativeRmse(const io::ImageF& a, const io::ImageF& b, double* meanA = nullptr, double* meanB = nullptr,
                    Window win = {}) {
    const int bs = 4;
    double se = 0, sa = 0, sb = 0;
    int n = 0;
    for (int by = std::max(0, win.y0); by + bs <= std::min(a.height, win.y1); by += bs)
        for (int bx = std::max(0, win.x0); bx + bs <= std::min(a.width, win.x1); bx += bs) {
            double va = 0, vb = 0;
            for (int y = by; y < by + bs; ++y)
                for (int x = bx; x < bx + bs; ++x)
                    for (int c = 0; c < 3; ++c) {
                        const size_t i = (static_cast<size_t>(y) * a.width + x) * 4 + c;
                        va += a.rgba[i];
                        vb += b.rgba[i];
                    }
            va /= bs * bs * 3;
            vb /= bs * bs * 3;
            se += (va - vb) * (va - vb);
            sa += va;
            sb += vb;
            ++n;
        }
    if (meanA) *meanA = sa / n;
    if (meanB) *meanB = sb / n;
    const double mean = 0.5 * (sa + sb) / n;
    return std::sqrt(se / n) / std::max(mean, 1e-9);
}

render::RenderOutput renderWith(vk::Context& ctx, const render::GpuScene& gpu, const scene::Camera& cam, int w, int h,
                                uint32_t spp, const char* integrator, uint32_t seed, double* seconds = nullptr) {
    render::RenderSettings rs;
    rs.width = w;
    rs.height = h;
    rs.spp = spp;
    rs.integrator = integrator;
    rs.frame.camera = cam;
    rs.frame.maxDepth = 12;
    rs.frame.rrStartDepth = 12;
    rs.frame.radianceClamp = 0.0f;
    rs.frame.seed = seed;
    auto out = render::renderOffscreen(ctx, gpu, rs);
    if (seconds) *seconds = out.seconds;
    return out;
}

}  // namespace

TEST_CASE("integrators agree: pt, bdpt, vcm") {
    vk::Context ctx;
    ctx.init({});
    scene::Scene s;
    s.materials = {diffuse("floor", glm::vec3(0.6f)), diffuse("wall", glm::vec3(0.5f, 0.55f, 0.6f)),
                   diffuse("box", glm::vec3(0.7f, 0.3f, 0.2f)), emitter("light", 20000.0f), {}};
    s.materials[4].name = "metal";
    s.materials[4].base_color = glm::vec3(0.9f, 0.8f, 0.6f);
    s.materials[4].base_metalness = 1.0f;
    s.materials[4].specular_roughness = 0.3f;
    geom::Mesh m;
    test::addQuad(m, {-100, 0, -100}, {100, 0, -100}, {100, 0, 100}, {-100, 0, 100}, {0, 1, 0}, 0);
    test::addQuad(m, {-100, 0, -40}, {100, 0, -40}, {100, 120, -40}, {-100, 120, -40}, {0, 0, 1}, 1);
    test::addBox(m, {-35, 0, -20}, {-5, 30, 10}, 2);
    test::addSphere(m, {25, 15, 0}, 15, 4);
    test::addQuad(m, {-10, 70, -10}, {10, 70, -10}, {10, 70, 10}, {-10, 70, 10}, {0, -1, 0}, 3);
    test::finishScene(s, std::move(m));
    const io::ImageF env = test::uniformEnv(0.05f);
    render::GpuScene gpu;
    gpu.build(ctx, s, env, render::buildEnvDistribution(env));
    const scene::Camera cam = scene::orbitCamera({0, 25, 0}, 220.0f, 25.0, 15.0, 40.0, 4.0f / 3.0f);

    const auto pt = renderWith(ctx, gpu, cam, 160, 120, 2048, "pt", 1);
    const auto bdpt = renderWith(ctx, gpu, cam, 160, 120, 1024, "bdpt", 2);
    const auto vcm = renderWith(ctx, gpu, cam, 160, 120, 1024, "vcm", 3);
    const auto pt2 = renderWith(ctx, gpu, cam, 160, 120, 2048, "pt", 4);
    gpu.destroy();

    double mPt = 0, mB = 0, mV = 0, mPt2 = 0;
    const double ePt = relativeRmse(pt.color, pt2.color, &mPt, &mPt2);
    const double eB = relativeRmse(pt.color, bdpt.color, nullptr, &mB);
    const double eV = relativeRmse(pt.color, vcm.color, nullptr, &mV);
    std::printf("integrator agreement: mean pt %.5f, bdpt %.5f, vcm %.5f; rel. RMSE vs pt: pt(seed 2) %.4f, bdpt %.4f, "
                "vcm %.4f\n",
                mPt, mB, mV, ePt, eB, eV);
    std::string err;
    const auto out = std::filesystem::path(KRBR_SOURCE_DIR) / "out";
    std::filesystem::create_directories(out);
    io::savePngTonemapped(out / "p6_agree_pt.png", pt.color, {0.0f}, err);
    io::savePngTonemapped(out / "p6_agree_bdpt.png", bdpt.color, {0.0f}, err);
    io::savePngTonemapped(out / "p6_agree_vcm.png", vcm.color, {0.0f}, err);
    CHECK(std::abs(mB / mPt - 1.0) < 0.01);
    CHECK(std::abs(mV / mPt - 1.0) < 0.02);
    // 画素ごとの差は、PT どうし（乱数の種だけ違う）のノイズの大きさと同程度
    CHECK(eB < 2.0 * ePt + 0.01);
    CHECK(eV < 2.0 * ePt + 0.02);
}

TEST_CASE("caustics through a transparent cover converge with vcm") {
    vk::Context ctx;
    ctx.init({});
    const scene::MaterialLibrary lib = scene::MaterialLibrary::builtin();
    scene::Scene s;
    s.materials = {diffuse("board", glm::vec3(0.15f, 0.45f, 0.2f)), emitter("led", 2.0e6f), *lib.find("acrylic"),
                   *lib.find("acrylic")};
    // ボールレンズ（半径 12、中心の高さ 30、焦点距離 nR / 2(n−1) ≈ 18 mm）が、高さ 76 の LED の像を床に結ぶ
    geom::Mesh m;
    test::addQuad(m, {-150, 0, -150}, {150, 0, -150}, {150, 0, 150}, {-150, 0, 150}, {0, 1, 0}, 0);
    test::addQuad(m, {-1.5f, 76, -1.5f}, {1.5f, 76, -1.5f}, {1.5f, 76, 1.5f}, {-1.5f, 76, 1.5f}, {0, -1, 0}, 1);
    test::addSphere(m, {0, 30, 0}, 12, 2);                   // ボールレンズ
    test::addBox(m, {-160, 90, -160}, {160, 94, 160}, 3);        // 筐体のふた（アクリル板）
    test::finishScene(s, std::move(m));
    const io::ImageF env = test::uniformEnv(0.0f);
    render::GpuScene gpu;
    gpu.build(ctx, s, env, render::buildEnvDistribution(env));
    const int W = 320, H = 240;
    const scene::Camera cam = scene::orbitCamera({0, 0, 0}, 230.0f, 50.0, 0.0, 40.0, static_cast<float>(W) / H);
    // 集光点（床の原点）のまわりの 48 x 48 画素で比べる（原点は画面の中央に写る）
    const Window win{W / 2 - 24, H / 2 - 24, W / 2 + 24, H / 2 + 24};

    const auto out = std::filesystem::path(KRBR_SOURCE_DIR) / "out";
    std::filesystem::create_directories(out);
    struct Res {
        const char* name;
        double noise, mean, seconds;
    };
    std::vector<Res> res;
    for (const char* integ : {"pt", "bdpt", "vcm"}) {
        double t1 = 0, t2 = 0, mA = 0, mB = 0;
        const auto a = renderWith(ctx, gpu, cam, W, H, 256, integ, 11, &t1);
        const auto b = renderWith(ctx, gpu, cam, W, H, 256, integ, 12, &t2);
        const double noise = relativeRmse(a.color, b.color, &mA, &mB, win);
        res.push_back({integ, noise, 0.5 * (mA + mB), 0.5 * (t1 + t2)});
        std::string err;
        io::savePngTonemapped(out / (std::string("p6_caustic_") + integ + ".png"), a.color, {0.0f}, err);
    }
    // 正しい値に収束していること：十分なサンプル数の PT と、集光点のまわりの平均を比べる（VCM の偏りは半径の分だけ）
    double mRef = 0, mVcm = 0;
    {
        const auto ref = renderWith(ctx, gpu, cam, W, H, 16384, "pt", 21);
        const auto vcm = renderWith(ctx, gpu, cam, W, H, 1024, "vcm", 22);
        relativeRmse(ref.color, vcm.color, &mRef, &mVcm, win);
        std::string err;
        io::savePngTonemapped(out / "p6_caustic_vcm_1024.png", vcm.color, {0.0f}, err);
        io::savePngTonemapped(out / "p6_caustic_pt_16384.png", ref.color, {0.0f}, err);
    }
    gpu.destroy();
    std::printf("caustic scene: mean around the caustic, pt 16384 spp %.4f, vcm 1024 spp %.4f\n", mRef, mVcm);
    CHECK(std::abs(mVcm / mRef - 1.0) < 0.05);
    for (const auto& r : res)
        std::printf("caustic scene, %-4s: noise around the caustic (rel. RMSE between two seeds) %.3f, mean %.4f, "
                    "%.2f s for 256 spp\n",
                    r.name, r.noise, r.mean, r.seconds);
    // VCM は PT・BDPT よりはっきりノイズが少ない（収束する）
    CHECK(res[2].noise < 0.5 * res[0].noise);
    CHECK(res[2].noise < 0.5 * res[1].noise);
    CHECK(res[2].noise < 0.15);
}
