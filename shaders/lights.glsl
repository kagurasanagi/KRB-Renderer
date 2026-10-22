// 光源：環境光（シーンを囲む球を覆う円盤から入る光として扱う）と、発光する三角形（片面、余弦分布で放射）。
// 光源は「環境光を確率 F.lightParams.x で、残りは三角形を 輝度 × 面積 に比例して」選ぶ。全積分器で共通
#ifndef KRBR_LIGHTS_GLSL
#define KRBR_LIGHTS_GLSL

struct Emitter {
    vec4 p0;  // w: 材質の番号（ビット）
    vec4 p1;  // w: 面積
    vec4 p2;
};
layout(set = 0, binding = KRBR_BINDING_EMITTERS, std430) readonly buffer Emitters { Emitter emitters[]; };
layout(set = 0, binding = KRBR_BINDING_EMITTER_CDF, std430) readonly buffer EmitterCdf { float emitterCdf[]; };

// 輝度。重みは Rust の発光体の表（emitter_power）と共通（shader_consts.rs の LUMINANCE_WEIGHTS）
float luminanceOf(vec3 c) { return dot(c, KRBR_LUMINANCE_WEIGHTS); }
float envPickProb() { return F.lightParams.x; }
float sceneRadius() { return F.lightParams.z; }

// 環境光を「シーンを囲む球（半径 R）を覆う円盤から入る平行な光」とみなしたときの、光線を放つ確率密度
// （面積 × 立体角あたり）。directPdfW は方向の確率密度（立体角あたり、光源を選ぶ確率を含む）
float envEmissionPdfW(float directPdfW) {
    float R = sceneRadius();
    return directPdfW / (PI * R * R);
}

// 発光面の点を直接サンプリングで選ぶ確率密度（面積あたり、光源を選ぶ確率を含む）。e は材質の発光（RGB）
float emitterPdfA(vec3 e) {
    return F.lightParams.y > 0.0 ? (1.0 - envPickProb()) * luminanceOf(e) / F.lightParams.y : 0.0;
}
// 環境光の方向の確率密度（立体角あたり、光源を選ぶ確率を含む）
float envLightPdfW(vec3 dir) { return envPickProb() * envPdfDir(dir); }

uint findEmitter(float u) {
    uint n = uint(F.lightParams.w);
    uint lo = 0u, hi = n;
    while (lo + 1u < hi) {
        uint mid = (lo + hi) >> 1;
        if (emitterCdf[mid] <= u) lo = mid; else hi = mid;
    }
    return lo;
}

vec3 sampleTriangle(Emitter e, vec2 u, out vec3 n) {
    float su = sqrt(u.x);
    float b0 = 1.0 - su, b1 = u.y * su;
    n = normalize(cross(e.p1.xyz - e.p0.xyz, e.p2.xyz - e.p0.xyz));
    return b0 * e.p0.xyz + b1 * e.p1.xyz + (1.0 - b0 - b1) * e.p2.xyz;
}

struct LightSample {
    vec3 wi;             // 点から光源への方向
    float dist;          // 光源までの距離（環境光は大きな値）
    vec4 Le;             // 放射輝度
    float directPdfW;    // この方向を直接サンプリングで選ぶ確率密度（立体角、光源を選ぶ確率を含む）
    float emissionPdfW;  // 光源側から、この光線を放つ確率密度（面積 × 立体角、光源を選ぶ確率を含む）
    float cosAtLight;
    bool isEnv;
};

// 点 p から見た光源の直接サンプリング
bool sampleDirectLight(vec3 p, inout Rng rng, out LightSample ls) {
    float u = rnd(rng);
    vec2 u2 = rnd2(rng);
    float pEnv = envPickProb();
    if (u < pEnv) {
        float pdfW;
        ls.wi = envSample(u2, pdfW);
        if (pdfW <= 0.0) return false;
        ls.dist = 1e30;
        ls.Le = envSpectrum(ls.wi);
        ls.directPdfW = pdfW * pEnv;
        ls.emissionPdfW = envEmissionPdfW(ls.directPdfW);
        ls.cosAtLight = 1.0;
        ls.isEnv = true;
        return true;
    }
    if (F.lightParams.w < 1.0) return false;
    Emitter e = emitters[findEmitter((u - pEnv) / max(1.0 - pEnv, 1e-6))];
    vec3 n;
    vec3 q = sampleTriangle(e, u2, n);
    vec3 d = q - p;
    float d2 = dot(d, d);
    ls.dist = sqrt(d2);
    ls.wi = d / ls.dist;
    ls.cosAtLight = dot(n, -ls.wi);
    if (ls.cosAtLight <= 1e-6) return false;  // 裏側（片面の光源）
    Material m = materials[floatBitsToUint(e.p0.w)];
    ls.Le = specUnbounded(m.emission.rgb, m.specEmission);
    float pA = emitterPdfA(m.emission.rgb);
    ls.directPdfW = pA * d2 / ls.cosAtLight;
    ls.emissionPdfW = pA * ls.cosAtLight * INV_PI;
    ls.isEnv = false;
    return ls.directPdfW > 0.0;
}

// 光源から光線を放つ（光源側の経路の始まり）。throughput = Le · cos / emissionPdfW
struct LightEmission {
    vec3 origin;
    vec3 dir;
    vec4 throughput;
    float emissionPdfW;
    // 直接サンプリングでこの点を選ぶ確率密度（光源を選ぶ確率を含む）。単位は光源による：発光面は面積あたり、
    // 環境光は立体角あたり（環境光の点は無限遠なので面積では表せない。SmallVCM の BackgroundLight と同じ慣習）
    float directPdf;
    float cosLight;
    bool finite;
};

vec2 concentricDisk(vec2 u) {
    vec2 o = 2.0 * u - 1.0;
    if (o.x == 0.0 && o.y == 0.0) return vec2(0.0);
    float r, th;
    if (abs(o.x) > abs(o.y)) {
        r = o.x;
        th = 0.25 * PI * (o.y / o.x);
    } else {
        r = o.y;
        th = 0.5 * PI - 0.25 * PI * (o.x / o.y);
    }
    return r * vec2(cos(th), sin(th));
}

bool emitLight(inout Rng rng, out LightEmission le) {
    float u = rnd(rng);
    vec2 u2 = rnd2(rng), u3 = rnd2(rng);
    float pEnv = envPickProb();
    if (u < pEnv) {
        float pdfW;
        vec3 toEnv = envSample(u2, pdfW);
        if (pdfW <= 0.0) return false;
        float R = sceneRadius();
        vec3 t, b;
        basis(toEnv, t, b);
        vec2 d = concentricDisk(u3);
        le.origin = F.sceneCenter.xyz + R * (toEnv + d.x * t + d.y * b);
        le.dir = -toEnv;
        le.directPdf = pdfW * pEnv;
        le.emissionPdfW = envEmissionPdfW(le.directPdf);
        le.cosLight = 1.0;
        le.finite = false;
        le.throughput = envSpectrum(toEnv) / le.emissionPdfW;
        return true;
    }
    if (F.lightParams.w < 1.0) return false;
    Emitter e = emitters[findEmitter((u - pEnv) / max(1.0 - pEnv, 1e-6))];
    vec3 n;
    vec3 q = sampleTriangle(e, u2, n);
    vec3 l = sampleCosineHemisphere(u3);
    if (l.z <= 1e-6) return false;
    vec3 t, b;
    basis(n, t, b);
    le.dir = toWorld(l, t, b, n);
    le.origin = offsetRay(q, n);
    Material m = materials[floatBitsToUint(e.p0.w)];
    float pA = emitterPdfA(m.emission.rgb);
    if (pA <= 0.0) return false;
    le.directPdf = pA;
    le.cosLight = l.z;
    le.emissionPdfW = pA * l.z * INV_PI;
    le.finite = true;
    le.throughput = specUnbounded(m.emission.rgb, m.specEmission) * (PI / pA);  // Le · cos / (pA · cos/π)
    return true;
}

#endif
