#pragma once
// 環境マップの重点的サンプリング用の分布（輝度 × sinθ に比例）
#include <cstdint>
#include <vector>

#include "io/image_io.hpp"

namespace krbr::render {

struct EnvDistribution {
    uint32_t width = 0, height = 0;
    std::vector<float> marginalCdf;     // height + 1
    std::vector<float> conditionalCdf;  // height * (width + 1)
    std::vector<float> pdf;             // width * height（(u,v) 上の密度。∫∫ pdf du dv = 1）
    double integral = 0.0;
};

// 分布は最大 maxWidth まで縮小して作る（放射輝度の参照は元の解像度のまま）
EnvDistribution buildEnvDistribution(const io::ImageF& img, uint32_t maxWidth = 2048);

}  // namespace krbr::render
