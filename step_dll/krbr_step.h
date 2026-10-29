#pragma once
// STEP 変換器 DLL（krbr_step.dll）の C の API。
// OpenCASCADE はこの DLL の中だけで使い、本体（Rust の krbr.exe）は必要なときにこの DLL を読み込んで呼ぶ。
// 置き場所：krbr.exe と同じフォルダの dll\step\（OpenCASCADE の DLL もそこにまとめる）。krbr.exe は dll\step\、
// 古い配置の step\、開発中のビルド出力 build\release\step\ の順に探す（krbr-model の library.rs）
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

// STEP ファイルを読んで三角形に分割し、.krbm 形式（step_dll/mesh_file.hpp）で out_file に書く。
//   step_file / out_file : UTF-8 のパス
//   linear_deflection    : 弦の許容誤差（mm）、angular_deflection : 角度の許容誤差（rad）
// 戻り値 0 = 成功。失敗時は err に理由（UTF-8、err_len まで）を書いて非 0
//   （1: パスが null、2: 読み込み・分割の失敗、3: 書き込みの失敗、4: 予期しない例外。例外はこの関数の外へ出さない）
KRBR_STEP_API int krbr_step_to_mesh_file(const char* step_file, double linear_deflection, double angular_deflection,
                                         const char* out_file, char* err, size_t err_len);

// この DLL の API の版と、書く .krbm の形式の版
KRBR_STEP_API int krbr_step_api_version(void);
KRBR_STEP_API int krbr_step_mesh_file_version(void);

// 呼んだスレッドで最後の krbr_step_to_mesh_file にかかった時間の内訳（ミリ秒）を ms に n 個まで書き、書いた数を返す。
// 並び：0 ReadFile、1 Transfer、2 BRepMesh（分割）、3 三角形の取り出し、4 .krbm の書き出し、5 全体。
// 失敗した変換では、そこまでの段階だけが 0 以外。後から足した関数なので、無い DLL もある（呼ぶ側は有無を確かめる）
#define KRBR_STEP_TIMINGS 6
KRBR_STEP_API size_t krbr_step_last_timings(double* ms, size_t n);

#ifdef __cplusplus
}
#endif
