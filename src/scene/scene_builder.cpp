#include <algorithm>
#include <atomic>
#include <chrono>
#include <thread>
#include <cmath>
#include <glm/gtc/matrix_transform.hpp>
#include <map>
#include <numbers>

#include "core/log.hpp"
#include "core/platform.hpp"
#include "geom/relief.hpp"
#include "raster/board_sdf.hpp"
#include "scene/mask_presets.hpp"
#include "scene/scene.hpp"

namespace krbr::scene {

using kicad::Footprint;
using kicad::GrKind;
using kicad::Vec2;

namespace {

uint32_t addMaterial(Scene& s, Material m) {
    s.materials.push_back(std::move(m));
    return static_cast<uint32_t>(s.materials.size() - 1);
}

glm::vec3 stackupColor(const std::string& name, glm::vec3 fallback) {
    // KiCad の stackup の色名（"Green" など。プリセットと同じ表）または "#RRGGBBAA"
    if (const MaskPreset* p = findMaskPreset(name)) return p->srgb;
    if (name.size() >= 7 && name[0] == '#') {
        auto hex = [&](size_t i) { return std::stoi(name.substr(i, 2), nullptr, 16) / 255.0f; };
        try {
            return {hex(1), hex(3), hex(5)};
        } catch (...) {
        }
    }
    return fallback;
}

glm::vec3 resolvedMaskColor(const kicad::Board& board, const SceneOptions& o) {
    return o.maskColorSet ? o.maskColor : stackupColor(board.stackup.maskColorFront, o.maskColor);
}

Material makeMaskMaterial(glm::vec3 maskSrgb, float roughness, float scatter) {
    Material mask;
    mask.name = "solder_mask";
    // コートとして使う：coat_color がレジストを通る光の色、base_* は顔料自身の散乱
    mask.coat_weight = 1.0f;
    // 設定の色は「銅の上で見えるレジストの色」とし、銅の平均反射率（約 0.7）で割って透過色にする
    mask.coat_color = glm::min(srgbToLinear(maskSrgb) / 0.7f, glm::vec3(1.0f));
    mask.coat_roughness = roughness;
    mask.coat_ior = 1.5f;
    mask.base_color = srgbToLinear(maskSrgb) * 0.6f;
    mask.base_weight = scatter;  // シェーダではこの値を「散乱を選ぶ確率」として使う
    mask.specular_roughness = 0.5f;
    mask.bump_pattern = BumpPattern::Noise;  // 塗膜のゆず肌（光沢が強く映り込みが揺れやすいので、ごく弱く）
    mask.bump_strength = 0.006f;
    mask.bump_scale = 0.45f;
    mask.roughness_variation = 0.1f;
    return mask;
}

Material makeSilkMaterial(glm::vec3 silkSrgb) {
    Material silk;
    silk.name = "silkscreen";
    silk.base_color = srgbToLinear(silkSrgb);
    silk.specular_roughness = 0.65f;
    silk.bump_pattern = BumpPattern::Noise;  // インクのざらつき
    silk.bump_strength = 0.2f;
    silk.bump_scale = 0.06f;
    silk.roughness_variation = 0.3f;
    return silk;
}

void setupBoardMaterials(Scene& s, const kicad::Board& board, const SceneOptions& o) {
    Material top;
    top.name = "board_top";
    top.kind = MaterialKind::BoardSurface;
    top.boardSide = 0;
    Material bottom = top;
    bottom.name = "board_bottom";
    bottom.boardSide = 1;

    Material fr4;
    fr4.name = "fr4";
    fr4.base_color = srgbToLinear(o.fr4Color);
    fr4.specular_roughness = 0.55f;
    fr4.specular_ior = 1.5f;
    // わずかな透光性：入った光は基材の中で吸収されながら進み、反対側やふちから拡散して出る
    // 透光性：拡散して中へ入り、吸収されながら進む（krbr では subsurface をこの簡易な形で扱う）
    fr4.subsurface_weight = 0.15f;
    fr4.subsurface_color = {0.85f, 0.72f, 0.38f};
    fr4.subsurface_radius = 1.0f;
    fr4.bump_pattern = BumpPattern::Weave;  // ガラス繊維の織り（7628 程度のピッチ）
    fr4.bump_strength = 0.12f;
    fr4.bump_scale = 0.45f;
    fr4.roughness_variation = 0.25f;

    Material copper;
    copper.name = "copper";
    copper.base_color = {0.955f, 0.638f, 0.538f};
    copper.base_metalness = 1.0f;
    copper.specular_roughness = 0.25f;
    copper.bump_pattern = BumpPattern::Noise;
    copper.bump_strength = 0.015f;
    copper.bump_scale = 0.05f;
    copper.roughness_variation = 0.3f;

    // 表面処理：設定 > stackup の copper_finish > ENIG
    std::string finish = o.finish;
    if (finish == "auto") {
        const std::string f = board.stackup.copperFinish;
        if (f.find("HAL") != std::string::npos || f.find("HASL") != std::string::npos) finish = "HASL";
        else if (f.find("OSP") != std::string::npos) finish = "OSP";
        else if (f.find("Silver") != std::string::npos) finish = "HASL";
        else finish = "ENIG";
    }
    Material fin;
    fin.name = "finish_" + finish;
    fin.base_metalness = 1.0f;
    fin.bump_pattern = BumpPattern::Noise;
    if (finish == "HASL") {
        // はんだの盛り上がりによる、ゆるやかで大きな凹凸
        fin.base_color = {0.80f, 0.80f, 0.79f};
        fin.specular_roughness = 0.22f;
        fin.bump_strength = 0.10f;
        fin.bump_scale = 0.6f;
        fin.roughness_variation = 0.2f;
    } else if (finish == "OSP") {
        fin = copper;
        fin.name = "finish_OSP";
        fin.specular_roughness = 0.3f;
    } else {
        // 無電解ニッケル金：細かい粒状感
        fin.base_color = {1.0f, 0.766f, 0.336f};
        fin.specular_roughness = 0.18f;
        fin.bump_strength = 0.012f;
        fin.bump_scale = 0.05f;
        fin.roughness_variation = 0.2f;
    }

    const glm::vec3 maskSrgb = resolvedMaskColor(board, o);
    const Material mask = makeMaskMaterial(maskSrgb, o.maskRoughness, o.maskScatter);

    const Material silk = makeSilkMaterial(o.silkColor);

    s.boardMats.top = addMaterial(s, top);
    s.boardMats.bottom = addMaterial(s, bottom);
    s.boardMats.fr4 = addMaterial(s, fr4);
    s.boardMats.edge = s.boardMats.fr4;
    s.boardMats.copper = addMaterial(s, copper);
    s.boardMats.finish = addMaterial(s, fin);
    s.boardMats.plated = s.boardMats.finish;
    s.boardMats.mask = addMaterial(s, mask);
    s.boardMats.silk = addMaterial(s, silk);
    Material maskedFr4;
    maskedFr4.name = "solder_mask_on_fr4";
    maskedFr4.kind = MaterialKind::Masked;
    maskedFr4.maskedBase = s.boardMats.fr4;
    maskedFr4.maskedMask = s.boardMats.mask;
    Material maskedCu = maskedFr4;
    maskedCu.name = "solder_mask_on_copper";
    maskedCu.maskedBase = s.boardMats.copper;
    s.boardMats.maskOnFr4 = addMaterial(s, maskedFr4);
    s.boardMats.maskOnCopper = addMaterial(s, maskedCu);
    log::info("board materials: finish {}, mask sRGB ({:.2f}, {:.2f}, {:.2f}){}", finish, maskSrgb.r, maskSrgb.g,
              maskSrgb.b, o.maskPreset.empty() ? "" : " [" + o.maskPreset + "]");
}

// KiCad の 3D ビューアと同じ並びで部品・モデルの変換を作る（KiCad 3D 座標、mm、Z 上向き）
glm::dmat4 kicadModelMatrix(const Footprint& fp, const kicad::Model3D& m, double thickness) {
    using namespace glm;
    const double pi = std::numbers::pi;
    dmat4 M(1.0);
    M = translate(M, dvec3(fp.pos.x, -fp.pos.y, fp.back ? -thickness / 2 : thickness / 2));
    M = rotate(M, radians(fp.angleDeg), dvec3(0, 0, 1));
    if (fp.back) {
        M = rotate(M, pi, dvec3(0, 1, 0));
        M = rotate(M, pi, dvec3(0, 0, 1));
    }
    M = translate(M, m.offset);
    M = rotate(M, radians(-m.rotate.z), dvec3(0, 0, 1));
    M = rotate(M, radians(-m.rotate.y), dvec3(0, 1, 0));
    M = rotate(M, radians(-m.rotate.x), dvec3(1, 0, 0));
    M = scale(M, m.scale);
    return M;
}

// KiCad 3D 座標（x, y=上が正, z=基板の上方向）-> ワールド（X = x - cx, Y = z, Z = -y - cy）
glm::dmat4 kicadToWorld(Vec2 center) {
    glm::dmat4 R(1.0);
    R[0] = {1, 0, 0, 0};
    R[1] = {0, 0, -1, 0};
    R[2] = {0, 1, 0, 0};
    R[3] = {-center.x, 0, -center.y, 1};
    return R;
}

// 縁を 45° に面取りした箱（中心が原点、半分の大きさ h、面取りの幅 r）。面ごとに法線を持つ（平らな面）。
// 6 つの面・12 本の辺の面取り・8 つの角の三角形からなる凸な立体で、三角形は外向き（透明な材質で内外を判定するため）
geom::Mesh beveledBox(glm::vec3 h, float r, uint32_t mat) {
    geom::Mesh m;
    r = std::min({r, h.x * 0.95f, h.y * 0.95f, h.z * 0.95f});
    // 角 s（各成分 ±1）の近くの 3 点：軸 k の面の上にある点
    auto P = [&](glm::vec3 s, int k) {
        glm::vec3 p = s * (h - glm::vec3(r));
        p[k] = s[k] * h[k];
        return p;
    };
    auto poly = [&](std::vector<glm::vec3> v) {
        glm::vec3 c(0.0f);
        for (auto& p : v) c += p;
        c /= static_cast<float>(v.size());
        glm::vec3 n = glm::normalize(glm::cross(v[1] - v[0], v[2] - v[0]));
        if (glm::dot(n, c) < 0) {  // 外向き（原点から離れる向き）にそろえる
            std::reverse(v.begin(), v.end());
            n = -n;
        }
        const uint32_t base = static_cast<uint32_t>(m.positions.size());
        for (auto& p : v) m.addVertex(p, n);
        for (uint32_t i = 1; i + 1 < v.size(); ++i) m.addTriangle(base, base + i, base + i + 1, mat);
    };
    for (int k = 0; k < 3; ++k) {
        const int a = (k + 1) % 3, b = (k + 2) % 3;
        for (float sk : {-1.0f, 1.0f}) {
            // 面 k
            std::vector<glm::vec3> f;
            for (auto [sa, sb] : {std::pair{1.0f, 1.0f}, {1.0f, -1.0f}, {-1.0f, -1.0f}, {-1.0f, 1.0f}}) {
                glm::vec3 s;
                s[k] = sk;
                s[a] = sa;
                s[b] = sb;
                f.push_back(P(s, k));
            }
            poly(f);
            // 面 k と面 a の間の面取り（軸 b に沿う）
            for (float sa : {-1.0f, 1.0f}) {
                glm::vec3 s1, s2;
                s1[k] = s2[k] = sk;
                s1[a] = s2[a] = sa;
                s1[b] = 1.0f;
                s2[b] = -1.0f;
                poly({P(s1, k), P(s2, k), P(s2, a), P(s1, a)});
            }
        }
    }
    // 角の三角形
    for (int i = 0; i < 8; ++i) {
        const glm::vec3 s((i & 1) ? 1.0f : -1.0f, (i & 2) ? 1.0f : -1.0f, (i & 4) ? 1.0f : -1.0f);
        poly({P(s, 0), P(s, 1), P(s, 2)});
    }
    return m;
}

// 部品の外形線（Courtyard）またはパッドから、部品座標系（未回転）での範囲を求める
bool footprintLocalBox(const kicad::Board& board, int fpIndex, Vec2& lo, Vec2& hi) {
    const Footprint& fp = board.footprints[fpIndex];
    const std::string crt = fp.back ? "B.CrtYd" : "F.CrtYd";
    lo = Vec2{1e30};
    hi = Vec2{-1e30};
    auto add = [&](Vec2 p) {
        const Vec2 l = kicad::rotatePoint(p - fp.pos, -fp.angleDeg);
        lo = glm::min(lo, l);
        hi = glm::max(hi, l);
    };
    for (const auto& g : board.graphics) {
        if (g.footprint != fpIndex || g.layer != crt) continue;
        switch (g.kind) {
            case GrKind::Line:
                add(g.start);
                add(g.end);
                break;
            case GrKind::Arc:
                for (Vec2 p : geom::arcPoints(g.start, g.mid, g.end, 0.01)) add(p);
                break;
            case GrKind::Circle:
                for (Vec2 p : geom::circlePoints(g.start, glm::length(g.end - g.start), 0.01)) add(p);
                break;
            case GrKind::Poly:
            case GrKind::Curve:
                for (Vec2 p : g.pts) add(p);
                break;
        }
    }
    if (lo.x <= hi.x) return true;
    for (const auto& p : board.pads) {
        if (p.footprint != fpIndex) continue;
        for (const auto& path : geom::padToPaths(p))
            for (const auto& q : path) add(geom::fromPt(q));
    }
    if (lo.x <= hi.x) {
        lo -= Vec2{0.1, 0.1};
        hi += Vec2{0.1, 0.1};
        return true;
    }
    return false;
}

// 代替の箱の高さ（mm）：設定（> 0）があればそれ。無ければ面積が大きいほど高く（√面積 × 0.25、0.3〜5 mm）。
// 0603 の抵抗くらいで 0.3、5 x 4 mm の IC で約 1.1、10 x 10 mm で 2.5、20 x 20 mm 以上で 5
double placeholderHeightFor(double fixed, Vec2 lo, Vec2 hi) {
    if (fixed > 0.0) return fixed;
    const double area = std::max((hi.x - lo.x) * (hi.y - lo.y), 0.0);
    return std::clamp(0.25 * std::sqrt(area), 0.3, 5.0);
}

// 代替の箱（中心が原点の実寸のメッシュ）を置く行列：部品の向きに回し、基板の表（裏）の面の上（下）に置く。
// 伸縮や反転を含めない（透明な材質の内外の判定と、面取りの幅を保つため）
glm::mat4 placeholderMatrix(const Footprint& fp, Vec2 lo, Vec2 hi, double height, double thickness, Vec2 center) {
    const Vec2 ex = kicad::rotatePoint({1, 0}, fp.angleDeg);
    const Vec2 ez = kicad::rotatePoint({0, 1}, fp.angleDeg);
    const Vec2 c = fp.pos + kicad::rotatePoint((lo + hi) * 0.5, fp.angleDeg);
    constexpr double lift = 0.01;  // 基板の面とぴったり重ならないように少し浮かせる
    const double yc = (thickness / 2 + lift + height / 2) * (fp.back ? -1.0 : 1.0);
    glm::mat4 M(1.0f);
    M[0] = {static_cast<float>(ex.x), 0, static_cast<float>(ex.y), 0};
    M[1] = {0, 1, 0, 0};
    M[2] = {static_cast<float>(ez.x), 0, static_cast<float>(ez.y), 0};
    M[3] = {static_cast<float>(c.x - center.x), static_cast<float>(yc), static_cast<float>(c.y - center.y), 1};
    return M;
}

void addInstanceBounds(Scene& s, uint32_t mesh, const glm::mat4& xf) {
    const geom::Bounds3 b = s.meshes[mesh].mesh.bounds();
    if (!b.valid()) return;
    for (int i = 0; i < 8; ++i) {
        const glm::vec3 c{(i & 1) ? b.hi.x : b.lo.x, (i & 2) ? b.hi.y : b.lo.y, (i & 4) ? b.hi.z : b.lo.z};
        const glm::vec3 w = glm::vec3(xf * glm::vec4(c, 1.0f));
        s.bounds.add(w);
        s.framingPoints.push_back(w);
    }
}

// 凹凸メッシュをタイルごとに作る（キャッシュにある有効なタイルはそのまま使う）。タイルは並列に作る
void buildReliefTiles(Scene& s, const kicad::Board& board, const geom::ReliefParams& rp, const SceneOptions& opts,
                      ReliefTileCache* cache) {
    const auto t0 = std::chrono::steady_clock::now();
    ReliefTileCache local;
    ReliefTileCache& c = cache ? *cache : local;
    if (!c.matches(s.board)) c.reset(s.board, opts.layerArt.pxPerMm);
    const uint32_t rmap[5] = {s.boardMats.fr4, s.boardMats.finish, s.boardMats.maskOnFr4, s.boardMats.maskOnCopper,
                              s.boardMats.silk};

    // 面を張る範囲（表裏それぞれ、ふさがれたビアの穴を含める）
    geom::Paths base[2];
    std::vector<geom::ReliefDimple> tentedVias[2];  // くぼみ（タイルごとに、かかるものだけ渡す）
    for (int side = 0; side < 2; ++side) {
        geom::Paths tented;
        const bool tentDefault = side == 0 ? board.setup.tentFront : board.setup.tentBack;
        for (const auto& v : board.vias) {
            const std::optional<bool> tent = side == 0 ? v.tentFront : v.tentBack;
            if (tent.value_or(tentDefault) && v.drill > 0) {
                geom::append(tented, geom::disc(v.pos, v.drill / 2));
                tentedVias[side].push_back({v.pos, v.drill / 2, true});
            }
        }
        base[side] = geom::reliefBase(s.board, tented);
    }

    std::vector<std::pair<int, int>> todo;  // (面, タイル)
    for (int side = 0; side < 2; ++side)
        for (int t = 0; t < c.count(); ++t)
            if (!c.valid[side][t]) todo.emplace_back(side, t);
    std::atomic<size_t> next{0};
    auto worker = [&] {
        for (size_t k; (k = next.fetch_add(1)) < todo.size();) {
            const auto [side, t] = todo[k];
            const kicad::Box2 rect = c.tileRect(t);
            // 距離場のために、タイルより少し広い範囲（kSdfRangeMm 以上）の図形を集める
            int px0, py0, px1, py1;
            c.tilePixels(t, px0, py0, px1, py1);
            const raster::Grid& G = c.sdfGrid;
            const int margin = static_cast<int>(std::ceil(raster::kSdfRangeMm * G.pxPerMm)) + 2;
            const int ex0 = std::max(0, px0 - margin), ey0 = std::max(0, py0 - margin);
            const int ex1 = std::min(G.width, px1 + margin), ey1 = std::min(G.height, py1 + margin);
            kicad::Box2 wide;
            wide.lo = G.origin + kicad::Vec2{ex0, ey0} / G.pxPerMm;
            wide.hi = G.origin + kicad::Vec2{ex1, ey1} / G.pxPerMm;
            wide.add(rect);
            const auto sp = raster::collectSidePaths(board, static_cast<raster::Side>(side), opts.layerArt, &wide);
            const geom::Paths rectPath{
                geom::toPath({rect.lo, {rect.hi.x, rect.lo.y}, rect.hi, {rect.lo.x, rect.hi.y}})};
            const geom::Paths tileBase = geom::intersect(base[side], rectPath);
            geom::ReliefInput in{geom::intersect(sp.copper, rectPath), geom::intersect(sp.maskOpen, rectPath),
                                 geom::intersect(sp.silk, rectPath), {}, &tileBase};
            for (const auto& d : tentedVias[side]) {
                // 円がこのタイルにかかるものは平らな面から除く。くぼみの面は中心を含むタイルだけが作る
                if (d.pos.x + d.radius < rect.lo.x || d.pos.x - d.radius > rect.hi.x || d.pos.y + d.radius < rect.lo.y ||
                    d.pos.y - d.radius > rect.hi.y)
                    continue;
                const bool centerHere = d.pos.x >= rect.lo.x && d.pos.x < rect.hi.x && d.pos.y >= rect.lo.y &&
                                        d.pos.y < rect.hi.y;
                in.dimples.push_back({d.pos, d.radius, centerHere});
            }
            {
                // 距離場（広い範囲で求め、タイルの画素だけを残す）
                raster::Grid eg{G.origin + kicad::Vec2{ex0, ey0} / G.pxPerMm, G.pxPerMm, ex1 - ex0, ey1 - ey0};
                ReliefTileCache::SdfPatch patch{px0, py0, px1 - px0, py1 - py0, {}, {}};
                std::vector<float> cov;
                std::vector<uint8_t> enc;
                for (int layer = 0; layer < 2; ++layer) {
                    raster::rasterizeCoverage(layer == 0 ? sp.copper : sp.maskOpen, eg, cov);
                    raster::encodeSdf(cov, eg.width, eg.height, G.pxPerMm, enc);
                    auto& dst = layer == 0 ? patch.copper : patch.maskOpen;
                    dst.resize(static_cast<size_t>(patch.w) * patch.h);
                    for (int y = 0; y < patch.h; ++y)
                        std::copy_n(enc.begin() + static_cast<size_t>(y + py0 - ey0) * eg.width + (px0 - ex0),
                                    patch.w, dst.begin() + static_cast<size_t>(y) * patch.w);
                }
                c.sdf[side][t] = std::move(patch);
            }
            geom::Mesh m;
            geom::buildRelief(s.board, side, in, rp, m, &rect);
            for (auto& id : m.triMaterial) id = rmap[std::min<uint32_t>(id, 4)];
            c.tiles[side][t] = std::move(m);
            c.valid[side][t] = 1;
        }
    };
    const unsigned nthreads = std::clamp(std::thread::hardware_concurrency() / 2, 1u, 12u);
    std::vector<std::thread> pool;
    for (unsigned i = 0; i < std::min<size_t>(nthreads, todo.size()); ++i) pool.emplace_back(worker);
    for (auto& th : pool) th.join();
    c.lastBuilt = static_cast<int>(todo.size());

    // 作り直したタイルの距離場だけを、基板全体の画像（RG8）に書き込む。範囲外は「遠く外側」
    {
        const raster::Grid& G = c.sdfGrid;
        const size_t bytes = static_cast<size_t>(G.width) * G.height * 2;
        for (int side = 0; side < 2; ++side)
            if (c.sdfImage[side].size() != bytes) c.sdfImage[side].assign(bytes, 255);
        for (const auto& [side, t] : todo) {
            const auto& p = c.sdf[side][t];
            for (int y = 0; y < p.h; ++y) {
                uint8_t* row = c.sdfImage[side].data() + (static_cast<size_t>(p.y0 + y) * G.width + p.x0) * 2;
                const uint8_t* cu = p.copper.data() + static_cast<size_t>(y) * p.w;
                const uint8_t* mo = p.maskOpen.data() + static_cast<size_t>(y) * p.w;
                for (int x = 0; x < p.w; ++x) {
                    row[x * 2 + 0] = cu[x];
                    row[x * 2 + 1] = mo[x];
                }
            }
        }
        c.lastRebuiltTiles = todo;
        s.layers.grid = G;
        s.layersAreSdf = true;
        s.sdf = &c;
        s.sdfDirty = todo;
    }

    size_t tris = 0;
    for (int side = 0; side < 2; ++side)
        for (int t = 0; t < c.count(); ++t) {
            const geom::Mesh& m = c.tiles[side][t];
            if (m.triangleCount() == 0) continue;
            tris += m.triangleCount();
            const uint32_t id = static_cast<uint32_t>(s.meshes.size());
            s.meshes.push_back({std::format("relief_{}_{}", side == 0 ? "front" : "back", t), m});
            s.instances.push_back({id, glm::mat4(1.0f), 0});
        }
    const double ms = std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - t0).count();
    log::info("relief: rebuilt {} of {} tiles ({}x{}, {:.0f} mm) in {:.0f} ms, {} triangles", todo.size(),
              2 * c.count(), c.nx, c.ny, c.tileSize, ms, tris);
}

}  // namespace

bool ReliefTileCache::matches(const geom::BoardGeometry& g) const {
    return nx > 0 && bounds.lo == g.bounds.lo && bounds.hi == g.bounds.hi && center == g.center &&
           coreHalf == g.coreHalf;
}

void ReliefTileCache::reset(const geom::BoardGeometry& g, double pxPerMm) {
    bounds = {g.bounds.lo, g.bounds.hi};
    center = g.center;
    coreHalf = g.coreHalf;
    nx = std::max(1, static_cast<int>(std::ceil((bounds.hi.x - bounds.lo.x) / tileSize)));
    ny = std::max(1, static_cast<int>(std::ceil((bounds.hi.y - bounds.lo.y) / tileSize)));
    for (int s = 0; s < 2; ++s) {
        tiles[s].assign(count(), {});
        valid[s].assign(count(), 0);
        sdf[s].assign(count(), {});
    }
    // 距離場の格子：タイルの外周の広げ分（1 mm）を含める。画素はタイルの境と揃える
    sdfGrid.pxPerMm = pxPerMm;
    sdfGrid.origin = bounds.lo - kicad::Vec2{1.0, 1.0};
    sdfGrid.width = static_cast<int>(std::ceil((nx * tileSize + 2.0) * pxPerMm));
    sdfGrid.height = static_cast<int>(std::ceil((ny * tileSize + 2.0) * pxPerMm));
}

void ReliefTileCache::tilePixels(int index, int& x0, int& y0, int& x1, int& y1) const {
    const kicad::Box2 r = tileRect(index);
    auto px = [&](double v, double o) { return static_cast<int>(std::lround((v - o) * sdfGrid.pxPerMm)); };
    x0 = std::clamp(px(r.lo.x, sdfGrid.origin.x), 0, sdfGrid.width);
    y0 = std::clamp(px(r.lo.y, sdfGrid.origin.y), 0, sdfGrid.height);
    x1 = std::clamp(px(r.hi.x, sdfGrid.origin.x), 0, sdfGrid.width);
    y1 = std::clamp(px(r.hi.y, sdfGrid.origin.y), 0, sdfGrid.height);
}

void ReliefTileCache::invalidateAll() {
    for (auto& v : valid) std::fill(v.begin(), v.end(), 0);
}

void ReliefTileCache::invalidate(const std::vector<kicad::Box2>& dirty) {
    for (int t = 0; t < count(); ++t) {
        // レジストの広がりなどを見込んで、少し広げた範囲で判定する
        const kicad::Box2 r = tileRect(t).expanded(1.0);
        for (const auto& d : dirty)
            if (r.intersects(d)) {
                valid[0][t] = valid[1][t] = 0;
                break;
            }
    }
}

kicad::Box2 ReliefTileCache::tileRect(int index) const {
    const int i = index % nx, j = index / nx;
    kicad::Box2 r;
    r.lo = bounds.lo + kicad::Vec2{i * tileSize, j * tileSize};
    r.hi = r.lo + kicad::Vec2{tileSize, tileSize};
    // 外周のタイルは基板の外まで少し広げる（端の取りこぼしを防ぐ）
    if (i == 0) r.lo.x -= 1.0;
    if (j == 0) r.lo.y -= 1.0;
    if (i == nx - 1) r.hi.x += 1.0;
    if (j == ny - 1) r.hi.y += 1.0;
    return r;
}

void updateMaskMaterial(Scene& s, const kicad::Board& board, const SceneOptions& o) {
    if (s.boardMats.mask >= s.materials.size() || s.boardMats.silk >= s.materials.size()) return;
    s.materials[s.boardMats.mask] = makeMaskMaterial(resolvedMaskColor(board, o), o.maskRoughness, o.maskScatter);
    s.materials[s.boardMats.silk] = makeSilkMaterial(o.silkColor);  // レジストに合わせたシルクの色
}

Scene buildScene(const SceneContext& ctx) {
    const auto& board = ctx.board;
    const auto& opts = ctx.options;
    Scene s;
    setupBoardMaterials(s, board, opts);

    // 基板
    geom::ReliefParams rp = opts.reliefParams;
    if (opts.reliefCopperFromStackup && board.stackup.copperThickness > 0) rp.copper = board.stackup.copperThickness;
    geom::BoardBuildOptions bo;
    if (opts.relief) {
        bo.coreHalf = geom::reliefCoreHalf(board.stackup.thickness, rp);
        bo.caps = false;  // 上下面は凹凸メッシュが受け持つ
    }
    s.board = geom::buildBoardGeometry(board, bo);
    s.mask.onCopper = static_cast<float>(rp.maskOnCopper);
    s.mask.onSubstrate = static_cast<float>(rp.maskOnSubstrate);
    s.mask.copper = static_cast<float>(rp.copper);
    s.mask.atEdge = std::min(s.mask.atEdge, s.mask.onCopper);
    {
        SceneMesh bm;
        bm.name = "board";
        bm.mesh = s.board.mesh;
        const uint32_t remap[4] = {s.boardMats.top, s.boardMats.bottom, s.boardMats.edge, s.boardMats.plated};
        for (auto& m : bm.mesh.triMaterial) m = remap[std::min<uint32_t>(m, 3)];
        s.meshes.push_back(std::move(bm));
        s.instances.push_back({0, glm::mat4(1.0f)});
        addInstanceBounds(s, 0, glm::mat4(1.0f));
        if (opts.relief) buildReliefTiles(s, board, rp, opts, ctx.reliefCache);
    }
    if (!opts.relief) {
        log::ScopedTimer t("layer textures");
        s.layers = raster::buildLayerTextures(board, s.board, opts.layerArt);
    }

    // 代替表示の箱：縁を面取りした透明な水晶（石英：nd 1.544、アッベ数 70）。色は中を 5 mm 進んだときの透過の色
    Material ph;
    ph.name = "placeholder";
    ph.base_color = glm::vec3(1.0f);
    ph.specular_roughness = 0.0f;
    ph.specular_ior = 1.544f;
    ph.transmission_weight = 1.0f;
    ph.transmission_color = srgbToLinear(opts.placeholderColor);
    ph.transmission_depth = 5.0f;
    ph.transmission_dispersion_scale = 1.0f;
    ph.transmission_dispersion_abbe_number = 70.0f;
    const uint32_t phMat = addMaterial(s, ph);

    const glm::dmat4 toWorld = kicadToWorld(s.board.center);
    std::map<const model::ModelMesh*, uint32_t> meshOf;
    std::map<std::string, uint32_t> materialSetOf;  // 部位ごとの材質の組み合わせ -> 材質表の先頭

    for (size_t fi = 0; fi < board.footprints.size(); ++fi) {
        const Footprint& fp = board.footprints[fi];
        if (fp.dnp && !opts.includeDnp) continue;
        int wanted = 0, placed = 0;
        for (const auto& m : fp.models) {
            if (m.hidden || m.opacity <= 0.0) continue;
            ++wanted;
            MissingModel miss{fp.reference, fp.libId, fp.value, m.path, {}, {}};
            const auto res = ctx.resolver.resolve(m.path);
            miss.resolved = platform::pathToUtf8(res.path);
            if (!res.found) {
                miss.reason = res.reason;
                s.missing.push_back(std::move(miss));
                continue;
            }
            const auto lr = ctx.asyncModels ? ctx.library.request(res.path) : ctx.library.get(res.path);
            if (lr.pending) {
                ++s.pendingModels;  // 分割が終わるまで箱で仮表示（不足とは扱わない）
                continue;
            }
            if (!lr.mesh) {
                miss.reason = lr.error;
                s.missing.push_back(std::move(miss));
                continue;
            }
            uint32_t meshId;
            if (auto it = meshOf.find(lr.mesh.get()); it != meshOf.end()) {
                meshId = it->second;
            } else {
                SceneMesh sm;
                sm.name = platform::pathToUtf8(res.path.filename());
                sm.mesh = lr.mesh->mesh;  // triMaterial は部位の番号（インスタンスごとの材質表で引く）
                const uint32_t nparts = static_cast<uint32_t>(std::max<size_t>(1, lr.mesh->parts.size()));
                for (auto& t : sm.mesh.triMaterial) t = std::min(t, nparts - 1);
                meshId = static_cast<uint32_t>(s.meshes.size());
                s.meshes.push_back(std::move(sm));
                meshOf[lr.mesh.get()] = meshId;
            }
            // 部位ごとに材質を決める（部品番号・モデル・規則の順）
            const std::string modelFile = platform::pathToUtf8(res.path.filename());
            std::vector<ResolvedMaterial> resolved;
            std::string setKey;
            const auto& parts = lr.mesh->parts;
            const size_t nparts = std::max<size_t>(1, parts.size());
            for (size_t pi = 0; pi < nparts; ++pi) {
                const model::ModelPart* part = pi < parts.size() ? &parts[pi] : nullptr;
                const MaterialQuery q{fp.reference, m.path, lr.fileHash, modelFile, part};
                ResolvedMaterial r = ctx.materials.resolve(q);
                MaterialAssignment a;
                a.reference = fp.reference;
                a.footprint = fp.libId;
                a.modelSpec = m.path;
                a.modelFile = modelFile;
                a.modelHash = lr.fileHash;
                if (part) {
                    for (const auto& k : part->keys()) a.partKeys += (a.partKeys.empty() ? "" : " ") + k;
                }
                a.material = r.name;
                a.source = r.source;
                s.assignments.push_back(std::move(a));
                const glm::vec3 c = r.material.base_color;
                setKey += std::format("{}|{:.4f},{:.4f},{:.4f};", r.name, c.r, c.g, c.b);
                resolved.push_back(std::move(r));
            }
            uint32_t materialBase;
            if (auto it = materialSetOf.find(setKey); it != materialSetOf.end()) {
                materialBase = it->second;
            } else {
                materialBase = static_cast<uint32_t>(s.materials.size());
                for (auto& r : resolved) {
                    r.material.name = modelFile + ":" + r.name;
                    s.materials.push_back(r.material);
                }
                materialSetOf[setKey] = materialBase;
            }
            const glm::mat4 xf = glm::mat4(toWorld * kicadModelMatrix(fp, m, s.board.thickness));
            s.instances.push_back({meshId, xf, materialBase});
            addInstanceBounds(s, meshId, xf);
            ++placed;
            ++s.modelInstances;
        }
        if (wanted > 0 && placed == 0) {
            Vec2 lo, hi;
            if (!footprintLocalBox(board, static_cast<int>(fi), lo, hi)) {
                lo = {-0.5, -0.5};
                hi = {0.5, 0.5};
            }
            const double height = placeholderHeightFor(opts.placeholderHeight, lo, hi);
            log::debug("placeholder {}: {:.1f} x {:.1f} mm, height {:.2f} mm", fp.reference, hi.x - lo.x, hi.y - lo.y, height);
            const glm::vec3 half(static_cast<float>(hi.x - lo.x) / 2, static_cast<float>(height) / 2,
                                 static_cast<float>(hi.y - lo.y) / 2);
            // 面取りの幅：0.2 mm（縁が細く光って見える程度。小さい箱では beveledBox が辺の長さに合わせて縮める）
            const float bevel = 0.2f;
            const uint32_t boxMesh = static_cast<uint32_t>(s.meshes.size());
            s.meshes.push_back({"placeholder_" + fp.reference, beveledBox(half, bevel, phMat)});
            const glm::mat4 xf =
                placeholderMatrix(fp, lo, hi, height, s.board.thickness, s.board.center);
            s.instances.push_back({boxMesh, xf});
            addInstanceBounds(s, boxMesh, xf);
            ++s.placeholders;
        }
    }
    // まだ描画に反映していない OpenPBR の要素を知らせる（値は GPU まで渡している）
    {
        std::map<std::string, std::vector<std::string>> unsupported;
        for (const auto& m : s.materials) {
            if (m.transmission_weight > 0 && m.geometry_thin_walled) unsupported["thin_walled"].push_back(m.name);
            if (m.transmission_weight > 0 && glm::length(m.transmission_scatter) > 0)
                unsupported["transmission_scatter"].push_back(m.name);
            if (m.fuzz_weight > 0) unsupported["fuzz"].push_back(m.name);
            if (m.thin_film_weight > 0) unsupported["thin_film"].push_back(m.name);
            if (m.geometry_opacity < 1) unsupported["geometry_opacity"].push_back(m.name);
            if (m.specular_roughness_anisotropy > 0 || m.coat_roughness_anisotropy > 0)
                unsupported["anisotropy"].push_back(m.name);
        }
        for (const auto& [what, names] : unsupported)
            log::warn("not rendered yet: {} used by {} material(s), e.g. {}", what, names.size(), names.front());
    }
    log::info("scene: {} meshes, {} instances ({} models, {} placeholders), {} materials", s.meshes.size(),
              s.instances.size(), s.modelInstances, s.placeholders, s.materials.size());
    return s;
}

}  // namespace krbr::scene
