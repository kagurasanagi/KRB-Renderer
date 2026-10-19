// 積分器のシェーダを、ray query（計算シェーダ）と ray tracing pipeline（レイ生成シェーダ）のどちらでも
// 同じ本文から作るための切り替え。.comp の先頭で #version と GL_GOOGLE_include_directive の直後に含める。
//   既定             : 計算シェーダ。GL_EXT_ray_query で交差判定（RTX など ray_query を持つ GPU）
//   KRBR_RT_PIPELINE : レイ生成シェーダ。GL_EXT_ray_tracing で traceRayEXT（GTX 10 系など ray_query が無い GPU）
#ifndef KRBR_STAGE_GLSL
#define KRBR_STAGE_GLSL

#ifdef KRBR_RT_PIPELINE
#extension GL_EXT_ray_tracing : require
#define KRBR_INVOCATION_ID gl_LaunchIDEXT
#else
#extension GL_EXT_ray_query : require
layout(local_size_x = 8, local_size_y = 8, local_size_z = 1) in;
#define KRBR_INVOCATION_ID gl_GlobalInvocationID
#endif

#endif
