// 表面の微細な凹凸用のノイズ
#ifndef KRBR_NOISE_GLSL
#define KRBR_NOISE_GLSL

// 整数格子点から [-1,1]^3 の勾配ベクトル
vec3 hashGrad(ivec3 p) {
    uint h = pcgHash(uint(p.x) * 73856093u ^ pcgHash(uint(p.y) * 19349663u ^ pcgHash(uint(p.z) * 83492791u)));
    uint h2 = pcgHash(h), h3 = pcgHash(h2);
    return vec3(float(h & 0xFFFFu), float(h2 & 0xFFFFu), float(h3 & 0xFFFFu)) * (2.0 / 65535.0) - 1.0;
}

// 勾配ノイズと解析的な微分（Inigo Quilez の noised を整数ハッシュで）。x: 値, yzw: 勾配
vec4 noised(vec3 x) {
    ivec3 i = ivec3(floor(x));
    vec3 f = fract(x);
    vec3 u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    vec3 du = 30.0 * f * f * (f * (f - 2.0) + 1.0);

    vec3 ga = hashGrad(i + ivec3(0, 0, 0));
    vec3 gb = hashGrad(i + ivec3(1, 0, 0));
    vec3 gc = hashGrad(i + ivec3(0, 1, 0));
    vec3 gd = hashGrad(i + ivec3(1, 1, 0));
    vec3 ge = hashGrad(i + ivec3(0, 0, 1));
    vec3 gf = hashGrad(i + ivec3(1, 0, 1));
    vec3 gg = hashGrad(i + ivec3(0, 1, 1));
    vec3 gh = hashGrad(i + ivec3(1, 1, 1));

    float va = dot(ga, f - vec3(0, 0, 0));
    float vb = dot(gb, f - vec3(1, 0, 0));
    float vc = dot(gc, f - vec3(0, 1, 0));
    float vd = dot(gd, f - vec3(1, 1, 0));
    float ve = dot(ge, f - vec3(0, 0, 1));
    float vf = dot(gf, f - vec3(1, 0, 1));
    float vg = dot(gg, f - vec3(0, 1, 1));
    float vh = dot(gh, f - vec3(1, 1, 1));

    float v = va + u.x * (vb - va) + u.y * (vc - va) + u.z * (ve - va) + u.x * u.y * (va - vb - vc + vd) +
              u.y * u.z * (va - vc - ve + vg) + u.z * u.x * (va - vb - ve + vf) +
              (-va + vb + vc - vd + ve - vf - vg + vh) * u.x * u.y * u.z;
    vec3 d = ga + u.x * (gb - ga) + u.y * (gc - ga) + u.z * (ge - ga) + u.x * u.y * (ga - gb - gc + gd) +
             u.y * u.z * (ga - gc - ge + gg) + u.z * u.x * (ga - gb - ge + gf) +
             (-ga + gb + gc - gd + ge - gf - gg + gh) * u.x * u.y * u.z +
             du * (vec3(vb, vc, ve) - va + u.yzx * vec3(va - vb - vc + vd, va - vc - ve + vg, va - vb - ve + vf) +
                   u.zxy * vec3(va - vb - ve + vf, va - vb - vc + vd, va - vc - ve + vg) +
                   u.yzx * u.zxy * (-va + vb + vc - vd + ve - vf - vg + vh));
    return vec4(v, d);
}

// fBm（3 オクターブ）。p は「凹凸 1 個」を 1 とする単位。戻り値の勾配も同じ単位
vec4 fbmd(vec3 p) {
    vec4 sum = vec4(0.0);
    float amp = 0.6, freq = 1.0;
    for (int o = 0; o < 3; ++o) {
        vec4 n = noised(p * freq + vec3(float(o) * 17.3));
        sum += amp * vec4(n.x, n.yzw * freq);
        amp *= 0.5;
        freq *= 2.03;
    }
    return sum;
}

// ガラス繊維の平織り。q は糸のピッチを 1 とする単位（x: たて糸の向き, y: よこ糸の向き）
float weaveHeight(vec2 q) {
    vec2 c = floor(q);
    vec2 f = fract(q) - 0.5;
    bool warpTop = mod(c.x + c.y, 2.0) < 1.0;
    // 糸の断面は丸く、交差するところで上下が入れ替わる
    float warp = cos(f.y * PI) * (warpTop ? 1.0 : 0.55 + 0.45 * abs(2.0 * f.x));
    float weft = cos(f.x * PI) * (warpTop ? 0.55 + 0.45 * abs(2.0 * f.y) : 1.0);
    // なめらかな max（樹脂に埋まった糸の境目は角張らない）
    const float k = 0.25;
    float hm = clamp(0.5 + 0.5 * (warp - weft) / k, 0.0, 1.0);
    return mix(weft, warp, hm) + k * hm * (1.0 - hm);
}

// 織りの高さの勾配（有限差分）と、糸と樹脂の境目を表す値（色のむら用）
vec3 weaveGrad(vec2 q, out float h) {
    const float e = 0.02;
    h = weaveHeight(q);
    float hx = weaveHeight(q + vec2(e, 0.0));
    float hy = weaveHeight(q + vec2(0.0, e));
    return vec3((hx - h) / e, (hy - h) / e, 0.0);
}

#endif
