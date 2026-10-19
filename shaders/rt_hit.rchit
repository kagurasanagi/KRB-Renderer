#version 460
// ray tracing pipeline の最近接ヒット：交点の生の情報をペイロードに書くだけ
#extension GL_EXT_ray_tracing : require
#extension GL_GOOGLE_include_directive : require

#include "rt_payload.glsl"

layout(location = 0) rayPayloadInEXT RtHit payload;
hitAttributeEXT vec2 attribs;

void main() {
    payload.o2w = gl_ObjectToWorldEXT;
    payload.w2o = gl_WorldToObjectEXT;
    payload.bc = attribs;
    payload.t = gl_HitTEXT;
    payload.instCustom = uint(gl_InstanceCustomIndexEXT);
    payload.instId = uint(gl_InstanceID);
    payload.prim = uint(gl_PrimitiveID);
    payload.hit = 1u;
}
