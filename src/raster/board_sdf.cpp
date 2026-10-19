#include "raster/board_sdf.hpp"

#include <algorithm>
#include <cmath>
#include <limits>

namespace krbr::raster {

namespace {

constexpr double kInf = 1e20;

// 1 次元の距離変換（放物線の下側包絡線）。f は 0（対象）か kInf
void dt1d(const double* f, int n, double* d, int* v, double* z) {
    int k = 0;
    v[0] = 0;
    z[0] = -kInf;
    z[1] = kInf;
    auto inter = [&](int q, int p) { return ((f[q] + double(q) * q) - (f[p] + double(p) * p)) / (2.0 * (q - p)); };
    for (int q = 1; q < n; ++q) {
        double s = inter(q, v[k]);
        while (s <= z[k] && k > 0) {
            --k;
            s = inter(q, v[k]);
        }
        if (s <= z[k]) {  // k == 0 で、最初の放物線を置き換える
            v[0] = q;
            continue;
        }
        ++k;
        v[k] = q;
        z[k] = s;
        z[k + 1] = kInf;
    }
    k = 0;
    for (int q = 0; q < n; ++q) {
        while (z[k + 1] < q) ++k;
        const double dq = q - v[k];
        d[q] = dq * dq + f[v[k]];
    }
}

// target の画素までの距離の 2 乗
std::vector<float> squaredDistance(const std::vector<uint8_t>& mask, uint8_t target, int w, int h) {
    std::vector<double> g(static_cast<size_t>(w) * h);
    for (size_t i = 0; i < g.size(); ++i) g[i] = mask[i] == target ? 0.0 : kInf;
    const int n = std::max(w, h);
    std::vector<double> f(n), d(n), z(n + 1);
    std::vector<int> v(n);
    for (int x = 0; x < w; ++x) {  // 列
        for (int y = 0; y < h; ++y) f[y] = g[static_cast<size_t>(y) * w + x];
        dt1d(f.data(), h, d.data(), v.data(), z.data());
        for (int y = 0; y < h; ++y) g[static_cast<size_t>(y) * w + x] = d[y];
    }
    for (int y = 0; y < h; ++y) {  // 行
        double* row = g.data() + static_cast<size_t>(y) * w;
        std::copy(row, row + w, f.begin());
        dt1d(f.data(), w, d.data(), v.data(), z.data());
        std::copy(d.begin(), d.begin() + w, row);
    }
    std::vector<float> out(g.size());
    for (size_t i = 0; i < g.size(); ++i) out[i] = static_cast<float>(std::min(g[i], 1e12));
    return out;
}

}  // namespace

std::vector<float> signedDistancePx(const std::vector<uint8_t>& inside, int w, int h) {
    const auto toInside = squaredDistance(inside, 1, w, h);
    const auto toOutside = squaredDistance(inside, 0, w, h);
    std::vector<float> sd(static_cast<size_t>(w) * h);
    for (size_t i = 0; i < sd.size(); ++i) {
        // 縁は隣り合う画素の中心の中間にあるので 0.5 画素ずらす
        if (inside[i])
            sd[i] = -(std::sqrt(std::min(toOutside[i], 1e12f)) - 0.5f);
        else
            sd[i] = std::sqrt(std::min(toInside[i], 1e12f)) - 0.5f;
    }
    return sd;
}

void encodeSdf(const std::vector<float>& coverage, int w, int h, double pxPerMm, std::vector<uint8_t>& out) {
    std::vector<uint8_t> inside(coverage.size());
    for (size_t i = 0; i < coverage.size(); ++i) inside[i] = coverage[i] >= 0.5f ? 1 : 0;
    const auto sd = signedDistancePx(inside, w, h);
    out.resize(sd.size());
    const float scale = static_cast<float>(127.0 / (kSdfRangeMm * pxPerMm));
    for (size_t i = 0; i < sd.size(); ++i) {
        // 縁の画素は被覆率で位置を細かく補う（2 値化で失った画素以下の位置）
        float s = sd[i];
        if (std::abs(s) < 1.0f) s = 0.5f - coverage[i];
        out[i] = static_cast<uint8_t>(std::clamp(std::lround(128.0f + s * scale), 0L, 255L));
    }
}

}  // namespace krbr::raster
