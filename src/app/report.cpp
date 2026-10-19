#include "app/report.hpp"

#include <algorithm>
#include <nlohmann/json.hpp>

#include "core/log.hpp"
#include "core/platform.hpp"

namespace krbr::app {

void writeMissingReport(const std::vector<scene::MissingModel>& missing, const std::filesystem::path& pcb,
                        const std::filesystem::path& outDir) {
    nlohmann::json j;
    j["board"] = platform::pathToUtf8(std::filesystem::absolute(pcb));
    j["count"] = missing.size();
    j["missing"] = nlohmann::json::array();
    std::string txt = std::format("missing / failed 3D models: {}\n", missing.size());
    size_t wRef = 9, wFp = 9;
    for (const auto& m : missing) {
        wRef = std::max(wRef, m.reference.size());
        wFp = std::max(wFp, m.footprint.size());
    }
    txt += std::format("{:<{}}  {:<{}}  {}\n", "reference", wRef, "footprint", wFp, "model / reason");
    for (const auto& m : missing) {
        j["missing"].push_back({{"reference", m.reference},
                                {"footprint", m.footprint},
                                {"value", m.value},
                                {"model", m.spec},
                                {"resolved_path", m.resolved},
                                {"reason", m.reason}});
        txt += std::format("{:<{}}  {:<{}}  {}\n{:<{}}  {:<{}}    -> {}\n", m.reference, wRef, m.footprint, wFp, m.spec,
                           "", wRef, "", wFp, m.reason);
    }
    if (missing.empty()) {
        log::info("all 3D models resolved");
    } else {
        log::warn("{} 3D model(s) missing or failed; shown as placeholder boxes:", missing.size());
        for (const auto& m : missing) log::warn("  {:<6} {}  [{}]  {}", m.reference, m.footprint, m.reason, m.spec);
    }
    const auto jsonPath = outDir / "missing_models.json";
    const auto txtPath = outDir / "missing_models.txt";
    if (!platform::writeTextFile(jsonPath, j.dump(2) + "\n")) log::warn("cannot write {}", platform::pathToUtf8(jsonPath));
    if (!platform::writeTextFile(txtPath, txt)) log::warn("cannot write {}", platform::pathToUtf8(txtPath));
    log::info("wrote {}", platform::pathToUtf8(jsonPath));
}

}  // namespace krbr::app
