// 単方向パストレーシングの本体（光源の直接サンプリング ＋ BSDF サンプリングを MIS で結合）。
// pathtrace.comp のほか、BDPT / VCM のカメラ側の経路が基材の中へ入ったとき（subsurface）の続きにも使う
#ifndef KRBR_PTCORE_GLSL
#define KRBR_PTCORE_GLSL

// ロシアンルーレットの続ける確率の範囲（経路の重みの最大の成分から決める）。下限は、暗い経路を打ち切りすぎて
// 生き残った経路の重みが 1 / q で大きくなりすぎない（ホタルにならない）ため、上限は明るい経路もいずれ終わるため
#define PT_RR_MIN_PROB 0.05
#define PT_RR_MAX_PROB 0.95

struct PtState {
    vec3 origin;
    vec3 dir;
    vec4 beta;           // 代表波長以外を打ち切ったら heroOnly で書き換えた値
    float prevPdf;       // 直前の BSDF サンプリングの確率密度。0 は基材の中を進む光線（外へ抜けた光は捨て、光源は MIS なしで数える）
    bool prevNee;        // 直前の方向は直接サンプリングでも選べた（false：透過の向きなど BSDF サンプリングだけの戦略で、MIS の重みは 1）
    bool inside;         // 基材の中を進んでいる（拡散の透過で入った）
    vec4 insideSigma;
    vec4 mediumSigma;    // 透明な物体の中での吸収係数
    bool terminated;     // 代表波長以外を打ち切った
    uint rough;          // 間接光の上限をかけるかの判定に使う頂点の数（clampIndirect）。カメラから最初の鏡面でない頂点までの
                         // ガラス・鏡（isDeltaLike）は数えない（ガラス越し・鏡に映った物の直接光は抑えない）
};

// 光源への直接サンプリング（NEE）。p は光線の始点、l の判定は呼び出し側の基底で
vec4 directLightPt(Shade s, vec3 p, vec3 ng, vec3 ns, vec3 t, vec3 b, vec3 v, vec4 beta, inout Rng rng) {
    LightSample ls;
    if (!sampleDirectLight(p, rng, ls)) return vec4(0.0);
    if (dot(ls.wi, ng) <= 0.0) return vec4(0.0);
    vec3 l = toLocal(ls.wi, t, b, ns);
    if (l.z <= 0.0) return vec4(0.0);
    vec4 f = bsdfEval(s, v, l);
    if (f.x + f.y + f.z + f.w <= 0.0) return vec4(0.0);
    if (ls.isEnv ? occluded(p, ls.wi) : occludedDist(p, ls.wi, ls.dist * (1.0 - 1e-4))) return vec4(0.0);
    float w = powerHeuristic(ls.directPdfW, bsdfPdf(s, v, l));
    return beta * f * l.z * ls.Le * (w / ls.directPdfW);
}

// 経路を depth0 番目の線分から最後まで追う（depth 番目の線分の先の頂点は、始点から depth + 1 本目の線分の先）。
// 経路の線分は maxPathLength() 本まで：最後の頂点では光源・環境光に当たった分だけを数え、直接サンプリングも散乱もしない
// （同じ長さの経路を、直接サンプリングでも BSDF サンプリングでも作れるようにして、MIS の両方がそろうように）。
// 返り値は放射輝度（beta をかけた後）。depth0 == 0 のときだけ、最初の交点のアルベドと法線を書く
// firstPos：depth0 == 0 のとき、最初に当たった点（ワールド座標）と 1（当たらなければ書かない）
vec4 ptTrace(inout PtState st, uint depth0, inout Rng rng, inout vec3 albedo, inout vec3 normal, inout vec4 firstPos) {
    vec4 L = vec4(0.0);
    for (uint depth = depth0;; ++depth) {
        Hit hit;
        if (!traceClosest(st.origin, st.dir, hit)) {
            vec4 Le = envSpectrum(st.dir);
            if (depth == 0u) {
                L += st.beta * Le;  // beta はカメラの重み（周辺減光など）
                albedo = clamp(envRadiance(st.dir), 0.0, 1.0);
            } else if (st.prevPdf > 0.0) {  // prevPdf = 0 は基材の中など（中から抜けた光は捨てる）
                float w = st.prevNee ? powerHeuristic(st.prevPdf, envLightPdfW(st.dir)) : 1.0;
                L += clampIndirect(st.beta * Le * w, st.rough, st.terminated);
            }
            break;
        }
        // 透明な物体の中を通ってきた分の吸収
        if (any(greaterThan(st.mediumSigma, vec4(0.0)))) st.beta *= exp(-st.mediumSigma * hit.t);

        vec3 wo = -st.dir;
        bool frontFace;
        vec3 ng, ns;
        orientNormals(hit, wo, frontFace, ng, ns);
        uint roughBefore = st.rough;
        // 発光面（片面）。直接サンプリングと MIS で組み合わせる
        Material hm = materials[hit.material];
        if (frontFace && luminanceOf(hm.emission.rgb) > 0.0) {
            vec4 Le = specUnbounded(hm.emission.rgb, hm.specEmission);
            if (depth == 0u) {
                L += st.beta * Le;
            } else {
                float cosL = dot(hit.ng, wo);
                float lightPdfW = emitterPdfA(hm.emission.rgb) * hit.t * hit.t / max(cosL, 1e-6);
                float w = st.prevPdf > 0.0 && st.prevNee ? powerHeuristic(st.prevPdf, lightPdfW) : 1.0;
                L += clampIndirect(st.beta * Le * w, roughBefore, st.terminated);
            }
        }

        bool dispersive;
        Shade s = shadeVertex(hit, wo, frontFace, ns, rng, dispersive);
        // 分散のある材質では、屈折の向きが波長で変わるので代表波長以外を打ち切る
        if (dispersive && !st.terminated) {
            st.beta = heroOnly(st.beta);
            st.terminated = true;
        }
        if (st.rough > 0u || !isDeltaLike(s)) st.rough++;
        if (depth == 0u) {
            albedo = shadeAlbedoRgb(s);
            normal = ns;
            firstPos = vec4(hit.pos, 1.0);
        }
        // 最後の頂点：ここで終わる経路（光源に当たった分）だけを数えた
        if (depth + 1u >= maxPathLength()) break;

        if (st.inside) {
            // ---- 基材の中から面に当たった：ここまでの吸収をかけ、外へ拡散して出るか、中へはね返る
            st.beta *= exp(-st.insideSigma * hit.t);
            vec3 p = offsetRay(hit.pos, ng);  // ng は中側を向いている
            Material im = materials[innerMaterial(hit.material)];
            if (opaqueFromInside(im)) {
                // 銅・シルクのインクなど光を通さない面：その面の反射率で中へ拡散してはね返る（残りは吸収される）
                vec3 t, b;
                basis(ng, t, b);
                st.dir = toWorld(sampleCosineHemisphere(rnd2(rng)), t, b, ng);
                st.beta *= innerReflectance(im);
                st.origin = p;
                st.prevPdf = 0.0;
                continue;
            }
            // 外へ出る：レジストがあれば、その厚みを 1 回通る吸収を受ける（拡散の出射、f = 1/π）
            vec3 nOut = -ng;
            vec4 tint = s.coatWeight > 0.0 ? coatTransmittance1(s, 1.0) : vec4(1.0);
            st.beta *= tint;
            vec3 pOut = offsetRay(hit.pos, nOut);
            vec3 t, b;
            basis(nOut, t, b);
            {
                LightSample ls;
                if (sampleDirectLight(pOut, rng, ls)) {
                    float c = dot(ls.wi, nOut);
                    if (c > 0.0 && !(ls.isEnv ? occluded(pOut, ls.wi) : occludedDist(pOut, ls.wi, ls.dist * (1.0 - 1e-4)))) {
                        float w = powerHeuristic(ls.directPdfW, c * INV_PI);
                        L += clampIndirect(st.beta * INV_PI * c * ls.Le * (w / ls.directPdfW), st.rough, st.terminated);
                    }
                }
            }
            vec3 l = sampleCosineHemisphere(rnd2(rng));
            st.dir = toWorld(l, t, b, nOut);
            st.prevPdf = l.z * INV_PI;
            st.prevNee = true;
            st.origin = pOut;
            st.inside = false;
            continue;
        }

        // ---- 拡散の透過：一部の光は基材の中へ入る（確率 sssWeight で選び、重みは打ち消し合う）
        if (s.sssWeight > 0.0 && rnd(rng) < s.sssWeight) {
            // レジストがあれば、その厚みを v の向きで 1 回通る
            vec4 tint = s.coatWeight > 0.0 ? coatTransmittance1(s, max(dot(ns, wo), 1e-3)) : vec4(1.0);
            st.beta *= tint;
            vec3 nIn = -ng;
            vec3 t, b;
            basis(nIn, t, b);
            st.dir = toWorld(sampleCosineHemisphere(rnd2(rng)), t, b, nIn);
            st.origin = offsetRay(hit.pos, nIn);
            st.inside = true;
            st.insideSigma = s.sssSigma;
            st.prevPdf = 0.0;
            continue;
        }

        vec3 t, b;
        basis(ns, t, b);
        vec3 v = toLocal(wo, t, b, ns);
        if (v.z <= 1e-6) break;

        vec3 p = offsetRay(hit.pos, ng);

        // 光源の直接サンプリング（NEE）。反射の向き（面の表側）だけを試す。透過の向きは、閉じた物体の外から入る光線なら物体自身に
        // さえぎられるので試さない（中から外へ抜ける光線では光源に届きうるので、そちらは BSDF サンプリングだけで重み 1 で数える）
        L += clampIndirect(directLightPt(s, p, ng, ns, t, b, v, st.beta, rng), st.rough, st.terminated);

        // BSDF サンプリング
        vec3 l = bsdfSample(s, v, rng);
        if (l.z == 0.0) break;
        float pdf = bsdfPdf(s, v, l);
        if (pdf <= 0.0) break;
        vec4 f = bsdfEval(s, v, l);
        vec3 wi = toWorld(l, t, b, ns);
        bool transmit = l.z < 0.0;
        // シェーディング法線と幾何法線の不一致で、選んだ側と反対の側に出る方向は捨てる
        if (transmit ? dot(wi, ng) >= 0.0 : dot(wi, ng) <= 0.0) break;
        st.beta *= f * abs(l.z) / pdf;
        st.prevPdf = pdf;
        st.prevNee = !transmit;
        if (transmit) {
            // 面を通り抜けた：表から入ったなら中の吸収を、裏から出たなら外（空気）に戻す
            st.mediumSigma = frontFace ? s.transSigma : vec4(0.0);
            st.origin = offsetRay(hit.pos, -ng);
        } else {
            st.origin = p;
        }
        st.dir = wi;

        // ロシアンルーレット（rr_start_depth から。BDPT / VCM の continuationProb は別の決め方。bidir.glsl を参照）
        if (depth + 1u >= F.counts.y) {
            float q = clamp(max(max(st.beta.x, st.beta.y), max(st.beta.z, st.beta.w)), PT_RR_MIN_PROB, PT_RR_MAX_PROB);
            if (rnd(rng) > q) break;
            st.beta /= q;
        }
    }
    return L;
}

#endif
