#pragma once
#include <filesystem>
#include <vector>

#include "scene/scene.hpp"

namespace krbr::app {

// 不足・失敗したモデルの一覧をログに出し、missing_models.json / missing_models.txt を書く（outDir が空ならログだけ）
void writeMissingReport(const std::vector<scene::MissingModel>& missing, const std::filesystem::path& pcb,
                        const std::filesystem::path& outDir);

}  // namespace krbr::app
