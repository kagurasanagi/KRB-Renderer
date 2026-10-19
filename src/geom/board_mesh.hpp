#pragma once
// 基板外形を押し出した板（上下面・側面・穴の内壁）を作る
#include "geom/mesh.hpp"
#include "geom/shapes.hpp"

namespace krbr::geom {

// 基板メッシュの三角形ごとの材質区分
enum BoardMat : uint32_t {
    BoardTop = 0,     // 上面（層テクスチャで材質を決める）
    BoardBottom = 1,  // 下面
    BoardEdge = 2,    // 外形の側面・メッキなし穴（FR4 の断面）
    BoardPlated = 3,  // メッキ穴の内壁
};

struct Bounds2 {
    Vec2 lo{1e30}, hi{-1e30};
    void add(Vec2 p) {
        lo = glm::min(lo, p);
        hi = glm::max(hi, p);
    }
    bool valid() const { return lo.x <= hi.x; }
    Vec2 center() const { return (lo + hi) * 0.5; }
    Vec2 size() const { return hi - lo; }
};

struct BoardGeometry {
    Paths region;     // 穴を開ける前の基板領域
    Paths body;       // 穴を開けた後の基板領域
    Bounds2 bounds;   // 基板座標での外形範囲
    Vec2 center{};    // ワールド原点に置く基板座標
    double thickness = 1.6;
    Mesh mesh;        // ワールド座標（X = x - cx, Y = 上, Z = y - cy）
    int holeCount = 0;
    bool outlineFromFallback = false;
};

BoardGeometry buildBoardGeometry(const kicad::Board& board);

// 基板座標 -> ワールド座標
inline glm::vec3 boardToWorld(Vec2 p, double y, Vec2 center) {
    return {static_cast<float>(p.x - center.x), static_cast<float>(y), static_cast<float>(p.y - center.y)};
}

}  // namespace krbr::geom
