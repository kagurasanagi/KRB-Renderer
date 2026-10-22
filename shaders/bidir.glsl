// 双方向の積分器（BDPT / VCM）の共通部分。MIS の重みは Georgiev et al. 2012 "Light Transport Simulation with
// Vertex Connection and Merging" と、その実装の説明（SmallVCM）の再帰的な形（dVCM, dVC, dVM）で求める。
// BDPT は merging を使わない場合（vcmParams.w = 0）として同じコードで扱う。
// 1 回の繰り返し：vcm_light.comp で全画素ぶんの光源側の経路を作って頂点を保存し、カメラへ直接つなぐ（画面へ足し込む）。
// 次に vcm_camera.comp で、各画素のカメラ側の経路から、光源への直接サンプリング・光源側の頂点への接続・近傍のまとめを行う。
#ifndef KRBR_BIDIR_GLSL
#define KRBR_BIDIR_GLSL

#extension GL_EXT_shader_atomic_float : require

#define NO_VERTEX 0xFFFFFFFFu

// 光源側の頂点（96 バイト。Rust の gpu_types.rs の GpuLightVertex と同じ並びで、バッファの大きさはそれから求める）
struct LightVertex {
    vec4 posInst;     // xyz: 位置, w: インスタンス（ビット）
    vec4 pObjMat;     // xyz: オブジェクト座標, w: 材質（ビット）
    uvec4 dirs;       // x: 幾何法線, y: 凹凸の前のシェーディング法線（ともに光の来た側、8 面体で圧縮）, z: 前の頂点への方向, w: 材質を作り直す乱数の状態
    vec4 throughput;
    vec4 mis;         // x: dVCM, y: dVC, z: dVM, w: 続ける確率
    uvec4 links;      // x: 同じ経路の 1 つ前の頂点, y: 同じセルの次の頂点, z: 経路の長さ, w: 1 なら代表波長以外を打ち切った
};

layout(set = 0, binding = KRBR_BINDING_LIGHT_VERTICES, std430) buffer LightVertices { LightVertex lightVerts[]; };
layout(set = 0, binding = KRBR_BINDING_PATH_HEADS, std430) buffer PathHeads { uint pathHead[]; };  // 経路ごとの最後の頂点
layout(set = 0, binding = KRBR_BINDING_CELL_HEADS, std430) buffer CellHeads { uint cellHead[]; };  // 近傍探索の格子（セルごとの連結リスト）
// 並びは KRBR_VCM_COUNTER_*：保存しようとした頂点の数（容量を超えても数える）、容量、merging で走査を打ち切った回数
layout(set = 0, binding = KRBR_BINDING_COUNTERS, std430) buffer Counters { uint counters[]; };
// カメラへ直接つないだ寄与（画素ごとに KRBR_VCM_SPLAT_FLOATS_PER_PIXEL 個。表示の色の xyz）
layout(set = 0, binding = KRBR_BINDING_SPLAT, std430) buffer Splat { float splat[]; };

uint splatIndex(uvec2 px) { return (px.y * F.dims.x + px.x) * uint(KRBR_VCM_SPLAT_FLOATS_PER_PIXEL); }

// ---------------------------------------------------------------- 乱数の系列
// 光源側の経路は、同じ画素・同じサンプル番号のカメラ側の経路と別の系列にする（種に混ぜる値。√2 の小数部で、
// SHA-256 の初期値として知られる任意の定数）
#define RNG_STREAM_LIGHT_PATH 0x6a09e667u
// 波長を選ぶ系列（種に足す値）と、VCM でサンプル番号ごとに散らす掛け算の値（どちらも他の系列と重ならない任意の値）
#define RNG_STREAM_WAVELENGTH 17u
#define RNG_SAMPLE_SCATTER 9781u

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
    uint stream = pcgHash(pc.seed + RNG_STREAM_WAVELENGTH);
    uint h = useMerging() ? pcgHash(pc.sampleIndex * RNG_SAMPLE_SCATTER + stream)
                          : pcgHash(pathIdx + pcgHash(pc.sampleIndex + stream));
    sampleWavelengths(float(h >> 8) * (1.0 / 16777216.0));
}

// 代表波長以外を打ち切った経路の寄与：両側の打ち切りの OR で 1 回だけ heroOnly をかける（spectral.glsl の説明を参照）
vec4 maskTerminated(vec4 c, bool terminated) { return terminated ? heroOnly(c) : c; }

// ---------------------------------------------------------------- 材質の性質
// ほぼ鏡面の頂点（isDeltaLike、bsdf.glsl）は、接続・まとめを行わず、サンプリングだけで通り抜ける。

// 経路を続ける確率（ロシアンルーレット）。MIS の確率密度にも含めるので、どちら向きの経路から見ても同じ値になるよう、
// 頂点の材質（アルベド）だけで決める。PT のように経路の深さ（rr_start_depth）で変えると、光源側から数えた深さと
// カメラ側から数えた深さが違うので、戦略ごとに確率密度の見積もりが食い違い、MIS の重みの和が 1 にならない（SmallVCM と同じ設計）。
// 上乗せ分と下限は、暗い面でも続ける確率を少し高めにして、生き残った経路の重み（1 / 確率）が大きくなりすぎない（ホタルに
// ならない）ため。明るい面（アルベド 0.95 以上）では打ち切らず、経路は最大の長さか BSDF の吸収で終わる
#define BIDIR_RR_ALBEDO_BIAS 0.05
#define BIDIR_RR_MIN_PROB 0.1
float continuationProb(Shade s) {
    vec3 a = shadeAlbedoRgb(s);
    return clamp(max(a.r, max(a.g, a.b)) + BIDIR_RR_ALBEDO_BIAS, BIDIR_RR_MIN_PROB, 1.0);
}

// 光源側（重要度の輸送）でシェーディング法線を使うときの補正（Veach 1997, 5.3 節）の上限。幾何法線とシェーディング法線が
// 大きくずれた面すれすれの向きでは補正が発散して少数のサンプルがホタルになるので、そこで頭打ちにする（その分だけ暗い側へ偏る。
// 凹凸の傾きは小さいので、効くのは面すれすれの向きだけ）
#define SHADING_CORRECTION_MAX 10.0
float shadingCorrection(vec3 wo, vec3 wi, vec3 ns, vec3 ng) {
    float num = abs(dot(wo, ns)) * abs(dot(wi, ng));
    float den = abs(dot(wo, ng)) * abs(dot(wi, ns));
    return den > 1e-8 ? min(num / den, SHADING_CORRECTION_MAX) : 0.0;
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
        // ほぼ鏡面の頂点：この頂点での接続の戦略は無いものとし（dVCM = 0）、逆向きの確率密度は順向きと同じとみなす
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

// ---------------------------------------------------------------- 近傍探索の格子
uint cellHash(ivec3 c) {
    return (uint(c.x) * 73856093u ^ uint(c.y) * 19349663u ^ uint(c.z) * 83492791u) % KRBR_VCM_GRID_SIZE;
}
ivec3 cellOf(vec3 p, float cellSize) { return ivec3(floor(p / cellSize)); }

// ---------------------------------------------------------------- 寄与を表示の色（RGB か XYZ）へ
vec3 toOutputColor(vec4 c) { return spectralMode() ? spectrumToXyz(c) : c.rgb; }

#endif
