// フェーズ3：保存の監視と差分更新。基板のコピーを書き換え、保存から画像の更新までの時間を測る（ウィンドウは開かない）
#include <doctest/doctest.h>

#include <cstdio>
#include <thread>

#include "app/watch.hpp"
#include "render/vk/context.hpp"
#include "io/image_io.hpp"
#include "render/denoiser.hpp"
#include "core/platform.hpp"
#include "test_util.hpp"

using namespace krbr;
namespace fs = std::filesystem;

namespace {

// 保存をまねる（KiCad と同じく、ファイル全体を書き直す）
void saveWithReplace(const fs::path& pcb, const std::string& from, const std::string& to, size_t startPos = 0) {
    auto text = platform::readTextFile(pcb);
    REQUIRE(text);
    const size_t pos = text->find(from, startPos);
    REQUIRE(pos != std::string::npos);
    text->replace(pos, from.size(), to);
    REQUIRE(platform::writeTextFile(pcb, *text));
}

// 動かす部品：KiCad 標準の 0603 コンデンサ（C_0603_1608Metric）を使う最初の部品。
// 基板（KRBR_TEST_PCB）によって部品番号や座標が違うので、S 式の文字列から探す
struct Target {
    std::string ref, atFrom, atTo;  // "(at x y ...)" の元と、x を 1.5 mm ずらしたもの
    size_t offset = 0;              // 部品の S 式の始まり（同じ座標が他にあっても、この部品を書き換える）
};
Target findTarget(const fs::path& pcb) {
    auto text = platform::readTextFile(pcb);
    REQUIRE(text);
    const size_t fp = text->find("(footprint \"Capacitor_SMD:C_0603_1608Metric\"");
    REQUIRE_MESSAGE(fp != std::string::npos, "no Capacitor_SMD:C_0603_1608Metric footprint on the test board");
    const size_t at = text->find("(at ", fp);
    const size_t end = text->find(')', at);
    REQUIRE(at != std::string::npos);
    const std::string atFrom = text->substr(at, end - at + 1);  // 例 "(at 118.4 112.5 90)"
    double x = 0, y = 0;
    REQUIRE(sscanf(atFrom.c_str(), "(at %lf %lf", &x, &y) == 2);
    char buf[64];
    snprintf(buf, sizeof buf, "(at %g %g", x + 1.5, y);
    size_t rest = atFrom.find(' ', atFrom.find(' ', 4) + 1);  // 回転があればそのまま残す
    if (rest == std::string::npos) rest = atFrom.size() - 1;
    const std::string atTo = std::string(buf) + atFrom.substr(rest);
    std::string ref = "?";
    const size_t rp = text->find("(property \"Reference\" \"", fp);
    if (rp != std::string::npos) {
        const size_t s = rp + std::string("(property \"Reference\" \"").size();
        ref = text->substr(s, text->find('"', s) - s);
    }
    return {ref, atFrom, atTo, fp};
}

// 基板に出てこない（＝キャッシュに無い）KiCad 標準のコンデンサのモデル
std::string unusedCapacitorModel(const fs::path& pcb) {
    auto text = platform::readTextFile(pcb);
    REQUIRE(text);
    for (const char* name : {"C_1206_3216Metric", "C_1210_3225Metric", "C_2220_5750Metric", "C_0805_2012Metric"})
        if (text->find(name) == std::string::npos) return name;
    FAIL("every candidate capacitor model is already used on the test board");
    return {};
}

// 保存から画像の更新までの目標時間。1 秒は ray query を持つ GPU（RTX 3080 Ti）での値。
// ray_query が無く ray tracing pipeline のエミュレーションで描く GPU（GTX 1060 など）では、
// 描画とノイズ除去（CPU）で 10 秒を超えるので、目標を緩める（仕組みの検証が目的で、速さの検証ではない）
double saveToImageBudgetMs() {
    for (const auto& d : vk::Context::enumerate())
        if (d.rayQuery && d.accelerationStructure) return 1000.0;
    return 30000.0;
}

}  // namespace

TEST_CASE("watch: save -> updated image within 1 s, only nearby relief tiles rebuilt") {
    const double budgetMs = saveToImageBudgetMs();
    test::TempWorkspace ws("watch");
    // 3D モデルのキャッシュも空のものにする（裏での分割を確かめるため）
    test::setEnv("KRBR_CACHE_DIR", platform::pathToUtf8(ws.dir / "cache"));
    struct ResetCache {
        ~ResetCache() { test::setEnv("KRBR_CACHE_DIR", ""); }
    } resetCache;
    fs::create_directories(ws.dir / "proj");
    const fs::path pcb = ws.dir / "proj" / "board.kicad_pcb";
    fs::copy_file(test::samplePcb(), pcb);

    app::WatchOptions o;
    o.common.pcb = platform::pathToUtf8(pcb);
    o.common.hdr = platform::pathToUtf8(test::sampleHdr());
    o.common.out = platform::pathToUtf8(ws.dir / "out" / "watch.png");
    o.common.spp = 32;  // 既定の 1920x1080
    app::WatchRunner runner;
    REQUIRE(runner.open(o));
    CHECK(fs::exists(runner.output()));

    // 保存の時刻の分解能を超えるよう、少し待ってから書き換える
    std::this_thread::sleep_for(std::chrono::milliseconds(50));

    const Target target = findTarget(pcb);
    SUBCASE("move a component") {
        saveWithReplace(pcb, target.atFrom, target.atTo, target.offset);
        auto u = runner.waitAndUpdate(std::chrono::seconds(10));
        REQUIRE(u.has_value());
        MESSAGE("move " << target.ref << " " << target.atFrom << " -> " << target.atTo << ": " << u->sinceSaveMs << " ms after save; reload " << u->reloadMs << " ms ("
                             << u->stats.tilesRebuilt << " tiles), gpu " << u->gpuMs << " ms, render " << u->renderMs
                             << " ms (denoise " << u->denoiseMs << " ms), write " << u->writeMs << " ms");
        for (const auto& b : u->stats.diff.dirty)
            MESSAGE("dirty: (" << b.lo.x << ", " << b.lo.y << ") - (" << b.hi.x << ", " << b.hi.y << ")");
        CHECK(u->stats.diff.changed == 1);
        CHECK(u->stats.diff.added == 0);
        CHECK_FALSE(u->stats.diff.full);
        CHECK(u->stats.tilesRebuilt > 0);
        CHECK(u->stats.tilesRebuilt <= 16);  // 全体（200 枚）ではなく周辺だけ
        CHECK(u->sinceSaveMs < budgetMs);
        if (render::Denoiser::compiledIn()) CHECK(u->denoiseMs > 0.0);  // ノイズ除去込みで 1 秒以内
        // 速さ優先で書いた（圧縮しない）PNG が読めること
        io::ImageF img;
        std::string err;
        REQUIRE_MESSAGE(io::loadHdrImage(runner.output(), img, err), err);
        CHECK(img.width == 1920);
        CHECK(img.height == 1080);
    }
    SUBCASE("a new 3D model is shown as a box first, then meshed in the background") {
        // 0603 コンデンサのモデルを、まだ分割していない（テスト用の空のキャッシュにない）ものに変える
        const std::string to = unusedCapacitorModel(pcb);
        MESSAGE("replace model of " << target.ref << " with " << to);
        saveWithReplace(pcb, "Capacitor_SMD.3dshapes/C_0603_1608Metric.step", "Capacitor_SMD.3dshapes/" + to + ".step",
                        target.offset);
        auto u = runner.waitAndUpdate(std::chrono::seconds(10));
        REQUIRE(u.has_value());
        CHECK(u->pendingModels == 1);
        CHECK(u->sinceSaveMs < budgetMs);  // 分割を待たずに更新する
        auto done = runner.waitAndUpdate(std::chrono::seconds(30));
        REQUIRE(done.has_value());
        CHECK(done->pendingModels == 0);
    }
    SUBCASE("save without changes rebuilds nothing") {
        auto text = platform::readTextFile(pcb);
        REQUIRE(platform::writeTextFile(pcb, *text + "\n"));
        auto u = runner.waitAndUpdate(std::chrono::seconds(10));
        REQUIRE(u.has_value());
        CHECK(u->stats.diff.changed + u->stats.diff.added + u->stats.diff.removed == 0);
        CHECK(u->stats.tilesRebuilt == 0);
    }
}
