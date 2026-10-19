#include "geom/board_mesh.hpp"

#include <array>
#include <cmath>
#include <mapbox/earcut.hpp>

#include "core/log.hpp"

namespace krbr::geom {

using kicad::Board;
using kicad::GrKind;
using kicad::PadType;

namespace {

constexpr double kJoinTol = 1e-3;  // 外形の端点をつなぐ許容差（mm）

// Edge.Cuts の図形を閉じた輪郭の集合にする
std::vector<std::vector<Vec2>> outlineLoops(const Board& board, int& openChains) {
    std::vector<std::vector<Vec2>> loops, open;
    for (const auto& g : board.graphics) {
        if (g.layer != "Edge.Cuts") continue;
        switch (g.kind) {
            case GrKind::Line: open.push_back({g.start, g.end}); break;
            case GrKind::Arc: open.push_back(arcPoints(g.start, g.mid, g.end)); break;
            case GrKind::Curve: open.push_back(bezierPoints(g.pts)); break;
            case GrKind::Circle: loops.push_back(circlePoints(g.start, glm::length(g.end - g.start))); break;
            case GrKind::Poly:
                if (g.pts.size() >= 3) loops.push_back(g.pts);
                break;
        }
    }
    auto near = [](Vec2 a, Vec2 b) { return glm::length(a - b) < kJoinTol; };
    std::vector<bool> used(open.size(), false);
    openChains = 0;
    for (size_t i = 0; i < open.size(); ++i) {
        if (used[i]) continue;
        used[i] = true;
        std::vector<Vec2> chain = open[i];
        bool extended = true;
        while (extended && !near(chain.front(), chain.back())) {
            extended = false;
            for (size_t j = 0; j < open.size(); ++j) {
                if (used[j]) continue;
                const auto& p = open[j];
                if (near(chain.back(), p.front())) {
                    chain.insert(chain.end(), p.begin() + 1, p.end());
                } else if (near(chain.back(), p.back())) {
                    chain.insert(chain.end(), p.rbegin() + 1, p.rend());
                } else if (near(chain.front(), p.back())) {
                    chain.insert(chain.begin(), p.begin(), p.end() - 1);
                } else if (near(chain.front(), p.front())) {
                    chain.insert(chain.begin(), p.rbegin(), p.rend() - 1);
                } else {
                    continue;
                }
                used[j] = true;
                extended = true;
            }
        }
        if (near(chain.front(), chain.back()))
            chain.pop_back();
        else
            ++openChains;
        if (chain.size() >= 3) loops.push_back(std::move(chain));
    }
    return loops;
}

struct PlatedHole {
    Vec2 a, b;  // 長円の中心線（円なら a == b）
    double r;
};

double distToSegment(Vec2 p, Vec2 a, Vec2 b) {
    const Vec2 ab = b - a;
    const double l2 = glm::dot(ab, ab);
    const double t = l2 > 0 ? std::clamp(glm::dot(p - a, ab) / l2, 0.0, 1.0) : 0.0;
    return glm::length(p - (a + t * ab));
}

void addHole(Vec2 center, double angleDeg, Vec2 size, bool oval, bool plated, Paths& holes,
             std::vector<PlatedHole>& platedHoles) {
    append(holes, drillToPaths(center, angleDeg, size, oval));
    if (!plated) return;
    const double minDim = std::min(size.x, size.y);
    const double l = (std::max(size.x, size.y) - minDim) / 2;
    const Vec2 dir = size.x > size.y ? Vec2{1, 0} : Vec2{0, 1};
    if (!oval || l <= 0)
        platedHoles.push_back({center, center, size.x / 2});
    else
        platedHoles.push_back({center + kicad::rotatePoint(-l * dir, angleDeg),
                               center + kicad::rotatePoint(l * dir, angleDeg), minDim / 2});
}

struct Cap {
    double y;
    float normalY;  // +1: 上向きの面, -1: 下向き
    uint32_t mat;
};

void triangulateFaces(const Clipper2Lib::PolyPath64& node, const std::vector<Cap>& caps, Vec2 center, Mesh& mesh) {
    for (size_t i = 0; i < node.Count(); ++i) {
        const auto* outer = node.Child(i);  // 外側の輪郭
        using Pt = std::array<double, 2>;
        std::vector<std::vector<Pt>> poly;
        std::vector<Vec2> flat;
        auto addRing = [&](const Path& p) {
            std::vector<Pt> ring;
            for (const auto& q : p) {
                const Vec2 v = fromPt(q);
                ring.push_back({v.x, v.y});
                flat.push_back(v);
            }
            poly.push_back(std::move(ring));
        };
        addRing(outer->Polygon());
        for (size_t h = 0; h < outer->Count(); ++h) addRing(outer->Child(h)->Polygon());
        const std::vector<uint32_t> idx = mapbox::earcut<uint32_t>(poly);

        for (const Cap& cap : caps) {
            const uint32_t base = static_cast<uint32_t>(mesh.positions.size());
            for (Vec2 v : flat) mesh.addVertex(boardToWorld(v, cap.y, center), {0, cap.normalY, 0});
            for (size_t t = 0; t + 2 < idx.size(); t += 3) {
                if (cap.normalY > 0)
                    mesh.addTriangle(base + idx[t], base + idx[t + 1], base + idx[t + 2], cap.mat);
                else
                    mesh.addTriangle(base + idx[t], base + idx[t + 2], base + idx[t + 1], cap.mat);
            }
        }
        // 穴の中の島（さらに内側の外形）
        for (size_t h = 0; h < outer->Count(); ++h) triangulateFaces(*outer->Child(h), caps, center, mesh);
    }
}

void collectRings(const Clipper2Lib::PolyPath64& node, std::vector<const Path*>& rings) {
    for (size_t i = 0; i < node.Count(); ++i) {
        rings.push_back(&node.Child(i)->Polygon());
        collectRings(*node.Child(i), rings);
    }
}

// 輪郭の側面。材質が左側にある向き（Clipper の外周は正の面積、穴は負）で外向き法線を決める
template <class MatFn>
void buildWalls(const Path& ring, double yTop, double yBot, Vec2 center, MatFn matAt, Mesh& mesh) {
    const size_t n = ring.size();
    if (n < 3) return;
    std::vector<Vec2> pts = fromPath(ring);
    // Clipper2 の出力は外周が正の面積（シューレース公式）、穴が負。どちらも材質は
    // 進行方向 d の左側 (-dy, dx) にあるので、外向き法線は (dy, -dx)
    std::vector<Vec2> en(n);
    for (size_t i = 0; i < n; ++i) {
        const Vec2 d = pts[(i + 1) % n] - pts[i];
        const double len = glm::length(d);
        en[i] = len > 0 ? Vec2{d.y, -d.x} / len : Vec2{0, 0};
    }
    const double cosSmooth = std::cos(40.0 * 3.14159265358979 / 180.0);
    auto vtxNormal = [&](size_t edge, size_t vtxEdge) -> Vec2 {
        // vtxEdge は頂点を共有する隣の辺
        const Vec2 a = en[edge], b = en[vtxEdge];
        if (glm::dot(a, b) >= cosSmooth) {
            const Vec2 s = a + b;
            const double l = glm::length(s);
            return l > 0 ? s / l : a;
        }
        return a;
    };
    for (size_t i = 0; i < n; ++i) {
        const size_t j = (i + 1) % n;
        const Vec2 p0 = pts[i], p1 = pts[j];
        if (glm::length(p1 - p0) <= 0) continue;
        const Vec2 n0 = vtxNormal(i, (i + n - 1) % n);
        const Vec2 n1 = vtxNormal(i, j);
        const uint32_t mat = matAt((p0 + p1) * 0.5);
        const glm::vec3 wn0{static_cast<float>(n0.x), 0.f, static_cast<float>(n0.y)};
        const glm::vec3 wn1{static_cast<float>(n1.x), 0.f, static_cast<float>(n1.y)};
        const uint32_t a = mesh.addVertex(boardToWorld(p0, yTop, center), wn0);
        const uint32_t b = mesh.addVertex(boardToWorld(p1, yTop, center), wn1);
        const uint32_t c = mesh.addVertex(boardToWorld(p1, yBot, center), wn1);
        const uint32_t d = mesh.addVertex(boardToWorld(p0, yBot, center), wn0);
        mesh.addTriangle(a, c, b, mat);
        mesh.addTriangle(a, d, c, mat);
    }
}

}  // namespace

void extrudeRegion(const Paths& region, double yFace, double yBase, bool faceUp, uint32_t faceMat, uint32_t wallMat,
                   Vec2 center, Mesh& mesh) {
    if (region.empty()) return;
    Clipper2Lib::PolyTree64 tree;
    Clipper2Lib::Clipper64 clip;
    clip.AddSubject(region);
    clip.Execute(Clipper2Lib::ClipType::Union, Clipper2Lib::FillRule::NonZero, tree);
    triangulateFaces(tree, {{yFace, faceUp ? 1.0f : -1.0f, faceMat}}, center, mesh);
    if (std::abs(yFace - yBase) < 1e-9) return;
    std::vector<const Path*> rings;
    collectRings(tree, rings);
    const double hi = std::max(yFace, yBase), lo = std::min(yFace, yBase);
    for (const Path* r : rings) buildWalls(*r, hi, lo, center, [&](Vec2) { return wallMat; }, mesh);
}

BoardGeometry buildBoardGeometry(const Board& board, const BoardBuildOptions& opts) {
    BoardGeometry geo;
    geo.thickness = board.stackup.thickness;
    geo.coreHalf = opts.coreHalf > 0 ? opts.coreHalf : geo.thickness / 2;

    int openChains = 0;
    auto loops = outlineLoops(board, openChains);
    if (openChains) log::warn("Edge.Cuts: {} outline chain(s) are not closed; closing them", openChains);
    Paths loopPaths;
    for (const auto& l : loops) loopPaths.push_back(toPath(l));
    geo.region = unite(loopPaths, Clipper2Lib::FillRule::EvenOdd);

    if (geo.region.empty()) {
        // 外形が無い場合はパッド・配線の範囲から仮の外形を作る
        Bounds2 b;
        for (const auto& p : board.pads) b.add(p.pos);
        for (const auto& t : board.tracks) {
            b.add(t.start);
            b.add(t.end);
        }
        if (!b.valid()) b = Bounds2{{-10, -10}, {10, 10}};
        b.lo -= Vec2{2, 2};
        b.hi += Vec2{2, 2};
        geo.region = {toPath({b.lo, {b.hi.x, b.lo.y}, b.hi, {b.lo.x, b.hi.y}})};
        geo.outlineFromFallback = true;
        log::warn("no closed Edge.Cuts outline; using bounding box of copper items");
    }
    for (const auto& path : geo.region)
        for (const auto& q : path) geo.bounds.add(fromPt(q));
    geo.center = geo.bounds.center();

    // 穴
    Paths holes;
    std::vector<PlatedHole> plated;
    for (const auto& p : board.pads) {
        if (!p.drill.present) continue;
        if (p.type != PadType::ThruHole && p.type != PadType::NpThruHole) continue;
        const Vec2 c = p.pos + kicad::rotatePoint(p.drill.offset, p.angleDeg);
        addHole(c, p.angleDeg, p.drill.size, p.drill.oval, p.type == PadType::ThruHole, holes, plated);
        ++geo.holeCount;
    }
    for (const auto& v : board.vias) {
        if (v.drill <= 0) continue;
        addHole(v.pos, 0, {v.drill, v.drill}, false, true, holes, plated);
        ++geo.holeCount;
    }

    Clipper2Lib::PolyTree64 tree;
    Clipper2Lib::Clipper64 clip;
    clip.AddSubject(geo.region);
    clip.AddClip(unite(holes));
    clip.Execute(Clipper2Lib::ClipType::Difference, Clipper2Lib::FillRule::NonZero, tree);
    geo.body = Clipper2Lib::PolyTreeToPaths64(tree);

    const double yTop = geo.coreHalf, yBot = -geo.coreHalf;
    if (opts.caps) triangulateFaces(tree, {{yTop, 1.0f, BoardTop}, {yBot, -1.0f, BoardBottom}}, geo.center, geo.mesh);
    std::vector<const Path*> rings;
    collectRings(tree, rings);
    auto wallMat = [&](Vec2 mid) -> uint32_t {
        for (const auto& h : plated)
            if (std::abs(distToSegment(mid, h.a, h.b) - h.r) < 0.02) return BoardPlated;
        return BoardEdge;
    };
    for (const Path* r : rings) buildWalls(*r, yTop, yBot, geo.center, wallMat, geo.mesh);

    log::info("board: {:.2f} x {:.2f} mm, thickness {:.2f} mm, {} holes, {} triangles", geo.bounds.size().x,
              geo.bounds.size().y, geo.thickness, geo.holeCount, geo.mesh.triangleCount());
    return geo;
}

}  // namespace krbr::geom
