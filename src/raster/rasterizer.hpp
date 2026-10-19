#pragma once
#include <vector>

#include "geom/shapes.hpp"

namespace krbr::raster {

using geom::Paths;
using geom::Vec2;

// 基板座標の矩形を画素に対応づける。画素 (0,0) の左上角が origin、y は下向き（基板座標と同じ）
struct Grid {
    Vec2 origin{};
    double pxPerMm = 50.0;
    int width = 0;
    int height = 0;
};

// 非ゼロ規則で塗った多角形の被覆率（0..1）を cov（width*height）に書き込む。
// 縦は 4 本のサブ走査線、横は厳密な被覆面積でアンチエイリアスする。
void rasterizeCoverage(const Paths& paths, const Grid& grid, std::vector<float>& cov);

}  // namespace krbr::raster
