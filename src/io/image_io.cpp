#include "io/image_io.hpp"

#include <algorithm>
#include <cmath>
#include <cstring>

#include "core/platform.hpp"

#define STB_IMAGE_IMPLEMENTATION
#include <stb_image.h>
#define STB_IMAGE_WRITE_IMPLEMENTATION
#include <stb_image_write.h>
#include <tinyexr.h>

namespace krbr::io {

namespace {

std::string lowerExt(const std::filesystem::path& p) {
    std::string e = platform::pathToUtf8(p.extension());
    std::transform(e.begin(), e.end(), e.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
    return e;
}

float linearToSrgb(float x) {
    x = std::clamp(x, 0.0f, 1.0f);
    return x <= 0.0031308f ? 12.92f * x : 1.055f * std::pow(x, 1.0f / 2.4f) - 0.055f;
}

// Stephen Hill の ACES fitted（sRGB/Rec.709 リニア入力）
void acesFitted(float& r, float& g, float& b) {
    const float i0 = 0.59719f * r + 0.35458f * g + 0.04823f * b;
    const float i1 = 0.07600f * r + 0.90834f * g + 0.01566f * b;
    const float i2 = 0.02840f * r + 0.13383f * g + 0.83777f * b;
    auto rrt = [](float v) {
        const float a = v * (v + 0.0245786f) - 0.000090537f;
        const float b2 = v * (0.983729f * v + 0.4329510f) + 0.238081f;
        return a / b2;
    };
    const float o0 = rrt(i0), o1 = rrt(i1), o2 = rrt(i2);
    r = 1.60475f * o0 - 0.53108f * o1 - 0.07367f * o2;
    g = -0.10208f * o0 + 1.10813f * o1 - 0.00605f * o2;
    b = -0.00327f * o0 - 0.07276f * o1 + 1.07602f * o2;
}

bool writePng(const std::filesystem::path& p, int w, int h, const std::vector<unsigned char>& px, std::string& err) {
    std::error_code ec;
    if (p.has_parent_path()) std::filesystem::create_directories(p.parent_path(), ec);
    int len = 0;
    unsigned char* mem = stbi_write_png_to_mem(px.data(), w * 4, w, h, 4, &len);
    if (!mem) {
        err = "PNG encode failed";
        return false;
    }
    const bool ok = platform::writeTextFile(p, std::string(reinterpret_cast<char*>(mem), static_cast<size_t>(len)));
    STBIW_FREE(mem);
    if (!ok) err = "cannot write " + platform::pathToUtf8(p);
    return ok;
}

}  // namespace

bool loadHdrImage(const std::filesystem::path& p, ImageF& out, std::string& err) {
    auto data = platform::readBinaryFile(p);
    if (!data) {
        err = "cannot read " + platform::pathToUtf8(p);
        return false;
    }
    const std::string ext = lowerExt(p);
    if (ext == ".exr") {
        float* rgba = nullptr;
        int w = 0, h = 0;
        const char* e = nullptr;
        if (LoadEXRFromMemory(&rgba, &w, &h, data->data(), data->size(), &e) != TINYEXR_SUCCESS) {
            err = e ? e : "EXR load failed";
            if (e) FreeEXRErrorMessage(e);
            return false;
        }
        out.width = w;
        out.height = h;
        out.rgba.assign(rgba, rgba + static_cast<size_t>(w) * h * 4);
        std::free(rgba);
        return true;
    }
    int w = 0, h = 0, c = 0;
    float* px = stbi_loadf_from_memory(data->data(), static_cast<int>(data->size()), &w, &h, &c, 4);
    if (!px) {
        err = stbi_failure_reason() ? stbi_failure_reason() : "image load failed";
        return false;
    }
    out.width = w;
    out.height = h;
    out.rgba.assign(px, px + static_cast<size_t>(w) * h * 4);
    stbi_image_free(px);
    return true;
}

bool saveExr(const std::filesystem::path& p, const ImageF& img, std::string& err) {
    std::error_code ec;
    if (p.has_parent_path()) std::filesystem::create_directories(p.parent_path(), ec);
    // RGB の 32bit float で保存（A は使わない）
    EXRHeader header;
    InitEXRHeader(&header);
    EXRImage image;
    InitEXRImage(&image);
    const size_t n = static_cast<size_t>(img.width) * img.height;
    std::vector<float> ch[3];
    for (auto& c : ch) c.resize(n);
    for (size_t i = 0; i < n; ++i) {
        ch[0][i] = img.rgba[i * 4 + 0];
        ch[1][i] = img.rgba[i * 4 + 1];
        ch[2][i] = img.rgba[i * 4 + 2];
    }
    // tinyexr はチャンネルを名前の逆順（B, G, R）で並べる慣例
    float* ptrs[3] = {ch[2].data(), ch[1].data(), ch[0].data()};
    image.images = reinterpret_cast<unsigned char**>(ptrs);
    image.width = img.width;
    image.height = img.height;
    image.num_channels = 3;
    header.num_channels = 3;
    std::vector<EXRChannelInfo> infos(3);
    std::strcpy(infos[0].name, "B");
    std::strcpy(infos[1].name, "G");
    std::strcpy(infos[2].name, "R");
    header.channels = infos.data();
    int types[3] = {TINYEXR_PIXELTYPE_FLOAT, TINYEXR_PIXELTYPE_FLOAT, TINYEXR_PIXELTYPE_FLOAT};
    header.pixel_types = types;
    header.requested_pixel_types = types;
    header.compression_type = TINYEXR_COMPRESSIONTYPE_ZIP;
    unsigned char* mem = nullptr;
    const char* e = nullptr;
    const size_t size = SaveEXRImageToMemory(&image, &header, &mem, &e);
    if (size == 0) {
        err = e ? e : "EXR encode failed";
        if (e) FreeEXRErrorMessage(e);
        return false;
    }
    const bool ok = platform::writeTextFile(p, std::string(reinterpret_cast<char*>(mem), size));
    std::free(mem);
    if (!ok) err = "cannot write " + platform::pathToUtf8(p);
    return ok;
}

bool savePngTonemapped(const std::filesystem::path& p, const ImageF& img, const ToneMapOptions& opts,
                       std::string& err) {
    const float scale = std::exp2(opts.exposureEv);
    const size_t n = static_cast<size_t>(img.width) * img.height;
    std::vector<unsigned char> px(n * 4);
    for (size_t i = 0; i < n; ++i) {
        float r = std::max(0.0f, img.rgba[i * 4 + 0] * scale);
        float g = std::max(0.0f, img.rgba[i * 4 + 1] * scale);
        float b = std::max(0.0f, img.rgba[i * 4 + 2] * scale);
        acesFitted(r, g, b);
        // 量子化の縞を減らす小さなディザ
        const float d = (static_cast<float>((i * 2654435761u) & 255u) / 255.0f - 0.5f) / 255.0f;
        px[i * 4 + 0] = static_cast<unsigned char>(std::lround(std::clamp(linearToSrgb(r) + d, 0.0f, 1.0f) * 255));
        px[i * 4 + 1] = static_cast<unsigned char>(std::lround(std::clamp(linearToSrgb(g) + d, 0.0f, 1.0f) * 255));
        px[i * 4 + 2] = static_cast<unsigned char>(std::lround(std::clamp(linearToSrgb(b) + d, 0.0f, 1.0f) * 255));
        px[i * 4 + 3] = 255;
    }
    return writePng(p, img.width, img.height, px, err);
}

bool savePngLinear(const std::filesystem::path& p, const ImageF& img, bool srgb, std::string& err) {
    const size_t n = static_cast<size_t>(img.width) * img.height;
    std::vector<unsigned char> px(n * 4);
    for (size_t i = 0; i < n; ++i) {
        for (int c = 0; c < 3; ++c) {
            float v = img.rgba[i * 4 + c];
            v = srgb ? linearToSrgb(v) : std::clamp(v, 0.0f, 1.0f);
            px[i * 4 + c] = static_cast<unsigned char>(std::lround(v * 255));
        }
        px[i * 4 + 3] = 255;
    }
    return writePng(p, img.width, img.height, px, err);
}

bool savePngRgba8(const std::filesystem::path& p, int w, int h, const unsigned char* rgba, std::string& err) {
    std::vector<unsigned char> px(rgba, rgba + static_cast<size_t>(w) * h * 4);
    return writePng(p, w, h, px, err);
}

}  // namespace krbr::io
