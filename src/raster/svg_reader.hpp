#pragma once
// kicad-cli が出力する SVG（pcb export svg）を多角形に変換する最小限のリーダー。
// 対応: <g>/<path>/<circle>/<polyline>/<polygon>/<rect> と style の fill/stroke/stroke-width の継承。
#include <string>

#include "geom/shapes.hpp"

namespace krbr::raster {

struct SvgResult {
    geom::Paths paths;  // 基板座標（mm）
    int elements = 0;
    bool ok = false;
    std::string error;
};

SvgResult readKicadSvg(const std::string& text);

}  // namespace krbr::raster
