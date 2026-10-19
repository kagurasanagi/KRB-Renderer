// フェーズ3：保存の監視と差分更新。基板のコピーを書き換え、保存から画像の更新までの時間を測る（ウィンドウは開かない）
#include <doctest/doctest.h>

#include <thread>

#include "app/watch.hpp"
#include "io/image_io.hpp"
#include "core/platform.hpp"
#include "test_util.hpp"

using namespace krbr;
namespace fs = std::filesystem;

namespace {

// 保存をまねる（KiCad と同じく、ファイル全体を書き直す）
void saveWithReplace(const fs::path& pcb, const std::string& from, const std::string& to) {
    auto text = platform::readTextFile(pcb);
    REQUIRE(text);
    const size_t pos = text->find(from);
    REQUIRE(pos != std::string::npos);
    text->replace(pos, from.size(), to);
    REQUIRE(platform::writeTextFile(pcb, *text));
}

}  // namespace

TEST_CASE("watch: save -> updated image within 1 s, only nearby relief tiles rebuilt") {
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
    o.common.hdr = platform::pathToUtf8(test::sourceDir() / "hdr" / "billiard_hall_4k.exr");
    o.common.out = platform::pathToUtf8(ws.dir / "out" / "watch.png");
    o.common.spp = 32;  // 既定の 1920x1080
    app::WatchRunner runner;
    REQUIRE(runner.open(o));
    CHECK(fs::exists(runner.output()));

    // 保存の時刻の分解能を超えるよう、少し待ってから書き換える
    std::this_thread::sleep_for(std::chrono::milliseconds(50));

    SUBCASE("move a component (C11)") {
        saveWithReplace(pcb, "(at 56.8 102.3)", "(at 58.3 102.3)");
        auto u = runner.waitAndUpdate(std::chrono::seconds(10));
        REQUIRE(u.has_value());
        MESSAGE("move C11: " << u->sinceSaveMs << " ms after save; reload " << u->reloadMs << " ms ("
                             << u->stats.tilesRebuilt << " tiles), gpu " << u->gpuMs << " ms, render " << u->renderMs
                             << " ms, write " << u->writeMs << " ms");
        for (const auto& b : u->stats.diff.dirty)
            MESSAGE("dirty: (" << b.lo.x << ", " << b.lo.y << ") - (" << b.hi.x << ", " << b.hi.y << ")");
        CHECK(u->stats.diff.changed == 1);
        CHECK(u->stats.diff.added == 0);
        CHECK_FALSE(u->stats.diff.full);
        CHECK(u->stats.tilesRebuilt > 0);
        CHECK(u->stats.tilesRebuilt <= 16);  // 全体（200 枚）ではなく周辺だけ
        CHECK(u->sinceSaveMs < 1000.0);
        // 速さ優先で書いた（圧縮しない）PNG が読めること
        io::ImageF img;
        std::string err;
        REQUIRE_MESSAGE(io::loadHdrImage(runner.output(), img, err), err);
        CHECK(img.width == 1920);
        CHECK(img.height == 1080);
    }
    SUBCASE("a new 3D model is shown as a box first, then meshed in the background") {
        // C11 のモデルを、まだ分割していない（テスト用の空のキャッシュにない）ものに変える
        saveWithReplace(pcb, "Capacitor_SMD.3dshapes/C_0603_1608Metric.step", "Capacitor_SMD.3dshapes/C_1206_3216Metric.step");
        auto u = runner.waitAndUpdate(std::chrono::seconds(10));
        REQUIRE(u.has_value());
        CHECK(u->pendingModels == 1);
        CHECK(u->sinceSaveMs < 1000.0);  // 分割を待たずに更新する
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
