// フェーズ2の完了条件：
//  - 同じ IC（3D モデル）の材質を全体設定で変えると、別のプロジェクトでも反映される
//  - 部品番号ごとの上書きが、それより優先される
// 全体設定は一時フォルダに向ける（本物の %APPDATA%\krbr には触れない）。対象の基板はコピーして使う
#include <doctest/doctest.h>

#include "app/material_cmd.hpp"
#include "core/platform.hpp"
#include "test_util.hpp"

using namespace krbr;

namespace {

const std::string kModel = "UQFN-28-1EP_4x4mm_P0.4mm_EP2.35x2.35mm.step";  // U2

std::map<std::string, std::string> partsOf(const std::vector<scene::MaterialAssignment>& rows, const std::string& ref,
                                           std::string* source = nullptr) {
    std::map<std::string, std::string> out;
    for (const auto& r : rows)
        if (r.reference == ref) {
            out[r.partKeys] = r.material;
            if (source) *source = r.source;
        }
    return out;
}

}  // namespace

TEST_CASE("global model material is shared across projects; reference override wins") {
    test::TempWorkspace ws("projects");
    namespace fs = std::filesystem;
    for (const char* p : {"projA", "projB"}) {
        fs::create_directories(ws.dir / p);
        fs::copy_file(test::samplePcb(), ws.dir / p / "board.kicad_pcb");
    }
    const std::string pcbA = platform::pathToUtf8(ws.dir / "projA" / "board.kicad_pcb");
    const std::string pcbB = platform::pathToUtf8(ws.dir / "projB" / "board.kicad_pcb");
    fs::create_directories(ws.configDir() / "materials");
    REQUIRE(platform::writeTextFile(ws.configDir() / "materials" / "test.toml",
                                    "[material.test_red]\nbase_color = [0.8, 0.02, 0.02]\n"
                                    "[material.test_blue]\nbase_color = [0.02, 0.02, 0.8]\n"));

    std::string err;
    // 変更前：組み込みの規則で決まっている
    auto before = app::computeAssignments(pcbB, err);
    REQUIRE(err.empty());
    REQUIRE_FALSE(partsOf(before, "U2").empty());

    // 全体設定で U2 のモデルを test_red に（プロジェクト A から設定）
    app::MaterialSetOptions o;
    o.pcb = pcbA;
    o.model = kModel;
    o.material = "test_red";
    o.global = true;
    REQUIRE_MESSAGE(app::setMaterial(o, err), err);
    CHECK(fs::exists(ws.configDir() / "model_map.toml"));

    for (const auto& pcb : {pcbA, pcbB}) {
        std::string source;
        const auto parts = partsOf(app::computeAssignments(pcb, err), "U2", &source);
        REQUIRE_FALSE(parts.empty());
        for (const auto& [k, m] : parts) CHECK_MESSAGE(m == "test_red", pcb << " part " << k);
        CHECK(source == "model (global)");
    }

    // プロジェクト A だけ、U2 を部品番号で上書き
    app::MaterialSetOptions r;
    r.pcb = pcbA;
    r.ref = "U2";
    r.material = "test_blue";
    REQUIRE_MESSAGE(app::setMaterial(r, err), err);
    CHECK(fs::exists(ws.dir / "projA" / "board.krbr" / "overrides.toml"));
    {
        std::string source;
        const auto a = partsOf(app::computeAssignments(pcbA, err), "U2", &source);
        for (const auto& [k, m] : a) CHECK(m == "test_blue");
        CHECK(source == "ref U2 (project)");
        const auto b = partsOf(app::computeAssignments(pcbB, err), "U2");
        for (const auto& [k, m] : b) CHECK(m == "test_red");
    }

    // プロジェクト B では端子だけを部品番号で上書き（他の部位は全体設定のまま）
    app::MaterialSetOptions pr;
    pr.pcb = pcbB;
    pr.ref = "U2";
    pr.part = "kicad:metal_grey_pins";
    pr.material = "metal_gold";
    REQUIRE_MESSAGE(app::setMaterial(pr, err), err);
    const auto b = partsOf(app::computeAssignments(pcbB, err), "U2");
    int gold = 0, red = 0;
    for (const auto& [k, m] : b) {
        if (k.find("kicad:metal_grey_pins") != std::string::npos) {
            CHECK(m == "metal_gold");
            ++gold;
        } else {
            CHECK(m == "test_red");
            ++red;
        }
    }
    CHECK(gold == 1);
    CHECK(red >= 1);

    // 存在しない材質名はエラー
    app::MaterialSetOptions bad = r;
    bad.material = "no_such_material";
    CHECK_FALSE(app::setMaterial(bad, err));
}
