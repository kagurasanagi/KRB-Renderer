#version 460
// ray tracing pipeline のミス（最近接ヒット用）：何にも当たらなかった
#extension GL_EXT_ray_tracing : require
#extension GL_GOOGLE_include_directive : require

#include "rt_payload.glsl"

layout(location = 0) rayPayloadInEXT RtHit payload;

void main() { payload.hit = 0u; }
