// OpenPBR Surface の簡易版：コート（誘電体 GGX）＋ベース（金属 GGX / 誘電体 GGX ＋ 拡散）
// 局所座標系（法線 = +Z）で評価する。eval / pdf / sample は同じ確率分布を使い、MIS に使える。
#ifndef KRBR_BSDF_GLSL
#define KRBR_BSDF_GLSL

struct Shade {
    vec3 baseColor;
    float baseWeight;
    float metalness;
    vec3 specColor;
    float specWeight;
    float roughness;
    float ior;
    float coatWeight;
    vec3 coatColor;
    float coatRoughness;
    float coatIor;
};

Shade shadeFromMaterial(Material m) {
    Shade s;
    s.baseColor = m.baseColor.rgb;
    s.baseWeight = m.baseColor.a;
    s.specColor = m.specColor.rgb;
    s.specWeight = m.specColor.a;
    s.metalness = m.params0.x;
    s.roughness = m.params0.y;
    s.ior = m.params0.z;
    s.coatWeight = m.params0.w;
    s.coatColor = m.coatColor.rgb;
    s.coatRoughness = m.coatColor.a;
    s.coatIor = m.params1.x;
    return s;
}

float fresnelDielectric(float cosi, float eta) {
    float c = abs(cosi);
    float g2 = eta * eta - 1.0 + c * c;
    if (g2 < 0.0) return 1.0;
    float g = sqrt(g2);
    float a = (g - c) / (g + c);
    float b = (c * (g + c) - 1.0) / (c * (g - c) + 1.0);
    return 0.5 * a * a * (1.0 + b * b);
}

vec3 fresnelSchlick(vec3 f0, float c) {
    float m = clamp(1.0 - c, 0.0, 1.0);
    float m2 = m * m;
    return f0 + (vec3(1.0) - f0) * (m2 * m2 * m);
}

float ggxAlpha(float roughness) { return max(roughness * roughness, 1e-3); }

float ggxD(float NoH, float a2) {
    float d = NoH * NoH * (a2 - 1.0) + 1.0;
    return a2 / (PI * d * d);
}

float ggxLambda(float NoX, float a2) {
    float c2 = NoX * NoX;
    float t2 = max(0.0, 1.0 - c2) / max(c2, 1e-8);
    return 0.5 * (-1.0 + sqrt(1.0 + a2 * t2));
}

float ggxG1(float NoX, float a2) { return 1.0 / (1.0 + ggxLambda(NoX, a2)); }
float ggxG2(float NoV, float NoL, float a2) { return 1.0 / (1.0 + ggxLambda(NoV, a2) + ggxLambda(NoL, a2)); }

// 可視法線分布のサンプリング（Heitz 2018、等方）
vec3 sampleGgxVndf(vec3 v, float a, vec2 u) {
    vec3 vh = normalize(vec3(a * v.x, a * v.y, v.z));
    float lensq = vh.x * vh.x + vh.y * vh.y;
    vec3 t1 = lensq > 0.0 ? vec3(-vh.y, vh.x, 0.0) * inversesqrt(lensq) : vec3(1.0, 0.0, 0.0);
    vec3 t2 = cross(vh, t1);
    float r = sqrt(u.x);
    float phi = 2.0 * PI * u.y;
    float p1 = r * cos(phi);
    float p2 = r * sin(phi);
    float s = 0.5 * (1.0 + vh.z);
    p2 = (1.0 - s) * sqrt(max(0.0, 1.0 - p1 * p1)) + s * p2;
    vec3 nh = p1 * t1 + p2 * t2 + sqrt(max(0.0, 1.0 - p1 * p1 - p2 * p2)) * vh;
    return normalize(vec3(a * nh.x, a * nh.y, max(1e-6, nh.z)));
}

// 反射方向 l の確率密度（VNDF で h を選んで反射した場合）
float pdfGgxReflect(vec3 v, vec3 l, float a) {
    vec3 h = normalize(v + l);
    float a2 = a * a;
    return ggxD(h.z, a2) * ggxG1(v.z, a2) / (4.0 * v.z);
}

struct LobeProbs {
    float coat;
    float spec;  // コートを選ばなかった場合に、ベースの鏡面を選ぶ確率
};

LobeProbs lobeProbs(Shade s, vec3 v) {
    LobeProbs p;
    float fc = fresnelDielectric(v.z, s.coatIor);
    p.coat = s.coatWeight > 0.0 ? s.coatWeight * clamp(2.0 * fc, 0.15, 0.85) : 0.0;
    float fd = s.specWeight * fresnelDielectric(v.z, s.ior);
    float pSpecDielectric = s.baseWeight > 0.0 ? clamp(2.0 * fd, 0.1, 0.9) : 1.0;
    p.spec = mix(pSpecDielectric, 1.0, s.metalness);
    return p;
}

// f(v, l)（cos 項は含まない）
vec3 bsdfEval(Shade s, vec3 v, vec3 l) {
    if (v.z <= 0.0 || l.z <= 0.0) return vec3(0.0);
    vec3 h = normalize(v + l);
    float VoH = max(dot(v, h), 0.0);
    vec3 result = vec3(0.0);

    // コート
    float fcV = fresnelDielectric(v.z, s.coatIor);
    float fcL = fresnelDielectric(l.z, s.coatIor);
    if (s.coatWeight > 0.0) {
        float ac = ggxAlpha(s.coatRoughness);
        float a2 = ac * ac;
        float fc = fresnelDielectric(VoH, s.coatIor);
        result += s.coatWeight * fc * ggxD(h.z, a2) * ggxG2(v.z, l.z, a2) / (4.0 * v.z * l.z);
    }
    vec3 coatAtten = mix(vec3(1.0), s.coatColor * (1.0 - fcV) * (1.0 - fcL), s.coatWeight);

    // ベースの鏡面（誘電体と金属で分布は共通、フレネルだけ異なる）
    float a = ggxAlpha(s.roughness);
    float a2 = a * a;
    float spec = ggxD(h.z, a2) * ggxG2(v.z, l.z, a2) / (4.0 * v.z * l.z);
    vec3 fDiel = vec3(s.specWeight * fresnelDielectric(VoH, s.ior)) * s.specColor;
    vec3 fMetal = fresnelSchlick(s.baseColor, VoH) * s.specWeight;
    vec3 base = mix(fDiel, fMetal, s.metalness) * spec;

    // 拡散（誘電体のみ）
    float fdV = s.specWeight * fresnelDielectric(v.z, s.ior);
    float fdL = s.specWeight * fresnelDielectric(l.z, s.ior);
    base += (1.0 - s.metalness) * s.baseWeight * s.baseColor * INV_PI * (1.0 - fdV) * (1.0 - fdL);

    return result + coatAtten * base;
}

float bsdfPdf(Shade s, vec3 v, vec3 l) {
    if (v.z <= 0.0 || l.z <= 0.0) return 0.0;
    LobeProbs p = lobeProbs(s, v);
    float pdf = 0.0;
    if (p.coat > 0.0) pdf += p.coat * pdfGgxReflect(v, l, ggxAlpha(s.coatRoughness));
    float rest = 1.0 - p.coat;
    pdf += rest * p.spec * pdfGgxReflect(v, l, ggxAlpha(s.roughness));
    pdf += rest * (1.0 - p.spec) * l.z * INV_PI;
    return pdf;
}

// 方向をサンプリングして l を返す。失敗時は l.z <= 0
vec3 bsdfSample(Shade s, vec3 v, inout Rng rng) {
    LobeProbs p = lobeProbs(s, v);
    float u = rnd(rng);
    vec2 u2 = rnd2(rng);
    if (u < p.coat) {
        vec3 h = sampleGgxVndf(v, ggxAlpha(s.coatRoughness), u2);
        return reflect(-v, h);
    }
    u = (u - p.coat) / max(1.0 - p.coat, 1e-6);
    if (u < p.spec) {
        vec3 h = sampleGgxVndf(v, ggxAlpha(s.roughness), u2);
        return reflect(-v, h);
    }
    return sampleCosineHemisphere(u2);
}

vec3 shadeAlbedo(Shade s) {
    vec3 base = mix(s.baseColor * s.baseWeight, s.baseColor, s.metalness);
    return mix(base, base * s.coatColor, s.coatWeight);
}

#endif
