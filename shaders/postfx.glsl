// 後処理（tonemap.comp・glow.comp・denoise_prep.comp・denoise_blend.comp）で共通の部分。
// 画像の保存（CPU。rust/crates/krbr-render/src/image_io.rs の tonemap_rgba8 と glow.rs）も同じ式にそろえる
// （tests/glow.rs が GPU と CPU の画素を比べる）
#ifndef KRBR_POSTFX_GLSL
#define KRBR_POSTFX_GLSL

// 色の行列（列 m0..m2。XYZ ⇄ リニア sRGB）をかける。m0.w < 0.5 ならそのまま（Rust 側が w に「変換するか」を入れて渡す）
vec3 applyColorMatrix(vec3 c, vec4 m0, vec4 m1, vec4 m2) {
    if (m0.w < 0.5) return c;
    return mat3(m0.xyz, m1.xyz, m2.xyz) * c;
}

// 累積の画素（A がサンプル数）を、露出をかけたリニア sRGB の放射輝度へ：サンプル数で割り、XYZ ならリニア sRGB へ変換し、
// 負を 0 にして露出をかける
vec3 resolveAccum(vec4 a, vec4 m0, vec4 m1, vec4 m2, float exposureScale) {
    return a.a > 0.0 ? max(applyColorMatrix(a.rgb / a.a, m0, m1, m2), vec3(0.0)) * exposureScale : vec3(0.0);
}

// 解像度が半分の段での、画素 p の中心の座標（画素の中心が整数）。glow.rs の up_coord と同じ
vec2 halfResCoord(uvec2 p) { return (vec2(p) + 0.5) * 0.5 - 0.5; }

#endif
