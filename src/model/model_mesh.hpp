#pragma once
#include <filesystem>
#include <memory>
#include <string>
#include <vector>

#include "geom/mesh.hpp"

namespace krbr::model {

// モデルの部位（STEP の XDE の名前と面の色の組）。材質はこの単位で割り当てる
struct ModelPart {
    std::string name;  // XDE の名前（部品・サブシェイプ）。無ければ空
    glm::vec3 srgb{0.5f};  // STEP に書かれた色の値（表示用の sRGB）
    float alpha = 1.0f;
    bool hasColor = false;

    glm::vec3 linearColor() const;
    std::string hex() const;            // "#rrggbb"
    std::string kicadColorName() const;  // KiCad 標準モデルの色なら "metal_grey_pins" など。違えば空
    // 対応表の照合に使うキーの候補（優先順）：名前、"kicad:<色名>"、"#rrggbb"
    std::vector<std::string> keys() const;
};

// 3D モデルを分割したメッシュ。座標は mm（モデル自身の座標系、KiCad 3D と同じ Z 上向き）。
// mesh.triMaterial は parts の番号
struct ModelMesh {
    geom::Mesh mesh;
    std::vector<ModelPart> parts;
};

struct MeshParams {
    double linearDeflection = 0.01;  // mm
    double angularDeflection = 0.35;  // rad
};

struct LoadResult {
    std::shared_ptr<const ModelMesh> mesh;
    bool fromCache = false;
    bool pending = false;  // 裏で分割中（ModelLibrary::request）
    std::string fileHash;  // ファイル内容の XXH64（16 進）。材質の対応表の照合にも使う
    std::string error;
    double seconds = 0.0;
};

// モデル形式ごとの読み込み器（STEP / VRML ...）。後から形式を追加できる
class IModelLoader {
public:
    virtual ~IModelLoader() = default;
    virtual const char* name() const = 0;
    virtual bool canLoad(const std::filesystem::path& p) const = 0;
    virtual LoadResult load(const std::filesystem::path& p, const MeshParams& params) const = 0;
};

std::unique_ptr<IModelLoader> createStepLoader();
// STEP 変換器 DLL（step\krbr_step.dll）が使えるか。why に無い理由
bool stepSupportAvailable(std::string* why = nullptr);

}  // namespace krbr::model
