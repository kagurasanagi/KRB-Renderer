// フェーズ2の完了条件：
//  - 同じ IC（3D モデル）の材質を全体設定で変えると、別のプロジェクトでも反映される
//  - 部品番号ごとの上書きが、それより優先される
// 全体設定は一時フォルダに向ける（本物の %APPDATA%\krbr には触れない）。対象の基板はコピーして使う
#include <doctest/doctest.h>

#include <optional>

#include "app/material_cmd.hpp"
#include "core/platform.hpp"
#include "test_util.hpp"

using namespace krbr;

namespace {

// 試す部品：KiCad 標準の色（kicad:metal_grey_pins）を持つ IC（U…）のうち、部位が 2 つ以上あるもの。
// 基板（KRBR_TEST_PCB）によって部品番号が違うので、解析結果から選ぶ
struct Subject {
    std::string ref, modelSpec;
};
std::optional<Subject> pickSubject(const std::vector<scene::MaterialAssignment>& rows) {
    std::map<std::string, std::vector<const scene::MaterialAssignment*>> byRef;
    for (const auto& r : rows)
        if (!r.reference.empty() && r.reference[0] == 'U') byRef[r.reference].push_back(&r);
    for (const auto& [ref, list] : byRef) {
        if (list.size() < 2) continue;
        int pins = 0;
        for (const auto* r : list)
            if (r->partKeys.find("kicad:metal_grey_pins") != std::string::npos) ++pins;
        if (pins == 1) return Subject{ref, list.front()->modelSpec};
    }
    return std::nullopt;
}

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
    const auto subject = pickSubject(before);
    REQUIRE_MESSAGE(subject.has_value(), "no IC with kicad:metal_grey_pins and another part on " << pcbB);
    const std::string ref = subject->ref;
    MESSAGE("subject: " << ref << " (" << subject->modelSpec << ")");
    REQUIRE_FALSE(partsOf(before, ref).empty());

    // 全体設定で対象のモデルを test_red に（プロジェクト A から設定）
    app::MaterialSetOptions o;
    o.pcb = pcbA;
    o.model = subject->modelSpec;
    o.material = "test_red";
    o.global = true;
    REQUIRE_MESSAGE(app::setMaterial(o, err), err);
    CHECK(fs::exists(ws.configDir() / "model_map.toml"));

    for (const auto& pcb : {pcbA, pcbB}) {
        std::string source;
        const auto parts = partsOf(app::computeAssignments(pcb, err), ref, &source);
        REQUIRE_FALSE(parts.empty());
        for (const auto& [k, m] : parts) CHECK_MESSAGE(m == "test_red", pcb << " part " << k);
        CHECK(source == "model (global)");
    }

    // プロジェクト A だけ、部品番号で上書き
    app::MaterialSetOptions r;
    r.pcb = pcbA;
    r.ref = ref;
    r.material = "test_blue";
    REQUIRE_MESSAGE(app::setMaterial(r, err), err);
    CHECK(fs::exists(ws.dir / "projA" / "board.krbr" / "overrides.toml"));
    {
        std::string source;
        const auto a = partsOf(app::computeAssignments(pcbA, err), ref, &source);
        for (const auto& [k, m] : a) CHECK(m == "test_blue");
        CHECK(source == "ref " + ref + " (project)");
        const auto b = partsOf(app::computeAssignments(pcbB, err), ref);
        for (const auto& [k, m] : b) CHECK(m == "test_red");
    }

    // プロジェクト B では端子だけを部品番号で上書き（他の部位は全体設定のまま）
    app::MaterialSetOptions pr;
    pr.pcb = pcbB;
    pr.ref = ref;
    pr.part = "kicad:metal_grey_pins";
    pr.material = "metal_gold";
    REQUIRE_MESSAGE(app::setMaterial(pr, err), err);
    const auto b = partsOf(app::computeAssignments(pcbB, err), ref);
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
