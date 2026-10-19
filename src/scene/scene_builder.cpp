#include <algorithm>
#include <cmath>
#include <glm/gtc/matrix_transform.hpp>
#include <map>
#include <numbers>

#include "core/log.hpp"
#include "core/platform.hpp"
#include "geom/relief.hpp"
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
    // KiCad の stackup の色名（"Green" など）または "#RRGGBBAA"
    static const std::map<std::string, glm::vec3> named = {
        {"Green", {0.05f, 0.42f, 0.16f}},  {"Red", {0.62f, 0.07f, 0.06f}},    {"Blue", {0.05f, 0.18f, 0.52f}},
        {"Yellow", {0.80f, 0.68f, 0.10f}}, {"Purple", {0.30f, 0.10f, 0.42f}}, {"Black", {0.04f, 0.04f, 0.04f}},
        {"White", {0.90f, 0.90f, 0.88f}},  {"Matte Black", {0.05f, 0.05f, 0.05f}},
    };
    if (auto it = named.find(name); it != named.end()) return it->second;
    if (name.size() >= 7 && name[0] == '#') {
        auto hex = [&](size_t i) { return std::stoi(name.substr(i, 2), nullptr, 16) / 255.0f; };
        try {
            return {hex(1), hex(3), hex(5)};
        } catch (...) {
        }
    }
    return fallback;
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

    const glm::vec3 maskSrgb =
        o.maskColorSet ? o.maskColor : stackupColor(board.stackup.maskColorFront, o.maskColor);
    Material mask;
    mask.name = "solder_mask";
    // コートとして使う：coat_color がレジストを通る光の色、base_* は顔料自身の散乱
    mask.coat_weight = 1.0f;
    // 設定の色は「銅の上で見えるレジストの色」とし、銅の平均反射率（約 0.7）で割って透過色にする
    mask.coat_color = glm::min(srgbToLinear(maskSrgb) / 0.7f, glm::vec3(1.0f));
    mask.coat_roughness = o.maskRoughness;
    mask.coat_ior = 1.5f;
    mask.base_color = srgbToLinear(maskSrgb) * 0.6f;
    mask.base_weight = o.maskScatter;  // シェーダではこの値を「散乱を選ぶ確率」として使う
    mask.specular_roughness = 0.5f;
    mask.bump_pattern = BumpPattern::Noise;  // 塗膜のゆず肌
    mask.bump_strength = 0.012f;
    mask.bump_scale = 0.45f;
    mask.roughness_variation = 0.25f;

    Material silk;
    silk.name = "silkscreen";
    silk.base_color = srgbToLinear(o.silkColor);
    silk.specular_roughness = 0.65f;
    silk.bump_pattern = BumpPattern::Noise;  // インクのざらつき
    silk.bump_strength = 0.2f;
    silk.bump_scale = 0.06f;
    silk.roughness_variation = 0.3f;

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
    log::info("board materials: finish {}, mask sRGB ({:.2f}, {:.2f}, {:.2f})", finish, maskSrgb.r, maskSrgb.g,
              maskSrgb.b);
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

geom::Mesh unitCube(uint32_t mat) {
    geom::Mesh m;
    const glm::vec3 n[6] = {{1, 0, 0}, {-1, 0, 0}, {0, 1, 0}, {0, -1, 0}, {0, 0, 1}, {0, 0, -1}};
    for (const auto& nn : n) {
        // 面の中心 = 0.5 + 0.5n、残り2軸で正方形
        glm::vec3 a = glm::abs(nn).x > 0 ? glm::vec3(0, 1, 0) : glm::vec3(1, 0, 0);
        glm::vec3 b = glm::cross(nn, a);
        const glm::vec3 c = glm::vec3(0.5f) + 0.5f * nn;
        const uint32_t base = static_cast<uint32_t>(m.positions.size());
        m.addVertex(c - 0.5f * a - 0.5f * b, nn);
        m.addVertex(c + 0.5f * a - 0.5f * b, nn);
        m.addVertex(c + 0.5f * a + 0.5f * b, nn);
        m.addVertex(c - 0.5f * a + 0.5f * b, nn);
        m.addTriangle(base, base + 1, base + 2, mat);
        m.addTriangle(base, base + 2, base + 3, mat);
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

glm::mat4 placeholderMatrix(const Footprint& fp, Vec2 lo, Vec2 hi, double height, double thickness, Vec2 center) {
    // 単位立方体 [0,1]^3 -> 部品座標系の箱 -> ワールド
    const Vec2 ex = kicad::rotatePoint({hi.x - lo.x, 0}, fp.angleDeg);
    const Vec2 ez = kicad::rotatePoint({0, hi.y - lo.y}, fp.angleDeg);
    const Vec2 o = fp.pos + kicad::rotatePoint(lo, fp.angleDeg);
    const double y0 = fp.back ? -thickness / 2 : thickness / 2;
    const double h = fp.back ? -height : height;
    glm::mat4 M(1.0f);
    M[0] = {static_cast<float>(ex.x), 0, static_cast<float>(ex.y), 0};
    M[1] = {0, static_cast<float>(h), 0, 0};
    M[2] = {static_cast<float>(ez.x), 0, static_cast<float>(ez.y), 0};
    M[3] = {static_cast<float>(o.x - center.x), static_cast<float>(y0), static_cast<float>(o.y - center.y), 1};
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

}  // namespace

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
    {
        SceneMesh bm;
        bm.name = "board";
        bm.mesh = s.board.mesh;
        const uint32_t remap[4] = {s.boardMats.top, s.boardMats.bottom, s.boardMats.edge, s.boardMats.plated};
        for (auto& m : bm.mesh.triMaterial) m = remap[std::min<uint32_t>(m, 3)];
        if (opts.relief) {
            log::ScopedTimer t("relief");
            const uint32_t rmap[5] = {s.boardMats.fr4, s.boardMats.finish, s.boardMats.maskOnFr4,
                                      s.boardMats.maskOnCopper, s.boardMats.silk};
            for (int side = 0; side < 2; ++side) {
                const auto sp = raster::collectSidePaths(board, static_cast<raster::Side>(side), opts.layerArt);
                geom::ReliefInput in{sp.copper, sp.maskOpen, sp.silk, {}};
                // レジストでふさがれたビアは穴の上にも面を張る
                const bool tentDefault = side == 0 ? board.setup.tentFront : board.setup.tentBack;
                for (const auto& v : board.vias) {
                    const std::optional<bool> tent = side == 0 ? v.tentFront : v.tentBack;
                    if (tent.value_or(tentDefault) && v.drill > 0) geom::append(in.tentedHoles, geom::disc(v.pos, v.drill / 2));
                }
                geom::Mesh rm;
                geom::buildRelief(s.board, side, in, rp, rm);
                const uint32_t base = static_cast<uint32_t>(bm.mesh.positions.size());
                bm.mesh.positions.insert(bm.mesh.positions.end(), rm.positions.begin(), rm.positions.end());
                bm.mesh.normals.insert(bm.mesh.normals.end(), rm.normals.begin(), rm.normals.end());
                for (uint32_t i : rm.indices) bm.mesh.indices.push_back(base + i);
                for (uint32_t m : rm.triMaterial) bm.mesh.triMaterial.push_back(rmap[std::min<uint32_t>(m, 4)]);
            }
        }
        s.meshes.push_back(std::move(bm));
        s.instances.push_back({0, glm::mat4(1.0f)});
        addInstanceBounds(s, 0, glm::mat4(1.0f));
    }
    {
        log::ScopedTimer t("layer textures");
        s.layers = raster::buildLayerTextures(board, s.board, opts.layerArt);
    }

    // 代替表示の箱
    Material ph;
    ph.name = "placeholder";
    ph.base_color = srgbToLinear(opts.placeholderColor);
    ph.specular_roughness = 0.6f;
    ph.bump_pattern = BumpPattern::Noise;
    ph.bump_strength = 0.02f;
    ph.bump_scale = 0.5f;
    const uint32_t phMat = addMaterial(s, ph);
    uint32_t cubeMesh = UINT32_MAX;

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
            const auto lr = ctx.library.get(res.path);
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
            if (cubeMesh == UINT32_MAX) {
                cubeMesh = static_cast<uint32_t>(s.meshes.size());
                s.meshes.push_back({"placeholder_box", unitCube(phMat)});
            }
            const glm::mat4 xf =
                placeholderMatrix(fp, lo, hi, opts.placeholderHeight, s.board.thickness, s.board.center);
            s.instances.push_back({cubeMesh, xf});
            addInstanceBounds(s, cubeMesh, xf);
            ++s.placeholders;
        }
    }
    // まだ描画に反映していない OpenPBR の要素を知らせる（値は GPU まで渡している）
    {
        std::map<std::string, std::vector<std::string>> unsupported;
        for (const auto& m : s.materials) {
            if (m.transmission_weight > 0) unsupported["transmission (phase 5)"].push_back(m.name);
            if (m.subsurface_weight > 0) unsupported["subsurface"].push_back(m.name);
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
