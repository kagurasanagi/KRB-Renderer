#pragma once
// 3D モデルの分割結果のファイル形式（.krbm）。krbr 本体と STEP 変換器 DLL（krbr_step）と Rust 版で共有する。
//   "KRBM" + u32[4]{version, 頂点数, 三角形数, 部位数}
//   + 頂点 float3[n] + 法線 float3[n] + 添字 u32[3t] + 三角形ごとの部位番号 u32[t]
//   + 部位ごとに { u32 名前の長さ, 名前(UTF-8), float[5]{r, g, b, a, 色あり} }
#include <filesystem>
#include <memory>

#include "model/model_mesh.hpp"

namespace krbr::model {

constexpr uint32_t kMeshFileVersion = 3;  // v2: 部位（XDE の名前と色）, v3: 色を STEP の sRGB 値で保存

bool writeMeshFile(const std::filesystem::path& p, const ModelMesh& m);
std::shared_ptr<ModelMesh> readMeshFile(const std::filesystem::path& p);

}  // namespace krbr::model
