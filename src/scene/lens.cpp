#include "scene/lens.hpp"

#include <cmath>
#include <fstream>
#include <numbers>
#include <sstream>

#include "core/platform.hpp"

namespace krbr::scene {

LensSystem LensSystem::doubleGauss50() {
    // D-GAUSS F/2 22deg HFOV（US patent 2,673,491 Tronnier, Modern Lens Design p.312、50 mm に縮小。pbrt の dgauss.50mm.dat）
    LensSystem s;
    s.name = "double Gauss 50mm f/2";
    s.surfaces = {{29.475f, 3.76f, 1.67f, 25.2f},  {84.83f, 0.12f, 1.0f, 25.2f},   {19.275f, 4.025f, 1.67f, 23.0f},
                  {40.77f, 3.275f, 1.699f, 23.0f}, {12.75f, 5.705f, 1.0f, 18.0f},  {0.0f, 4.5f, 1.0f, 17.1f},
                  {-14.495f, 1.18f, 1.603f, 17.0f}, {40.77f, 6.065f, 1.658f, 20.0f}, {-20.385f, 0.19f, 1.0f, 20.0f},
                  {437.065f, 3.22f, 1.717f, 20.0f}, {-39.73f, 5.0f, 1.0f, 20.0f}};
    return s;
}

bool LensSystem::load(const std::filesystem::path& p, LensSystem& out, std::string& err) {
    std::ifstream f(p);
    if (!f) {
        err = "cannot open " + platform::pathToUtf8(p);
        return false;
    }
    out = {};
    out.name = platform::pathToUtf8(p.filename());
    std::string line;
    while (std::getline(f, line)) {
        if (auto c = line.find('#'); c != std::string::npos) line.resize(c);
        std::istringstream ss(line);
        LensSurface s;
        if (!(ss >> s.radius >> s.thickness >> s.ior >> s.aperture)) continue;
        if (s.ior == 0.0f) s.ior = 1.0f;  // 絞りの行は屈折率 0 と書く流儀もある
        out.surfaces.push_back(s);
    }
    if (out.surfaces.size() < 2) {
        err = "no lens surfaces in " + platform::pathToUtf8(p);
        return false;
    }
    return true;
}

std::vector<float> LensSystem::vertexZ() const {
    std::vector<float> z(surfaces.size());
    float acc = 0.0f;
    for (size_t i = 0; i < surfaces.size(); ++i) {
        z[i] = acc;
        acc -= surfaces[i].thickness;
    }
    return z;
}

bool LensSystem::trace(glm::dvec3& o, glm::dvec3& d, bool fromScene, float filmDistance) const {
    (void)filmDistance;
    const std::vector<float> zs = vertexZ();
    const int n = static_cast<int>(surfaces.size());
    for (int k = 0; k < n; ++k) {
        const int i = fromScene ? k : n - 1 - k;
        const LensSurface& s = surfaces[i];
        const double zv = zs[i];
        const double ap2 = 0.25 * s.aperture * s.aperture;
        glm::dvec3 p;
        if (s.radius == 0.0f) {
            if (std::abs(d.z) < 1e-12) return false;
            p = o + d * ((zv - o.z) / d.z);
            if (p.x * p.x + p.y * p.y > ap2) return false;
            o = p;
            continue;
        }
        const glm::dvec3 c(0, 0, zv - s.radius);
        const glm::dvec3 oc = o - c;
        const double b = glm::dot(oc, d), cc = glm::dot(oc, oc) - double(s.radius) * s.radius;
        const double disc = b * b - cc;
        if (disc < 0) return false;
        const double sq = std::sqrt(disc);
        const double t0 = -b - sq, t1 = -b + sq;
        // 頂点に近い側の交点（有効径の中の面）
        double t = -1;
        for (double tc : {t0, t1}) {
            if (tc <= 1e-9) continue;
            if (t < 0 || std::abs((o + d * tc).z - zv) < std::abs((o + d * t).z - zv)) t = tc;
        }
        if (t < 0) return false;
        p = o + d * t;
        if (p.x * p.x + p.y * p.y > ap2) return false;
        glm::dvec3 nrm = glm::normalize(p - c);
        if (glm::dot(nrm, d) > 0) nrm = -nrm;
        // 屈折率：面 i の後ろ（フィルム側）の媒質は surfaces[i].ior、前（物体側）は surfaces[i-1].ior（i = 0 は空気）
        const double nFilmSide = s.ior, nSceneSide = i > 0 ? surfaces[i - 1].ior : 1.0;
        const double eta = fromScene ? nSceneSide / nFilmSide : nFilmSide / nSceneSide;
        const double cosi = -glm::dot(nrm, d);
        const double k2 = 1.0 - eta * eta * (1.0 - cosi * cosi);
        if (k2 < 0) return false;  // 全反射
        d = glm::normalize(eta * d + (eta * cosi - std::sqrt(k2)) * nrm);
        o = p;
    }
    return true;
}

float LensSystem::filmDistanceFor(float focusMm) const {
    // 光軸上の物体の点から、前の面の有効径の 5% の高さを通る光線を追い、後ろで光軸と交わる位置に像ができる
    const double h = 0.05 * 0.5 * surfaces.front().aperture;
    glm::dvec3 o, d;
    if (focusMm <= 0.0f || focusMm > 1e6f) {
        o = {h, 0, 1.0};
        d = {0, 0, -1};
    } else {
        o = {0, 0, focusMm};
        d = glm::normalize(glm::dvec3(h, 0, 0) - o);
    }
    if (!trace(o, d, true, 0.0f) || std::abs(d.x) < 1e-12) return surfaces.back().thickness;
    const double t = -o.x / d.x;
    const double zImage = o.z + d.z * t;
    const double zLast = vertexZ().back();
    return static_cast<float>(std::max(zLast - zImage, 0.1));
}

float LensSystem::effectiveFocalLength() const {
    const double h = 0.05 * 0.5 * surfaces.front().aperture;
    glm::dvec3 o{h, 0, 1.0}, d{0, 0, -1};
    if (!trace(o, d, true, 0.0f) || std::abs(d.x) < 1e-12) return 50.0f;
    const double zPrincipal = o.z + d.z * ((h - o.x) / d.x);  // 出ていく光線の高さが h になる位置（後側主点）
    const double zFocus = o.z + d.z * (-o.x / d.x);
    return static_cast<float>(std::abs(zPrincipal - zFocus));
}

float LensSystem::centerWeight(float filmDistance) const {
    const double zFilm = vertexZ().back() - filmDistance;
    const double r = 0.5 * surfaces.back().aperture;
    const double zRear = vertexZ().back();
    // 後ろの面の有効径の円を層化してサンプリングし、通った光線の cos⁴θ · A / z² を平均する
    const int N = 64;
    double sum = 0;
    for (int j = 0; j < N; ++j)
        for (int i = 0; i < N; ++i) {
            const double u = (i + 0.5) / N, v = (j + 0.5) / N;
            const double rr = r * std::sqrt(u), ph = 2 * std::numbers::pi * v;
            const glm::dvec3 pr(rr * std::cos(ph), rr * std::sin(ph), zRear);
            glm::dvec3 o(0, 0, zFilm), d = glm::normalize(pr - o);
            const double c = d.z;
            if (!trace(o, d, false, filmDistance)) continue;
            sum += c * c * c * c;
        }
    const double area = std::numbers::pi * r * r;
    const double z = zRear - zFilm;
    return static_cast<float>(sum / (N * N) * area / (z * z));
}

}  // namespace krbr::scene
