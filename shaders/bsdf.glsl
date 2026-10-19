// OpenPBR Surface の簡易版：コート（誘電体 GGX）＋ベース（金属 GGX / 誘電体 GGX ＋ 拡散 / 誘電体の透過）
// 局所座標系（法線 = +Z、v は必ず +Z 側）で評価する。eval / pdf / sample は同じ確率分布を使い、MIS に使える。
// 色は vec4：分光モードでは 4 波長の値、RGB モードでは xyz に RGB（w は使わない）
#ifndef KRBR_BSDF_GLSL
#define KRBR_BSDF_GLSL

struct Shade {
    vec4 baseColor;
    float baseWeight;
    float metalness;
    vec4 specColor;
    float specWeight;
    float roughness;
    float ior;  // 透過する材質では、v の側から見た相対屈折率（裏から当たったら 1/n）
    float coatWeight;
    vec4 coatColor;
    float coatRoughness;
    float coatIor;
    // コートの吸収を Beer–Lambert で求める場合（coatThickness > 0）。coatColor の代わりに使う
    vec4 coatSigma;        // 吸収係数（1/mm）
    float coatThickness;   // 厚み（mm）
    // 拡散の透過（subsurface の簡易版：基材の中へ拡散して入る割合と、中での吸収）。0 なら不透明
    float sssWeight;
    vec4 sssSigma;         // 中での吸収係数（1/mm）
    // 誘電体の透過（ガラス・アクリルなど）。屈折して中へ入る
    float transWeight;
    vec4 transSigma;       // 中での吸収係数（1/mm）。transmission_depth = 0 なら 0
    vec4 transTint;        // 面を通るたびにかける色（transmission_depth = 0 のとき √transmission_color）
    bool metallic;         // 中から当たったとき光を通さない（銅など）
    // ノイズ除去用のアルベド（RGB のまま持つ）
    vec3 baseRgb;
    vec3 coatRgb;
    vec3 transRgb;
};

Shade shadeFromMaterial(Material m) {
    Shade s;
    s.baseColor = specReflect(m.baseColor.rgb, m.specBase);
    s.baseWeight = m.baseColor.a;
    s.specColor = specReflect(m.specColor.rgb, m.specSpecular);
    s.specWeight = m.specColor.a;
    s.metalness = m.params0.x;
    s.roughness = m.params0.y;
    s.ior = m.params0.z;
    s.coatWeight = m.params0.w;
    s.coatColor = specReflect(m.coatColor.rgb, m.specCoat);
    s.coatRoughness = m.coatColor.a;
    s.coatIor = m.params1.x;
    s.coatSigma = vec4(0.0);
    s.coatThickness = 0.0;
    s.sssWeight = m.params3.x;
    vec4 sssColor = specReflect(m.subsurface.rgb, m.specSubsurface);
    s.sssSigma = -log(max(sssColor, vec4(1e-4))) / max(m.subsurface.a, 1e-3);
    s.transWeight = m.transmission.x;
    vec4 transColor = specReflect(m.transmission2.rgb, m.specTrans);
    float depth = m.transmission2.a;
    s.transSigma = depth > 0.0 ? -log(max(transColor, vec4(1e-4))) / depth : vec4(0.0);
    s.transTint = depth > 0.0 ? vec4(1.0) : sqrt(max(transColor, vec4(0.0)));
    s.metallic = m.params0.x > 0.5;
    s.baseRgb = m.baseColor.rgb;
    s.coatRgb = m.coatColor.rgb;
    s.transRgb = m.transmission2.rgb;
    return s;
}

// コートを通る光の透過率（入射と出射の 2 回、コートの中での屈折後の角度で光路長を求める）
vec4 coatTransmittance(Shade s, float cosV, float cosL) {
    if (s.coatThickness <= 0.0) return s.coatColor;
    float n2 = s.coatIor * s.coatIor;
    float cv = sqrt(max(1e-4, 1.0 - (1.0 - cosV * cosV) / n2));
    float cl = sqrt(max(1e-4, 1.0 - (1.0 - cosL * cosL) / n2));
    return exp(-s.coatSigma * s.coatThickness * (1.0 / cv + 1.0 / cl));
}

float fresnelDielectric(float cosi, float eta) {
    float c = abs(cosi);
    float g2 = eta * eta - 1.0 + c * c;
    if (g2 < 0.0) return 1.0;  // 全反射
    float g = sqrt(g2);
    float a = (g - c) / (g + c);
    float b = (c * (g + c) - 1.0) / (c * (g - c) + 1.0);
    return 0.5 * a * a * (1.0 + b * b);
}

vec4 fresnelSchlick(vec4 f0, float c) {
    float m = clamp(1.0 - c, 0.0, 1.0);
    float m2 = m * m;
    return f0 + (vec4(1.0) - f0) * (m2 * m2 * m);
}

// 粗さ 0 でも、ごく狭い GGX として扱う（α = 1e-4 で 0.006° ほどの広がり）
float ggxAlpha(float roughness) { return max(roughness * roughness, 1e-4); }

// D(h)。h は単位ベクトル。1 − cos²θ の桁落ちを避けるため、h の接線成分から直接求める
float ggxD(vec3 h, float a2) {
    float d = dot(h.xy, h.xy) + a2 * h.z * h.z;
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

// 可視法線分布で h を選ぶ確率密度（h の立体角あたり）
float pdfGgxVndf(vec3 v, vec3 h, float a2) { return ggxG1(v.z, a2) * max(dot(v, h), 0.0) * ggxD(h, a2) / v.z; }

// 反射方向 l の確率密度（VNDF で h を選んで反射した場合）
float pdfGgxReflect(vec3 v, vec3 l, float a) {
    vec3 h = normalize(v + l);
    float a2 = a * a;
    return ggxD(h, a2) * ggxG1(v.z, a2) / (4.0 * v.z);
}

struct LobeProbs {
    float coat;
    float spec;  // コートを選ばなかった場合に、ベースの鏡面（反射または透過）を選ぶ確率
};

LobeProbs lobeProbs(Shade s, vec3 v) {
    LobeProbs p;
    float fc = fresnelDielectric(v.z, s.coatIor);
    p.coat = s.coatWeight > 0.0 ? s.coatWeight * clamp(2.0 * fc, 0.15, 0.85) : 0.0;
    float fd = s.specWeight * fresnelDielectric(v.z, s.ior);
    float pSpecDielectric = s.baseWeight > 0.0 ? clamp(2.0 * fd, 0.1, 0.9) : 1.0;
    p.spec = mix(mix(pSpecDielectric, 1.0, s.transWeight), 1.0, s.metalness);
    return p;
}

// マイクロファセットの法線 h で、反射を選ぶ確率（残りは透過）。フレネルに比例させる
float reflectProb(Shade s, float VoH) {
    float t = s.transWeight * (1.0 - s.metalness);
    if (t <= 0.0) return 1.0;
    float F = s.specWeight * fresnelDielectric(VoH, s.ior);
    float T = t * (1.0 - F);
    return F + T > 0.0 ? F / (F + T) : 1.0;
}

// 透過の半ベクトル（+Z 側に向けたもの）。eta = 向こう側 / v の側の屈折率
vec3 transmitHalf(vec3 v, vec3 l, float eta) {
    vec3 h = normalize(l * eta + v);
    return h.z < 0.0 ? -h : h;
}

// f(v, l)（cos 項は含まない）。l.z < 0 は透過
vec4 bsdfEval(Shade s, vec3 v, vec3 l) {
    if (v.z <= 0.0 || l.z == 0.0) return vec4(0.0);
    float a = ggxAlpha(s.roughness);
    float a2 = a * a;
    float fcV = fresnelDielectric(v.z, s.coatIor);

    if (l.z < 0.0) {
        // ---- 誘電体の透過（Walter et al. 2007）
        float t = s.transWeight * (1.0 - s.metalness);
        if (t <= 0.0) return vec4(0.0);
        float eta = s.ior;
        vec3 h = transmitHalf(v, l, eta);
        float VoH = dot(v, h), LoH = dot(l, h);
        if (VoH <= 0.0 || LoH >= 0.0) return vec4(0.0);  // 裏を向いたマイクロファセット
        float F = s.specWeight * fresnelDielectric(VoH, eta);
        float denom = LoH + VoH / eta;
        float ft = ggxD(h, a2) * ggxG2(v.z, -l.z, a2) * abs(LoH * VoH / (l.z * v.z * denom * denom)) * (1.0 - F);
        ft /= eta * eta;  // 放射輝度は屈折率の 2 乗に比例して変わる
        vec4 coatAtten = mix(vec4(1.0), s.coatColor * (1.0 - fcV), s.coatWeight);
        return t * ft * s.transTint * coatAtten;
    }

    vec3 h = normalize(v + l);
    float VoH = max(dot(v, h), 0.0);
    vec4 result = vec4(0.0);

    // コート
    float fcL = fresnelDielectric(l.z, s.coatIor);
    if (s.coatWeight > 0.0) {
        float ac = ggxAlpha(s.coatRoughness);
        float ac2 = ac * ac;
        float fc = fresnelDielectric(VoH, s.coatIor);
        result += vec4(s.coatWeight * fc * ggxD(h, ac2) * ggxG2(v.z, l.z, ac2) / (4.0 * v.z * l.z));
    }
    vec4 coatAtten = mix(vec4(1.0), coatTransmittance(s, v.z, l.z) * (1.0 - fcV) * (1.0 - fcL), s.coatWeight);

    // ベースの鏡面（誘電体と金属で分布は共通、フレネルだけ異なる）
    float spec = ggxD(h, a2) * ggxG2(v.z, l.z, a2) / (4.0 * v.z * l.z);
    vec4 fDiel = vec4(s.specWeight * fresnelDielectric(VoH, s.ior)) * s.specColor;
    vec4 fMetal = fresnelSchlick(s.baseColor, VoH) * s.specWeight;
    vec4 base = mix(fDiel, fMetal, s.metalness) * spec;

    // 拡散（誘電体の、透過しない部分）
    float fdV = s.specWeight * fresnelDielectric(v.z, s.ior);
    float fdL = s.specWeight * fresnelDielectric(l.z, s.ior);
    base += (1.0 - s.metalness) * (1.0 - s.transWeight) * s.baseWeight * s.baseColor * INV_PI * (1.0 - fdV) * (1.0 - fdL);

    return result + coatAtten * base;
}

float bsdfPdf(Shade s, vec3 v, vec3 l) {
    if (v.z <= 0.0 || l.z == 0.0) return 0.0;
    LobeProbs p = lobeProbs(s, v);
    float rest = 1.0 - p.coat;
    float a = ggxAlpha(s.roughness);
    if (l.z < 0.0) {
        if (s.transWeight * (1.0 - s.metalness) <= 0.0) return 0.0;
        float eta = s.ior;
        vec3 h = transmitHalf(v, l, eta);
        float VoH = dot(v, h), LoH = dot(l, h);
        if (VoH <= 0.0 || LoH >= 0.0) return 0.0;
        float denom = LoH + VoH / eta;
        float dwh = abs(LoH) / (denom * denom);
        return rest * p.spec * pdfGgxVndf(v, h, a * a) * dwh * (1.0 - reflectProb(s, VoH));
    }
    float pdf = 0.0;
    if (p.coat > 0.0) pdf += p.coat * pdfGgxReflect(v, l, ggxAlpha(s.coatRoughness));
    vec3 h = normalize(v + l);
    pdf += rest * p.spec * pdfGgxReflect(v, l, a) * reflectProb(s, max(dot(v, h), 0.0));
    pdf += rest * (1.0 - p.spec) * l.z * INV_PI;
    return pdf;
}

// 方向をサンプリングして l を返す。失敗時は l = 0
vec3 bsdfSample(Shade s, vec3 v, inout Rng rng) {
    LobeProbs p = lobeProbs(s, v);
    float u = rnd(rng);
    vec2 u2 = rnd2(rng);
    if (u < p.coat) {
        vec3 h = sampleGgxVndf(v, ggxAlpha(s.coatRoughness), u2);
        vec3 l = reflect(-v, h);
        return l.z > 0.0 ? l : vec3(0.0);
    }
    u = (u - p.coat) / max(1.0 - p.coat, 1e-6);
    if (u < p.spec) {
        vec3 h = sampleGgxVndf(v, ggxAlpha(s.roughness), u2);
        float VoH = dot(v, h);
        if (u / p.spec < reflectProb(s, VoH)) {
            vec3 l = reflect(-v, h);
            return l.z > 0.0 ? l : vec3(0.0);
        }
        vec3 l = refract(-v, h, 1.0 / s.ior);
        return l.z < 0.0 ? normalize(l) : vec3(0.0);
    }
    return sampleCosineHemisphere(u2);
}

// ノイズ除去用のアルベド（RGB）
vec3 shadeAlbedoRgb(Shade s) {
    vec3 base = mix(s.baseRgb * s.baseWeight, s.baseRgb, s.metalness);
    base = mix(base, s.transRgb, s.transWeight * (1.0 - s.metalness));
    return mix(base, base * s.coatRgb, s.coatWeight);
}

#endif
