// カメラ：ピンホール・薄レンズ・実レンズ（rust/crates/krbr-scene/src/lens.rs と、定数を作る krbr-render/src/kernel.rs の fill_lens に対応）。
// 薄レンズは、BDPT / VCM の「光源側の頂点をカメラへ直接つなぐ」戦略のために、カメラ側と同じ分布でレンズ上の点を選べる。
// その点を固定すれば、画像の 1 画素あたりの確率密度はピンホールと同じ形（像面までの距離² / cos³θ）になる
#ifndef KRBR_CAMERA_GLSL
#define KRBR_CAMERA_GLSL

#define LENS_PINHOLE 0u
#define LENS_THIN 1u
#define LENS_REALISTIC 2u

uint lensModel() { return uint(F.lens0.x + 0.5); }

// 分散の形 (1/λ² − 1/λd²) / (1/λF² − 1/λC²)。Cauchy の式では (n(λ) − nd) / (nF − nC) に等しい
float dispersionShape(float l) {
    const float lF = 486.1, lC = 656.3, ld = 587.6;
    return (1.0 / (l * l) - 1.0 / (ld * ld)) / (1.0 / (lF * lF) - 1.0 / (lC * lC));
}

// 色収差は波長で光線が変わるので、代表波長以外を打ち切る
bool cameraIsChromatic() {
    return spectralMode() && lensModel() == LENS_THIN && (F.lens3.y > 0.0 || F.lens2.y != 0.0);
}
// 倍率色収差：代表波長の像の大きさ（d 線に対する比）
float lateralScale() { return cameraIsChromatic() ? 1.0 + F.lens2.y * dispersionShape(gLambda.x) : 1.0; }

// ピントの合う距離（軸上色収差があれば代表波長での値。フィルムの位置は d 線で決めてある）
float focusDistance() {
    float s = F.lens0.z;
    if (spectralMode() && F.lens3.y > 0.0) {
        float n = F.lens3.x + F.lens3.y * dispersionShape(gLambda.x);
        float f = F.lens2.w * (F.lens3.x - 1.0) / (n - 1.0);
        float inv = 1.0 / f - 1.0 / F.lens2.z;
        s = inv > 1e-9 ? 1.0 / inv : 1e9;
    }
    return s;
}

// 絞りの形（p は絞りの半径を 1 とした座標）：羽根の枚数の正多角形と円の間を丸みで補間した境界の内側
bool insideAperture(vec2 p) {
    float r2 = dot(p, p);
    if (r2 > 1.0) return false;
    float N = F.lens0.w;
    if (N < 3.0) return true;
    float sector = 2.0 * PI / N;
    float phi = atan(p.y, p.x) - F.lens1.x;
    float a = mod(phi, sector) - 0.5 * sector;  // 辺の中央からの角度（頂点は回転の角度から sector ごと）
    float rPoly = cos(PI / N) / cos(a);
    return sqrt(r2) <= mix(rPoly, 1.0, F.lens1.y);
}

// 絞りの中の点を一様に選ぶ（円から棄却法）。単位は mm（レンズ面の座標）
vec2 sampleAperture(inout Rng rng) {
    for (int i = 0; i < 32; ++i) {
        vec2 p = concentricDisk(rnd2(rng));
        if (insideAperture(p)) return p * F.lens0.y;
    }
    return vec2(0.0);
}

vec3 camRightN() { return normalize(F.camRight.xyz); }
vec3 camUpN() { return normalize(F.camUp.xyz); }

// 口径食：レンズ上の点 lp（mm）から方向 d（ワールド）へ出る光が、前の円形の開口を通るか
bool passesFrontAperture(vec2 lp, vec3 d) {
    float D = F.lens1.z;
    if (D <= 0.0) return true;
    float dz = dot(d, F.camForward.xyz);
    vec2 q = lp + vec2(dot(d, camRightN()), dot(d, camUpN())) / dz * D;
    return dot(q, q) <= F.lens1.w * F.lens1.w;
}

float pow4(float c) { return (c * c) * (c * c); }

// ---------------------------------------------------------------- 実レンズの追跡（フィルム側から物体側へ）
// 座標：x 右、y 上、z が物体側（前の面の頂点が z = 0）
bool traceRealisticLens(inout vec3 o, inout vec3 d) {
    int n = int(F.lens3.w + 0.5);
    for (int i = n - 1; i >= 0; --i) {
        vec4 s = F.lensSurfaces[i];
        float R = s.x, zv = s.y, ap2 = s.w * s.w;
        if (R == 0.0) {
            vec3 p = o + d * ((zv - o.z) / d.z);
            if (dot(p.xy, p.xy) > ap2) return false;
            o = p;
            continue;
        }
        vec3 c = vec3(0.0, 0.0, zv - R);
        vec3 oc = o - c;
        float b = dot(oc, d), cc = dot(oc, oc) - R * R;
        float disc = b * b - cc;
        if (disc < 0.0) return false;
        float sq = sqrt(disc);
        float t0 = -b - sq, t1 = -b + sq;
        float t = -1.0;
        if (t0 > 1e-6) t = t0;
        if (t1 > 1e-6 && (t < 0.0 || abs(o.z + d.z * t1 - zv) < abs(o.z + d.z * t - zv))) t = t1;
        if (t < 0.0) return false;
        vec3 p = o + d * t;
        if (dot(p.xy, p.xy) > ap2) return false;
        vec3 nrm = normalize(p - c);
        if (dot(nrm, d) > 0.0) nrm = -nrm;
        float nFilm = s.z, nScene = i > 0 ? F.lensSurfaces[i - 1].z : 1.0;
        d = refract(d, nrm, nFilm / nScene);
        if (dot(d, d) == 0.0) return false;
        d = normalize(d);
        o = p;
    }
    return true;
}

// ---------------------------------------------------------------- カメラの光線
struct CameraRay {
    vec3 origin;
    vec3 dir;
    float weight;      // 周辺減光・口径食・実レンズの明るさ（0 なら光が通らない）
    float imageScale;  // 倍率色収差による像の大きさ（確率密度の換算に使う）
};

// 画面の正規化座標 ndc（-1..1、y は下向き）からカメラの光線を作る
CameraRay generateCameraRay(vec2 ndc, inout Rng rng) {
    CameraRay r;
    r.weight = 1.0;
    r.imageScale = 1.0;
    uint model = lensModel();
    if (model == LENS_REALISTIC) {
        // フィルムの点（像は上下左右が逆になる）から、射出瞳（後ろの面のうち、光がレンズを通る範囲）の中の点へ。
        // 範囲はフィルムの光軸からの距離ごとの表（lensPupil。点が +x 軸の上のときの矩形）を、点の向きへ回して使う。
        // 後ろの面全体から選ぶと、絞ったとき（F 22 など）光線の大半がレンズの中で止まって無駄になる
        vec3 pf = vec3(-ndc.x * F.lens4.x, ndc.y * F.lens4.y, F.lens4.z);
        int n = int(F.lens3.w + 0.5);
        float zRear = F.lensSurfaces[n - 1].y;
        float rf = length(pf.xy);
        int bin = min(int(rf / max(F.lens5.x, 1e-6) * float(KRBR_LENS_PUPIL_BINS)), KRBR_LENS_PUPIL_BINS - 1);
        vec4 box = F.lensPupil[bin];
        // 開発用（lens5.y）：後ろの面全体を囲む矩形から選ぶ（前の方法と同じ分布の広さ）
        if (F.lens5.y > 0.5) box = vec4(-F.lens4.w, -F.lens4.w, F.lens4.w, F.lens4.w);
        float area = (box.z - box.x) * (box.w - box.y);
        vec2 q = mix(box.xy, box.zw, rnd2(rng));
        vec2 axis = rf > 1e-6 ? pf.xy / rf : vec2(1.0, 0.0);
        vec3 pr = vec3(axis.x * q.x - axis.y * q.y, axis.y * q.x + axis.x * q.y, zRear);
        if (F.lens5.y > 0.5) area = (box.z - box.x) * (box.w - box.y);
        vec3 o = pf, d = normalize(pr - pf);
        float dz = zRear - F.lens4.z;
        r.weight = area > 0.0 ? pow4(d.z) * area / (dz * dz) * F.lens3.z : 0.0;
        if (!traceRealisticLens(o, d)) r.weight = 0.0;
        vec3 R = camRightN(), U = camUpN(), W = F.camForward.xyz;
        r.origin = F.camPos.xyz + o.x * R + o.y * U + o.z * W;
        r.dir = normalize(d.x * R + d.y * U + d.z * W);
        return r;
    }
    r.imageScale = lateralScale();
    vec2 n2 = ndc / r.imageScale;
    vec3 dPin = normalize(F.camForward.xyz + n2.x * F.camRight.xyz - n2.y * F.camUp.xyz);
    float cosPin = dot(dPin, F.camForward.xyz);
    if (F.lens2.x > 0.5) r.weight *= pow4(cosPin);
    if (model == LENS_THIN) {
        vec2 lp = sampleAperture(rng);
        r.origin = F.camPos.xyz + lp.x * camRightN() + lp.y * camUpN();
        vec3 q = F.camPos.xyz + dPin * (focusDistance() / cosPin);
        r.dir = normalize(q - r.origin);
        if (!passesFrontAperture(lp, r.dir)) r.weight = 0.0;
    } else {
        r.origin = F.camPos.xyz;
        r.dir = dPin;
    }
    return r;
}

// 光源側の頂点 x をカメラへつなぐ：レンズ上の点を選び、写る画素を求める（実レンズでは使わない）
struct CameraConnection {
    vec3 lensPos;
    vec2 raster;
    float cosAtCamera;  // レンズ上の点から x への方向と、カメラの前方との余弦
    float weight;
    float imageScale;
};

bool connectToLens(vec3 x, inout Rng rng, out CameraConnection cc) {
    uint model = lensModel();
    if (model == LENS_REALISTIC) return false;
    cc.imageScale = lateralScale();
    vec2 lp = vec2(0.0);
    cc.lensPos = F.camPos.xyz;
    if (model == LENS_THIN) {
        lp = sampleAperture(rng);
        cc.lensPos += lp.x * camRightN() + lp.y * camUpN();
    }
    vec3 d = normalize(x - cc.lensPos);
    cc.cosAtCamera = dot(d, F.camForward.xyz);
    if (cc.cosAtCamera <= 1e-6) return false;
    if (model == LENS_THIN && !passesFrontAperture(lp, d)) return false;
    // ピントの面との交点を、レンズの中心から見た方向（ピンホールの方向）にする
    vec3 dPin = model == LENS_THIN ? normalize(cc.lensPos + d * (focusDistance() / cc.cosAtCamera) - F.camPos.xyz) : d;
    float cosPin = dot(dPin, F.camForward.xyz);
    vec3 rr = F.camRight.xyz, u = F.camUp.xyz;
    vec2 ndc = vec2(dot(dPin, rr) / dot(rr, rr), -dot(dPin, u) / dot(u, u)) / cosPin * cc.imageScale;
    if (abs(ndc.x) >= 1.0 || abs(ndc.y) >= 1.0) return false;
    cc.raster = (ndc + 1.0) * 0.5 * vec2(F.dims.xy);
    cc.weight = F.lens2.x > 0.5 ? pow4(cosPin) : 1.0;
    return true;
}

#endif
