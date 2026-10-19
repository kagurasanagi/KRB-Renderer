#pragma once
// 3D モデルの分割結果を、ファイル内容のハッシュをキーにしてディスクへ保存する
#include <condition_variable>
#include <deque>
#include <map>
#include <mutex>
#include <optional>
#include <set>
#include <thread>
#include <memory>
#include <vector>

#include "model/model_mesh.hpp"

namespace krbr::model {

class ModelLibrary {
public:
    ModelLibrary(std::filesystem::path cacheDir, MeshParams params, bool useCache);
    ~ModelLibrary();

    // 読み込んで返す（同じ実行内では同じパスを1回だけ読む）
    LoadResult get(const std::filesystem::path& p);
    // キャッシュにあればすぐ返す。無ければ裏で分割を始めて pending を返す（保存の監視中に使う）
    LoadResult request(const std::filesystem::path& p);

    struct Stats {
        int loaded = 0, cacheHits = 0, failed = 0;
    };
    Stats stats() const;
    size_t completed() const;  // 裏で分割し終えた数（増えたらシーンを作り直す）
    size_t pending() const;

private:
    struct Job {
        std::filesystem::path path, cacheFile;
        std::string fileHash;
    };
    const IModelLoader* loaderFor(const std::filesystem::path& p) const;
    std::optional<LoadResult> lookup(const std::filesystem::path& p, const IModelLoader* loader,
                                     std::filesystem::path& cacheFile, std::string& fileHash);
    LoadResult loadNow(const std::filesystem::path& p, const IModelLoader* loader,
                       const std::filesystem::path& cacheFile, const std::string& fileHash);
    void workerLoop();

    std::filesystem::path cacheDir_;
    MeshParams params_;
    bool useCache_;
    std::vector<std::unique_ptr<IModelLoader>> loaders_;
    mutable std::mutex mutex_;
    std::condition_variable cv_;
    std::map<std::filesystem::path, LoadResult> memo_;
    std::set<std::filesystem::path> queued_;
    std::deque<Job> queue_;
    std::thread worker_;
    bool stop_ = false;
    size_t completed_ = 0;
    Stats stats_;
};

bool writeMeshFile(const std::filesystem::path& p, const ModelMesh& m);
std::shared_ptr<ModelMesh> readMeshFile(const std::filesystem::path& p);

}  // namespace krbr::model
