#pragma once
// 層ごとの図形を基板面のテクスチャに描き込む。シェーダはこれを見て材質（レジスト・銅・シルク・基材）を決める。
#include <cstdint>
#include <vector>

#include "geom/board_mesh.hpp"
#include "raster/rasterizer.hpp"

namespace krbr::raster {

enum Side { Front = 0, Back = 1 };

struct LayerArtOptions {
    double pxPerMm = 50.0;   // 20 µm/px
    int maxDim = 8192;
    double marginMm = 0.5;
    bool clipSilkByMask = true;
    const geom::Paths* extraSilk[2] = {nullptr, nullptr};  // kicad-cli から得たシルク（線フォントの文字など）
};

struct LayerTextures {
    Grid grid;                       // 基板座標と画素の対応（表裏共通）
    std::vector<uint8_t> rgba[2];    // RGBA8: R=銅箔, G=レジスト開口, B=シルク, A=255
    struct Stats {
        size_t copperPaths = 0, maskPaths = 0, silkPaths = 0;
    } stats[2];
};

// 面ごとの銅箔・レジスト開口・シルクの多角形（和をとった後）
struct SidePaths {
    geom::Paths copper, maskOpen, silk;
};
// clip を渡すと、その矩形にかかる項目だけを集め、結果も矩形で切り抜く（タイル単位の作り直し用）
SidePaths collectSidePaths(const kicad::Board& board, Side side, const LayerArtOptions& opts,
                           const kicad::Box2* clip = nullptr);

LayerTextures buildLayerTextures(const kicad::Board& board, const geom::BoardGeometry& geo,
                                 const LayerArtOptions& opts);

}  // namespace krbr::raster
