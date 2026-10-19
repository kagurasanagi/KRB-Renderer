#pragma once
// kicad-cli（ヘッドレス）で、自前では形を作れない線フォントの文字を含むシルク層を SVG として得る。
// 対象の .kicad_pcb は一時フォルダへコピーしてから処理する（kicad-cli はプロジェクトに .kicad_prl を作るため）。
#include <filesystem>
#include <optional>

#include "geom/shapes.hpp"
#include "kicad/board.hpp"

namespace krbr::app {

std::optional<std::filesystem::path> findKicadCli(const std::string& configured);

// 表裏のシルク層の多角形（失敗した面は nullopt）
struct SilkPlot {
    std::optional<geom::Paths> side[2];
    bool fromCache = false;
};

SilkPlot plotSilkWithKicadCli(const std::filesystem::path& cli, const kicad::Board& board,
                              const std::filesystem::path& cacheDir, bool useCache);

}  // namespace krbr::app
