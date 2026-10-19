#pragma once
// レジスト（ソルダーマスク）の色のプリセット：KiCad の stackup の色名と、基板メーカーが選ばせる色
#include <glm/glm.hpp>
#include <string>
#include <string_view>
#include <vector>

namespace krbr::scene {

struct MaskPreset {
    std::string name;   // 例 "jlcpcb-purple"（小文字・ハイフン。照合は大小・区切りを無視）
    glm::vec3 srgb;     // 銅の上で見えるレジストの色（sRGB）
    float roughness;    // コートの粗さ（光沢 0.1〜、つや消し 0.5〜）
    float scatter;      // 顔料自身が光を散らす割合（0 = 透明なコート、1 = 不透明）
    glm::vec3 silkSrgb; // このレジストに合わせたシルクの色（黄・白のレジストでは黒いシルク）
    std::string note;
};

const std::vector<MaskPreset>& maskPresets();
// 名前で探す。"JLCPCB Purple" / "jlcpcb:purple" / "jlcpcb_purple" も同じ扱い。無ければ nullptr
const MaskPreset* findMaskPreset(std::string_view name);

}  // namespace krbr::scene
