// 材質の形式・材質ライブラリ・対応表と解決順序の単体テスト
#include <doctest/doctest.h>

#include "scene/material_io.hpp"
#include "scene/material_library.hpp"
#include "scene/model_map.hpp"
#include "test_util.hpp"

using namespace krbr;
using namespace krbr::scene;

namespace {

model::ModelPart kicadPins() {
    model::ModelPart p;
    p.srgb = {0.824f, 0.820f, 0.781f};
    p.hasColor = true;
    return p;
}

model::ModelPart namedPart(const std::string& name, glm::vec3 srgb) {
    model::ModelPart p;
    p.name = name;
    p.srgb = srgb;
    p.hasColor = true;
    return p;
}

}  // namespace

TEST_CASE("model part keys: name, kicad color name, hex") {
    const auto pins = kicadPins();
    CHECK(pins.kicadColorName() == "metal_grey_pins");
    const auto k = pins.keys();
    REQUIRE(k.size() == 2);
    CHECK(k[0] == "kicad:metal_grey_pins");
    CHECK(k[1] == "#d2d1c7");
    const auto named = namedPart("EP", {0.1f, 0.2f, 0.3f});
    CHECK(named.keys().front() == "EP");
}

TEST_CASE("OpenPBR material TOML round trip") {
    Material m;
    m.base_color = {0.1f, 0.2f, 0.3f};
    m.specular_roughness = 0.42f;
    m.transmission_weight = 1.0f;
    m.transmission_dispersion_scale = 1.0f;
    m.transmission_dispersion_abbe_number = 57.0f;
    m.geometry_thin_walled = true;
    m.bump_pattern = BumpPattern::Noise;
    m.bump_strength = 0.02f;
    m.sellmeier = Sellmeier{{1.0f, 0.2f, 0.9f}, {0.006f, 0.02f, 100.0f}};
    const toml::table t = writeMaterial(m);
    Material r;
    std::vector<std::string> unknown;
    readMaterial(t, r, &unknown);
    CHECK(unknown.empty());
    CHECK(r.base_color.g == doctest::Approx(0.2f));
    CHECK(r.specular_roughness == doctest::Approx(0.42f));
    CHECK(r.transmission_dispersion_abbe_number == doctest::Approx(57.0f));
    CHECK(r.geometry_thin_walled);
    CHECK(r.bump_pattern == BumpPattern::Noise);
    REQUIRE(r.sellmeier.has_value());
    CHECK(r.sellmeier->C[2] == doctest::Approx(100.0f));
}

TEST_CASE("dispersion from Abbe number reproduces nd and V") {
    Material m;
    m.specular_ior = 1.491f;
    m.transmission_dispersion_scale = 1.0f;
    m.transmission_dispersion_abbe_number = 57.0f;
    const float nd = m.iorAt(587.6f), nF = m.iorAt(486.1f), nC = m.iorAt(656.3f);
    CHECK(nd == doctest::Approx(1.491f).epsilon(1e-4));
    CHECK((nd - 1.0f) / (nF - nC) == doctest::Approx(57.0f).epsilon(1e-3));
    m.transmission_dispersion_scale = 0.0f;  // 分散なし
    CHECK(m.iorAt(400.0f) == doctest::Approx(1.491f));
}

TEST_CASE("material library: user file overrides only the written keys") {
    MaterialLibrary lib = MaterialLibrary::builtin();
    const Material* tin = lib.find("metal_tin");
    REQUIRE(tin);
    CHECK(tin->base_color_from_model);
    const float bump = tin->bump_strength;
    std::string err;
    REQUIRE(lib.loadText("[material.metal_tin]\nspecular_roughness = 0.5\n[material.mine]\nbase_color = [1,0,0]\n",
                         "test", err));
    tin = lib.find("metal_tin");
    CHECK(tin->specular_roughness == doctest::Approx(0.5f));
    CHECK(tin->bump_strength == doctest::Approx(bump));
    CHECK(tin->base_color_from_model);
    const Material* mine = lib.find("mine");
    REQUIRE(mine);
    CHECK_FALSE(mine->base_color_from_model);
}

TEST_CASE("resolution order: ref > project model > global model > rules > default") {
    MaterialLibrary lib = MaterialLibrary::builtin();
    std::string err;
    REQUIRE(lib.loadText("[material.a]\n[material.b]\n[material.c]\n[material.d]\n", "test", err));
    ProjectOverrides project;
    GlobalModelMap global;
    const MaterialResolver res(lib, project, global);
    const auto pins = kicadPins();
    MaterialQuery q{"U3", "${KICAD10_3DMODEL_DIR}/X.step", "abcd", "X.step", &pins};

    CHECK(res.resolve(q).name == "metal_tin");  // 組み込みの規則（kicad:metal_grey_pins）
    CHECK(res.resolve(q).source == "rule 4 (builtin)");  // 先頭の 3 つは電解コンデンサのモデル用

    global.rules.push_back({"kicad:*", "", {}, {}, {}, {}, {}, {}, "d"});
    CHECK(res.resolve(q).name == "d");
    global.upsert("${KICAD10_3DMODEL_DIR}/X.step", "").assign.material = "c";
    CHECK(res.resolve(q).name == "c");
    CHECK(res.resolve(q).source == "model (global)");
    project.upsert("", "abcd").assign.material = "b";  // ハッシュだけで照合
    CHECK(res.resolve(q).name == "b");
    CHECK(res.resolve(q).source == "model (project)");
    project.refs["U3"].material = "a";
    CHECK(res.resolve(q).name == "a");
    CHECK(res.resolve(q).source == "ref U3 (project)");

    // 部位ごとの指定は、その部位にだけ効く
    project.refs["U3"] = {};
    project.refs["U3"].parts["#ffffff"] = "a";
    CHECK(res.resolve(q).name == "b");
    project.refs["U3"].parts["kicad:metal_grey_pins"] = "a";
    CHECK(res.resolve(q).name == "a");

    // 材質ライブラリにない名前は飛ばして次の段階へ
    project.refs["U3"].parts["kicad:metal_grey_pins"] = "does_not_exist";
    CHECK(res.resolve(q).name == "b");
}

TEST_CASE("model entries match by unexpanded path or by content hash") {
    ModelEntry e{"${KIPRJMOD}/a.step", "1234", {}};
    CHECK(e.matches("${KIPRJMOD}/a.step", ""));
    CHECK(e.matches("C:/elsewhere/renamed.step", "1234"));
    CHECK_FALSE(e.matches("C:/elsewhere/renamed.step", "9999"));
}

TEST_CASE("overrides and model map files round trip") {
    test::TempWorkspace ws("materials_roundtrip");
    ProjectOverrides p;
    p.refs["U3"].material = "epoxy_black";
    p.refs["U3"].parts["kicad:metal_grey_pins"] = "metal_gold";
    p.upsert("${KIPRJMOD}/x.step", "beef").assign.material = "plastic";
    std::string err;
    REQUIRE(p.save(ws.dir / "overrides.toml", err));
    ProjectOverrides q;
    REQUIRE(q.load(ws.dir / "overrides.toml", err));
    CHECK(q.refs["U3"].material == "epoxy_black");
    CHECK(q.refs["U3"].parts["kicad:metal_grey_pins"] == "metal_gold");
    REQUIRE(q.models.size() == 1);
    CHECK(q.models[0].hash == "beef");

    GlobalModelMap g;
    g.upsert("a.step", "").assign.parts["*"] = "ceramic";
    g.rules.push_back({"", "*LED*", {}, {}, {}, {}, 0.5f, {}, "glossy_plastic"});
    REQUIRE(g.save(ws.dir / "model_map.toml", err));
    GlobalModelMap h;
    REQUIRE(h.load(ws.dir / "model_map.toml", err));
    REQUIRE(h.rules.size() == 1);
    CHECK(h.rules[0].model == "*LED*");
    CHECK(*h.rules[0].valMin == doctest::Approx(0.5f));
    CHECK(h.models[0].assign.parts["*"] == "ceramic");
}
