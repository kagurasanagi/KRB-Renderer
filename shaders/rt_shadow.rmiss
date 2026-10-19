#version 460
// ray tracing pipeline のミス（影の光線用）：さえぎるものがなかった
#extension GL_EXT_ray_tracing : require

layout(location = 1) rayPayloadInEXT uint shadowPayload;

void main() { shadowPayload = 0u; }
