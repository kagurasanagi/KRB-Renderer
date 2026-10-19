#include "raster/layer_art.hpp"

#include <algorithm>
#include <cmath>

#include "core/log.hpp"

namespace krbr::raster {

using kicad::Board;
using kicad::PadType;
using namespace geom;

namespace {

bool padHasCopper(const kicad::Pad& p) {
    // メッキなし穴は、パッドが穴より大きいときだけ銅箔を持つ（KiCad と同じ扱い）
    if (p.type != PadType::NpThruHole) return true;
    if (!p.drill.present) return true;
    return p.size.x > p.drill.size.x + 1e-6 || p.size.y > p.drill.size.y + 1e-6;
}

bool viaOnSide(const kicad::Via& v, const std::string& cu) { return v.layerFrom == cu || v.layerTo == cu; }

struct SidePaths {
    Paths copper, maskOpen, silk;
};

SidePaths collectSide(const Board& board, Side side, const LayerArtOptions& opts) {
    const std::string cu = side == Front ? "F.Cu" : "B.Cu";
    const std::string mask = side == Front ? "F.Mask" : "B.Mask";
    const std::string silk = side == Front ? "F.SilkS" : "B.SilkS";
    const bool tentDefault = side == Front ? board.setup.tentFront : board.setup.tentBack;
    SidePaths sp;

    for (const auto& t : board.tracks)
        if (t.layer == cu) append(sp.copper, trackToPaths(t));
    for (const auto& p : board.pads) {
        if (p.onLayer(cu) && padHasCopper(p)) append(sp.copper, padToPaths(p));
        if (p.onLayer(mask)) append(sp.maskOpen, padToPaths(p, p.maskMargin.value_or(board.setup.padToMaskClearance)));
    }
    for (const auto& v : board.vias) {
        if (!viaOnSide(v, cu)) continue;
        append(sp.copper, disc(v.pos, v.size / 2));
        const std::optional<bool> tent = side == Front ? v.tentFront : v.tentBack;
        if (!tent.value_or(tentDefault)) append(sp.maskOpen, disc(v.pos, v.size / 2));
    }
    for (const auto& z : board.zones) {
        if (z.layer == cu) sp.copper.push_back(toPath(z.pts));
        if (z.layer == mask) sp.maskOpen.push_back(toPath(z.pts));
    }
    for (const auto& g : board.graphics) {
        if (g.layer == cu) append(sp.copper, graphicToPaths(g));
        else if (g.layer == mask) append(sp.maskOpen, graphicToPaths(g));
        else if (g.layer == silk) append(sp.silk, graphicToPaths(g));
    }
    if (opts.extraSilk[side]) append(sp.silk, *opts.extraSilk[side]);

    sp.copper = unite(sp.copper);
    sp.maskOpen = unite(sp.maskOpen);
    sp.silk = unite(sp.silk);
    if (opts.clipSilkByMask) sp.silk = subtract(sp.silk, sp.maskOpen);
    return sp;
}

}  // namespace

LayerTextures buildLayerTextures(const Board& board, const BoardGeometry& geo, const LayerArtOptions& opts) {
    LayerTextures tex;
    const Vec2 lo = geo.bounds.lo - Vec2{opts.marginMm, opts.marginMm};
    const Vec2 size = geo.bounds.size() + Vec2{2 * opts.marginMm, 2 * opts.marginMm};
    double ppm = opts.pxPerMm;
    const double maxSide = std::max(size.x, size.y);
    if (maxSide * ppm > opts.maxDim) {
        ppm = opts.maxDim / maxSide;
        log::warn("layer texture resolution reduced to {:.1f} px/mm (max {} px)", ppm, opts.maxDim);
    }
    tex.grid.origin = lo;
    tex.grid.pxPerMm = ppm;
    tex.grid.width = std::max(1, static_cast<int>(std::ceil(size.x * ppm)));
    tex.grid.height = std::max(1, static_cast<int>(std::ceil(size.y * ppm)));

    const size_t npx = static_cast<size_t>(tex.grid.width) * tex.grid.height;
    std::vector<float> cov;
    for (Side side : {Front, Back}) {
        SidePaths sp = collectSide(board, side, opts);
        tex.stats[side] = {sp.copper.size(), sp.maskOpen.size(), sp.silk.size()};
        auto& out = tex.rgba[side];
        out.assign(npx * 4, 0);
        for (size_t i = 0; i < npx; ++i) out[i * 4 + 3] = 255;
        const Paths* layers[3] = {&sp.copper, &sp.maskOpen, &sp.silk};
        for (int ch = 0; ch < 3; ++ch) {
            rasterizeCoverage(*layers[ch], tex.grid, cov);
            for (size_t i = 0; i < npx; ++i)
                out[i * 4 + ch] = static_cast<uint8_t>(std::lround(std::clamp(cov[i], 0.0f, 1.0f) * 255.0f));
        }
    }
    log::info("layer textures: {} x {} px ({:.1f} px/mm); front cu/mask/silk paths {}/{}/{}, back {}/{}/{}",
              tex.grid.width, tex.grid.height, ppm, tex.stats[0].copperPaths, tex.stats[0].maskPaths,
              tex.stats[0].silkPaths, tex.stats[1].copperPaths, tex.stats[1].maskPaths, tex.stats[1].silkPaths);
    return tex;
}

}  // namespace krbr::raster
