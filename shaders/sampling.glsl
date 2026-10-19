#ifndef KRBR_SAMPLING_GLSL
#define KRBR_SAMPLING_GLSL

// ---------------------------------------------------------------- 乱数（PCG）
uint pcgHash(uint v) {
    uint state = v * 747796405u + 2891336453u;
    uint word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
    return (word >> 22u) ^ word;
}

struct Rng {
    uint state;
};

Rng rngInit(uvec2 pixel, uint sampleIndex, uint seed) {
    Rng r;
    r.state = pcgHash(pixel.x + pcgHash(pixel.y + pcgHash(sampleIndex + pcgHash(seed))));
    return r;
}

float rnd(inout Rng r) {
    r.state = r.state * 747796405u + 2891336453u;
    uint word = ((r.state >> ((r.state >> 28u) + 4u)) ^ r.state) * 277803737u;
    word = (word >> 22u) ^ word;
    return float(word >> 8) * (1.0 / 16777216.0);
}

vec2 rnd2(inout Rng r) { return vec2(rnd(r), rnd(r)); }

// ---------------------------------------------------------------- 座標系
void basis(vec3 n, out vec3 t, out vec3 b) {
    // Duff et al. 2017
    float s = n.z >= 0.0 ? 1.0 : -1.0;
    float a = -1.0 / (s + n.z);
    float c = n.x * n.y * a;
    t = vec3(1.0 + s * n.x * n.x * a, s * c, -s * n.x);
    b = vec3(c, s + n.y * n.y * a, -n.y);
}

vec3 toLocal(vec3 v, vec3 t, vec3 b, vec3 n) { return vec3(dot(v, t), dot(v, b), dot(v, n)); }
vec3 toWorld(vec3 v, vec3 t, vec3 b, vec3 n) { return v.x * t + v.y * b + v.z * n; }

vec3 sampleCosineHemisphere(vec2 u) {
    float r = sqrt(u.x);
    float phi = 2.0 * PI * u.y;
    return vec3(r * cos(phi), r * sin(phi), sqrt(max(0.0, 1.0 - u.x)));
}

float powerHeuristic(float a, float b) {
    float a2 = a * a, b2 = b * b;
    return a2 / max(a2 + b2, 1e-30);
}

// ---------------------------------------------------------------- 自己交差を避ける光線の始点（Wächter & Binder, Ray Tracing Gems 6章）
vec3 offsetRay(vec3 p, vec3 n) {
    const float origin = 1.0 / 32.0;
    const float floatScale = 1.0 / 65536.0;
    const float intScale = 256.0;
    ivec3 ofi = ivec3(intScale * n);
    vec3 pi = vec3(intBitsToFloat(floatBitsToInt(p.x) + ((p.x < 0.0) ? -ofi.x : ofi.x)),
                   intBitsToFloat(floatBitsToInt(p.y) + ((p.y < 0.0) ? -ofi.y : ofi.y)),
                   intBitsToFloat(floatBitsToInt(p.z) + ((p.z < 0.0) ? -ofi.z : ofi.z)));
    return vec3(abs(p.x) < origin ? p.x + floatScale * n.x : pi.x,
                abs(p.y) < origin ? p.y + floatScale * n.y : pi.y,
                abs(p.z) < origin ? p.z + floatScale * n.z : pi.z);
}

#endif
