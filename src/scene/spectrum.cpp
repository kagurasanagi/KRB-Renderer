#include "scene/spectrum.hpp"

#include <algorithm>
#include <atomic>
#include <chrono>
#include <memory>
#include <cmath>
#include <fstream>
#include <mutex>
#include <thread>

#include "core/log.hpp"
#include "core/platform.hpp"

namespace krbr::spectrum {

namespace {

// 区分的ガウス関数：λ < μ では σ1、それ以外では σ2
float g(float l, float mu, float s1, float s2) {
    const float t = (l - mu) / (l < mu ? s1 : s2);
    return std::exp(-0.5f * t * t);
}

}  // namespace

glm::vec3 cmf(float l) {
    const float x = 1.056f * g(l, 599.8f, 37.9f, 31.0f) + 0.362f * g(l, 442.0f, 16.0f, 26.7f) -
                    0.065f * g(l, 501.1f, 20.4f, 26.2f);
    const float y = 0.821f * g(l, 568.8f, 46.9f, 40.5f) + 0.286f * g(l, 530.9f, 16.3f, 31.1f);
    const float z = 1.217f * g(l, 437.0f, 11.8f, 36.0f) + 0.681f * g(l, 459.0f, 26.0f, 13.8f);
    return {x, y, z};
}

float cmfYIntegral() {
    static const float v = [] {
        double s = 0;
        for (float l = kLambdaMin; l < kLambdaMax; l += 1.0f) s += cmf(l + 0.5f).y;
        return static_cast<float>(s);
    }();
    return v;
}

glm::vec3 whiteE() {
    static const glm::vec3 w = integrateXyz([](float) { return 1.0f; });
    return w;
}

glm::mat3 xyzToLinearSrgb() {
    static const glm::mat3 m = [] {
        // glm は列優先。下は行で書いた行列の転置
        const glm::mat3 xyzD65ToSrgb = glm::transpose(glm::mat3(3.2404542f, -1.5371385f, -0.4985314f,  //
                                                                -0.9692660f, 1.8760108f, 0.0415560f,   //
                                                                0.0556434f, -0.2040259f, 1.0572252f));
        const glm::mat3 bradford = glm::transpose(glm::mat3(0.8951f, 0.2664f, -0.1614f,  //
                                                            -0.7502f, 1.7135f, 0.0367f,  //
                                                            0.0389f, -0.0685f, 1.0296f));
        const glm::vec3 srcW = bradford * whiteE();
        const glm::vec3 dstW = bradford * glm::vec3(0.95047f, 1.0f, 1.08883f);
        const glm::mat3 scale(dstW.x / srcW.x, 0, 0, 0, dstW.y / srcW.y, 0, 0, 0, dstW.z / srcW.z);
        const glm::mat3 adapt = glm::inverse(bradford) * scale * bradford;
        return xyzD65ToSrgb * adapt;
    }();
    return m;
}

glm::mat3 linearSrgbToXyz() {
    static const glm::mat3 m = glm::inverse(xyzToLinearSrgb());
    return m;
}

// ---------------------------------------------------------------- 係数の表（Jakob & Hanika 2019）

namespace {

// 誤差は CIELAB で測る（人の目に近い）。白色点は E
glm::dvec3 xyzToLab(glm::dvec3 xyz) {
    const glm::dvec3 w = glm::dvec3(whiteE());
    auto f = [](double t) {
        const double d = 6.0 / 29.0;
        return t > d * d * d ? std::cbrt(t) : t / (3 * d * d) + 4.0 / 29.0;
    };
    const double fx = f(xyz.x / w.x), fy = f(xyz.y / w.y), fz = f(xyz.z / w.z);
    return {116 * fy - 16, 500 * (fx - fy), 200 * (fy - fz)};
}

struct Fitter {
    // 積分を速くするため、λ ごとの等色関数を前もって並べておく（5 nm 刻み）
    std::vector<double> lambdaX;  // 正規化した x
    std::vector<glm::dvec3> cmfW;  // 等色関数 × 刻み / ∫ȳ
    Fitter() {
        const double step = 5.0;
        const double norm = 1.0 / (cmfYIntegral());
        for (double l = kLambdaMin + step / 2; l < kLambdaMax; l += step) {
            lambdaX.push_back((l - kLambdaMin) / (kLambdaMax - kLambdaMin));
            cmfW.push_back(glm::dvec3(cmf(static_cast<float>(l))) * step * norm);
        }
    }
    glm::dvec3 rgbOf(const glm::dvec3& c) const {
        glm::dvec3 xyz(0.0);
        for (size_t i = 0; i < lambdaX.size(); ++i) {
            const double x = lambdaX[i];
            const double z = (c.x * x + c.y) * x + c.z;
            const double s = 0.5 + z / (2.0 * std::sqrt(1.0 + z * z));
            xyz += s * cmfW[i];
        }
        return xyz;
    }
    glm::dvec3 residual(const glm::dvec3& c, const glm::dvec3& targetLab) const {
        return xyzToLab(rgbOf(c)) - targetLab;
    }
    // Gauss–Newton（数値微分のヤコビアン）。残差が減るまで歩幅を半分にする（色域の端で発散しないように）
    void fit(glm::dvec3& c, const glm::dvec3& rgb) const {
        const glm::dvec3 targetLab = xyzToLab(glm::dmat3(linearSrgbToXyz()) * rgb);
        glm::dvec3 r = residual(c, targetLab);
        for (int it = 0; it < 40 && glm::length(r) >= 1e-4; ++it) {
            glm::dmat3 J;
            for (int k = 0; k < 3; ++k) {
                glm::dvec3 a = c, b = c;
                a[k] -= 1e-4;
                b[k] += 1e-4;
                J[k] = (residual(b, targetLab) - residual(a, targetLab)) / 2e-4;
            }
            if (std::abs(glm::determinant(J)) < 1e-15) break;
            const glm::dvec3 delta = glm::inverse(J) * r;
            const double r0 = glm::length(r);
            bool improved = false;
            for (double step = 1.0; step > 1e-3; step *= 0.5) {
                glm::dvec3 cn = c - step * delta;
                const double m = std::max({std::abs(cn.x), std::abs(cn.y), std::abs(cn.z)});
                if (m > 400) cn *= 400 / m;
                const glm::dvec3 rn = residual(cn, targetLab);
                if (glm::length(rn) < r0) {
                    c = cn;
                    r = rn;
                    improved = true;
                    break;
                }
            }
            if (!improved) break;
        }
    }
};

double smoothstep(double x) { return x * x * (3.0 - 2.0 * x); }

}  // namespace

Rgb2SpecTable buildRgb2SpecTable() {
    constexpr int N = Rgb2SpecTable::kRes;
    Rgb2SpecTable t;
    for (int i = 0; i < N; ++i) t.scale[i] = static_cast<float>(smoothstep(smoothstep(i / double(N - 1))));
    t.data.assign(static_cast<size_t>(3) * N * N * N * 3, 0.0f);
    const Fitter fitter;
    // (l, y, x) ごとに z を動かしながら、隣の解を初期値にして解く（中ほどの明るさから上下へ）
    std::atomic<int> next{0};
    auto worker = [&] {
        for (int job; (job = next.fetch_add(1)) < 3 * N * N;) {
            const int l = job / (N * N), yi = (job / N) % N, xi = job % N;
            auto solveRange = [&](int from, int to, int stepDir, glm::dvec3 c) {
                for (int zi = from; zi != to; zi += stepDir) {
                    const double z = t.scale[zi];
                    glm::dvec3 rgb;
                    rgb[l] = z;
                    rgb[(l + 1) % 3] = xi / double(N - 1) * z;
                    rgb[(l + 2) % 3] = yi / double(N - 1) * z;
                    fitter.fit(c, rgb);
                    float* out = &t.data[((static_cast<size_t>(l) * N + zi) * N * N + static_cast<size_t>(yi) * N + xi) * 3];
                    out[0] = static_cast<float>(c.x);
                    out[1] = static_cast<float>(c.y);
                    out[2] = static_cast<float>(c.z);
                }
                return c;
            };
            const int start = N / 5;
            const glm::dvec3 c0 = solveRange(start, N, 1, glm::dvec3(0.0));
            (void)c0;
            // 下側は、start の解から始める
            glm::dvec3 cs(t.data[((static_cast<size_t>(l) * N + start) * N * N + static_cast<size_t>(yi) * N + xi) * 3 + 0],
                          t.data[((static_cast<size_t>(l) * N + start) * N * N + static_cast<size_t>(yi) * N + xi) * 3 + 1],
                          t.data[((static_cast<size_t>(l) * N + start) * N * N + static_cast<size_t>(yi) * N + xi) * 3 + 2]);
            solveRange(start - 1, -1, -1, cs);
        }
    };
    const unsigned n = std::clamp(std::thread::hardware_concurrency() / 2, 1u, 12u);
    std::vector<std::thread> pool;
    for (unsigned i = 0; i < n; ++i) pool.emplace_back(worker);
    for (auto& th : pool) th.join();
    return t;
}

glm::vec3 Rgb2SpecTable::lookup(glm::vec3 rgb) const {
    constexpr int N = kRes;
    rgb = glm::clamp(rgb, glm::vec3(0.0f), glm::vec3(1.0f));
    // 灰色は解析的に（定数のスペクトル。pbrt-v4 と同じ）。白に近いと、表の補間では 1 に届かないため
    if (rgb.r == rgb.g && rgb.g == rgb.b) {
        const float v = std::clamp(rgb.r, 1e-7f, 1.0f - 1e-7f);
        return {0.0f, 0.0f, (v - 0.5f) / std::sqrt(v * (1.0f - v))};
    }
    int l = 0;
    if (rgb[1] > rgb[l]) l = 1;
    if (rgb[2] > rgb[l]) l = 2;
    const float z = rgb[l];
    if (z <= 0.0f) return {0.0f, 0.0f, -50.0f};  // 黒
    const float x = rgb[(l + 1) % 3] / z * (N - 1), y = rgb[(l + 2) % 3] / z * (N - 1);
    const int zi = std::clamp(static_cast<int>(std::upper_bound(scale.begin(), scale.end(), z) - scale.begin()) - 1, 0, N - 2);
    const float zf = (z - scale[zi]) / (scale[zi + 1] - scale[zi]);
    const int xi = std::min(static_cast<int>(x), N - 2), yi = std::min(static_cast<int>(y), N - 2);
    const float xf = x - xi, yf = y - yi;
    glm::vec3 c(0.0f);
    for (int dz = 0; dz < 2; ++dz)
        for (int dy = 0; dy < 2; ++dy)
            for (int dx = 0; dx < 2; ++dx) {
                const float w = (dz ? zf : 1 - zf) * (dy ? yf : 1 - yf) * (dx ? xf : 1 - xf);
                const float* p = &data[((static_cast<size_t>(l) * N + zi + dz) * N * N +
                                        static_cast<size_t>(yi + dy) * N + xi + dx) * 3];
                c += w * glm::vec3(p[0], p[1], p[2]);
            }
    return c;
}

namespace {
std::mutex gTableMutex;
std::unique_ptr<Rgb2SpecTable> gTable;
std::atomic<const Rgb2SpecTable*> gLoaded{nullptr};
}  // namespace

const Rgb2SpecTable* loadedRgb2SpecTable() { return gLoaded.load(); }

const Rgb2SpecTable& rgb2specTable(const std::filesystem::path& cacheFile) {
    std::lock_guard lock(gTableMutex);
    if (gTable) return *gTable;
    auto table = std::make_unique<Rgb2SpecTable>();
    constexpr int N = Rgb2SpecTable::kRes;
    const size_t count = static_cast<size_t>(3) * N * N * N * 3;
    {
        std::ifstream f(cacheFile, std::ios::binary);
        if (f) {
            table->data.resize(count);
            if (f.read(reinterpret_cast<char*>(table->scale.data()), sizeof(float) * N) &&
                f.read(reinterpret_cast<char*>(table->data.data()), static_cast<std::streamsize>(count * sizeof(float)))) {
                log::info("spectral: RGB -> spectrum table from cache");
                gTable = std::move(table);
                gLoaded = gTable.get();
                return *gTable;
            }
        }
    }
    const auto t0 = std::chrono::steady_clock::now();
    *table = buildRgb2SpecTable();
    log::info("spectral: built RGB -> spectrum table ({}^3 x 3) in {:.1f} s", N,
              std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count());
    std::error_code ec;
    std::filesystem::create_directories(cacheFile.parent_path(), ec);
    std::ofstream f(cacheFile, std::ios::binary);
    f.write(reinterpret_cast<const char*>(table->scale.data()), sizeof(float) * N);
    f.write(reinterpret_cast<const char*>(table->data.data()), static_cast<std::streamsize>(count * sizeof(float)));
    gTable = std::move(table);
    gLoaded = gTable.get();
    return *gTable;
}

const Rgb2SpecTable& rgb2specTable() { return rgb2specTable(platform::cacheDir() / "spectral" / "rgb2spec_v1.bin"); }

}  // namespace krbr::spectrum
