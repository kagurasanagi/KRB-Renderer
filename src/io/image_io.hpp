#pragma once
#include <filesystem>
#include <string>
#include <vector>

namespace krbr::io {

// RGBA float のリニア画像
struct ImageF {
    int width = 0, height = 0;
    std::vector<float> rgba;
};

// .hdr（Radiance）/ .exr を読み込む
bool loadHdrImage(const std::filesystem::path& p, ImageF& out, std::string& err);

bool saveExr(const std::filesystem::path& p, const ImageF& img, std::string& err);

struct ToneMapOptions {
    float exposureEv = 0.0f;
};
// ACES（Hill による近似）でトーンマッピングして sRGB の 8bit PNG にする
bool savePngTonemapped(const std::filesystem::path& p, const ImageF& img, const ToneMapOptions& opts, std::string& err);
// 値をそのまま（0..1 を sRGB 変換して）PNG にする。デバッグ・AOV の確認用
bool savePngLinear(const std::filesystem::path& p, const ImageF& img, bool srgb, std::string& err);
bool savePngRgba8(const std::filesystem::path& p, int w, int h, const unsigned char* rgba, std::string& err);

}  // namespace krbr::io
