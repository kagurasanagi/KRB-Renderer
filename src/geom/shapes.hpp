#pragma once
// KiCad の図形・パッド・配線を多角形（Clipper2 の整数座標）に変換する
#include <clipper2/clipper.h>

#include <vector>

#include "kicad/board.hpp"

namespace krbr::geom {

using kicad::Vec2;
using Paths = Clipper2Lib::Paths64;
using Path = Clipper2Lib::Path64;

// 1 単位 = 10 nm
constexpr double kScale = 1e5;
// 円弧を折れ線にするときの許容誤差（mm）
constexpr double kArcTol = 0.002;

Clipper2Lib::Point64 toPt(Vec2 p);
Vec2 fromPt(const Clipper2Lib::Point64& p);
Path toPath(const std::vector<Vec2>& pts);
std::vector<Vec2> fromPath(const Path& p);

std::vector<Vec2> arcPoints(Vec2 a, Vec2 m, Vec2 b, double tol = kArcTol);
std::vector<Vec2> circlePoints(Vec2 c, double r, double tol = kArcTol);
std::vector<Vec2> bezierPoints(const std::vector<Vec2>& ctrl, double tol = kArcTol);

Paths strokePolyline(const std::vector<Vec2>& pts, double width, bool closed);
Paths disc(Vec2 c, double r);
Paths graphicToPaths(const kicad::Graphic& g);
// パッドの形（inflate > 0 でレジスト開口などの拡大）
Paths padToPaths(const kicad::Pad& p, double inflate = 0.0);
Paths trackToPaths(const kicad::Track& t);
// 穴の形（円または長円）
Paths drillToPaths(Vec2 center, double angleDeg, Vec2 size, bool oval);

Paths unite(const Paths& p, Clipper2Lib::FillRule rule = Clipper2Lib::FillRule::NonZero);
Paths subtract(const Paths& a, const Paths& b);
Paths intersect(const Paths& a, const Paths& b);
void append(Paths& dst, const Paths& src);

}  // namespace krbr::geom
