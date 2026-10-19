// 正距円筒図法の環境マップ。輝度 × sinθ に比例した重点的サンプリング
#ifndef KRBR_ENV_GLSL
#define KRBR_ENV_GLSL

// 環境マップ座標の方向 d <-> (u, v)。θ = vπ（+Y が天頂）、φ = (u - 0.5)2π
vec2 dirToUv(vec3 d) {
    float u = 0.5 + atan(d.x, -d.z) * (0.5 * INV_PI);
    float v = acos(clamp(d.y, -1.0, 1.0)) * INV_PI;
    return vec2(u, v);
}

vec3 uvToDir(vec2 uv) {
    float theta = uv.y * PI;
    float phi = (uv.x - 0.5) * 2.0 * PI;
    float st = sin(theta);
    return vec3(st * sin(phi), cos(theta), -st * cos(phi));
}

vec3 rotateY(vec3 d, float a) {
    float c = cos(a), s = sin(a);
    return vec3(c * d.x + s * d.z, d.y, -s * d.x + c * d.z);
}

vec3 envRadiance(vec3 worldDir) {
    vec3 d = rotateY(worldDir, -F.envParams.y);
    return textureLod(envTex, dirToUv(d), 0.0).rgb * F.envParams.x;
}

// 単位立体角あたりの確率密度
float envPdfDir(vec3 worldDir) {
    vec3 d = rotateY(worldDir, -F.envParams.y);
    vec2 uv = dirToUv(d);
    uint w = F.dims.z, h = F.dims.w;
    uint i = min(uint(uv.x * float(w)), w - 1u);
    uint j = min(uint(uv.y * float(h)), h - 1u);
    float sinTheta = sin(uv.y * PI);
    if (sinTheta <= 0.0) return 0.0;
    return envPdf[j * w + i] / (2.0 * PI * PI * sinTheta);
}

// cdf[offset .. offset+n]（n+1 個、先頭 0・末尾 1）から区間を二分探索
uint findInterval(uint offset, uint n, float u, bool marginal) {
    uint lo = 0u, hi = n;
    while (lo + 1u < hi) {
        uint mid = (lo + hi) >> 1;
        float c = marginal ? envMarginalCdf[offset + mid] : envConditionalCdf[offset + mid];
        if (c <= u) lo = mid; else hi = mid;
    }
    return lo;
}

// 方向と pdf（立体角）を返す。pdf=0 なら失敗
vec3 envSample(vec2 u, out float pdf) {
    uint w = F.dims.z, h = F.dims.w;
    uint j = findInterval(0u, h, u.y, true);
    float c0 = envMarginalCdf[j], c1 = envMarginalCdf[j + 1u];
    float dv = (c1 > c0) ? (u.y - c0) / (c1 - c0) : 0.5;
    uint off = j * (w + 1u);
    uint i = findInterval(off, w, u.x, false);
    float d0 = envConditionalCdf[off + i], d1 = envConditionalCdf[off + i + 1u];
    float du = (d1 > d0) ? (u.x - d0) / (d1 - d0) : 0.5;
    vec2 uv = vec2((float(i) + du) / float(w), (float(j) + dv) / float(h));
    float sinTheta = sin(uv.y * PI);
    if (sinTheta <= 0.0) {
        pdf = 0.0;
        return vec3(0, 1, 0);
    }
    pdf = envPdf[j * w + i] / (2.0 * PI * PI * sinTheta);
    return rotateY(uvToDir(uv), F.envParams.y);
}

#endif
