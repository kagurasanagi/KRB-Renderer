// 交点と材質：光線との交差、表面の微細な凹凸、基板面・レジストの材質の組み立て（全積分器で共通）
#ifndef KRBR_SCENE_GLSL
#define KRBR_SCENE_GLSL

struct Hit {
    vec3 pos;
    float t;    // 光線の始点からの距離
    vec3 pObj;  // オブジェクト座標（表面の模様をインスタンスに貼り付けるため）
    vec3 ng;    // 幾何法線（ワールド、正規化済み）
    vec3 ns;    // シェーディング法線
    mat3 w2o;   // ワールド -> オブジェクトの 3x3（勾配・法線の変換用）
    uint instance;
    uint material;
};

// 交差判定の生の結果（ray query と ray tracing pipeline で共通の形）
struct RawHit {
    mat4x3 o2w, w2o;
    vec2 bc;
    float t;
    uint instCustom, instId, prim;
};

// 光線が当たるインスタンスの種類（加速構造のインスタンスのマスクと AND する。隠した部品の種類のビットを落としてある。
// rust/crates/krbr-scene/src/scene.rs の visibility）
uint rayCullMask() { return F.counts.z & 0xFFu; }

#ifdef KRBR_RT_PIPELINE
// ray tracing pipeline：traceRayEXT と最近接ヒット・ミスのシェーダ（rt_hit.rchit / rt_miss.rmiss / rt_shadow.rmiss）
#include "rt_payload.glsl"
layout(location = 0) rayPayloadEXT RtHit rtPayload;
layout(location = 1) rayPayloadEXT uint rtShadowPayload;

bool rawTraceClosest(vec3 origin, vec3 dir, out RawHit r) {
    traceRayEXT(tlas, gl_RayFlagsOpaqueEXT, rayCullMask(), 0u, 0u, 0u, origin, 0.0, dir, 1e30, 0);
    if (rtPayload.hit == 0u) return false;
    r.o2w = rtPayload.o2w;
    r.w2o = rtPayload.w2o;
    r.bc = rtPayload.bc;
    r.t = rtPayload.t;
    r.instCustom = rtPayload.instCustom;
    r.instId = rtPayload.instId;
    r.prim = rtPayload.prim;
    return true;
}

bool rawOccluded(vec3 origin, vec3 dir, float tmax) {
    rtShadowPayload = 1u;  // 影のミスシェーダが 0 にする
    traceRayEXT(tlas, gl_RayFlagsOpaqueEXT | gl_RayFlagsTerminateOnFirstHitEXT | gl_RayFlagsSkipClosestHitShaderEXT,
                rayCullMask(), 0u, 0u, 1u, origin, 0.0, dir, tmax, 1);
    return rtShadowPayload != 0u;
}
#else
// ray query：計算シェーダの中で辿る
bool rawTraceClosest(vec3 origin, vec3 dir, out RawHit r) {
    rayQueryEXT rq;
    rayQueryInitializeEXT(rq, tlas, gl_RayFlagsOpaqueEXT, rayCullMask(), origin, 0.0, dir, 1e30);
    while (rayQueryProceedEXT(rq)) {
    }
    if (rayQueryGetIntersectionTypeEXT(rq, true) == gl_RayQueryCommittedIntersectionNoneEXT) return false;
    r.o2w = rayQueryGetIntersectionObjectToWorldEXT(rq, true);
    r.w2o = rayQueryGetIntersectionWorldToObjectEXT(rq, true);
    r.bc = rayQueryGetIntersectionBarycentricsEXT(rq, true);
    r.t = rayQueryGetIntersectionTEXT(rq, true);
    r.instCustom = uint(rayQueryGetIntersectionInstanceCustomIndexEXT(rq, true));
    r.instId = uint(rayQueryGetIntersectionInstanceIdEXT(rq, true));
    r.prim = uint(rayQueryGetIntersectionPrimitiveIndexEXT(rq, true));
    return true;
}

bool rawOccluded(vec3 origin, vec3 dir, float tmax) {
    rayQueryEXT rq;
    rayQueryInitializeEXT(rq, tlas, gl_RayFlagsOpaqueEXT | gl_RayFlagsTerminateOnFirstHitEXT, rayCullMask(), origin, 0.0, dir,
                          tmax);
    while (rayQueryProceedEXT(rq)) {
    }
    return rayQueryGetIntersectionTypeEXT(rq, true) != gl_RayQueryCommittedIntersectionNoneEXT;
}
#endif

bool traceClosest(vec3 origin, vec3 dir, out Hit hit) {
    RawHit r;
    if (!rawTraceClosest(origin, dir, r)) return false;

    InstanceInfo ii = instanceInfos[r.instCustom];
    uint meshId = ii.mesh;
    uint prim = r.prim;
    vec2 bc = r.bc;
    mat4x3 o2w = r.o2w;
    mat4x3 w2o = r.w2o;

    MeshInfo mi = meshes[meshId];
    uint i0 = indices[mi.indexOffset + prim * 3u + 0u] + mi.vertexOffset;
    uint i1 = indices[mi.indexOffset + prim * 3u + 1u] + mi.vertexOffset;
    uint i2 = indices[mi.indexOffset + prim * 3u + 2u] + mi.vertexOffset;
    Vertex v0 = vertices[i0], v1 = vertices[i1], v2 = vertices[i2];
    float w0 = 1.0 - bc.x - bc.y;
    vec3 pObj = w0 * v0.pos + bc.x * v1.pos + bc.y * v2.pos;
    vec3 nObj = w0 * v0.nrm + bc.x * v1.nrm + bc.y * v2.nrm;
    vec3 ngObj = cross(v1.pos - v0.pos, v2.pos - v0.pos);

    hit.w2o = mat3(w2o);
    hit.t = r.t;
    hit.pObj = pObj;
    hit.pos = o2w * vec4(pObj, 1.0);
    hit.ng = normalize(ngObj * hit.w2o);  // 法線は逆転置で変換
    vec3 ns = nObj * hit.w2o;
    hit.ns = dot(ns, ns) > 1e-20 ? normalize(ns) : hit.ng;
    hit.instance = r.instId;
    hit.material = ii.materialBase + triMats[mi.triOffset + prim];
    return true;
}

// 距離 tmax までにさえぎるものがあるか（発光面への直接サンプリング用）
bool occludedDist(vec3 origin, vec3 dir, float tmax) { return rawOccluded(origin, dir, tmax); }

// 保存しておいた値から交点を作り直す（光源側の頂点で材質を評価し直すため）
Hit rebuildHit(vec3 pos, vec3 pObj, vec3 ng, vec3 ns, uint instance, uint material) {
    Hit h;
    h.pos = pos;
    h.t = 0.0;
    h.pObj = pObj;
    h.ng = ng;
    h.ns = ns;
    InstanceInfo ii = instanceInfos[instance];
    h.w2o = mat3(ii.w2o0.xyz, ii.w2o1.xyz, ii.w2o2.xyz);
    h.instance = instance;
    h.material = material;
    return h;
}

bool occluded(vec3 origin, vec3 dir) { return rawOccluded(origin, dir, 1e30); }

// ---------------------------------------------------------------- 表面の微細な凹凸

struct Detail {
    vec3 grad;       // 高さの勾配（オブジェクト座標、傾きの単位）
    float roughMul;  // 粗さへの倍率
    float colorMul;  // 色への倍率
};

// 画素 1 つが覆う広さ（mm）：カメラからの距離 × 画素 1 つ分の角度（1 / 像面までの距離）。模様を薄める目安に使う
float pixelFootprint(vec3 p) {
    return length(p - F.camPos.xyz) / max(imagePlaneDist(), 1e-6);
}

// footprint：画素が覆う広さ（mm）。模様の周期が画素と同じくらいまで細かくなると、平均されずに砂粒のようなノイズに
// 見えるので、そこへ近づくほど模様を弱める（周期が十分に大きい寄りの画では元のまま）
void addDetail(Material m, vec3 pObj, vec3 nObj, vec3 seed, float weight, float footprint, inout Detail d) {
    uint pattern = uint(m.params2.x);
    float scale = max(m.params2.z, 1e-4);
    float px = scale / max(footprint, 1e-6);  // 模様の周期が何画素か
    // 凹凸は細かい倍音（fbm）を含むので早めに、粗さのむら（周期 scale × 2.7 の 1 段）は画素の数倍まで残す
    float strength = m.params2.y * weight * smoothstep(3.0, 12.0, px);
    float rv = m.params2.w * weight * smoothstep(3.0, 8.0, px * 2.7);
    if (pattern == BUMP_NOISE && strength != 0.0) {
        // 強さが 0（遠景で模様が画素より細かい、凹凸の強さ 0）なら、fbm（pcgHash 約 120 回）を計算しない。
        // 足すのは 0 なので結果は同じ（d.grad は +0 から始まり、±0 を足しても変わらない）
        vec4 n = fbmd(pObj / scale + seed);
        d.grad += strength * n.yzw;
    } else if (pattern == BUMP_WEAVE) {
        float h;
        if (abs(nObj.y) > 0.5) {
            // 基板の面（XZ）に沿った織り目。糸と樹脂で色もわずかに変わる
            vec3 g = weaveGrad(pObj.xz / scale, h);
            d.grad += strength * vec3(g.x, 0.0, g.y);
        } else {
            // 側面・穴の内壁：ガラス布が水平に重なった層（約 0.1 mm ごと）
            vec4 n = noised(vec3((pObj.x + pObj.z) / scale, pObj.y / 0.05, 0.5) + seed);
            h = 0.5 + 0.5 * sin(pObj.y / 0.05 * PI + 1.5 * n.x);
            d.grad += strength * 0.5 * vec3(0.0, cos(pObj.y / 0.05 * PI) * PI / 0.05 * 0.02, 0.0);
        }
        d.colorMul *= mix(1.0, mix(0.86, 1.06, h), weight);
    }
    if (rv > 0.0) {
        float r = noised(pObj / (scale * 2.7) + seed + vec3(31.7, 7.1, 3.3)).x;
        d.roughMul *= clamp(1.0 + 2.0 * rv * r, 0.3, 2.0);
    }
}

vec3 instanceSeed(uint instance) {
    uint h = pcgHash(instance + 1u);
    return vec3(float(h & 1023u), float((h >> 10) & 1023u), float((h >> 20) & 1023u)) * 0.37;
}

// 下の材質のベースの層（拡散・鏡面・誘電体の透過のローブ）を、レジストの顔料の層 p に置き換える。
// コートは applyMask が設定し、拡散の透過（基材の中へ入る光）は下の材質のものをそのまま使う
void replaceBaseLayer(inout Shade s, Shade p) {
    s.baseColor = p.baseColor;
    s.baseRgb = p.baseRgb;
    s.baseWeight = 1.0;  // 顔料の散乱は確率 scatterProb で選んだので、選んだ後は重み 1 で散乱する
    s.metalness = p.metalness;
    s.specColor = p.specColor;
    s.specWeight = p.specWeight;
    s.roughness = p.roughness;
    s.ior = p.ior;
    s.transWeight = p.transWeight;
    s.transSigma = p.transSigma;
    s.transTint = p.transTint;
    s.transRgb = p.transRgb;
    s.conductor = p.conductor;
    s.condEta = p.condEta;
    s.condK = p.condK;
}

// レジストを上にかぶせる（下の材質 s を、レジストのコートで覆う）。thickness > 0 なら厚みに応じた吸収と散乱。
// 変えるのはベースの層（顔料の散乱を選んだとき）とコートだけで、拡散の透過は下の材質のまま
Shade applyMask(Shade s, Material mask, float thickness, inout Rng rng) {
    // レジストの顔料自身による散乱。厚いほど起きやすい（基準の厚みで mask の base_weight になる）
    float scatterProb = mask.baseColor.a;
    float tRef = F.maskParams.x;
    if (thickness > 0.0 && tRef > 0.0) {
        float k = -log(max(1.0 - mask.baseColor.a, 1e-3)) / tRef;
        scatterProb = 1.0 - exp(-k * thickness);
    }
    if (rnd(rng) < scatterProb) replaceBaseLayer(s, shadeFromMaterial(mask));
    s.coatWeight = mask.params0.w;
    s.coatColor = specReflect(mask.coatColor.rgb, mask.specCoat);
    s.coatRgb = mask.coatColor.rgb;
    s.coatRoughness = mask.coatColor.a;
    s.coatIor = mask.params1.x;
    if (thickness > 0.0 && tRef > 0.0) {
        // coat_color は「基準の厚みを垂直に往復したときの透過率」。そこから吸収係数を求める
        s.coatSigma = -log(max(s.coatColor, vec4(1e-4))) / (2.0 * tRef);
        s.coatThickness = thickness;
    }
    return s;
}

// ---------------------------------------------------------------- レジストの厚み t(x)（距離場から）

// 距離場の 8 bit の値（128 が縁、1 単位が 範囲 / 127 mm）を mm へ。範囲は krbr-geom の SDF_RANGE_MM（Rust から渡す）
float sdfDecode(float v) { return (v * 255.0 - 128.0) / 127.0 * F.maskParams2.w; }

vec2 boardSdf(vec3 p, uint side) {
    vec2 uv = vec2((p.x - F.boardMap.x) * F.boardMap.z, (p.z - F.boardMap.y) * F.boardMap.w);
    vec4 t = textureLod(boardTex, vec3(uv, float(side)), 0.0);
    return vec2(sdfDecode(t.r), sdfDecode(t.g));  // x: 銅の縁からの距離, y: レジスト開口の縁からの距離（外側が正）
}

// dc: 銅の縁からの距離（銅の外が正）, dm: レジスト開口の縁からの距離（レジストのある側が正）
float maskThickness(float dc, float dm) {
    float t;
    if (dc < 0.0) {
        // 銅の上：縁で流れて薄くなり、中央に向かって設定の厚みに近づく
        t = F.maskParams.z + (F.maskParams.x - F.maskParams.z) * (1.0 - exp(-(-dc) / F.maskParams.w));
    } else {
        // 基材の上：銅の壁ぎわにたまって厚くなる
        t = F.maskParams.y + F.maskParams2.x * exp(-dc / F.maskParams2.y);
    }
    // レジスト開口の縁に向かっても薄くなる
    return t * (0.35 + 0.65 * smoothstep(0.0, 0.05, dm));
}

// レジストの表面の高さ（基材の面から）。銅の縁は、レジストに覆われてなだらかになる
float maskSurfaceHeight(float dc, float dm) {
    float copper = F.maskParams2.z * (1.0 - smoothstep(-0.04, 0.04, dc));
    return copper + maskThickness(dc, dm);
}

// 交点の材質と、凹凸で揺らした法線を求める
Shade surfaceAt(Hit hit, inout vec3 ns, inout Rng rng) {
    Material m = materials[hit.material];
    uint kind = uint(m.params1.y);
    vec3 seed = instanceSeed(hit.instance);
    vec3 nObj = normalize(hit.w2o * ns);  // 法線をオブジェクト座標へ（剛体変換なら w2o をそのまま掛ける）
    Detail d = Detail(vec3(0.0), 1.0, 1.0);
    const float footprint = pixelFootprint(hit.pos);
    Shade s;
    uint maskMat = 0xFFFFFFFFu;
    uint baseMat = hit.material;
    if (kind == MAT_MASKED) {
        baseMat = uint(m.params1.z);
        maskMat = uint(m.params1.w);
        s = shadeFromMaterial(materials[baseMat]);
    } else {
        s = shadeFromMaterial(m);
    }

    if (maskMat != 0xFFFFFFFFu) {
        Material mask = materials[maskMat];
        // レジストの表面はレジストの凹凸。下が基材なら織り目がうっすら浮き出る
        addDetail(mask, hit.pObj, nObj, seed, 1.0, footprint, d);
        Material base = materials[baseMat];
        if (uint(base.params2.x) == BUMP_WEAVE) addDetail(base, hit.pObj, nObj, seed, 0.05, footprint, d);
        float thickness = 0.0;
        if (F.maskParams2.w > 0.0 && abs(nObj.y) > 0.5) {
            // 距離場からレジストの厚みと、縁のなだらかな傾き（法線）を求める。表裏は面の位置（基板の中心より上か下か）で決める：
            // ns は光線の来た側に向けてあるので、基材の中から当たった光線では逆を向く（以前は ns の向きで決めていて、
            // 中から当たると反対の面の距離場を読んでいた）。outward は ns が面の外を向いていれば 1、内を向いていれば -1
            uint side = hit.pObj.y > 0.0 ? 0u : 1u;
            float outward = (side == 0u ? nObj.y : -nObj.y) > 0.0 ? 1.0 : -1.0;
            vec2 sd = boardSdf(hit.pos, side);
            thickness = maskThickness(sd.x, sd.y);
            const float e = 0.01;  // 有限差分の幅（mm）
            vec2 sx = boardSdf(hit.pos + vec3(e, 0, 0), side), sz = boardSdf(hit.pos + vec3(0, 0, e), side);
            float h0 = maskSurfaceHeight(sd.x, sd.y);
            float dhx = (maskSurfaceHeight(sx.x, sx.y) - h0) / e;
            float dhz = (maskSurfaceHeight(sz.x, sz.y) - h0) / e;
            // 表面の高さは外向き（表は +Y、裏は -Y）に測るので、勾配は外向きの法線から引く（ns が内向きなら符号が逆）
            d.grad += outward * vec3(dhx, 0.0, dhz);
        }
        s.baseColor *= d.colorMul;
        s.baseRgb *= d.colorMul;
        s = applyMask(s, mask, thickness, rng);
        s.coatRoughness = clamp(s.coatRoughness * d.roughMul, 0.02, 1.0);
    } else {
        addDetail(materials[baseMat], hit.pObj, nObj, seed, 1.0, footprint, d);
        s.baseColor *= d.colorMul;
        s.baseRgb *= d.colorMul;
        s.roughness = clamp(s.roughness * d.roughMul, 0.02, 1.0);
    }

    // 勾配をワールドへ（法線と同じく逆転置）、法線を傾ける
    vec3 g = d.grad * hit.w2o;
    vec3 gt = g - dot(g, ns) * ns;
    if (dot(gt, gt) > 0.0) {
        vec3 nb = normalize(ns - gt);
        if (dot(nb, ns) > 0.2) ns = nb;
    }
    return s;
}

// ---------------------------------------------------------------- 交点での前処理（全積分器で共通）

// 透過する材質の屈折率（分光モードで分散があれば代表波長での値）
float iorOf(Material m) {
    if (spectralMode() && m.params3.w > 0.5) return m.params3.y + m.params3.z / (gLambda.x * gLambda.x);
    return m.params0.z;
}

// 光線の来た側に法線をそろえる（両面として扱う）。frontFace：幾何法線の表（閉じた形状の外）から当たった。
// シェーディング法線が来た側から見て裏を向いていたら幾何法線を使う
void orientNormals(Hit hit, vec3 wo, out bool frontFace, out vec3 ng, out vec3 ns) {
    frontFace = dot(hit.ng, wo) > 0.0;
    ng = frontFace ? hit.ng : -hit.ng;
    ns = dot(hit.ns, ng) < 0.0 ? -hit.ns : hit.ns;
    if (dot(ns, wo) <= 0.0) ns = ng;
}

// 交点の材質（凹凸で傾けた法線 ns を含む。凹凸で来た側から見て裏を向いたら元の法線に戻す）。透過する材質では
// v の側から見た相対屈折率を s.ior に入れる。dispersive：分光モードで分散のある材質（呼び出し側で代表波長以外を打ち切る）
Shade shadeVertex(Hit hit, vec3 wo, bool frontFace, inout vec3 ns, inout Rng rng, out bool dispersive) {
    vec3 nsBase = ns;
    Shade s = surfaceAt(hit, ns, rng);
    if (dot(ns, wo) <= 0.0) ns = nsBase;
    dispersive = false;
    if (s.transWeight > 0.0) {
        Material m = materials[hit.material];
        float n = iorOf(m);
        s.ior = frontFace ? n : 1.0 / n;
        dispersive = spectralMode() && m.params3.w > 0.5;
    }
    return s;
}

// ---------------------------------------------------------------- 基材の中から面に当たったとき（PT の subsurface）

// 中（基材の側）から見た面の材質：レジストの下の材質（レジストの顔料・コートは外側の層なので関係しない）
uint innerMaterial(uint id) {
    Material m = materials[id];
    return uint(m.params1.y) == MAT_MASKED ? uint(m.params1.z) : id;
}

// 中から当たった光を通さない材質：透過するローブ（拡散の透過・誘電体の透過）を持たないもの（銅・シルクのインク・樹脂など）。
// 金属（base_metalness > 0.5）も通さない
bool opaqueFromInside(Material m) { return m.params0.x > 0.5 || (m.params3.x <= 0.0 && m.transmission.x <= 0.0); }

// 光を通さない面が、中から当たった光を拡散して中へ返す割合：金属は垂直入射の反射率（base_color）、
// 誘電体はベースの拡散の反射率（base_color × base_weight）。残りは面で吸収される
vec4 innerReflectance(Material m) {
    vec4 c = specReflect(m.baseColor.rgb, m.specBase);
    return m.params0.x > 0.5 ? c : c * m.baseColor.a;
}

// ---------------------------------------------------------------- 間接光の上限（ホタル対策）

// 上限と比べる前に除く、推定のための倍率：カメラの重み（実レンズは絞りで遮られた光線の分を通り抜けた光線で補うので、
// 絞るほど大きい）。カメラの光線を作ったところで設定する
float gClampScale = 1.0;

// 上限の倍率：サンプル番号（VCM は繰り返しの回数）n に対して max(1, (n / 16)^(2/3))。16 spp までは設定値のまま、
// 1024 spp で 16 倍、4096 spp で 40 倍。寄与は負にならないので、上限 τ で抑えた値の分散は τ × 平均 以下になり、
// τ を n より遅く伸ばせば、緩めた分のばらつきも平均で消え、上限による偏りも積むほど 0 に近づく（収束する推定になる）。
// 伸ばし方は、ガラスの下（箱越しの太陽）での偏りとざらつきを測って決めた（4096 spp で偏り −5%、16384 spp で −1%）
float clampGrowth() { return max(1.0, pow(float(pc.sampleIndex + 1u) / 16.0, 2.0 / 3.0)); }

// 間接光の上限（ホタル対策）。terminated：代表波長以外を打ち切って代表波長を 4 倍にした寄与
vec4 clampRadiance(vec4 c, bool terminated) {
    if (F.envParams.z <= 0.0) return c;
    float m = F.envParams.z * gClampScale * (terminated ? 4.0 : 1.0) * clampGrowth();
    float mx = max(max(c.x, c.y), max(c.z, c.w));
    return mx > m ? c * (m / mx) : c;
}

// 上限をかけ始める頂点の数。光源とカメラの間の散乱の頂点を、カメラから最初の鏡面でない頂点までのガラス・鏡（isDeltaLike）を
// 除いて数える（PtState.rough と同じ数え方）。1 つ（最初の粗い頂点での直接光。ガラス越し・鏡に映った物を含む）は抑えず、
// 2 つ以上（間接光）だけを抑える。PT・BDPT・VCM のすべての戦略（光源に当たる・直接サンプリング・接続・まとめ・光源側から
// カメラへつなぐ）で、同じ経路には同じ判定になるように、それぞれが経路全体の頂点の数を渡す
#define CLAMP_MIN_ROUGH_VERTICES 2u

// 間接光なら上限をかける。roughVertices：上の数え方での経路全体の頂点の数
vec4 clampIndirect(vec4 c, uint roughVertices, bool terminated) {
    return roughVertices >= CLAMP_MIN_ROUGH_VERTICES ? clampRadiance(c, terminated) : c;
}

vec4 envSpectrum(vec3 dir) { return rgbToSpecUnbounded(envRadiance(dir)); }

#endif
