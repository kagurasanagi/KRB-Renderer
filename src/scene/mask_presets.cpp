#include "scene/mask_presets.hpp"

#include <cctype>

namespace krbr::scene {

namespace {
// 色は「銅の上で見えるレジストの色」（sRGB）。メーカーの色は写真からの目分量で、実測ではない。
// シルクの色は、黄・白のレジストでは黒、それ以外は白（メーカーの標準の組み合わせ）。
// roughness は 0.18 が標準の光沢、0.5 以上でつや消し。scatter は顔料の不透明さ（白・黒は高い）
const std::vector<MaskPreset> kPresets = {
    // KiCad の stackup の色名（(color "Green") など）
    {"green", {0.05f, 0.42f, 0.16f}, 0.18f, 0.3f, {0.93f, 0.93f, 0.90f}, "KiCad / 一般的な緑"},
    {"red", {0.62f, 0.07f, 0.06f}, 0.18f, 0.4f, {0.93f, 0.93f, 0.90f}, "KiCad"},
    {"blue", {0.05f, 0.18f, 0.52f}, 0.18f, 0.4f, {0.93f, 0.93f, 0.90f}, "KiCad"},
    {"yellow", {0.80f, 0.68f, 0.10f}, 0.18f, 0.5f, {0.08f, 0.08f, 0.08f}, "KiCad"},
    {"purple", {0.30f, 0.10f, 0.42f}, 0.18f, 0.4f, {0.93f, 0.93f, 0.90f}, "KiCad"},
    {"black", {0.04f, 0.04f, 0.04f}, 0.18f, 0.7f, {0.93f, 0.93f, 0.90f}, "KiCad"},
    {"white", {0.90f, 0.90f, 0.88f}, 0.22f, 0.9f, {0.08f, 0.08f, 0.08f}, "KiCad"},
    {"matte-black", {0.05f, 0.05f, 0.05f}, 0.55f, 0.8f, {0.93f, 0.93f, 0.90f}, "KiCad"},
    // JLCPCB の選択肢（Green / Purple / Red / Yellow / Blue / White / Black / Matte Black / Matte Green）
    {"jlcpcb-green", {0.08f, 0.40f, 0.19f}, 0.18f, 0.3f, {0.93f, 0.93f, 0.90f}, "JLCPCB Green：やや明るい緑、光沢"},
    {"jlcpcb-purple", {0.33f, 0.12f, 0.46f}, 0.18f, 0.4f, {0.93f, 0.93f, 0.90f}, "JLCPCB Purple：青みの紫"},
    {"jlcpcb-red", {0.60f, 0.08f, 0.07f}, 0.18f, 0.4f, {0.93f, 0.93f, 0.90f}, "JLCPCB Red"},
    {"jlcpcb-yellow", {0.85f, 0.70f, 0.10f}, 0.18f, 0.5f, {0.08f, 0.08f, 0.08f}, "JLCPCB Yellow"},
    {"jlcpcb-blue", {0.05f, 0.20f, 0.50f}, 0.18f, 0.4f, {0.93f, 0.93f, 0.90f}, "JLCPCB Blue"},
    {"jlcpcb-white", {0.92f, 0.92f, 0.88f}, 0.25f, 0.9f, {0.08f, 0.08f, 0.08f}, "JLCPCB White：ほぼ不透明"},
    {"jlcpcb-black", {0.06f, 0.06f, 0.06f}, 0.15f, 0.7f, {0.93f, 0.93f, 0.90f}, "JLCPCB Black：光沢の黒"},
    {"jlcpcb-matte-black", {0.07f, 0.07f, 0.07f}, 0.55f, 0.8f, {0.93f, 0.93f, 0.90f}, "JLCPCB Matte Black"},
    {"jlcpcb-matte-green", {0.10f, 0.38f, 0.20f}, 0.50f, 0.5f, {0.93f, 0.93f, 0.90f}, "JLCPCB Matte Green"},
};

// 大小・区切り（空白、ハイフン、下線、コロン）を無視して比べるための正規化
std::string normalize(std::string_view s) {
    std::string out;
    for (unsigned char c : s) {
        if (c == ' ' || c == '-' || c == '_' || c == ':') continue;
        out.push_back(static_cast<char>(std::tolower(c)));
    }
    return out;
}
}  // namespace

const std::vector<MaskPreset>& maskPresets() { return kPresets; }

const MaskPreset* findMaskPreset(std::string_view name) {
    const std::string key = normalize(name);
    if (key.empty()) return nullptr;
    for (const auto& p : kPresets)
        if (normalize(p.name) == key) return &p;
    return nullptr;
}

}  // namespace krbr::scene
