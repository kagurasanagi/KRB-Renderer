// ray tracing pipeline 用のペイロード（レイ生成・最近接ヒット・ミスで共有）
#ifndef KRBR_RT_PAYLOAD_GLSL
#define KRBR_RT_PAYLOAD_GLSL

// 最近接ヒットの生の情報。交点の組み立て（頂点の補間・材質）はレイ生成側で行う
struct RtHit {
    mat4x3 o2w;      // オブジェクト → ワールド
    mat4x3 w2o;      // ワールド → オブジェクト
    vec2 bc;         // 重心座標
    float t;
    uint instCustom; // インスタンスのカスタム番号（InstanceInfo の添字）
    uint instId;     // TLAS 内のインスタンス番号
    uint prim;
    uint hit;        // 0 = 何にも当たらない
};

#endif
