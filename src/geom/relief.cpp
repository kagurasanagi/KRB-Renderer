#include "geom/relief.hpp"

#include <algorithm>
#include <cmath>
#include <numbers>

#include "core/log.hpp"

namespace krbr::geom {

Paths reliefBase(const BoardGeometry& geo, const Paths& tentedHoles) {
    if (tentedHoles.empty()) return geo.body;
    Paths a = geo.body;
    append(a, intersect(tentedHoles, geo.region));
    return unite(a);
}

namespace {

constexpr int kDimpleSegments = 24;  // くぼみの縁の分割数（平らな面から除く円と、くぼみの面で同じ頂点を使う）
constexpr int kDimpleRings = 4;

Path dimpleRim(const ReliefDimple& d) {
    std::vector<Vec2> pts;
    pts.reserve(kDimpleSegments);
    for (int i = 0; i < kDimpleSegments; ++i) {
        const double a = 2.0 * std::numbers::pi * i / kDimpleSegments;
        pts.push_back({d.pos.x + d.radius * std::cos(a), d.pos.y + d.radius * std::sin(a)});
    }
    return toPath(pts);
}

// すり鉢状のくぼみ：縁の高さ hRim から中心で depth だけ下がる（余弦の断面。縁では傾き 0 で周りの面につながる）
void buildDimple(const ReliefDimple& d, double hRim, double depth, int side, Vec2 center, uint32_t mat, Mesh& out) {
    const double sign = side == 0 ? 1.0 : -1.0;
    const double R = d.radius;
    auto height = [&](double r) { return hRim - depth * (0.5 + 0.5 * std::cos(std::numbers::pi * r / R)); };
    auto slope = [&](double r) { return depth * 0.5 * std::numbers::pi / R * std::sin(std::numbers::pi * r / R); };
    // 頂点：中心 1 つ ＋ 各リング kDimpleSegments 個（最外のリングは dimpleRim と同じ点）
    std::vector<uint32_t> ring[kDimpleRings + 1];
    const uint32_t c = out.addVertex(boardToWorld(d.pos, sign * height(0.0), center), {0.0f, static_cast<float>(sign), 0.0f});
    for (int k = 1; k <= kDimpleRings; ++k) {
        const double r = R * k / kDimpleRings;
        const double dh = slope(r);  // 中心へ向かって下がる：法線は外向きに傾く
        for (int i = 0; i < kDimpleSegments; ++i) {
            const double a = 2.0 * std::numbers::pi * i / kDimpleSegments;
            const Vec2 radial{std::cos(a), std::sin(a)};
            const Vec2 p{d.pos.x + r * radial.x, d.pos.y + r * radial.y};
            glm::vec3 n{static_cast<float>(-dh * radial.x), 1.0f, static_cast<float>(-dh * radial.y)};
            n = glm::normalize(n);
            n.y *= static_cast<float>(sign);
            ring[k].push_back(out.addVertex(boardToWorld(p, sign * height(r), center), n));
        }
    }
    auto tri = [&](uint32_t a, uint32_t b, uint32_t cc) {
        // 表は +Y、裏は -Y を向くように巻く
        const glm::vec3 n = glm::cross(out.positions[b] - out.positions[a], out.positions[cc] - out.positions[a]);
        if ((n.y > 0) == (sign > 0)) out.addTriangle(a, b, cc, mat);
        else out.addTriangle(a, cc, b, mat);
    };
    for (int i = 0; i < kDimpleSegments; ++i) {
        const int j = (i + 1) % kDimpleSegments;
        tri(c, ring[1][i], ring[1][j]);
        for (int k = 1; k < kDimpleRings; ++k) {
            tri(ring[k][i], ring[k + 1][i], ring[k + 1][j]);
            tri(ring[k][i], ring[k + 1][j], ring[k][j]);
        }
    }
}

}  // namespace

void buildRelief(const BoardGeometry& geo, int side, const ReliefInput& in, const ReliefParams& p, Mesh& out,
                 const kicad::Box2* clip) {
    // 面を張る範囲（タイルなら、その矩形の中だけ）
    Paths base = in.base ? *in.base : reliefBase(geo, in.tentedHoles);
    if (clip)
        base = intersect(base, {toPath({clip->lo, {clip->hi.x, clip->lo.y}, clip->hi, {clip->lo.x, clip->hi.y}})});
    // ふさがれたビアのくぼみ：平らな面からその円を除き、あとで同じ縁の点を使ってくぼみの面を張る
    const bool dimples = p.viaDimple > 0 && !in.dimples.empty();
    if (dimples) {
        Paths discs;
        for (const auto& d : in.dimples) discs.push_back(dimpleRim(d));
        base = subtract(base, discs);
    }
    if (base.empty() && !dimples) return;
    const Paths C = intersect(in.copper, base);
    const Paths O = intersect(in.maskOpen, base);
    const Paths M = subtract(base, O);
    const Paths noCu = subtract(base, C);
    const Paths S = intersect(in.silk, base);

    // 高さ（基材の面からの距離）
    const double hFr4 = 0.0;
    const double hFin = p.copper;
    const double hMaskFr4 = p.maskOnSubstrate;
    const double hMaskCu = p.copper + p.maskOnCopper;

    struct Region {
        Paths paths;
        double h;
        uint32_t faceMat, wallMat;
    };
    const Paths noCuOpen = intersect(noCu, O), cuOpen = intersect(C, O);
    const Paths noCuMask = intersect(noCu, M), cuMask = intersect(C, M);
    std::vector<Region> regions = {
        {subtract(noCuOpen, S), hFr4, ReliefFr4, ReliefFr4},
        {subtract(cuOpen, S), hFin, ReliefFinish, ReliefFinish},
        {subtract(noCuMask, S), hMaskFr4, ReliefMaskOnFr4, ReliefMaskOnFr4},
        {subtract(cuMask, S), hMaskCu, ReliefMaskOnCopper, ReliefMaskOnCopper},
        // シルクは下にあるものの上にさらに乗る
        {intersect(S, noCuOpen), hFr4 + p.silk, ReliefSilk, ReliefSilk},
        {intersect(S, cuOpen), hFin + p.silk, ReliefSilk, ReliefSilk},
        {intersect(S, noCuMask), hMaskFr4 + p.silk, ReliefSilk, ReliefSilk},
        {intersect(S, cuMask), hMaskCu + p.silk, ReliefSilk, ReliefSilk},
    };

    const double sign = side == 0 ? 1.0 : -1.0;
    const double y0 = sign * geo.coreHalf;
    const size_t tris0 = out.triangleCount();
    for (const auto& r : regions) {
        if (r.paths.empty()) continue;
        // 高さ 0 の領域（むき出しの基材）も、基材の側面とつながる面として作る
        extrudeRegion(r.paths, y0 + sign * r.h, y0, side == 0, r.faceMat, r.wallMat, geo.center, out);
    }
    if (dimples) {
        for (const auto& d : in.dimples) {
            if (!d.build) continue;
            // 縁は銅の上のレジストの高さ（ビアのランドは銅で、レジストに覆われている）。深さは半径の 6 割まで
            const double depth = std::min(p.viaDimple, d.radius * 0.6);
            buildDimple(d, hMaskCu, depth, side, geo.center, ReliefMaskOnCopper, out);
        }
    }
    log::debug("relief ({}): {} triangles; copper {:.0f} um, mask {:.0f}/{:.0f} um, silk {:.0f} um",
              side == 0 ? "front" : "back", out.triangleCount() - tris0, p.copper * 1000, p.maskOnCopper * 1000,
              p.maskOnSubstrate * 1000, p.silk * 1000);
}

}  // namespace krbr::geom
