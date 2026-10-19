// 分散の検証シーン（フェーズ5の完了条件）：アクリル（nd 1.491、アッベ数 57）とポリカーボネート（nd 1.585、アッベ数 30）の
// くさび形のプリズムを上下に並べ、その奥の縦縞の光源を透かして見る。分光モードでは、縞の縁が波長ごとにずれて色が分かれる。
// 赤と青の成分の横方向のずれ（画素）を相互相関で測り、ポリカーボネートはアクリルの約 2.3 倍（(nF−nC) の比）になることを確かめる。
// RGB モードでは分散がないので、ずれは 0。画像は out/p5_prisms_{spectral,rgb}.png に書く。
#include <doctest/doctest.h>

#include <cmath>
#include <cstdio>
#include <filesystem>

#include "io/image_io.hpp"
#include "render/gpu_scene.hpp"
#include "render/renderer.hpp"
#include "scene/material_library.hpp"

using namespace krbr;

namespace {

// 凸な立体の三角形を、外向きの法線（重心から離れる向き）になるように足す
void addConvexTri(geom::Mesh& m, glm::vec3 a, glm::vec3 b, glm::vec3 c, glm::vec3 centroid, uint32_t mat) {
    glm::vec3 n = glm::normalize(glm::cross(b - a, c - a));
    if (glm::dot(n, (a + b + c) / 3.0f - centroid) < 0) {
        std::swap(b, c);
        n = -n;
    }
    const uint32_t i = m.addVertex(a, n), j = m.addVertex(b, n), k = m.addVertex(c, n);
    m.addTriangle(i, j, k, mat);
}

// XZ 面の凸多角形を Y 方向に押し出した立体
void addPrism(geom::Mesh& m, const std::vector<glm::vec2>& xz, float y0, float y1, uint32_t mat) {
    glm::vec3 centroid(0.0f);
    for (auto p : xz) centroid += glm::vec3(p.x, 0.5f * (y0 + y1), p.y);
    centroid /= static_cast<float>(xz.size());
    const size_t n = xz.size();
    for (size_t i = 0; i < n; ++i) {
        const glm::vec2 p = xz[i], q = xz[(i + 1) % n];
        const glm::vec3 a(p.x, y0, p.y), b(q.x, y0, q.y), c(q.x, y1, q.y), d(p.x, y1, p.y);
        addConvexTri(m, a, b, c, centroid, mat);
        addConvexTri(m, a, c, d, centroid, mat);
    }
    for (size_t i = 1; i + 1 < n; ++i)
        for (float y : {y0, y1})
            addConvexTri(m, {xz[0].x, y, xz[0].y}, {xz[i].x, y, xz[i].y}, {xz[i + 1].x, y, xz[i + 1].y}, centroid, mat);
}

struct Shift {
    double acrylic = 0, polycarbonate = 0;
};

// 行ごとに、赤と青の横方向のずれ（画素、小数まで）を正規化相互相関で求めて平均する
double channelShift(const io::ImageF& img, int row0, int row1, int col0, int col1) {
    double sum = 0;
    int rows = 0;
    const int maxShift = 45;
    for (int y = row0; y < row1; ++y) {
        auto px = [&](int x, int c) { return static_cast<double>(img.rgba[(static_cast<size_t>(y) * img.width + x) * 4 + c]); };
        // 平均を引いてから相関をとる
        double mr = 0, mb = 0;
        for (int x = col0; x < col1; ++x) {
            mr += px(x, 0);
            mb += px(x, 2);
        }
        mr /= (col1 - col0);
        mb /= (col1 - col0);
        std::vector<double> corr(2 * maxShift + 1);
        for (int s = -maxShift; s <= maxShift; ++s) {
            double c = 0, er = 0, eb = 0;
            for (int x = col0 + maxShift; x < col1 - maxShift; ++x) {
                const double r = px(x, 0) - mr, b = px(x + s, 2) - mb;
                c += r * b;
                er += r * r;
                eb += b * b;
            }
            corr[s + maxShift] = c / std::sqrt(std::max(er * eb, 1e-30));
        }
        int best = 0;
        for (int i = 1; i < static_cast<int>(corr.size()); ++i)
            if (corr[i] > corr[best]) best = i;
        double sub = best - maxShift;
        if (best > 0 && best + 1 < static_cast<int>(corr.size())) {
            const double a = corr[best - 1], b = corr[best], c = corr[best + 1];
            const double d = a - 2 * b + c;
            if (d < 0) sub += 0.5 * (a - c) / d;
        }
        sum += sub;
        ++rows;
    }
    return rows ? sum / rows : 0.0;
}

}  // namespace

TEST_CASE("prism dispersion: acrylic vs polycarbonate") {
    vk::Context ctx;
    ctx.init({});

    const scene::MaterialLibrary lib = scene::MaterialLibrary::builtin();
    scene::Scene s;
    scene::Material emitter;
    emitter.name = "slit_light";
    emitter.base_color = glm::vec3(0.0f);
    emitter.specular_weight = 0.0f;
    emitter.emission_luminance = 1000.0f;  // 放射輝度 1
    s.materials.push_back(emitter);
    s.materials.push_back(*lib.find("acrylic"));
    s.materials.push_back(*lib.find("polycarbonate"));

    geom::Mesh mesh;
    // 遠くの縦縞の光源（幅 18 mm、間隔 72 mm。画面上で約 100 画素ごと）
    const float zBars = -4000.0f;
    for (float x = -1500.0f; x < 1500.0f; x += 72.0f) {
        const glm::vec3 a(x, -600, zBars), b(x + 18, -600, zBars), c(x + 18, 600, zBars), d(x, 600, zBars);
        const glm::vec3 n(0, 0, 1);
        const uint32_t i = mesh.addVertex(a, n), j = mesh.addVertex(b, n), k = mesh.addVertex(c, n), l = mesh.addVertex(d, n);
        mesh.addTriangle(i, j, k, 0);
        mesh.addTriangle(i, k, l, 0);
    }
    // くさび形のプリズム（頂角 12°、手前の面は z = 0）。上がアクリル、下がポリカーボネート。
    // 頂角が小さい（薄いプリズム）ので、色の分かれ方は (nF − nC) にほぼ比例し、形の違いの影響が小さい
    const float W = 30.0f, t0 = 1.0f, tanA = std::tan(glm::radians(12.0f));
    const std::vector<glm::vec2> wedge = {{-W / 2, 0.0f}, {W / 2, 0.0f}, {W / 2, -(t0 + W * tanA)}, {-W / 2, -t0}};
    addPrism(mesh, wedge, 2.0f, 14.0f, 1);
    addPrism(mesh, wedge, -14.0f, -2.0f, 2);
    s.meshes.push_back({"prism_scene", mesh});
    s.instances.push_back({0, glm::mat4(1.0f), 0});
    s.layers.grid.width = s.layers.grid.height = 1;
    s.layers.grid.pxPerMm = 1.0;
    s.layers.rgba[0] = s.layers.rgba[1] = {0, 0, 0, 255};

    io::ImageF env;  // 真っ暗な環境（光源は縞だけ）
    env.width = 16;
    env.height = 8;
    env.rgba.assign(16 * 8 * 4, 0.0f);
    for (size_t i = 3; i < env.rgba.size(); i += 4) env.rgba[i] = 1.0f;
    const auto dist = render::buildEnvDistribution(env);

    render::GpuScene gpu;
    gpu.build(ctx, s, env, dist);
    render::RenderSettings rs;
    rs.width = 960;
    rs.height = 600;
    rs.spp = 256;
    scene::Camera cam;
    cam.position = {0, 0, 400};
    cam.forward = {0, 0, -1};
    cam.right = {1, 0, 0};
    cam.up = {0, 1, 0};
    cam.tanHalfFovY = 20.0f / 400.0f;  // z = 0 で上下 ±20 mm（狭い画角で角度の分解能を上げる）
    cam.aspect = 1.6f;
    rs.frame.camera = cam;
    rs.frame.maxDepth = 16;
    rs.frame.radianceClamp = 0.0f;

    // 画面上のプリズムの範囲（z = 0 の面で）：列は x = ±11 mm、行は各プリズムの中央 ±4 mm
    auto col = [&](float x) { return static_cast<int>((x / (20.0f * cam.aspect) + 1.0f) * 0.5f * rs.width); };
    auto row = [&](float y) { return static_cast<int>((-y / 20.0f + 1.0f) * 0.5f * rs.height); };
    const int c0 = col(-11.0f), c1 = col(11.0f);

    const std::filesystem::path outDir = std::filesystem::path(KRBR_SOURCE_DIR) / "out";
    std::filesystem::create_directories(outDir);
    Shift result[2];
    for (int mode = 0; mode < 2; ++mode) {
        rs.frame.spectral = mode == 1;
        const auto out = render::renderOffscreen(ctx, gpu, rs);
        std::string err;
        io::savePngTonemapped(outDir / (mode ? "p5_prisms_spectral.png" : "p5_prisms_rgb.png"), out.color, {0.0f}, err);
        result[mode].acrylic = channelShift(out.color, row(12.0f), row(4.0f), c0, c1);
        result[mode].polycarbonate = channelShift(out.color, row(-4.0f), row(-12.0f), c0, c1);
    }
    gpu.destroy();

    const double ra = std::abs(result[1].acrylic), rp = std::abs(result[1].polycarbonate);
    std::printf("prism red/blue shift (px): RGB acrylic %.2f, polycarbonate %.2f; spectral acrylic %.2f, polycarbonate %.2f "
                "(ratio %.2f, expected ~%.2f)\n",
                result[0].acrylic, result[0].polycarbonate, result[1].acrylic, result[1].polycarbonate, rp / std::max(ra, 1e-6),
                (0.585 / 30.0) / (0.491 / 57.0));
    // RGB モードでは分散がない
    CHECK(std::abs(result[0].acrylic) < 0.3);
    CHECK(std::abs(result[0].polycarbonate) < 0.3);
    // 分光モードでは色が分かれ、ポリカーボネートの方が大きく分かれる
    CHECK(ra > 3.0);
    CHECK(rp < 40.0);  // 相関の探索範囲（45）に張り付いていない
    CHECK(rp / ra > 1.9);
    CHECK(rp / ra < 2.6);
}
