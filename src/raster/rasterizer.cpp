#include "raster/rasterizer.hpp"

#include <algorithm>
#include <cmath>
#include <thread>

namespace krbr::raster {

namespace {

constexpr int kSub = 4;

struct Edge {
    double y0, y1;  // y0 < y1
    double x0;      // y0 での x
    double dxdy;
    int dir;
};

struct Crossing {
    double x;
    int dir;
};

void fillSpan(std::vector<float>& acc, int width, double xa, double xb, float w) {
    xa = std::clamp(xa, 0.0, static_cast<double>(width));
    xb = std::clamp(xb, 0.0, static_cast<double>(width));
    if (xb <= xa) return;
    const int ia = static_cast<int>(xa), ib = static_cast<int>(xb);
    if (ia == ib) {
        acc[ia] += static_cast<float>((xb - xa)) * w;
        return;
    }
    acc[ia] += static_cast<float>(ia + 1 - xa) * w;
    for (int i = ia + 1; i < ib; ++i) acc[i] += w;
    if (ib < width) acc[ib] += static_cast<float>(xb - ib) * w;
}

void rasterBand(const std::vector<Edge>& edges, int width, int rowBegin, int rowEnd, std::vector<float>& cov) {
    std::vector<const Edge*> active;
    // 帯の開始より前から続いている辺も含めて、帯に関係する辺を y0 順に処理する
    size_t next = 0;
    const double bandTop = rowBegin;
    while (next < edges.size() && edges[next].y0 <= bandTop) {
        if (edges[next].y1 > bandTop) active.push_back(&edges[next]);
        ++next;
    }
    std::vector<float> acc(width + 1);
    std::vector<Crossing> xs;
    const float w = 1.0f / kSub;
    for (int row = rowBegin; row < rowEnd; ++row) {
        std::fill(acc.begin(), acc.end(), 0.0f);
        for (int s = 0; s < kSub; ++s) {
            const double y = row + (s + 0.5) / kSub;
            while (next < edges.size() && edges[next].y0 <= y) active.push_back(&edges[next++]);
            xs.clear();
            size_t keep = 0;
            for (size_t i = 0; i < active.size(); ++i) {
                const Edge* e = active[i];
                if (e->y1 <= y) continue;
                active[keep++] = e;
                if (e->y0 <= y) xs.push_back({e->x0 + (y - e->y0) * e->dxdy, e->dir});
            }
            active.resize(keep);
            if (xs.empty()) continue;
            std::sort(xs.begin(), xs.end(), [](const Crossing& a, const Crossing& b) { return a.x < b.x; });
            int winding = 0;
            for (size_t i = 0; i + 1 < xs.size(); ++i) {
                winding += xs[i].dir;
                if (winding != 0) fillSpan(acc, width, xs[i].x, xs[i + 1].x, w);
            }
        }
        float* out = cov.data() + static_cast<size_t>(row) * width;
        for (int x = 0; x < width; ++x) out[x] = std::min(1.0f, acc[x]);
    }
}

}  // namespace

void rasterizeCoverage(const Paths& paths, const Grid& g, std::vector<float>& cov) {
    cov.assign(static_cast<size_t>(g.width) * g.height, 0.0f);
    std::vector<Edge> edges;
    for (const auto& path : paths) {
        const size_t n = path.size();
        if (n < 3) continue;
        for (size_t i = 0; i < n; ++i) {
            const Vec2 a = geom::fromPt(path[i]), b = geom::fromPt(path[(i + 1) % n]);
            const double ax = (a.x - g.origin.x) * g.pxPerMm, ay = (a.y - g.origin.y) * g.pxPerMm;
            const double bx = (b.x - g.origin.x) * g.pxPerMm, by = (b.y - g.origin.y) * g.pxPerMm;
            if (ay == by) continue;
            Edge e;
            if (ay < by) {
                e = {ay, by, ax, (bx - ax) / (by - ay), 1};
            } else {
                e = {by, ay, bx, (ax - bx) / (ay - by), -1};
            }
            if (e.y1 <= 0 || e.y0 >= g.height) continue;
            edges.push_back(e);
        }
    }
    if (edges.empty()) return;
    std::sort(edges.begin(), edges.end(), [](const Edge& a, const Edge& b) { return a.y0 < b.y0; });

    const int threads = std::clamp(static_cast<int>(std::thread::hardware_concurrency()) / 2, 1, 8);
    const int band = (g.height + threads - 1) / threads;
    std::vector<std::thread> pool;
    for (int t = 0; t < threads; ++t) {
        const int r0 = t * band, r1 = std::min(g.height, r0 + band);
        if (r0 >= r1) break;
        pool.emplace_back([&, r0, r1] { rasterBand(edges, g.width, r0, r1, cov); });
    }
    for (auto& th : pool) th.join();
}

}  // namespace krbr::raster
