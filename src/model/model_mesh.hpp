#pragma once
#include <filesystem>
#include <memory>
#include <string>
#include <vector>

#include "geom/mesh.hpp"

namespace krbr::model {

// 3D モデルを分割したメッシュ。座標は mm（モデル自身の座標系、KiCad 3D と同じ Z 上向き）。
// mesh.triMaterial は colors の番号
struct ModelMesh {
    geom::Mesh mesh;
    std::vector<glm::vec4> colors;  // リニア RGB + 不透明度
};

struct MeshParams {
    double linearDeflection = 0.01;  // mm
    double angularDeflection = 0.35;  // rad
};

struct LoadResult {
    std::shared_ptr<const ModelMesh> mesh;
    bool fromCache = false;
    std::string error;
    double seconds = 0.0;
};

// モデル形式ごとの読み込み器。BDPT 等とは独立に、形式（STEP / VRML ...）を後から追加できる
class IModelLoader {
public:
    virtual ~IModelLoader() = default;
    virtual const char* name() const = 0;
    virtual bool canLoad(const std::filesystem::path& p) const = 0;
    virtual LoadResult load(const std::filesystem::path& p, const MeshParams& params) const = 0;
};

std::unique_ptr<IModelLoader> createStepLoader();

}  // namespace krbr::model
