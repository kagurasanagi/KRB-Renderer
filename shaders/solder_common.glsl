// はんだの平衡形状の GPU 版（krbr-render の solder_gpu.rs、krbr-scene の solder_sim.rs と同じ式）で共通のもの。
// バッファはデバイスアドレス（uvec2）でプッシュ定数から受け取る。格子の点 (i, j, l) の番号は (l·ny + j)·nx + i
#extension GL_EXT_buffer_reference : require
#extension GL_EXT_buffer_reference_uvec2 : require

layout(buffer_reference, std430, buffer_reference_align = 4) buffer Floats { float v[]; };
layout(buffer_reference, std430, buffer_reference_align = 4) buffer Uints { uint v[]; };

// 点の中身（下位 8 ビット：0 = 空気、ほかは固体。solder_sim::Phase）と、はんだのままにする点（STICKY）
const uint PHASE_MASK = 0xffu;
const uint STICKY = 0x100u;
// 候補でない点の φ
const float NOT_CANDIDATE = -1e30;
