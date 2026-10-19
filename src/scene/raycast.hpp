#pragma once
// CPU での光線と三角形の交差（ビューアでクリックした点にピントを合わせる、といったまれな用途向け。全三角形を調べる）
#include <optional>

#include "scene/scene.hpp"

namespace krbr::scene {

// 最も近い交点までの距離（dir は単位ベクトル）。当たらなければ空
std::optional<float> raycast(const Scene& s, glm::vec3 origin, glm::vec3 dir);

}  // namespace krbr::scene
