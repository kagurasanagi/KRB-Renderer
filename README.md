# krbr — KiCad 連携 物理レンダラ（フェーズ1）

KiCad の `.kicad_pcb` を読み込み、HDR 環境光で照らした基板を、Vulkan のレイトレーシング（`VK_KHR_ray_query`）を使った GPU パストレーサーで描画するツールです。

```
# リアルタイム表示（既定。ウィンドウが開く）
krbr kicad/mini_dsp_radio.kicad_pcb --hdr hdr/historic_cloister_passage_4k.exr

# ファイルへ出力（ウィンドウは開かない）
krbr render kicad/mini_dsp_radio.kicad_pcb --hdr hdr/historic_cloister_passage_4k.exr --spp 256 --width 1920 --height 1080 --out out/board.png
```

### リアルタイム表示（`krbr <pcb>` / `krbr view <pcb>`）

フレームごとにサンプルを積み増して表示します。1フレームに積むサンプル数は、GPU のタイムスタンプで測った時間が約 10 ms に収まるように自動で調整します。`--spp`（既定 1024）まで積むと描画を止め、GPU を休ませて入力を待ちます。

ウィンドウ左上の**操作パネル**（`F1` / `Tab` で表示を切り替え）で、次の項目をその場で変えられます。

| 項目 | 内容 |
|---|---|
| 環境マップ | 同じフォルダの `.hdr` / `.exr` から選ぶか、「開く...」でファイルを選ぶ。読み込みは別スレッドで行い、終わったら差し替える |
| 強さ・回転 | 環境マップの明るさと、縦軸まわりの回転 |
| 露出 | EV のスライダー。「自動」は、いまの画像の対数平均輝度を中間の灰色（0.18）に合わせる |
| 凹凸の強さ | 表面の微細な凹凸の倍率（`bump_intensity`） |
| 表示 | 画像 / アルベド / 法線、最大 spp |
| カメラ | 画角、視点を戻す、保存 |

| 操作 | 内容 |
|---|---|
| 左ドラッグ | 回転（方位・仰角） |
| 右／中ドラッグ | 平行移動 |
| ホイール | 拡大・縮小 |
| `R` / `F` | カメラを初期位置（基板全体が入る構図）に戻す |
| `+` / `-` | 露出 ±0.25 EV（表示だけ。累積はやり直さない） |
| `[` / `]` | 環境マップを ±15° 回す |
| `1` / `2` / `3` | 画像 / アルベド / 法線 の表示 |
| `S` | 現在の累積を `--out`（既定 `out/view.png`）を元に `view_000.png` / `.exr` として保存 |
| `Esc` | 終了 |

ウィンドウのタイトルに、積んだサンプル数・フレーム時間・露出を表示します。
開発用に、非表示のウィンドウで描いて終了するテストモードがあります（`--test-frames N --screenshot path [--test-orbit 度]`）。画面には出ず、フォーカスも奪いません。

### ファイル出力（`krbr render`）

出力（`--out` と同じフォルダ）:

| ファイル | 内容 |
|---|---|
| `board.png` | トーンマッピング済み（ACES 近似、sRGB） |
| `board.exr` | リニアの放射輝度（RGB float32） |
| `board_albedo.exr` / `board_normal.exr` | 最初の交点のアルベド／ワールド法線（将来のノイズ除去用） |
| `missing_models.json` / `missing_models.txt` | 見つからない・読めなかった 3D モデルの一覧 |

## ビルド

前提: Visual Studio 2026（MSVC）、Vulkan SDK（`glslc`）、vcpkg（`VCPKG_ROOT`）。

```
scripts\build.cmd          # MSVC 環境を読み込み、低優先度で configure + build
build\release\krbr.exe probe
```

依存ライブラリは `vcpkg.json`（マニフェストモード）で `build/vcpkg_installed` に入ります。
release だけをビルドする独自トリプレット `triplets/x64-windows-rel.cmake` を使います（OpenCASCADE のビルド時間を半分にするため）。

| ライブラリ | 用途 |
|---|---|
| volk, Vulkan-Headers, VulkanMemoryAllocator | Vulkan（ローダーは動的読み込み） |
| GLFW 3.4 | リアルタイム表示のウィンドウと入力 |
| Dear ImGui 1.92 | ビューアの操作パネル。バックエンド（GLFW / Vulkan）は volk を使うよう `third_party/imgui_backends` を自前でビルド（MIT） |
| OpenCASCADE 8.0（TKDESTEP / TKXCAF / TKMesh / TKRWMesh） | STEP の読み込みと三角形分割（面の色つき） |
| Clipper2, earcut.hpp | 多角形の和・差・オフセット、三角形分割 |
| tinyexr, stb | EXR/HDR の入出力、PNG 出力 |
| toml++, nlohmann-json, CLI11, glm | 設定・レポート・コマンドライン・数学 |

## コマンド

| コマンド | 内容 |
|---|---|
| `krbr probe` | Vulkan デバイスとレイトレーシング拡張の確認、ヘッドレスデバイスの作成 |
| `krbr info <pcb> [--models]` | 基板の解析結果と 3D モデルのパス解決の確認（GPU は使わない） |
| `krbr <pcb> --hdr <file>`（= `krbr view`） | リアルタイム表示 |
| `krbr render <pcb> --hdr <file> [options]` | ファイルへ出力 |

`render` の主なオプション: `--spp`（既定 64）, `--width/--height`, `--elevation`（既定 35°）, `--azimuth`（既定 45°）, `--fov`, `--look-at X,Y`（KiCad の座標の点を注視）, `--zoom`（自動の構図からの拡大率）, `--exposure`, `--env-intensity`, `--env-rotation`, `--max-depth`, `--clamp`, `--init`, `--no-cache`, `--no-kicad-cli`, `--debug-layers`（層テクスチャを PNG で出力）, `--validate`, `--normal-priority`, `-v`。

## 作業中の PC への配慮

- `render` / `probe` / `info` では、ウィンドウ・サーフェス・スワップチェーンを作りません。画像を自動で開くこともしません。
- `view` は収束したら（`--spp` に達したら）描画を止めて入力を待つので、表示したままでも GPU をほとんど使いません。
- プロセスは BELOW_NORMAL 優先度で動きます（`--normal-priority` で解除）。
- GPU は、`render` では**グラフィックスを持たない計算専用キュー**を使います。`view` では、画面へ表示できるキューを使います。どちらも `VK_KHR_global_priority` が使えれば **LOW** で作ります。
- 1 サンプルずつ投入して完了を待つので、GPU を長時間占有しません。既定のサンプル数は 64 です。
- `.kicad_pcb`、KiCad のライブラリ・設定は読み取りのみです。
  - kicad-cli はプロジェクトフォルダに `.kicad_prl` を作るので、基板を**一時フォルダへコピーしてから**呼び出します。
- 設定ファイルをプロジェクトフォルダに書き出すのは `--init` を指定したときだけです（既存のファイルは上書きしません）。

## 設定ファイル

| 場所 | 内容 |
|---|---|
| `<プロジェクト>/<プロジェクト名>.krbr/board.toml` | 表面処理、レジスト・シルク・基材の色、層テクスチャの解像度、代替の箱、モデルのパスの変数・追加検索フォルダ、kicad-cli |
| `<プロジェクト>/<プロジェクト名>.krbr/environment.toml` | HDR、強度・回転、カメラ、解像度・spp・最大深さ・露出 |
| `%APPDATA%\krbr\model_map.toml` | 部品モデルの面の色・ファイル名から材質（OpenPBR パラメータ）を決める規則 |
| `%LOCALAPPDATA%\krbr\cache\meshes\` | STEP の分割結果のキャッシュ（キーはファイル内容の XXH64 ＋分割パラメータ） |
| `%LOCALAPPDATA%\krbr\cache\silk\` | kicad-cli によるシルク SVG のキャッシュ |

ファイルが無ければ既定値を使います。優先順位は「コマンドライン ＞ 設定ファイル ＞ 既定値」です。

## 処理の流れとモジュール構成

```
.kicad_pcb ─▶ kicad/ ─▶ geom/ ─────────┐
               │        raster/ ───────┤
               └▶ model/ (STEP, cache) ┼▶ scene/ ─▶ render/ (Vulkan) ─▶ io/ ─▶ PNG / EXR
HDR ─────────────────────────────────────────────▶ render/env_map
```

| モジュール | ファイル | 役割 |
|---|---|---|
| core | `log`, `platform`, `hash` | ログ、OS 依存部（キャッシュ・設定の場所、低優先度、ウィンドウを出さない子プロセス）、XXH64 |
| kicad | `sexpr`, `board_parser`, `model_resolver` | S式パーサ → `Board`（外形・層構成・配線・パッド・ビア・ベタ・シルク・穴・部品・3D モデル指定）。座標はすべて基板座標に変換済み。パス解決（`${KIPRJMOD}`, `${KICADn_3DMODEL_DIR}`, `${KISYS3DMOD}`、`kicad_common.json`、`.wrl`→`.step` の差し替え、追加検索フォルダ） |
| geom | `shapes`, `board_mesh` | パッド・配線・図形の多角形化（Clipper2、10 nm 単位の整数座標）。Edge.Cuts の輪郭の連結、穴あけ、上下面の三角形分割、側面・穴の内壁（メッキ穴は別材質） |
| raster | `rasterizer`, `layer_art`, `svg_reader` | 層ごとの多角形を基板面テクスチャ（R=銅, G=レジスト開口, B=シルク）へ描き込む。アンチエイリアス付き、マルチスレッド。kicad-cli の SVG の読み込み |
| model | `model_mesh`, `step_loader`, `mesh_cache` | `IModelLoader`（形式ごとの読み込み器）、STEP（OCCT XCAF、面の色つき）、ディスクキャッシュ |
| scene | `scene_builder`, `model_map`, `camera`, `material` | 材質（OpenPBR の名前）、部品の配置（KiCad 3D ビューアと同じ変換の順序）、代替の箱、カメラの自動調整 |
| render | `vk/context`, `gpu_scene`, `env_map`, `integrator`, `path_tracer`, `progressive`, `renderer`, `tonemapper` | Vulkan（表示は必要なときだけ）、BLAS/TLAS、環境マップの分布、積分器、累積の管理（`Progressive`：オフラインとビューアで共通）、表示用のトーンマッピング |
| io | `image_io` | HDR/EXR の読み込み、PNG（トーンマッピング）・EXR の書き出し |
| app | `config`, `kicad_cli`, `report`, `prepare`, `commands`, `viewer` | 設定ファイル、kicad-cli の呼び出し、missing_models の出力、シーンの準備（render / view 共通）、各コマンド、リアルタイム表示（GLFW、スワップチェーン、操作） |
| shaders | `pathtrace.comp`, `bsdf.glsl`, `env.glsl`, `sampling.glsl`, `common.glsl` | パストレーサー本体、BSDF、環境マップの重点的サンプリング |

### 座標系

- 基板座標: KiCad と同じ（mm、x 右・y 下）。
- ワールド: Y 上、`X = x − cx`、`Z = y − cy`（cx, cy は基板外形の中心）。基板は `Y ∈ [−t/2, t/2]` です。
- 部品の 3D モデルは KiCad 3D 座標（Z 上）で KiCad と同じ順に変換し、最後にワールドへ回します（`scene_builder.cpp` の `kicadModelMatrix` / `kicadToWorld`）。

### 基板面：凹凸の形状と材質（`geom/relief`）

銅箔・レジスト・シルクの厚みを、実際の形状として作ります（`board.toml` の `[relief]`）。
面ごとの多角形（Clipper2）から、次の領域を作り分けます。各領域は上面と、基材の面まで下ろした側壁を持ちます。

| 領域 | 基材の面からの高さ（既定） | 材質 |
|---|---|---|
| むき出しの基材 | 0 | FR4 |
| むき出しの銅（パッド） | 銅 35 µm（stackup） | 表面処理（ENIG / HASL / OSP） |
| 基材の上のレジスト | 25 µm | レジスト＋下に FR4（`Masked`） |
| 銅の上のレジスト | 35 + 12 µm | レジスト＋下に銅（`Masked`） |
| シルク | 下の面 ＋ 15 µm | シルク |

- 銅の上のレジストの上面が、板厚の半分（t/2）に来るようにしています。部品はそこに載ります。
- レジストでふさがれた（テンティング）ビアは、穴の上にもレジストの面を張ります。
- 材質の境界は形状そのもので決まるので、境界がぼやけません。
- `enabled = false` にすると、以前の層テクスチャ方式（平らな面に R=銅, G=レジスト開口, B=シルク を描き、材質を確率的に選ぶ）になります。

### 表面の微細な凹凸（シェーダ）

材質ごとに、法線を揺らすバンプと粗さのむらを付けて、のっぺりした CG らしさを抑えています（`shaders/noise.glsl`、`pathtrace.comp` の `surfaceAt`）。

| 材質 | 模様 |
|---|---|
| レジスト | ゆず肌状のゆるやかな凹凸。基材の上では、ガラス繊維の織り目がうっすら透ける |
| FR4 | 平織りの織り目（ピッチ 0.45 mm）と、糸・樹脂による色のむら。側面と穴の内壁は、水平に重なった層 |
| ENIG / HASL | 細かい粒状感 / はんだのゆるやかな盛り上がり |
| シルク | インクのざらつき |
| 部品（`model_map.toml` のプリセット） | 軽いむら |

- 強さは `board.toml` の `[surface] bump_intensity` で一括して調整できます（0 で無効）。
- 個別の材質では、`bump_pattern` / `bump_strength` / `bump_scale` / `roughness_variation` で調整できます。これは OpenPBR にはない krbr 独自のパラメータです。
- 模様はインスタンスごとにずらしているので、同じ部品が並んでも同じ模様にはなりません。

### ホタル対策

小さく明るい光源（HDR の中の電球など）が、拡散反射から光沢反射を経由して作る光の粒（ホタル）を抑えるため、**間接光だけ**に放射輝度の上限をかけています（既定 10、`--clamp` / `radiance_clamp`）。直接光のハイライトは制限しません。`0` にすると、偏りのない結果になります。

### 3D モデルが無い・読めない場合

モデルが見つからない、STEP の読み込みや分割に失敗した、のいずれでも処理は止めません。
部品の Courtyard（無ければパッドを囲む範囲）から箱を作り、仮の材質で表示します（高さは `board.toml` の `[placeholder] height_mm`）。
終了時に一覧をログ・`missing_models.txt`・`missing_models.json` に出します。

## 将来の拡張ポイント

| 拡張 | 入り口 |
|---|---|
| **BDPT / VCM** | `render/integrator.hpp` の `Integrator` を実装し、`createIntegrator()` に名前を足します。`GpuScene`（ジオメトリ・材質・TLAS・環境マップの分布）と `RenderTargets`（RGBA32F の累積、A=サンプル数）は共通なので、レンダラや出力側は変更不要です。光源側の経路や画面への投影の書き込み（splat）用のバッファは、積分器が自分で持ちます。 |
| **分光レンダリング** | BSDF は `shaders/bsdf.glsl` に、材質の取り出しは `shadeFromMaterial` / `boardShade` に閉じています。`Shade` を波長サンプルの値に置き換え、`envRadiance` をスペクトルに持ち上げる形で対応できます。材質のパラメータ名は OpenPBR に合わせています。 |
| **KiCad の保存を監視してリアルタイム更新** | ビューア（`app/viewer.cpp`）のループに、ファイルの変更検知を足します。解析（`kicad::loadBoard`）→ `scene::buildScene` → `GpuScene::build` は分かれていて、STEP の分割はキャッシュされます。変更時はシーンを作り直して `Progressive::setFrame` で累積を 0 からやり直せば済みます。 |
| **モデル形式の追加（VRML など）** | `model::IModelLoader` を実装して `ModelLibrary` に登録します。キャッシュはそのまま使えます。 |
| **材質の対応表の拡充** | `model_map.toml` の `[presets.*]` と `[[rules]]`（ファイル名のワイルドカード、HSV の条件）。 |
| **ノイズ除去** | `*_albedo.exr` / `*_normal.exr` を出しているので、OIDN などを後段に追加できます。 |

## 既知の制限（フェーズ1）

- 3D モデルは STEP のみ読み込みます。`.wrl` の指定は、同名の `.step` があれば差し替えます。無ければ代替の箱になります。
- パッドの `padstack`（層ごとに形が違うパッド）は既定の形だけを使います。内層は描きません。
- 線フォント（KiCad 標準のストロークフォント）の文字は `render_cache` を持たないため、kicad-cli の SVG 出力から取り込みます。kicad-cli が無ければ、その文字は描かれません（警告を出します）。
- レジストの縁は垂直な段差です（実物の、丸みのある縁や銅の縁でのなだらかな傾斜は再現していません）。
- 透明・半透明（LED のレンズなど）や透過は扱いません。
- プロセスの終了時に、NVIDIA ドライバの GPU コンテキストの解放で数秒かかることがあります。この間、CPU と GPU はほとんど使いません。
