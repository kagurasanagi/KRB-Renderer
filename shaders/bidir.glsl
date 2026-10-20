// 双方向の積分器（BDPT / VCM）の共通部分。MIS の重みは Georgiev et al. 2012 "Light Transport Simulation with
// Vertex Connection and Merging" と、その実装の説明（SmallVCM）の再帰的な形（dVCM, dVC, dVM）で求める。
// BDPT は merging を使わない場合（vcmParams.w = 0）として同じコードで扱う。
// 1 回の繰り返し：vcm_light.comp で全画素ぶんの光源側の経路を作って頂点を保存し、カメラへ直接つなぐ（画面へ足し込む）。
// 次に vcm_camera.comp で、各画素のカメラ側の経路から、光源への直接サンプリング・光源側の頂点への接続・近傍のまとめを行う。
#ifndef KRBR_BIDIR_GLSL
#define KRBR_BIDIR_GLSL

#extension GL_EXT_shader_atomic_float : require

#define NO_VERTEX 0xFFFFFFFFu
#define GRID_SIZE (1u << 21)

// 光源側の頂点（96 バイト）
struct LightVertex {
    vec4 posInst;     // xyz: 位置, w: インスタンス（ビット）
    vec4 pObjMat;     // xyz: オブジェクト座標, w: 材質（ビット）
    uvec4 dirs;       // x: 幾何法線, y: 凹凸の前のシェーディング法線（ともに光の来た側、8 面体で圧縮）, z: 前の頂点への方向, w: 材質を作り直す乱数の状態
    vec4 throughput;
    vec4 mis;         // x: dVCM, y: dVC, z: dVM, w: 続ける確率
    uvec4 links;      // x: 同じ経路の 1 つ前の頂点, y: 同じセルの次の頂点, z: 経路の長さ, w: 1 なら代表波長以外を打ち切った
};

layout(set = 0, binding = 20, std430) buffer LightVertices { LightVertex lightVerts[]; };
layout(set = 0, binding = 21, std430) buffer PathHeads { uint pathHead[]; };  // 経路ごとの最後の頂点
layout(set = 0, binding = 22, std430) buffer CellHeads { uint cellHead[]; };  // 近傍探索の格子（セルごとの連結リスト）
layout(set = 0, binding = 23, std430) buffer Counters { uint counters[]; };   // x: 保存した頂点の数, y: 容量
layout(set = 0, binding = 24, std430) buffer Splat { float splat[]; };        // カメラへ直接つないだ寄与（画素ごとに 4 つ）

// ---------------------------------------------------------------- 8 面体による単位ベクトルの圧縮
vec2 octWrap(vec2 v) { return (1.0 - abs(v.yx)) * vec2(v.x >= 0.0 ? 1.0 : -1.0, v.y >= 0.0 ? 1.0 : -1.0); }
uint octEncode(vec3 n) {
    n /= abs(n.x) + abs(n.y) + abs(n.z);
    vec2 e = n.z >= 0.0 ? n.xy : octWrap(n.xy);
    return packSnorm2x16(e);
}
vec3 octDecode(uint u) {
    vec2 e = unpackSnorm2x16(u);
    vec3 n = vec3(e, 1.0 - abs(e.x) - abs(e.y));
    if (n.z < 0.0) n.xy = octWrap(n.xy);
    return normalize(n);
}

// ---------------------------------------------------------------- 定数
float Mis(float x) { return x; }  // balance heuristic

bool useMerging() { return F.vcmParams.w > 0.5; }
float lightPathCount() { return F.vcmParams.z; }
uint maxPathLength() { return F.counts.x + 1u; }

struct VcmConst {
    float radius, radius2;
    float vmWeight;  // Mis(η_VCM)：merging がなければ 0
    float vcWeight;  // Mis(1/η_VCM)
    float vmNorm;    // 1 / (π r² N)
};

VcmConst vcmConst() {
    VcmConst c;
    // 半径は繰り返すたびに縮める（r_i = r_0 · i^((α−1)/2)）
    c.radius = max(F.vcmParams.x * pow(float(pc.sampleIndex + 1u), 0.5 * (F.vcmParams.y - 1.0)), 1e-5);
    c.radius2 = c.radius * c.radius;
    float eta = PI * c.radius2 * lightPathCount();
    c.vmWeight = useMerging() ? Mis(eta) : 0.0;
    c.vcWeight = Mis(1.0 / eta);
    c.vmNorm = 1.0 / eta;
    return c;
}

// 波長：BDPT は経路ごと（光源側とカメラ側で同じ番号の経路は同じ波長）。VCM は別の経路の頂点とまとめるので、繰り返しごとに全画素で共通
void bidirWavelengths(uint pathIdx) {
    if (!spectralMode()) return;
    uint h = useMerging() ? pcgHash(pc.sampleIndex * 9781u + pcgHash(pc.seed + 17u))
                          : pcgHash(pathIdx + pcgHash(pc.sampleIndex + pcgHash(pc.seed + 17u)));
    sampleWavelengths(float(h >> 8) * (1.0 / 16777216.0));
}

// 代表波長以外を打ち切った経路の寄与：代表波長だけを 4 倍に
vec4 maskTerminated(vec4 c, bool terminated) { return terminated ? vec4(4.0 * c.x, 0.0, 0.0, 0.0) : c; }

// ---------------------------------------------------------------- 材質の性質
// ほぼ鏡面の頂点（isDeltaLike、bsdf.glsl）は、接続・まとめを行わず、サンプリングだけで通り抜ける
// 経路を続ける確率（方向によらない値にする。MIS の確率密度にも含める）
float continuationProb(Shade s) {
    vec3 a = shadeAlbedoRgb(s);
    return clamp(max(a.r, max(a.g, a.b)) + 0.05, 0.1, 1.0);
}

// 光源側（重要度の輸送）でシェーディング法線を使うときの補正（Veach 1997, 5.3 節）
float shadingCorrection(vec3 wo, vec3 wi, vec3 ns, vec3 ng) {
    float num = abs(dot(wo, ns)) * abs(dot(wi, ng));
    float den = abs(dot(wo, ng)) * abs(dot(wi, ns));
    return den > 1e-8 ? min(num / den, 10.0) : 0.0;
}

// ---------------------------------------------------------------- 経路の状態
struct SubPath {
    vec3 origin;
    vec3 dir;
    vec4 throughput;
    uint pathLength;  // 始点（光源 / カメラ）から今の頂点までの線分の数
    float dVCM, dVC, dVM;
    bool terminated;
    bool finiteLight;  // 光源側：有限の光源（発光面）から出た
    vec4 mediumSigma;  // 透明な物体の中での吸収係数
};

// 交点に着いたときの MIS の量の更新（距離と、来た方向の余弦）
void misOnHit(inout SubPath sp, float dist, float cosFix, bool lightPath) {
    if (!lightPath || sp.pathLength > 1u || sp.finiteLight) sp.dVCM *= Mis(dist * dist);
    float c = Mis(max(abs(cosFix), 1e-6));
    sp.dVCM /= c;
    sp.dVC /= c;
    sp.dVM /= c;
}

// BSDF で次の方向を選び、経路の重みと MIS の量を更新する。続けられなければ false
bool scatterSubPath(inout SubPath sp, Shade s, vec3 hitPos, vec3 ng, vec3 ns, vec3 wo, bool frontFace, bool delta,
                    float contProb, bool lightPath, VcmConst vc, inout Rng rng) {
    vec3 t, b;
    basis(ns, t, b);
    vec3 v = toLocal(wo, t, b, ns);
    if (v.z <= 1e-6) return false;
    vec3 l = bsdfSample(s, v, rng);
    if (l.z == 0.0) return false;
    float pdf = bsdfPdf(s, v, l);
    if (pdf <= 0.0) return false;
    vec4 f = bsdfEval(s, v, l);
    vec3 wi = toWorld(l, t, b, ns);
    bool transmit = l.z < 0.0;
    if (transmit ? dot(wi, ng) >= 0.0 : dot(wi, ng) <= 0.0) return false;
    float revPdf = delta ? pdf : bsdfPdf(s, l, v);
    if (rnd(rng) > contProb) return false;
    pdf *= contProb;
    revPdf *= contProb;
    float cosOut = abs(l.z);
    vec4 factor = f * (cosOut / pdf);
    if (lightPath) {
        if (transmit) factor *= s.ior * s.ior;  // 重要度は屈折で放射輝度のように変わらない
        factor *= shadingCorrection(wo, wi, ns, ng);
    }
    if (delta) {
        sp.dVCM = 0.0;
        sp.dVC *= Mis(cosOut);
        sp.dVM *= Mis(cosOut);
    } else {
        sp.dVC = Mis(cosOut / pdf) * (sp.dVC * Mis(revPdf) + sp.dVCM + vc.vmWeight);
        sp.dVM = Mis(cosOut / pdf) * (sp.dVM * Mis(revPdf) + sp.dVCM * vc.vcWeight + 1.0);
        sp.dVCM = Mis(1.0 / pdf);
    }
    sp.throughput *= factor;
    if (transmit) {
        sp.mediumSigma = frontFace ? s.transSigma : vec4(0.0);
        sp.origin = offsetRay(hitPos, -ng);
    } else {
        sp.origin = offsetRay(hitPos, ng);
    }
    sp.dir = wi;
    return true;
}

// ---------------------------------------------------------------- カメラ（ピンホール）
float tanHalfFovY() { return length(F.camUp.xyz); }
float imagePlaneDist() { return float(F.dims.y) / (2.0 * tanHalfFovY()); }  // 画素を単位とした像面までの距離

// ワールドの方向 d（カメラから外向き）を画素の位置へ。画面外なら false
bool projectToRaster(vec3 d, out vec2 raster, out float cosAtCamera) {
    cosAtCamera = dot(F.camForward.xyz, d);
    if (cosAtCamera <= 1e-6) return false;
    vec3 r = F.camRight.xyz, u = F.camUp.xyz;
    float ndcX = dot(d, r) / dot(r, r) / cosAtCamera;
    float ndcY = -dot(d, u) / dot(u, u) / cosAtCamera;
    if (abs(ndcX) >= 1.0 || abs(ndcY) >= 1.0) return false;
    raster = (vec2(ndcX, ndcY) + 1.0) * 0.5 * vec2(F.dims.xy);
    return true;
}

// ---------------------------------------------------------------- 近傍探索の格子
uint cellHash(ivec3 c) {
    return (uint(c.x) * 73856093u ^ uint(c.y) * 19349663u ^ uint(c.z) * 83492791u) % GRID_SIZE;
}
ivec3 cellOf(vec3 p, float cellSize) { return ivec3(floor(p / cellSize)); }

// ---------------------------------------------------------------- 寄与を表示の色（RGB か XYZ）へ
vec3 toOutputColor(vec4 c) { return spectralMode() ? spectrumToXyz(c) : c.rgb; }

#endif
