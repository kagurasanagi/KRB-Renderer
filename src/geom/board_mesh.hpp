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
    double coreHalf = 0.8;  // 基材（側面・穴の内壁）の上下面の高さ ±coreHalf
    Mesh mesh;        // ワールド座標（X = x - cx, Y = 上, Z = y - cy）
    int holeCount = 0;
    bool outlineFromFallback = false;
};

struct BoardBuildOptions {
    double coreHalf = -1.0;  // 基材の半分の厚み（<=0 なら板厚の半分）
    bool caps = true;        // 上下面を作る（表面の凹凸を別に作る場合は false）
};

BoardGeometry buildBoardGeometry(const kicad::Board& board, const BoardBuildOptions& opts = {});

// region を押し出す：yFace の高さに面（faceUp で向き）、yFace〜yBase に側壁を作る（高さ 0 なら面だけ）
void extrudeRegion(const Paths& region, double yFace, double yBase, bool faceUp, uint32_t faceMat, uint32_t wallMat,
                   Vec2 center, Mesh& mesh);

// 基板座標 -> ワールド座標
inline glm::vec3 boardToWorld(Vec2 p, double y, Vec2 center) {
    return {static_cast<float>(p.x - center.x), static_cast<float>(y), static_cast<float>(p.y - center.y)};
}

}  // namespace krbr::geom
