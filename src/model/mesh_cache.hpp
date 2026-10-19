#pragma once
// 3D モデルの分割結果を、ファイル内容のハッシュをキーにしてディスクへ保存する
#include <map>
#include <memory>
#include <vector>

#include "model/model_mesh.hpp"

namespace krbr::model {

class ModelLibrary {
public:
    ModelLibrary(std::filesystem::path cacheDir, MeshParams params, bool useCache);

    // 同じ実行内では同じパスを1回だけ読む
    LoadResult get(const std::filesystem::path& p);

    struct Stats {
        int loaded = 0, cacheHits = 0, failed = 0;
    };
    const Stats& stats() const { return stats_; }

private:
    std::filesystem::path cacheDir_;
    MeshParams params_;
    bool useCache_;
    std::vector<std::unique_ptr<IModelLoader>> loaders_;
    std::map<std::filesystem::path, LoadResult> memo_;
    Stats stats_;
};

bool writeMeshFile(const std::filesystem::path& p, const ModelMesh& m);
std::shared_ptr<ModelMesh> readMeshFile(const std::filesystem::path& p);

}  // namespace krbr::model
