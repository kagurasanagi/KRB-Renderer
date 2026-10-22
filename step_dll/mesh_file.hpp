#pragma once
// 3D モデルの分割結果のファイル形式（.krbm）。STEP 変換器 DLL（krbr_step）が書き、Rust 版（krbr-model の mesh_file.rs）が読む。
//   "KRBM" + u32[4]{version, 頂点数, 三角形数, 部位数}
//   + 頂点 float3[n] + 法線 float3[n] + 添字 u32[3t] + 三角形ごとの部位番号 u32[t]
//   + 部位ごとに { u32 名前の長さ（kMaxPartNameBytes 以下）, 名前(UTF-8), float[5]{r, g, b, a, 色あり} }
#include <cstddef>
#include <filesystem>

#include "model_mesh.hpp"

namespace krbr::model {

constexpr uint32_t kMeshFileVersion = 3;  // v2: 部位（XDE の名前と色）, v3: 色を STEP の sRGB 値で保存

// 部位名の長さの上限（バイト）。読み手（mesh_file.rs）はこれを超える長さを壊れたファイルとして拒否するので、
// 書き手は UTF-8 の文字の境界で切り詰めてから書く
constexpr size_t kMaxPartNameBytes = 4096;

// 一時ファイルに書いてから置き換える（書きかけのファイルを残さない）。失敗なら false
bool writeMeshFile(const std::filesystem::path& p, const ModelMesh& m);

}  // namespace krbr::model
