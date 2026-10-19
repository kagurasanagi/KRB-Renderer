#include "geom/relief.hpp"

#include "core/log.hpp"

namespace krbr::geom {

void buildRelief(const BoardGeometry& geo, int side, const ReliefInput& in, const ReliefParams& p, Mesh& out) {
    // 面を張る範囲：穴を開けた基板＋レジストでふさがれたビアの穴
    const Paths base = in.tentedHoles.empty() ? geo.body : unite([&] {
        Paths a = geo.body;
        append(a, intersect(in.tentedHoles, geo.region));
        return a;
    }());
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
    log::info("relief ({}): {} triangles; copper {:.0f} um, mask {:.0f}/{:.0f} um, silk {:.0f} um",
              side == 0 ? "front" : "back", out.triangleCount() - tris0, p.copper * 1000, p.maskOnCopper * 1000,
              p.maskOnSubstrate * 1000, p.silk * 1000);
}

}  // namespace krbr::geom
