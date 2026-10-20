#include "app/kicad_cli.hpp"

#include <algorithm>

#include "core/hash.hpp"
#include "core/log.hpp"
#include "core/platform.hpp"
#include "raster/svg_reader.hpp"

namespace krbr::app {

namespace fs = std::filesystem;

std::optional<fs::path> findKicadCli(const std::string& configured) {
    std::error_code ec;
    if (!configured.empty()) {
        fs::path p = platform::u8path(configured);
        if (fs::is_regular_file(p, ec)) return p;
        log::warn("kicad-cli not found at {}", configured);
    }
#ifdef _WIN32
    const char* exe = "kicad-cli.exe";
    std::vector<fs::path> roots;
    for (const char* env : {"ProgramFiles", "ProgramW6432"})
        if (auto pf = platform::getEnv(env)) roots.push_back(platform::u8path(*pf) / "KiCad");
    // 新しい版を優先
    std::vector<fs::path> found;
    for (const auto& r : roots) {
        if (!fs::is_directory(r, ec)) continue;
        for (const auto& e : fs::directory_iterator(r, ec)) {
            fs::path c = e.path() / "bin" / exe;
            if (fs::is_regular_file(c, ec)) found.push_back(c);
        }
    }
    std::sort(found.begin(), found.end());
    if (!found.empty()) return found.back();
#else
    const char* exe = "kicad-cli";
    for (const char* d : {"/usr/bin", "/usr/local/bin"})
        if (fs::is_regular_file(fs::path(d) / exe, ec)) return fs::path(d) / exe;
#endif
    if (auto path = platform::getEnv("PATH")) {
#ifdef _WIN32
        const char sep = ';';
#else
        const char sep = ':';
#endif
        size_t start = 0;
        while (start <= path->size()) {
            size_t end = path->find(sep, start);
            if (end == std::string::npos) end = path->size();
            fs::path c = platform::u8path(path->substr(start, end - start)) / exe;
            if (fs::is_regular_file(c, ec)) return c;
            start = end + 1;
        }
    }
    return std::nullopt;
}

SilkPlot plotSilkWithKicadCli(const fs::path& cli, const kicad::Board& board, const fs::path& cacheDir,
                              bool useCache) {
    SilkPlot out;
    std::error_code ec;
    const fs::path pro = board.file.parent_path() / (board.projectName + ".kicad_pro");

    // キャッシュのキー：基板とプロジェクト（文字の変数）の内容
    std::string keySrc;
    if (auto b = platform::readTextFile(board.file)) keySrc += *b;
    if (auto p = platform::readTextFile(pro)) keySrc += *p;
    keySrc += platform::pathToUtf8(cli);
    const std::string key = toHex(xxh64(keySrc.data(), keySrc.size()));
    const fs::path cdir = cacheDir / "silk";
    const fs::path cached[2] = {cdir / (key + "-F.svg"), cdir / (key + "-B.svg")};

    std::optional<std::string> svg[2];
    if (useCache && fs::exists(cached[0], ec) && fs::exists(cached[1], ec)) {
        svg[0] = platform::readTextFile(cached[0]);
        svg[1] = platform::readTextFile(cached[1]);
        out.fromCache = svg[0] && svg[1];
    }
    if (!out.fromCache) {
        const fs::path work = platform::tempPath("kicad");
        fs::create_directories(work, ec);
        const fs::path pcbCopy = work / board.file.filename();
        fs::copy_file(board.file, pcbCopy, fs::copy_options::overwrite_existing, ec);
        if (ec) {
            log::warn("cannot copy board to temp dir: {}", ec.message());
            return out;
        }
        if (fs::exists(pro, ec)) fs::copy_file(pro, work / pro.filename(), fs::copy_options::overwrite_existing, ec);
        const char* layers[2] = {"F.SilkS", "B.SilkS"};
        for (int s = 0; s < 2; ++s) {
            const fs::path svgPath = work / (std::string(s == 0 ? "F" : "B") + "_silk.svg");
            const int rc = platform::runHidden(
                cli,
                {"pcb", "export", "svg", "--layers", layers[s], "--mode-single", "--exclude-drawing-sheet",
                 "--page-size-mode", "0", "-o", platform::pathToUtf8(svgPath), platform::pathToUtf8(pcbCopy)},
                work, work / "kicad-cli.log");
            if (rc != 0) {
                log::warn("kicad-cli failed for {} (exit {}), see {}", layers[s], rc,
                          platform::pathToUtf8(work / "kicad-cli.log"));
                continue;
            }
            svg[s] = platform::readTextFile(svgPath);
            if (svg[s] && useCache) {
                fs::create_directories(cdir, ec);
                platform::writeTextFile(cached[s], *svg[s]);
            }
        }
        fs::remove_all(work, ec);
    }
    for (int s = 0; s < 2; ++s) {
        if (!svg[s]) continue;
        auto r = raster::readKicadSvg(*svg[s]);
        if (r.ok) out.side[s] = geom::unite(r.paths);
        log::debug("kicad-cli silk {}: {} elements -> {} paths", s == 0 ? "front" : "back", r.elements,
                   out.side[s] ? out.side[s]->size() : 0);
    }
    return out;
}

}  // namespace krbr::app
