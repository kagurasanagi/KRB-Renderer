#include "geom/shapes.hpp"

#include <algorithm>
#include <cmath>
#include <numbers>

namespace krbr::geom {

using namespace Clipper2Lib;
using kicad::Graphic;
using kicad::GrKind;
using kicad::Pad;
using kicad::PadShape;

Point64 toPt(Vec2 p) { return Point64(std::llround(p.x * kScale), std::llround(p.y * kScale)); }
Vec2 fromPt(const Point64& p) { return {p.x / kScale, p.y / kScale}; }

Path toPath(const std::vector<Vec2>& pts) {
    Path p;
    p.reserve(pts.size());
    for (Vec2 v : pts) p.push_back(toPt(v));
    return p;
}

std::vector<Vec2> fromPath(const Path& p) {
    std::vector<Vec2> out;
    out.reserve(p.size());
    for (const auto& q : p) out.push_back(fromPt(q));
    return out;
}

static int segmentsFor(double r, double sweep, double tol) {
    if (r <= tol) return std::max(4, static_cast<int>(std::ceil(std::abs(sweep) / (std::numbers::pi / 4))));
    const double step = 2.0 * std::acos(1.0 - tol / r);
    return std::clamp(static_cast<int>(std::ceil(std::abs(sweep) / step)), 4, 4096);
}

std::vector<Vec2> arcPoints(Vec2 a, Vec2 m, Vec2 b, double tol) {
    const double d = 2.0 * (a.x * (m.y - b.y) + m.x * (b.y - a.y) + b.x * (a.y - m.y));
    if (std::abs(d) < 1e-12) return {a, b};
    const double a2 = glm::dot(a, a), m2 = glm::dot(m, m), b2 = glm::dot(b, b);
    const Vec2 c{(a2 * (m.y - b.y) + m2 * (b.y - a.y) + b2 * (a.y - m.y)) / d,
                 (a2 * (b.x - m.x) + m2 * (a.x - b.x) + b2 * (m.x - a.x)) / d};
    const double r = glm::length(a - c);
    const double t0 = std::atan2(a.y - c.y, a.x - c.x);
    const double tm = std::atan2(m.y - c.y, m.x - c.x);
    const double t1 = std::atan2(b.y - c.y, b.x - c.x);
    auto norm = [](double t) {
        t = std::fmod(t, 2 * std::numbers::pi);
        return t < 0 ? t + 2 * std::numbers::pi : t;
    };
    double sweep = norm(t1 - t0);
    if (norm(tm - t0) > sweep) sweep -= 2 * std::numbers::pi;
    const int n = segmentsFor(r, sweep, tol);
    std::vector<Vec2> out;
    out.reserve(n + 1);
    out.push_back(a);
    for (int i = 1; i < n; ++i) {
        const double t = t0 + sweep * i / n;
        out.push_back(c + r * Vec2{std::cos(t), std::sin(t)});
    }
    out.push_back(b);
    return out;
}

std::vector<Vec2> circlePoints(Vec2 c, double r, double tol) {
    const int n = segmentsFor(r, 2 * std::numbers::pi, tol);
    std::vector<Vec2> out;
    out.reserve(n);
    for (int i = 0; i < n; ++i) {
        const double t = 2 * std::numbers::pi * i / n;
        out.push_back(c + r * Vec2{std::cos(t), std::sin(t)});
    }
    return out;
}

std::vector<Vec2> bezierPoints(const std::vector<Vec2>& c, double tol) {
    if (c.size() != 4) return c;
    const double len = glm::length(c[1] - c[0]) + glm::length(c[2] - c[1]) + glm::length(c[3] - c[2]);
    const int n = std::clamp(static_cast<int>(std::ceil(std::sqrt(len / tol))), 4, 512);
    std::vector<Vec2> out;
    for (int i = 0; i <= n; ++i) {
        const double t = static_cast<double>(i) / n, u = 1 - t;
        out.push_back(u * u * u * c[0] + 3 * u * u * t * c[1] + 3 * u * t * t * c[2] + t * t * t * c[3]);
    }
    return out;
}

Paths strokePolyline(const std::vector<Vec2>& pts, double width, bool closed) {
    if (pts.empty() || width <= 0.0) return {};
    Paths in{toPath(pts)};
    if (pts.size() == 1 || (!closed && pts.size() == 2 && pts[0] == pts[1])) return disc(pts[0], width / 2);
    return InflatePaths(in, width / 2 * kScale, JoinType::Round, closed ? EndType::Joined : EndType::Round, 2.0,
                        kArcTol * kScale);
}

Paths disc(Vec2 c, double r) {
    if (r <= 0) return {};
    return {toPath(circlePoints(c, r))};
}

Paths graphicToPaths(const Graphic& g) {
    Paths out;
    switch (g.kind) {
        case GrKind::Line: append(out, strokePolyline({g.start, g.end}, g.width, false)); break;
        case GrKind::Arc: append(out, strokePolyline(arcPoints(g.start, g.mid, g.end), g.width, false)); break;
        case GrKind::Circle: {
            const double r = glm::length(g.end - g.start);
            if (g.filled)
                append(out, disc(g.start, r + g.width / 2));
            else
                append(out, strokePolyline(circlePoints(g.start, r), g.width, true));
            break;
        }
        case GrKind::Poly: {
            if (g.pts.size() < 2) break;
            if (g.filled && g.pts.size() >= 3) append(out, unite({toPath(g.pts)}));
            if (g.width > 0) append(out, strokePolyline(g.pts, g.width, true));
            break;
        }
        case GrKind::Curve: append(out, strokePolyline(bezierPoints(g.pts), g.width, false)); break;
    }
    return out;
}

namespace {

std::vector<Vec2> rectPts(Vec2 size) {
    const Vec2 h = size / 2.0;
    return {{-h.x, -h.y}, {h.x, -h.y}, {h.x, h.y}, {-h.x, h.y}};
}

std::vector<Vec2> chamferedRectPts(Vec2 size, double chamfer, unsigned corners) {
    const Vec2 h = size / 2.0;
    std::vector<Vec2> out;
    const double c = std::min({chamfer, h.x, h.y});
    // 左上から時計回り（y 下向き）
    if (corners & 1) {
        out.push_back({-h.x, -h.y + c});
        out.push_back({-h.x + c, -h.y});
    } else {
        out.push_back({-h.x, -h.y});
    }
    if (corners & 2) {
        out.push_back({h.x - c, -h.y});
        out.push_back({h.x, -h.y + c});
    } else {
        out.push_back({h.x, -h.y});
    }
    if (corners & 8) {
        out.push_back({h.x, h.y - c});
        out.push_back({h.x - c, h.y});
    } else {
        out.push_back({h.x, h.y});
    }
    if (corners & 4) {
        out.push_back({-h.x + c, h.y});
        out.push_back({-h.x, h.y - c});
    } else {
        out.push_back({-h.x, h.y});
    }
    return out;
}

// 原点中心・未回転の基本形
Paths basicShape(PadShape shape, Vec2 size, const Pad& p) {
    const double minDim = std::min(size.x, size.y);
    switch (shape) {
        case PadShape::Circle: return disc({0, 0}, size.x / 2);
        case PadShape::Rect: return {toPath(rectPts(size))};
        case PadShape::Oval: {
            if (std::abs(size.x - size.y) < 1e-9) return disc({0, 0}, size.x / 2);
            const double l = (std::max(size.x, size.y) - minDim) / 2;
            const std::vector<Vec2> seg = size.x > size.y ? std::vector<Vec2>{{-l, 0}, {l, 0}}
                                                          : std::vector<Vec2>{{0, -l}, {0, l}};
            return strokePolyline(seg, minDim, false);
        }
        case PadShape::Trapezoid: {
            const Vec2 h = size / 2.0, d = p.rectDelta / 2.0;
            return {toPath({{-h.x - d.y, h.y + d.x}, {h.x + d.y, h.y - d.x}, {h.x - d.y, -h.y + d.x},
                            {-h.x + d.y, -h.y - d.x}})};
        }
        case PadShape::RoundRect: {
            const double r = std::clamp(p.roundRatio, 0.0, 0.5) * minDim;
            Paths base{toPath(p.chamferCorners ? chamferedRectPts(size, p.chamferRatio * minDim, p.chamferCorners)
                                               : rectPts(size))};
            if (r <= 0) return base;
            Paths shrunk = InflatePaths(base, -r * kScale, JoinType::Miter, EndType::Polygon, 10.0);
            if (shrunk.empty()) return disc({0, 0}, minDim / 2);
            return InflatePaths(shrunk, r * kScale, JoinType::Round, EndType::Polygon, 2.0, kArcTol * kScale);
        }
        case PadShape::Custom: break;
    }
    return {};
}

Paths transformPaths(const Paths& in, Vec2 pos, double angleDeg) {
    Paths out;
    out.reserve(in.size());
    for (const auto& path : in) {
        Path q;
        q.reserve(path.size());
        for (const auto& pt : path) q.push_back(toPt(pos + kicad::rotatePoint(fromPt(pt), angleDeg)));
        out.push_back(std::move(q));
    }
    return out;
}

}  // namespace

Paths padToPaths(const Pad& p, double inflate) {
    Paths local;
    if (p.shape == PadShape::Custom) {
        append(local, basicShape(p.customAnchor, p.size, p));
        for (const auto& g : p.primitives) append(local, graphicToPaths(g));
        local = unite(local);
    } else {
        local = basicShape(p.shape, p.size, p);
    }
    if (inflate != 0.0 && !local.empty())
        local = InflatePaths(local, inflate * kScale, JoinType::Round, EndType::Polygon, 2.0, kArcTol * kScale);
    return transformPaths(local, p.pos, p.angleDeg);
}

Paths trackToPaths(const kicad::Track& t) {
    if (t.mid) return strokePolyline(arcPoints(t.start, *t.mid, t.end), t.width, false);
    return strokePolyline({t.start, t.end}, t.width, false);
}

Paths drillToPaths(Vec2 center, double angleDeg, Vec2 size, bool oval) {
    if (!oval || std::abs(size.x - size.y) < 1e-9) return disc(center, size.x / 2);
    const double minDim = std::min(size.x, size.y);
    const double l = (std::max(size.x, size.y) - minDim) / 2;
    const Vec2 dir = size.x > size.y ? Vec2{1, 0} : Vec2{0, 1};
    const Vec2 a = center + kicad::rotatePoint(-l * dir, angleDeg);
    const Vec2 b = center + kicad::rotatePoint(l * dir, angleDeg);
    return strokePolyline({a, b}, minDim, false);
}

Paths unite(const Paths& p, FillRule rule) { return Union(p, rule); }
Paths subtract(const Paths& a, const Paths& b) { return Difference(a, b, FillRule::NonZero); }
Paths intersect(const Paths& a, const Paths& b) { return Intersect(a, b, FillRule::NonZero); }
void append(Paths& dst, const Paths& src) { dst.insert(dst.end(), src.begin(), src.end()); }

}  // namespace krbr::geom
