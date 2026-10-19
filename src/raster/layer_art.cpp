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

kicad::Box2 graphicBox(const kicad::Graphic& g) {
    kicad::Box2 b;
    const double w = g.width / 2;
    if (g.kind == kicad::GrKind::Line || g.kind == kicad::GrKind::Arc) {  // 多角形などでは start / end は使わない
        b.add(g.start, w);
        b.add(g.end, w);
    }
    if (g.kind == kicad::GrKind::Arc) b.add(g.mid, w);
    if (g.kind == kicad::GrKind::Circle) b.add(g.start, glm::length(g.end - g.start) + w);
    for (Vec2 p : g.pts) b.add(p, w);
    return b;
}

kicad::Box2 pathsBox(const Path& p) {
    kicad::Box2 b;
    for (const auto& q : p) b.add(fromPt(q));
    return b;
}

}  // namespace

SidePaths collectSidePaths(const Board& board, Side side, const LayerArtOptions& opts, const kicad::Box2* clip) {
    const std::string cu = side == Front ? "F.Cu" : "B.Cu";
    const std::string mask = side == Front ? "F.Mask" : "B.Mask";
    const std::string silk = side == Front ? "F.SilkS" : "B.SilkS";
    const bool tentDefault = side == Front ? board.setup.tentFront : board.setup.tentBack;
    SidePaths sp;
    // clip があれば、その範囲（少し広げた範囲）にかかる項目だけを集める
    const kicad::Box2 area = clip ? clip->expanded(1.0) : kicad::Box2{};
    auto near = [&](const kicad::Box2& b) { return !clip || area.intersects(b); };
    auto nearPoint = [&](Vec2 p, double r) {
        kicad::Box2 b;
        b.add(p, r);
        return near(b);
    };

    for (const auto& t : board.tracks) {
        if (t.layer != cu) continue;
        kicad::Box2 b;
        b.add(t.start, t.width / 2);
        b.add(t.end, t.width / 2);
        if (t.mid) b.add(*t.mid, t.width / 2);
        if (near(b)) append(sp.copper, trackToPaths(t));
    }
    for (const auto& p : board.pads) {
        if (!nearPoint(p.pos, std::max(p.size.x, p.size.y) + 0.5)) continue;
        if (p.onLayer(cu) && padHasCopper(p)) append(sp.copper, padToPaths(p));
        if (p.onLayer(mask)) append(sp.maskOpen, padToPaths(p, p.maskMargin.value_or(board.setup.padToMaskClearance)));
    }
    for (const auto& v : board.vias) {
        if (!viaOnSide(v, cu) || !nearPoint(v.pos, v.size)) continue;
        append(sp.copper, disc(v.pos, v.size / 2));
        const std::optional<bool> tent = side == Front ? v.tentFront : v.tentBack;
        if (!tent.value_or(tentDefault)) append(sp.maskOpen, disc(v.pos, v.size / 2));
    }
    for (const auto& z : board.zones) {
        if (z.layer != cu && z.layer != mask) continue;
        if (clip) {
            kicad::Box2 b;
            for (Vec2 p : z.pts) b.add(p);
            if (!near(b)) continue;
        }
        if (z.layer == cu) sp.copper.push_back(toPath(z.pts));
        if (z.layer == mask) sp.maskOpen.push_back(toPath(z.pts));
    }
    for (const auto& g : board.graphics) {
        if (g.layer != cu && g.layer != mask && g.layer != silk) continue;
        if (clip && !near(graphicBox(g))) continue;
        if (g.layer == cu) append(sp.copper, graphicToPaths(g));
        else if (g.layer == mask) append(sp.maskOpen, graphicToPaths(g));
        else append(sp.silk, graphicToPaths(g));
    }
    if (opts.extraSilk[side])
        for (const auto& p : *opts.extraSilk[side])
            if (!clip || near(pathsBox(p))) sp.silk.push_back(p);

    sp.copper = unite(sp.copper);
    sp.maskOpen = unite(sp.maskOpen);
    sp.silk = unite(sp.silk);
    if (opts.clipSilkByMask) sp.silk = subtract(sp.silk, sp.maskOpen);
    if (clip) {
        const Paths rect{toPath({clip->lo, {clip->hi.x, clip->lo.y}, clip->hi, {clip->lo.x, clip->hi.y}})};
        sp.copper = intersect(sp.copper, rect);
        sp.maskOpen = intersect(sp.maskOpen, rect);
        sp.silk = intersect(sp.silk, rect);
    }
    return sp;
}

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
        SidePaths sp = collectSidePaths(board, side, opts);
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
