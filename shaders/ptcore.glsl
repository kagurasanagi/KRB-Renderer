// 単方向パストレーシングの本体（光源の直接サンプリング ＋ BSDF サンプリングを MIS で結合）。
// pathtrace.comp のほか、BDPT / VCM のカメラ側の経路が基材の中へ入ったとき（subsurface）の続きにも使う
#ifndef KRBR_PTCORE_GLSL
#define KRBR_PTCORE_GLSL

struct PtState {
    vec3 origin;
    vec3 dir;
    vec4 beta;           // 代表波長以外を打ち切ったら (4β₀, 0, 0, 0)
    float prevPdf;       // 直前の BSDF サンプリングの確率密度（0 は直接サンプリングと組み合わせない）
    bool inside;         // 基材の中を進んでいる（拡散の透過で入った）
    vec4 insideSigma;
    vec4 mediumSigma;    // 透明な物体の中での吸収係数
    bool terminated;     // 代表波長以外を打ち切った
};

// 透過する材質の屈折率（分光モードで分散があれば代表波長での値）
float iorOf(Material m) {
    if (spectralMode() && m.params3.w > 0.5) return m.params3.y + m.params3.z / (gLambda.x * gLambda.x);
    return m.params0.z;
}

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

// 経路を depth0 番目の線分から最後まで追う。返り値は放射輝度（beta をかけた後）。
// depth0 == 0 のときだけ、最初の交点のアルベドと法線を書く
vec4 ptTrace(inout PtState st, uint depth0, inout Rng rng, inout vec3 albedo, inout vec3 normal) {
    vec4 L = vec4(0.0);
    uint maxDepth = F.counts.x;
    for (uint depth = depth0; depth < maxDepth; ++depth) {
        Hit hit;
        if (!traceClosest(st.origin, st.dir, hit)) {
            vec4 Le = envSpectrum(st.dir);
            if (depth == 0u) {
                L += st.beta * Le;  // beta はカメラの重み（周辺減光など）
                albedo = clamp(envRadiance(st.dir), 0.0, 1.0);
            } else if (st.prevPdf > 0.0) {  // prevPdf = 0 は基材の中など（中から抜けた光は捨てる）
                float w = powerHeuristic(st.prevPdf, envLightPdfW(st.dir));
                vec4 c = st.beta * Le * w;
                L += depth >= 2u ? clampRadiance(c) : c;  // 間接光だけを抑える（ホタル対策）
            }
            break;
        }
        // 透明な物体の中を通ってきた分の吸収
        if (any(greaterThan(st.mediumSigma, vec4(0.0)))) st.beta *= exp(-st.mediumSigma * hit.t);

        vec3 wo = -st.dir;
        bool frontFace = dot(hit.ng, wo) > 0.0;  // 幾何法線は外向き（閉じた形状の表から当たった）
        // 発光面（片面）。直接サンプリングと MIS で組み合わせる
        Material hm = materials[hit.material];
        if (frontFace && luminanceOf(hm.emission.rgb) > 0.0) {
            vec4 Le = specUnbounded(hm.emission.rgb, hm.specEmission);
            if (depth == 0u) {
                L += st.beta * Le;
                albedo = clamp(hm.emission.rgb, 0.0, 1.0);
            } else {
                float cosL = dot(hit.ng, wo);
                float lightPdfW = emitterPdfA(hm.emission.rgb) * hit.t * hit.t / max(cosL, 1e-6);
                float w = st.prevPdf > 0.0 ? powerHeuristic(st.prevPdf, lightPdfW) : 1.0;
                L += clampRadiance(st.beta * Le * w);
            }
        }

        // 光線の来た側に法線をそろえる（両面として扱う）
        vec3 ng = frontFace ? hit.ng : -hit.ng;
        vec3 ns = dot(hit.ns, ng) < 0.0 ? -hit.ns : hit.ns;
        if (dot(ns, wo) <= 0.0) ns = ng;

        vec3 nsBase = ns;
        Shade s = surfaceAt(hit, ns, rng);
        if (dot(ns, wo) <= 0.0) ns = nsBase;  // 凹凸で裏を向いた場合は元に戻す

        if (s.transWeight > 0.0) {
            // 透過する材質：v の側から見た相対屈折率。分散があれば、ここで代表波長以外を打ち切る
            float n = iorOf(hm);
            s.ior = frontFace ? n : 1.0 / n;
            if (spectralMode() && hm.params3.w > 0.5 && !st.terminated) {
                st.beta = vec4(st.beta.x * 4.0, 0.0, 0.0, 0.0);
                st.terminated = true;
            }
        }

        if (st.inside) {
            // ---- 基材の中から面に当たった：ここまでの吸収をかけ、外へ拡散して出るか、中へはね返る
            st.beta *= exp(-st.insideSigma * hit.t);
            vec3 p = offsetRay(hit.pos, ng);  // ng は中側を向いている
            if (s.metallic || s.sssWeight <= 0.0 && s.coatWeight <= 0.0 && s.baseRgb.r > 0.5) {
                // 銅・不透明なシルクなど：中へ拡散してはね返る
                vec3 t, b;
                basis(ng, t, b);
                st.dir = toWorld(sampleCosineHemisphere(rnd2(rng)), t, b, ng);
                st.beta *= s.metallic ? s.baseColor * 0.8 : vec4(0.8);
                st.origin = p;
                st.prevPdf = 0.0;
                continue;
            }
            // 外へ出る：レジストがあれば、その厚みの吸収を受ける（拡散の出射、f = 1/π）
            vec3 nOut = -ng;
            vec4 tint = s.coatWeight > 0.0 ? sqrt(coatTransmittance(s, 1.0, 1.0)) : vec4(1.0);
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
                        L += clampRadiance(st.beta * INV_PI * c * ls.Le * (w / ls.directPdfW));
                    }
                }
            }
            vec3 l = sampleCosineHemisphere(rnd2(rng));
            st.dir = toWorld(l, t, b, nOut);
            st.prevPdf = l.z * INV_PI;
            st.origin = pOut;
            st.inside = false;
            continue;
        }

        // ---- 拡散の透過：一部の光は基材の中へ入る（確率 sssWeight で選び、重みは打ち消し合う）
        if (s.sssWeight > 0.0 && rnd(rng) < s.sssWeight) {
            vec4 tint = s.coatWeight > 0.0 ? sqrt(coatTransmittance(s, max(dot(ns, wo), 1e-3), 1.0)) : vec4(1.0);
            st.beta *= tint;
            vec3 nIn = -ng;
            vec3 t, b;
            basis(nIn, t, b);
            st.dir = toWorld(sampleCosineHemisphere(rnd2(rng)), t, b, nIn);
            st.origin = offsetRay(hit.pos, nIn);
            st.inside = true;
            st.insideSigma = s.sssSigma;
            st.prevPdf = 0.0;
            if (depth == 0u) {
                albedo = shadeAlbedoRgb(s);
                normal = ns;
            }
            continue;
        }
        if (depth == 0u) {
            albedo = shadeAlbedoRgb(s);
            normal = ns;
        }

        vec3 t, b;
        basis(ns, t, b);
        vec3 v = toLocal(wo, t, b, ns);
        if (v.z <= 1e-6) break;

        vec3 p = offsetRay(hit.pos, ng);

        // 光源の直接サンプリング（NEE）。透過の方向は、物体自身にさえぎられるので試さない
        {
            vec4 c = directLightPt(s, p, ng, ns, t, b, v, st.beta, rng);
            L += depth >= 1u ? clampRadiance(c) : c;
        }

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
        if (transmit) {
            // 面を通り抜けた：表から入ったなら中の吸収を、裏から出たなら外（空気）に戻す
            st.mediumSigma = frontFace ? s.transSigma : vec4(0.0);
            st.origin = offsetRay(hit.pos, -ng);
        } else {
            st.origin = p;
        }
        st.dir = wi;

        // ロシアンルーレット
        if (depth + 1u >= F.counts.y) {
            float q = clamp(max(max(st.beta.x, st.beta.y), max(st.beta.z, st.beta.w)), 0.05, 0.95);
            if (rnd(rng) > q) break;
            st.beta /= q;
        }
    }
    return L;
}

#endif
