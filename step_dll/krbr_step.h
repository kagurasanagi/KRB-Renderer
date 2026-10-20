#pragma once
// STEP 変換器 DLL（krbr_step.dll）の C の API。
// OpenCASCADE はこの DLL の中だけで使い、本体（Rust の krbr.exe）は必要なときにこの DLL を読み込んで呼ぶ。
// 置き場所：実行ファイルと同じフォルダの step\ サブフォルダ（OpenCASCADE の DLL もそこにまとめる）
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

#ifdef KRBR_STEP_EXPORTS
#define KRBR_STEP_API __declspec(dllexport)
#else
#define KRBR_STEP_API
#endif

// API の版（引数の形が変わったら上げる）
#define KRBR_STEP_API_VERSION 1

// STEP ファイルを読んで三角形に分割し、.krbm 形式（model/mesh_file.hpp）で out_file に書く。
//   step_file / out_file : UTF-8 のパス
//   linear_deflection    : 弦の許容誤差（mm）、angular_deflection : 角度の許容誤差（rad）
// 戻り値 0 = 成功。失敗時は err に理由（UTF-8、err_len まで）を書いて非 0
KRBR_STEP_API int krbr_step_to_mesh_file(const char* step_file, double linear_deflection, double angular_deflection,
                                         const char* out_file, char* err, size_t err_len);

// この DLL の API の版と、書く .krbm の形式の版
KRBR_STEP_API int krbr_step_api_version(void);
KRBR_STEP_API int krbr_step_mesh_file_version(void);

#ifdef __cplusplus
}
#endif
