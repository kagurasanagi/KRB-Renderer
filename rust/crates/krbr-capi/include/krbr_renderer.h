/*
 * KRB Renderer の描画のコア（krbr_renderer.dll）の C API。
 *
 * 物理ベースのパストレーサー（PT / BDPT / VCM、分光モード、OIDN のノイズ除去）を、ほかのアプリから使うためのもの。
 * 使い方：
 *   1. krbr_renderer_create で GPU（Vulkan 1.3 + ray tracing）を初期化する
 *   2. krbr_material_* で材質（OpenPBR Surface のパラメータ）を作り、krbr_scene_* でメッシュ・材質・配置を並べる
 *   3. krbr_renderer_set_scene でシーンを GPU に送り、カメラ・解像度・設定を決める
 *   4. krbr_renderer_render でサンプルを積み増し（少しずつ呼べば途中経過を見たり中断したりできる）、
 *      krbr_renderer_read / krbr_renderer_denoise で画像を読む
 *
 * 約束：
 *   - 座標の単位は mm、Y が上の右手系。行列は列優先（column-major）の 4x4（float 16 個。OpenGL・glm と同じ並び）
 *   - 色はすべてリニア（sRGB の表示値ではない）。画像は RGBA の float で、1 行目が上（Blender に渡すときは上下を反転）
 *   - 環境マップは正距円筒。画像の上端が +Y、横の中央が -Z の向き
 *   - 失敗した関数は -1（ポインタを返す関数は NULL）を返し、理由は krbr_last_error() で読める（呼んだスレッドごと）
 *   - ポインタの引数は、とくに書いていなければ NULL 不可。配列は書いてある数の要素を読める（書ける）こと
 *   - 1 つの KrbrRenderer を複数のスレッドから同時に使わないこと（別々の KrbrRenderer なら可）
 *   - 文字列はすべて UTF-8
 */
#ifndef KRBR_RENDERER_H
#define KRBR_RENDERER_H

#include <stddef.h>
#include <stdint.h>

#ifdef _WIN32
#define KRBR_API __declspec(dllimport)
#else
#define KRBR_API
#endif

#ifdef __cplusplus
extern "C" {
#endif

/* この値が krbr_api_version() と違えば、ヘッダと DLL の版が合っていない */
#define KRBR_API_VERSION 1

typedef struct KrbrMaterial KrbrMaterial;
typedef struct KrbrScene KrbrScene;
typedef struct KrbrRenderer KrbrRenderer;

/* ---------------------------------------------------------------- 全体 */

KRBR_API uint32_t krbr_api_version(void);
/* 呼んだスレッドで最後に失敗した理由。次にこのスレッドで失敗するまで有効 */
KRBR_API const char *krbr_last_error(void);
/* ログ（標準エラー出力）の細かさ：0 debug, 1 info（既定）, 2 warn, 3 error */
KRBR_API void krbr_set_log_level(int level);
/* ノイズ除去（OIDN の DLL）が使えそうなら 1 */
KRBR_API int krbr_denoiser_available(void);

/* ---------------------------------------------------------------- 材質
 * パラメータの名前は OpenPBR Surface 1.1 と同じ（base_color, specular_roughness, transmission_weight, coat_weight,
 * emission_luminance [cd/m²] など。既定値も仕様のとおり）。krbr の拡張：
 *   数：bump_strength（傾きの大きさ。0.05 で約 3°）、bump_scale（凹凸の大きさ mm）、roughness_variation（粗さのむら 0..1）
 *   文字列：bump_pattern（none / noise / weave）、conductor（測定値の金属：Au, Ag, Cu, Al, Ni, Ti, W, CuZn30, SS_austenitic など。none でやめる）、
 *           category（metal / plastic / rubber / ceramic / glass / gem / other）
 * 真偽値（geometry_thin_walled）は set_float に 0 / 1 で渡す。色のパラメータに set_float で数を渡すと灰色 */

/* name は NULL でもよい（材質の名前。ログに出るだけ） */
KRBR_API KrbrMaterial *krbr_material_create(const char *name);
KRBR_API void krbr_material_destroy(KrbrMaterial *material);
KRBR_API int krbr_material_set_float(KrbrMaterial *material, const char *param, float value);
KRBR_API int krbr_material_set_color(KrbrMaterial *material, const char *param, float r, float g, float b);
KRBR_API int krbr_material_set_string(KrbrMaterial *material, const char *param, const char *value);

/* ---------------------------------------------------------------- シーン */

KRBR_API KrbrScene *krbr_scene_create(void);
KRBR_API void krbr_scene_destroy(KrbrScene *scene);
/* 材質を写して足す。材質の番号（0 から）を返す。material はこの後捨ててよい */
KRBR_API int32_t krbr_scene_add_material(KrbrScene *scene, const KrbrMaterial *material);
/* index 番の材質を置き換える（krbr_renderer_update_materials で材質だけを送り直すとき） */
KRBR_API int krbr_scene_set_material(KrbrScene *scene, uint32_t index, const KrbrMaterial *material);
/* 三角形メッシュを写して足す。メッシュの番号を返す。
 *   positions          : vertex_count * 3 個（x, y, z）
 *   normals            : vertex_count * 3 個。NULL なら三角形から滑らかな法線を求める
 *   indices            : triangle_count * 3 個（頂点の番号）
 *   triangle_materials : triangle_count 個（材質の番号。インスタンスの material_base に足す）。NULL ならすべて 0 */
KRBR_API int32_t krbr_scene_add_mesh(KrbrScene *scene, const float *positions, const float *normals, uint32_t vertex_count,
                                     const uint32_t *indices, uint32_t triangle_count, const uint32_t *triangle_materials);
/* メッシュを置く。インスタンスの番号を返す。
 *   matrix : オブジェクト → ワールドの 4x4（列優先、16 個）。NULL なら単位行列
 *   material_base : 三角形の材質番号に足す値
 *   mask   : 表示マスク（krbr_renderer_set_visible_mask のビットと重なるものだけを描く。ふつうは 0xFF） */
KRBR_API int32_t krbr_scene_add_instance(KrbrScene *scene, uint32_t mesh, const float *matrix, uint32_t material_base, uint8_t mask);

/* ---------------------------------------------------------------- 描画 */

/* validation が 0 以外なら Vulkan の検証レイヤーを使う（開発用。遅い）。GPU が無ければ NULL */
KRBR_API KrbrRenderer *krbr_renderer_create(int validation);
KRBR_API void krbr_renderer_destroy(KrbrRenderer *renderer);
/* GPU の名前。renderer を捨てるまで有効 */
KRBR_API const char *krbr_renderer_device_name(const KrbrRenderer *renderer);

/* シーンを GPU に送る（前のシーンは捨てる。scene はこの後捨ててもよい）。累積は 0 から */
KRBR_API int krbr_renderer_set_scene(KrbrRenderer *renderer, const KrbrScene *scene);
/* 材質だけを送り直す（形状はそのまま。材質の数と発光は送ったシーンと同じであること）。累積は 0 から */
KRBR_API int krbr_renderer_update_materials(KrbrRenderer *renderer, const KrbrScene *scene);
/* 描くインスタンスの表示マスク（既定 0xFF）。累積は 0 から */
KRBR_API int krbr_renderer_set_visible_mask(KrbrRenderer *renderer, uint8_t mask);

/* 環境マップ（リニアの RGBA の float、width * height * 4 個、正距円筒）。既定は内蔵のスタジオ */
KRBR_API int krbr_renderer_set_environment(KrbrRenderer *renderer, const float *rgba, uint32_t width, uint32_t height);
/* .hdr / .exr のファイル、または "builtin:studio" */
KRBR_API int krbr_renderer_set_environment_file(KrbrRenderer *renderer, const char *path);
/* 一様な色の環境光 */
KRBR_API int krbr_renderer_set_environment_color(KrbrRenderer *renderer, float r, float g, float b);

/* 出力の画素数（既定 640x480）。カメラの縦横比も合わせる */
KRBR_API int krbr_renderer_set_resolution(KrbrRenderer *renderer, uint32_t width, uint32_t height);
/* position から target を見るカメラ（それぞれ 3 個）。up は画面の上のおおよその向き、fov_y_deg は縦の画角 */
KRBR_API int krbr_renderer_set_camera(KrbrRenderer *renderer, const float *position, const float *target, const float *up, float fov_y_deg);

/* 数の設定（変えると累積は 0 から）：
 *   env_intensity（環境光の強さ、既定 1）、env_rotation_deg（環境マップを Y 軸まわりに回す）、
 *   radiance_clamp（明るすぎるサンプルを抑える上限。0 で無効）、max_depth（既定 8）、rr_start_depth（既定 3）、seed、
 *   spectral（0 / 1。分光モード）、transparent_background（0 / 1。背景を透明にし、色の A にアルファ）、
 *   vcm_radius（mm。0 でシーンの大きさから）、vcm_alpha、bump_intensity（材質の凹凸に掛ける強さ。次の set_scene から）、
 *   レンズ：focal_length_mm、f_number、focus_distance_mm（0 で注視点まで）、blades、blade_rotation_deg、blade_roundness、
 *          natural_vignetting（0 / 1）、optical_vignetting、abbe_number、lateral_ca */
KRBR_API int krbr_renderer_set_float(KrbrRenderer *renderer, const char *name, float value);
/* 文字列の設定：integrator（pt / bdpt / vcm）、lens_model（pinhole / thin / realistic）、lens_file（realistic のレンズデータ） */
KRBR_API int krbr_renderer_set_string(KrbrRenderer *renderer, const char *name, const char *value);

/* samples サンプルを積み増す（終わるまで戻らない）。積んだ後のサンプル数を返す */
KRBR_API int64_t krbr_renderer_render(KrbrRenderer *renderer, uint32_t samples);
KRBR_API uint32_t krbr_renderer_samples(const KrbrRenderer *renderer);
/* 累積を 0 に戻す */
KRBR_API int krbr_renderer_reset(KrbrRenderer *renderer);
/* 今の累積を読む。aov：0 色、1 アルベド、2 法線。out は 幅 * 高さ * 4 個の float（count はその数） */
KRBR_API int krbr_renderer_read(const KrbrRenderer *renderer, int aov, float *out, size_t count);
/* 今の累積をノイズ除去して読む。quality：0 fast、1 balanced、2 high。strength：元の画像と混ぜる割合（0..1。1 で OIDN のまま） */
KRBR_API int krbr_renderer_denoise(const KrbrRenderer *renderer, int quality, float strength, float *out, size_t count);

#ifdef __cplusplus
}
#endif

#endif
