// 分光レンダリング（代表波長方式）：1 本のパスに 4 波長を載せる。RGB モードでは vec4 の xyz に RGB を入れ、w は使わない。
// C++ 側 scene/spectrum.hpp と同じ式（等色関数、シグモイド多項式、表の引き方）にそろえること
#ifndef KRBR_SPECTRAL_GLSL
#define KRBR_SPECTRAL_GLSL

#define LAMBDA_MIN 360.0
#define LAMBDA_MAX 830.0
#define RGB2SPEC_RES 64

// このパスの 4 波長（nm）。gLambda.x が代表波長
vec4 gLambda = vec4(550.0);

bool spectralMode() { return F.spectral.x > 0.5; }

float sigmoidPoly(vec3 c, float lambda) {
    float x = (lambda - LAMBDA_MIN) / (LAMBDA_MAX - LAMBDA_MIN);
    float z = (c.x * x + c.y) * x + c.z;
    return 0.5 + z / (2.0 * sqrt(1.0 + z * z));
}

vec4 sigmoid4(vec3 c) {
    return vec4(sigmoidPoly(c, gLambda.x), sigmoidPoly(c, gLambda.y), sigmoidPoly(c, gLambda.z), sigmoidPoly(c, gLambda.w));
}

// 反射率など 0..1 の色（係数は CPU で求めて材質に入れてある）
vec4 specReflect(vec3 rgb, vec4 coef) { return spectralMode() ? sigmoid4(coef.xyz) : vec4(rgb, 0.0); }
// 上限のない色（発光）。coef.w が倍率
vec4 specUnbounded(vec3 rgb, vec4 coef) { return spectralMode() ? coef.w * sigmoid4(coef.xyz) : vec4(rgb, 0.0); }

// 表を引いて係数を求める（三重線形補間）。環境光のように画素ごとに色が変わるもの用
vec3 rgb2specCoef(vec3 rgb) {
    rgb = clamp(rgb, 0.0, 1.0);
    if (rgb.r == rgb.g && rgb.g == rgb.b) {
        float v = clamp(rgb.r, 1e-7, 1.0 - 1e-7);
        return vec3(0.0, 0.0, (v - 0.5) / sqrt(v * (1.0 - v)));
    }
    int l = 0;
    if (rgb.g > rgb[l]) l = 1;
    if (rgb.b > rgb[l]) l = 2;
    float z = rgb[l];
    const float N1 = float(RGB2SPEC_RES - 1);
    float x = rgb[(l + 1) % 3] / z * N1, y = rgb[(l + 2) % 3] / z * N1;
    int lo = 0, hi = RGB2SPEC_RES - 1;
    while (lo + 1 < hi) {
        int mid = (lo + hi) >> 1;
        if (rgb2specScale[mid] <= z) lo = mid; else hi = mid;
    }
    int zi = min(lo, RGB2SPEC_RES - 2);
    float zf = (z - rgb2specScale[zi]) / (rgb2specScale[zi + 1] - rgb2specScale[zi]);
    int xi = min(int(x), RGB2SPEC_RES - 2), yi = min(int(y), RGB2SPEC_RES - 2);
    float xf = x - float(xi), yf = y - float(yi);
    vec3 c = vec3(0.0);
    for (int dz = 0; dz < 2; ++dz)
        for (int dy = 0; dy < 2; ++dy)
            for (int dx = 0; dx < 2; ++dx) {
                float w = (dz == 1 ? zf : 1.0 - zf) * (dy == 1 ? yf : 1.0 - yf) * (dx == 1 ? xf : 1.0 - xf);
                int idx = ((l * RGB2SPEC_RES + zi + dz) * RGB2SPEC_RES + yi + dy) * RGB2SPEC_RES + xi + dx;
                c += w * rgb2specData[idx].xyz;
            }
    return c;
}

// 上限のない RGB（環境光）→ 4 波長の値。s(λ) = 2m · sigmoid(rgb / 2m)（m は最大の成分）
vec4 rgbToSpecUnbounded(vec3 rgb) {
    if (!spectralMode()) return vec4(rgb, 0.0);
    float m = max(rgb.r, max(rgb.g, rgb.b));
    if (m <= 0.0) return vec4(0.0);
    return 2.0 * m * sigmoid4(rgb2specCoef(rgb / (2.0 * m)));
}

// 可視域に重みをつけた波長のサンプリング（pbrt-v4 の SampleVisibleWavelengths、360..830 nm）
float sampleVisibleWavelength(float u) { return 538.0 - 138.888889 * atanh(0.85691062 - 1.82750197 * u); }
float visibleWavelengthPdf(float l) {
    if (l < LAMBDA_MIN || l > LAMBDA_MAX) return 0.0;
    float c = cosh(0.0072 * (l - 538.0));
    return 0.0039398042 / (c * c);
}

// 4 波長を層化して選ぶ（u から 1/4 ずつずらす）
void sampleWavelengths(float u) {
    for (int i = 0; i < 4; ++i) gLambda[i] = sampleVisibleWavelength(fract(u + float(i) * 0.25));
}

// CIE 1931 等色関数の解析的な近似（Wyman, Sloan & Shirley 2013）
float gaussPiece(float l, float mu, float s1, float s2) {
    float t = (l - mu) / (l < mu ? s1 : s2);
    return exp(-0.5 * t * t);
}
vec3 cmf(float l) {
    float x = 1.056 * gaussPiece(l, 599.8, 37.9, 31.0) + 0.362 * gaussPiece(l, 442.0, 16.0, 26.7) -
              0.065 * gaussPiece(l, 501.1, 20.4, 26.2);
    float y = 0.821 * gaussPiece(l, 568.8, 46.9, 40.5) + 0.286 * gaussPiece(l, 530.9, 16.3, 31.1);
    float z = 1.217 * gaussPiece(l, 437.0, 11.8, 36.0) + 0.681 * gaussPiece(l, 459.0, 26.0, 13.8);
    return vec3(x, y, z);
}

// 4 波長の放射輝度 → XYZ（平坦なスペクトル 1 が Y = 1 になる尺度）
vec3 spectrumToXyz(vec4 L) {
    vec3 xyz = vec3(0.0);
    for (int i = 0; i < 4; ++i) {
        float p = visibleWavelengthPdf(gLambda[i]);
        if (p > 0.0) xyz += L[i] * cmf(gLambda[i]) / p;
    }
    return xyz * (0.25 * F.spectral.y);
}

vec3 rgbToXyz(vec3 c) { return mat3(F.rgbToXyz0.xyz, F.rgbToXyz1.xyz, F.rgbToXyz2.xyz) * c; }

#endif
