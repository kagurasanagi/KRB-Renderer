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

### 保存の監視と差分更新（フェーズ3）

ビューア（`--no-watch` で無効）と `krbr watch` は、`.kicad_pcb` の保存を見張って自動で読み直します。

- 更新日時とサイズを 100 ms ごとに調べ、変化が 200 ms 止まってから読みます（書き込み・置き換えの途中では読まない）。
- 部品・配線・ビア・ベタ・図形ごとに、UUID と S 式のハッシュと範囲を持ち、前回と比べて**変わった項目の新旧の範囲**だけを作り直します（`kicad::diffBoards`）。外形や層構成が変わったときは全体を作り直します。
- 凹凸メッシュは 8 mm 角のタイルに分けて持ち（`scene::ReliefTileCache`）、範囲にかかるタイルだけを並列で作り直します。部品を 1 つ動かすと、200 枚のうち 4 枚程度です。
- GPU の形状（BLAS/TLAS）はまとめて作り直します（数十 ms）。環境マップは送り直しません。
- キャッシュにない 3D モデルは、まず箱で表示し、裏の 1 本のスレッドで分割し終えたら差し替えます。
- 線フォントのシルクの文字が変わったときは、kicad-cli による描き直しを裏で行い、それまでは前の文字のままです。
- `krbr watch` は、トーンマッピングを GPU で行い、圧縮しない PNG（書き出しが速い。1080p で約 8 MB）を一時ファイル経由で置き換えます。

計測（1920x1080、32 spp、部品を 1 つ動かして保存）：保存から**ノイズ除去済みの**画像の書き出しまで **約 0.63 秒**（検出と待ち合わせ約 0.21、読み直しと作り直し 0.17（タイル 4 枚）、GPU 0.08、描画とノイズ除去 0.08（うちノイズ除去 0.02）、書き出し 0.09）。

### ノイズ除去（OIDN）

`render/denoiser` が Intel Open Image Denoise（RT フィルタ、HDR、補助情報としてアルベドと法線）を使います。

- Vulkan のデバイスと同じ GPU（UUID で選ぶ）の CUDA デバイスで動かし、Vulkan のバッファを外部メモリ（Win32 ハンドル）として OIDN と共有します。CPU を経由する転送はありません。共有できない環境では CPU デバイスに切り替えます。
- 累積画像をサンプル数で割って入力を作るシェーダ（`denoise_prep.comp`）→ OIDN → 結果を画像へ写し、表示用のトーンマッピングにつなぎます。
- ビューア：既定で有効（パネルで切り替え）。カメラを動かしている間は品質 fast で毎フレーム、止まったら high で 150 ms ごと、収束時にもう一度。1080p の high で約 20 ms、720p の fast で約 4 ms（RTX 3080 Ti）。
- `krbr watch`：既定で有効（high）。`krbr render --denoise` で `<名前>_denoised.png / .exr` も書き出します。`--no-denoise` で無効にできます。

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
powershell -File scripts\get_oidn.ps1   # ノイズ除去（OIDN 2.5.1 の公式バイナリ）を third_party/oidn へ。無くてもビルドできる
scripts\build.cmd          # MSVC 環境を読み込み、低優先度で configure + build
build\release\krbr.exe probe
```

依存ライブラリは `vcpkg.json`（マニフェストモード）で `build/vcpkg_installed` に入ります。
release だけをビルドする独自トリプレット `triplets/x64-windows-rel.cmake` を使います（OpenCASCADE のビルド時間を半分にするため）。

| ライブラリ | 用途 |
|---|---|
| volk, Vulkan-Headers, VulkanMemoryAllocator | Vulkan（ローダーは動的読み込み） |
| GLFW 3.4 | リアルタイム表示のウィンドウと入力 |
| Intel Open Image Denoise 2.5.1 | ノイズ除去。公式のビルド済みバイナリ（Apache-2.0）を `third_party/oidn` に置く（`scripts/get_oidn.ps1`、git には含めない） |
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
| `krbr watch <pcb> --hdr <file> [--out out/watch.png] [--spp 32]` | ヘッドレス：保存のたびに描き直して PNG を書き出す（ウィンドウは開かない） |
| `krbr material list` | 材質ライブラリの一覧 |
| `krbr material show <pcb> [--ref U3]` | 部品・部位ごとの材質と、どの段階で決まったか |
| `krbr material set <pcb> --ref U3 [--part K] --material M` | 部品番号ごとの上書き（プロジェクトの overrides.toml） |
| `krbr material set <pcb> --model <パス/ファイル名> [--part K] --material M [--global]` | 3D モデルごとの設定（プロジェクト、または `--global` で全体設定） |

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
| `<プロジェクト>/<プロジェクト名>.krbr/overrides.toml` | 材質の上書き（部品番号ごと・3D モデルごと） |
| `%APPDATA%\krbr\materials\*.toml` | 名前付きの材質（OpenPBR）。組み込みの材質に、書いた項目だけ重ねる |
| `%APPDATA%\krbr\model_map.toml` | 3D モデルごとの材質（他のプロジェクトと共有）と、色・名前の規則 |
| `%LOCALAPPDATA%\krbr\cache\meshes\` | STEP の分割結果のキャッシュ（キーはファイル内容の XXH64 ＋分割パラメータ） |
| `%LOCALAPPDATA%\krbr\cache\silk\` | kicad-cli によるシルク SVG のキャッシュ |

ファイルが無ければ既定値を使います。優先順位は「コマンドライン ＞ 設定ファイル ＞ 既定値」です。
全体設定とキャッシュの場所は、環境変数 `KRBR_CONFIG_DIR` / `KRBR_CACHE_DIR` で差し替えられます（テストで使用）。

### 材質システム（フェーズ2）

材質は OpenPBR Surface のパラメータで書きます（`scene/material.hpp`、色はリニア）。分散は `transmission_dispersion_abbe_number` と `transmission_dispersion_scale` で指定し、`specular_ior` を nd として扱います。アッベ数で表せない材質は、`[material.<名前>.krbr]` に Sellmeier の係数（`sellmeier_b`, `sellmeier_c`）を書けます。

```toml
# %APPDATA%\krbr\materials\my.toml
[material.epoxy_matte]
base_color = [0.02, 0.02, 0.02]
specular_roughness = 0.6
[material.epoxy_matte.krbr]
bump_pattern = "noise"
bump_strength = 0.015
bump_scale = 0.1
```

3D モデルの部位（STEP の XDE の名前、または面の色）ごとに、次の順で材質を決めます。

1. 部品番号ごとの上書き（プロジェクトの `overrides.toml` の `[ref.U3]`）
2. 3D モデルごとの上書き（プロジェクトの `overrides.toml` の `[[model]]`）
3. 3D モデルごとの設定（全体の `model_map.toml` の `[[model]]`。他のプロジェクトと共有）
4. 色・名前の規則（全体の `model_map.toml` の `[[rule]]`、次に組み込みの規則）
5. 既定値

- 3D モデルは、展開前のパス（`path`）か、ファイル内容の XXH64（`hash`）のどちらかが一致すれば同じとみなします。ファイルを別の場所に移しても、中身が同じなら設定が効きます。
- 部位のキーは、XDE の名前、`kicad:<色名>`（KiCad 標準モデルの色。例 `kicad:metal_grey_pins`、`kicad:black_body`）、`#rrggbb`（STEP に書かれた色）です。`"*"` は全部位に一致します。
- 材質ライブラリで `base_color` を書かない材質は、3D モデルの面の色を使います。
- transmission・subsurface・fuzz・thin_film・anisotropy・geometry_opacity は値を GPU まで渡していますが、描画にはまだ反映していません（使われるとログで知らせます。transmission と分散はフェーズ5）。発光（`emission_luminance`、cd/m²）は、放射輝度 1.0 = 1000 cd/m² として描きます。

## テスト

```
build\release\krbr_tests.exe
```

doctest で、次をウィンドウを開かずに確かめます。

- 材質の TOML の往復、アッベ数からの屈折率、材質ライブラリの重ね方
- 解決順序の5段階、パス・ハッシュでの照合、部位ごとの指定
- フェーズ3：基板のコピーを書き換え、保存から画像の更新まで 1 秒以内であること、作り直すタイルが周辺だけであること、変更のない保存では何も作り直さないこと、未分割のモデルは箱で先に更新し裏での分割後に差し替わること
- フェーズ2の完了条件：一時フォルダに2つのプロジェクトを作り、全体設定で U2 の材質を変えると両方に反映されること、部品番号の上書きが優先されること（全体設定は `KRBR_CONFIG_DIR` で一時フォルダに向ける）
- **白い炉テスト**：一様な白い環境光の中の球をオフスクリーンで描き、吸収のない材質で放射輝度が 1 になることを確かめます。エネルギーが増える（1 を超える）のは誤りとして扱います。粗さ 1 の金属は、単一散乱の GGX の理論値（数値積分で 0.409）と一致することを確かめます。

| 材質 | 平均 | 備考 |
|---|---|---|
| Lambert（鏡面なし） | 0.9999 | 理想値 1 |
| 完全な鏡 | 0.9998 | 理想値 1 |
| 金属 粗さ 0.3 | 0.977 | 単一散乱による損失 |
| 金属 粗さ 1.0 | 0.413 | 理論値 0.409（フェーズ8で多重散乱を補償する） |
| 白い誘電体 粗さ 0.3 | 0.906 | 層の重ね方の近似による損失 |
| コートつき Lambert | 0.905 | 同上 |

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
