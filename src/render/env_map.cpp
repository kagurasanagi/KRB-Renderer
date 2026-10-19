#include "render/env_map.hpp"

#include <algorithm>
#include <cmath>
#include <numbers>

namespace krbr::render {

EnvDistribution buildEnvDistribution(const io::ImageF& img, uint32_t maxWidth) {
    EnvDistribution d;
    const uint32_t f = std::max(1u, (static_cast<uint32_t>(img.width) + maxWidth - 1) / maxWidth);
    d.width = std::max(1u, static_cast<uint32_t>(img.width) / f);
    d.height = std::max(1u, static_cast<uint32_t>(img.height) / f);
    const uint32_t w = d.width, h = d.height;

    std::vector<double> func(static_cast<size_t>(w) * h);
    for (uint32_t j = 0; j < h; ++j) {
        const double sinTheta = std::sin((j + 0.5) / h * std::numbers::pi);
        for (uint32_t i = 0; i < w; ++i) {
            double sum = 0;
            for (uint32_t y = 0; y < f; ++y)
                for (uint32_t x = 0; x < f; ++x) {
                    const size_t sx = std::min<size_t>(static_cast<size_t>(i) * f + x, img.width - 1);
                    const size_t sy = std::min<size_t>(static_cast<size_t>(j) * f + y, img.height - 1);
                    const float* p = &img.rgba[(sy * img.width + sx) * 4];
                    sum += 0.2126 * std::max(0.0f, p[0]) + 0.7152 * std::max(0.0f, p[1]) + 0.0722 * std::max(0.0f, p[2]);
                }
            func[static_cast<size_t>(j) * w + i] = sum / (f * f) * sinTheta;
        }
    }

    d.conditionalCdf.resize(static_cast<size_t>(h) * (w + 1));
    std::vector<double> rowIntegral(h);
    for (uint32_t j = 0; j < h; ++j) {
        float* cdf = &d.conditionalCdf[static_cast<size_t>(j) * (w + 1)];
        double acc = 0;
        std::vector<double> tmp(w + 1);
        tmp[0] = 0;
        for (uint32_t i = 0; i < w; ++i) {
            acc += func[static_cast<size_t>(j) * w + i] / w;
            tmp[i + 1] = acc;
        }
        rowIntegral[j] = acc;
        for (uint32_t i = 0; i <= w; ++i) cdf[i] = acc > 0 ? static_cast<float>(tmp[i] / acc) : static_cast<float>(i) / w;
        cdf[w] = 1.0f;
    }
    d.marginalCdf.resize(h + 1);
    double acc = 0;
    std::vector<double> tmp(h + 1);
    for (uint32_t j = 0; j < h; ++j) {
        acc += rowIntegral[j] / h;
        tmp[j + 1] = acc;
    }
    d.integral = acc;
    for (uint32_t j = 0; j <= h; ++j)
        d.marginalCdf[j] = acc > 0 ? static_cast<float>(tmp[j] / acc) : static_cast<float>(j) / h;
    d.marginalCdf[h] = 1.0f;

    d.pdf.resize(static_cast<size_t>(w) * h);
    for (size_t k = 0; k < d.pdf.size(); ++k) d.pdf[k] = acc > 0 ? static_cast<float>(func[k] / acc) : 1.0f;
    return d;
}

}  // namespace krbr::render
