# KRB-Renderer — KiCad 連携 物理ベース レンダラー

**KRB-Renderer**（KiCad 連携 物理ベース レンダラー）は、KiCad の `.kicad_pcb` を読み込み、HDR 環境光で照らした基板を、Vulkan のレイトレーシング（`VK_KHR_ray_query`）を使った GPU パストレーサーで描画するツールです。

コマンド名・設定フォルダ名・環境変数の接頭辞は、短い **`krbr`** のままです（`krbr.exe`、`<プロジェクト名>.krbr/`、`%APPDATA%\krbr\`、`KRBR_CONFIG_DIR` など）。リポジトリ名だけが KRB-Renderer です。

```
# リアルタイム表示（既定。ウィンドウが開く）
krbr E:\program\kachkojir_RK3506G2\kicad\kachkojir_RK3506G2.kicad_pcb --hdr E:\desktop\hdr\brown_photostudio_02_4k.exr

# ファイルへ出力（ウィンドウは開かない）
krbr render E:\program\kachkojir_RK3506G2\kicad\kachkojir_RK3506G2.kicad_pcb --hdr E:\desktop\hdr\brown_photostudio_02_4k.exr --spp 256 --width 1920 --height 1080 --out out/board.png
```

### リアルタイム表示（`krbr <pcb>` / `krbr view <pcb>`）

フレームごとにサンプルを積み増して表示します。1フレームに積むサンプル数は、GPU のタイムスタンプで測った時間が約 10 ms に収まるように自動で調整します。最大のサンプル数（既定 2^12 = 4096）まで積むと描画を止め、GPU を休ませて入力を待ちます。最大はパネルで 2 の何乗か（2^4 = 16 〜 2^16 = 65536）で選びます。`--spp` を指定すると、それ以上で最も近い 2 の累乗になります。ノイズはサンプル数の平方根でしか減らない（半分にするには 4 倍の時間）ので、1 段上げるごとの変化は小さくなります。

ウィンドウ左上の**操作パネル**（`F1` / `Tab` で表示を切り替え）で、次の項目をその場で変えられます。

| 項目 | 内容 |
|---|---|
| 環境マップ | 同じフォルダの `.hdr` / `.exr` から選ぶか、「開く...」でファイルを選ぶ。読み込みは別スレッドで行い、終わったら差し替える |
| 強さ・回転 | 環境マップの明るさと、縦軸まわりの回転 |
| 露出 | EV のスライダー。「自動」は、いまの画像の対数平均輝度を中間の灰色（0.18）に合わせる |
| 凹凸の強さ | 表面の微細な凹凸の倍率（`bump_intensity`） |
| レジスト | レジストの色のプリセット（「設定どおり」か、KiCad の色名／JLCPCB の色）。色・粗さ・散乱をまとめて切り替え、累積をやり直す。基板を読み直しても選択は保たれる |
| 表示 | 画像 / アルベド / 法線、最大 spp（2 の累乗） |
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
| `D` | ノイズ除去を今かける（ノイズ除去が CPU のときだけ意味がある。下記） |
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
  - ノイズ除去が **CPU** になるとき（CUDA が使えない GPU。GTX 1060 など）は 1 回に数秒かかり画面が止まるので、ビューアは**収束時（最大 spp に達したとき）と、`D` キー／パネルの「今かける」のときだけ**かけます（常に品質 high）。それ以外は蓄積中のノイズのある画像をそのまま表示します。
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

前提（いずれも PATH や環境変数で見つかること）:

| もの | 確認したもの | 備考 |
|---|---|---|
| Visual Studio 2022/2026 の C++ ツール | VS 2026 Build Tools 18.10（MSVC 14.51） | `scripts\build.cmd` が vswhere で探す。Community でも Build Tools でもよい |
| CMake 3.25 以上 | 4.2 | |
| Ninja | 1.13（`winget install Ninja-build.Ninja`） | VS 同梱 → PATH → winget の順で探す。Build Tools には同梱されないので別途入れる |
| Vulkan SDK（`glslc`） | 1.4.350（`VULKAN_SDK`） | |
| vcpkg | `VCPKG_ROOT`（無ければ `%USERPROFILE%\vcpkg`） | マニフェストの baseline のコミットがローカルの vcpkg にあること（無ければ `git -C %VCPKG_ROOT% pull`） |
| KiCad 10 | `C:\Program Files\KiCad\10.0` | 3D モデルの既定フォルダと kicad-cli（シルクの線フォント）に使う |

```
powershell -File scripts\get_oidn.ps1   # ノイズ除去（OIDN 2.5.1 の公式バイナリ）を third_party/oidn へ。無くてもビルドできる
scripts\build.cmd          # MSVC 環境を読み込み、低優先度で configure + build（初回は vcpkg の依存のビルドも行う。OpenCASCADE に時間がかかる）
scripts\build.cmd --clean  # build\release を消してやり直す（それ以外の引数は cmake --build に渡す）
build\release\krbr.exe probe
```

テストが描く基板と環境マップは CMake のキャッシュ変数で指定します（既定はこの PC のもの。他の PC では `cmake -D` で差し替える）。

| 変数 | 既定値 |
|---|---|
| `KRBR_TEST_PCB` | `E:/program/kachkojir_RK3506G2/kicad/kachkojir_RK3506G2.kicad_pcb` |
| `KRBR_TEST_HDR` | `E:/desktop/hdr/brown_photostudio_02_4k.exr` |

依存ライブラリは `vcpkg.json`（マニフェストモード）で `build/vcpkg_installed` に入ります。
release だけをビルドする独自トリプレット `triplets/x64-windows-rel.cmake` を使います（OpenCASCADE のビルド時間を半分にするため）。

| ライブラリ | 用途 |
|---|---|
| volk, Vulkan-Headers, VulkanMemoryAllocator | Vulkan（ローダーは動的読み込み） |
| GLFW 3.4 | リアルタイム表示のウィンドウと入力 |
| Intel Open Image Denoise 2.5.1 | ノイズ除去。公式のビルド済みバイナリ（Apache-2.0）を `third_party/oidn` に置く（`scripts/get_oidn.ps1`、git には含めない） |
| Dear ImGui 1.92 | ビューアの操作パネル。バックエンド（GLFW / Vulkan）は volk を使うよう `third_party/imgui_backends` を自前でビルド（MIT） |
| OpenCASCADE 8.0（TKDESTEP / TKXCAF / TKMesh / TKRWMesh） | STEP の読み込みと三角形分割（面の色つき）。**変換器 DLL `step\krbr_step.dll` の中だけで使い**、本体はリンクしない。OpenCASCADE の DLL 一式も `step\` にまとめる |
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
| `krbr mask list` | レジストの色のプリセット一覧（KiCad の色名と、JLCPCB の選択肢） |
| `krbr material show <pcb> [--ref U3]` | 部品・部位ごとの材質と、どの段階で決まったか |
| `krbr material set <pcb> --ref U3 [--part K] --material M` | 部品番号ごとの上書き（プロジェクトの overrides.toml） |
| `krbr material set <pcb> --model <パス/ファイル名> [--part K] --material M [--global]` | 3D モデルごとの設定（プロジェクト、または `--global` で全体設定） |

`render` の主なオプション: `--spp`（既定 64）, `--width/--height`, `--elevation`（既定 35°）, `--azimuth`（既定 45°）, `--fov`, `--look-at X,Y`（KiCad の座標の点を注視）, `--zoom`（自動の構図からの拡大率）, `--exposure`, `--env-intensity`, `--env-rotation`, `--max-depth`, `--clamp`, `--spectral` / `--rgb`（色の計算）, `--mask-color <プリセット名 または r,g,b>`（レジストの色。下記）, `--lens pinhole|thin|realistic`, `--f-number`, `--focal-length`, `--focus-distance`, `--blades`, `--init`, `--no-cache`, `--no-kicad-cli`, `--debug-layers`（層テクスチャを PNG で出力）, `--validate`, `--normal-priority`, `-v`。

## GPU の要件と、この PC（GTX 1060）での状況

描画には Vulkan 1.3 と `VK_KHR_acceleration_structure` に加えて、**`VK_KHR_ray_query` か `VK_KHR_ray_tracing_pipeline`** のどちらかが必要です。交差判定は `shaders/scene.glsl` の `rawTraceClosest` / `rawOccluded` に閉じていて、GPU に応じて次のどちらかで作ります（本文のシェーダは同じ。`shaders/stage.glsl` で切り替え）。

| 方式 | 使う GPU | 仕組み |
|---|---|---|
| ray query（優先） | RTX など `ray_query` を持つもの | 計算シェーダの中で `rayQueryEXT` を辿る（`<名前>.comp.spv`） |
| ray tracing pipeline | GTX 10 系など `ray_query` が無いもの | 同じ本文をレイ生成シェーダとして組み（`<名前>.rgen.spv`、`-DKRBR_RT_PIPELINE`）、`traceRayEXT` → 最近接ヒット `rt_hit.rchit`（交点の生の情報をペイロードへ）／ミス `rt_miss.rmiss`／影用ミス `rt_shadow.rmiss`。再帰はせず、シェーダバインディングテーブルは `render/kernel.cpp` が組む |

| GPU | 状況 |
|---|---|
| RTX 3080 Ti（開発元の PC） | ray query。README の時間の数値はここで測ったもの |
| GTX 1060 6GB（2026-10 からのこの PC、ドライバ 581） | `ray_query` なし → ray tracing pipeline。RT コアが無いのでドライバのエミュレーションで、RTX より大きく遅い（下記） |

`krbr probe` でどちらになったかを表示します（`GPU: ... ray tracing pipeline (no ray_query)`）。

GTX 1060 での実測（kachkojir_RK3506G2、135 万三角形、PT、深さ 8）：

| 項目 | 時間 |
|---|---|
| `render` 640x360 | 40 ms/spp |
| `render` / `watch` 1920x1080 | 約 180〜200 ms/spp（32 spp で 6 秒前後） |
| ノイズ除去 1080p（OIDN、CPU の high） | 6〜8 秒 |
| 保存から画像の更新（watch テスト、32 spp、ノイズ除去込み） | 13〜16 秒（RTX 3080 Ti では 0.63 秒） |

白い炉テスト・積分器の一致・プリズムの分散などの GPU のテストは、ray tracing pipeline 版でも ray query 版と同じ値で通ります（25 件すべて合格）。watch テストの「保存から 1 秒以内」は、`ray_query` を持つ GPU が無い PC では 30 秒に緩めています（仕組みの検証が目的のため）。

そのほか、この PC では OIDN の CUDA デバイス（Volta 以降が必要）が使えないので、ノイズ除去は CPU（Xeon E3-1290 V2、AVX2 なし）で行われ、1080p の high で 6〜8 秒かかります。VRAM が 6 GB なので、環境マップは 8k ではなく 4k を使ってください。

## STEP 変換器 DLL（`step\krbr_step.dll`）

STEP の読み込み・三角形分割だけが OpenCASCADE を必要とするので、その部分を C の API 1 つの DLL に切り出しています。

- `src/step_dll/krbr_step.cpp`：`krbr_step_to_mesh_file(step, 弦の許容誤差, 角度の許容誤差, 出力.krbm, err, len)`。OpenCASCADE に触るのはこのファイルだけ
- 出力は `.krbm`（`src/model/mesh_file.hpp`。頂点・法線・添字・三角形ごとの部位番号・部位の名前と色）。メッシュキャッシュと同じ形式
- 本体（`model/step_loader.cpp`）は、exe と同じフォルダの `step\krbr_step.dll` を必要になったときに読み込み、無ければ STEP を「読めない」扱い（代替の箱）で動きます。API と形式の版を照合します
- `build\release\` には本体と OIDN・GLFW などの DLL だけが残り、OpenCASCADE の 24 本の DLL は `build\release\step\` に入ります
- Rust 版もこの DLL を同じ API で呼びます

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

### レジストの色

レジストの色は次の順で決まります：`--mask-color` ＞ `board.toml` の `mask_color` ＞ `.kicad_pcb` の stackup の色（KiCad の基板設定で選んだ色）＞ 緑。

`mask_color` と `--mask-color` には、sRGB の値（`[r, g, b]` / `r,g,b`）のほかに**プリセット名**を書けます。プリセットは色だけでなく、コートの粗さ（光沢／つや消し）と顔料の散乱（透明さ）もまとめて設定します。一覧は `krbr mask list`。

| プリセット | 内容 |
|---|---|
| `green` `red` `blue` `yellow` `purple` `black` `white` `matte-black` | KiCad の stackup の色名と同じもの。stackup に色があれば自動でこれが使われる |
| `jlcpcb-green` `jlcpcb-purple` `jlcpcb-red` `jlcpcb-yellow` `jlcpcb-blue` `jlcpcb-white` `jlcpcb-black` `jlcpcb-matte-black` `jlcpcb-matte-green` | JLCPCB の注文時に選べる色。白・黒は不透明寄り、matte は粗さを上げてある |

ビューアでは操作パネルの「表面」→「レジスト」でその場で切り替えられます（材質だけを送り直すので一瞬で、累積はやり直し）。

プリセットはシルクの色も決めます：黄・白のレジストでは黒、それ以外では白（`silk_color` を書いていればそちらが優先）。

名前の大小・区切りは無視します（`JLCPCB Purple`、`jlcpcb:purple` も同じ）。メーカーの色は写真からの目分量で、実測ではありません。プリセットの後に `mask_roughness` / `mask_scatter` を書けば、その項目だけ上書きできます。

```
krbr render board.kicad_pcb --hdr studio.exr --mask-color jlcpcb-purple
```
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
- **透過（`transmission_weight`）**：ガラス・アクリルのような誘電体の屈折（粗さのある GGX の透過、Walter et al. 2007）。中では `transmission_color` / `transmission_depth` で吸収されます（depth が 0 なら、面を通るたびに色をかける）。閉じた形状で、面の向き（三角形の巻き方）が外向きであることを前提にしています。
- **subsurface**：krbr では簡易な形で扱います。`subsurface_weight` の割合で光が拡散して中へ入り、`subsurface_radius`（mm）進むと `subsurface_color` の透過率になる吸収を受けながら進み、反対側の面やふちから拡散して出ます（FR4 の透光性に使用）。
- fuzz・thin_film・anisotropy・geometry_opacity・thin_walled・transmission_scatter は値を GPU まで渡していますが、描画には反映していません（使われるとログで知らせます）。発光（`emission_luminance`、cd/m²）は、放射輝度 1.0 = 1000 cd/m² として描きます。発光は片面（三角形の巻き方で決まる表側）から、余弦分布で放たれます。発光する三角形は光源として直接サンプリングします（輝度 × 面積に比例して選ぶ）。

## Rust 版（移植中）

`rust/` に Rust への移植を進めています（cargo のワークスペース。`rust-toolchain.toml` で MSVC の stable を指定）。シェーダ（GLSL → SPIR-V）はそのまま使い、C++ の本体を順に置き換えます。設定・キャッシュの場所と XXH64 のキーは C++ 版と同じで、メッシュキャッシュを共有します。

```
cd rust
cargo build --release
target
elease\krbr.exe info E:\program\kachkojir_RK3506G2\kicad\kachkojir_RK3506G2.kicad_pcb --models
cargo test
```

| 段階 | 内容 | 状況 |
|---|---|---|
| 1 | S 式パーサ、基板の構造（`krbr-kicad`）、3D モデルのパス解決、`info` | 済（C++ 版と同じ結果：250 部品・234 モデル解決） |
| 2 | STEP → メッシュの変換器を C++ の薄い DLL（`stepkrbr_step.dll`）に切り出し、Rust はキャッシュと DLL だけを読む（`krbr-model`、`krbr models`） | 済（C++ 版と同じキャッシュを共有：38 モデル・21 万三角形） |
| 3 | 基板メッシュ、凹凸タイル、層の多角形、距離場（`krbr-geom`：Clipper2・earcut の crate、`krbr geom`） | 済（外形・穴の数は C++ 版と一致。凹凸は 168 万三角形、C++ 版 150 万：Clipper2 の丸め継ぎ目の分割数の違い） |
| 4 | 描画（ash：デバイス、BLAS/TLAS、カーネル、累積、トーンマップ）。`render` が C++ 版と同じ画像になることを確認 | 未 |
| 5 | ビューア（winit + egui）、監視、ノイズ除去（OIDN）、材質 CLI | 未 |

## テスト

```
build\release\krbr_tests.exe
```

doctest で、次をウィンドウを開かずに確かめます。基板を使うテストは `KRBR_TEST_PCB` の基板をコピーして使い、部品番号や座標は基板の中から選びます（KiCad 標準の色を持つ IC、`C_0603_1608Metric` のコンデンサ）。

- 材質の TOML の往復、アッベ数からの屈折率、材質ライブラリの重ね方
- 解決順序の5段階、パス・ハッシュでの照合、部位ごとの指定
- フェーズ3：基板のコピーを書き換え、保存から画像の更新まで 1 秒以内であること、作り直すタイルが周辺だけであること、変更のない保存では何も作り直さないこと、未分割のモデルは箱で先に更新し裏での分割後に差し替わること
- フェーズ2の完了条件：一時フォルダに2つのプロジェクトを作り、全体設定で U2 の材質を変えると両方に反映されること、部品番号の上書きが優先されること（全体設定は `KRBR_CONFIG_DIR` で一時フォルダに向ける）
- 距離場：半平面と 1 点からの距離、8bit への詰め方
- 分光の土台：等色関数と白色点、RGB → スペクトルの往復（誤差 0.008 以下、値が 0..1 に収まること）、アッベ数からの屈折率
- **プリズムの分散**（フェーズ5の完了条件）：下記
- **BDPT / VCM の白い炉テスト**：上の主な材質（拡散・金属・誘電体・subsurface・ガラス・分光のアクリル）で、PT と同じ値（差 0.003 以下）
- **積分器の一致**（フェーズ6）：面光源・相互反射・光沢のある金属球のシーンを PT（2048 spp）・BDPT・VCM（1024 回）で描いて比べます。平均は 0.09825 / 0.09823 / 0.09829、4x4 画素ごとの相対的な差は PT どうし（種だけ違う）0.010、BDPT 0.009、VCM 0.017
- **集光模様の収束**（フェーズ6の完了条件）：下記
- **分光モードの色の保存**：色のついた拡散を白い炉の中で RGB / 分光の両方で描き、チャンネルごとの差が 0.02 + 3% 以内であること（実測 0.0003 以下）
- **白い炉テスト**：一様な白い環境光の中の球をオフスクリーンで描き、吸収のない材質で放射輝度が 1 になることを確かめます。エネルギーが増える（1 を超える）のは誤りとして扱います。粗さ 1 の金属は、単一散乱の GGX の理論値（数値積分で 0.409）と一致することを確かめます。

| 材質 | 平均 | 備考 |
|---|---|---|
| Lambert（鏡面なし） | 0.9999 | 理想値 1 |
| 完全な鏡 | 0.9998 | 理想値 1 |
| 金属 粗さ 0.3 | 0.977 | 単一散乱による損失 |
| 金属 粗さ 1.0 | 0.413 | 理論値 0.409（フェーズ8で多重散乱を補償する） |
| 拡散の透過（吸収なし） | 0.9994 | 理想値 1（基材の透光性の仕組み） |
| ガラス（粗さ 0） | 0.9998 | 理想値 1（反射＋屈折） |
| ガラス 粗さ 0.3 | 0.917 | 単一散乱による損失（中から当たったときの全反射が多い） |
| Lambert（分光） | 0.9996 | 理想値 1 |
| アクリル（分光、分散あり） | 0.9994 | 理想値 1（代表波長以外を打ち切っても偏りがない） |
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
| scene | `scene_builder`, `model_map`, `camera`, `lens`, `raycast`, `material`, `spectrum` | 材質（OpenPBR の名前）、部品の配置（KiCad 3D ビューアと同じ変換の順序）、代替の箱、カメラの自動調整、等色関数と RGB → スペクトルの表 |
| render | `vk/context`, `gpu_scene`, `env_map`, `integrator`, `kernel`, `path_tracer`, `progressive`, `renderer`, `tonemapper` | Vulkan（表示は必要なときだけ）、BLAS/TLAS、環境マップの分布、積分器、累積の管理（`Progressive`：オフラインとビューアで共通）、表示用のトーンマッピング |
| io | `image_io` | HDR/EXR の読み込み、PNG（トーンマッピング）・EXR の書き出し |
| app | `config`, `kicad_cli`, `report`, `prepare`, `commands`, `viewer` | 設定ファイル、kicad-cli の呼び出し、missing_models の出力、シーンの準備（render / view 共通）、各コマンド、リアルタイム表示（GLFW、スワップチェーン、操作） |
| shaders | `pathtrace.comp`, `vcm_light.comp`, `vcm_camera.comp`, `camera.glsl`, `ptcore.glsl`, `bidir.glsl`, `scene.glsl`, `lights.glsl`, `bsdf.glsl`, `spectral.glsl`, `env.glsl`, `sampling.glsl`, `common.glsl` | パストレーサー、BDPT / VCM の光源側・カメラ側、交点と材質の組み立て、光源（環境光・発光面）、BSDF、分光、環境マップの重点的サンプリング |

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
- レジストでふさがれた（テンティング）ビアは、穴の上にもレジストの面を張ります。実物ではレジストが穴に落ち込んで浅いくぼみになるので、ドリル径の円に余弦断面のすり鉢（既定の深さ 50 µm、`via_dimple_um`。0 で平ら）を作ります。KiCad の基板設定でテンティングを「いいえ」にしたビア（または `(tenting none)` のビア）は穴が開いたままです。
- 材質の境界は形状そのもので決まるので、境界がぼやけません。
- `enabled = false` にすると、以前の層テクスチャ方式（平らな面に R=銅, G=レジスト開口, B=シルク を描き、材質を確率的に選ぶ）になります。

### 基板専用の材質（フェーズ4）

**レジストの厚み t(x)**：タイルごとに、銅箔とレジスト開口の縁からの符号付き距離場（20 µm/画素、±0.3 mm、厳密なユークリッド距離変換）を求め、RG8 のテクスチャにします（`raster/board_sdf`、保存のたびに変わったタイルの範囲だけを GPU に送る）。シェーダはそこから厚みを決めます（`maskThickness`）。

- 銅の上：縁では流れて薄く（4 µm）、縁から離れると設定の厚み（12 µm）に近づく（距離 0.06 mm で変化）
- 基材の上：銅の壁ぎわにたまって厚く（25 µm ＋ 最大 10 µm）
- レジスト開口の縁に向かっても薄くなる
- 表面の高さ（銅＋レジスト）の勾配で法線を傾け、銅の縁がレジストに覆われてなだらかになった様子（縁のハイライト）を出す

**Beer–Lambert**：レジストはコートとして、厚み t と、コートの中で屈折した後の角度での光路長（入射と出射の 2 回）から透過率 `exp(-σ t (1/cosθi + 1/cosθo))` を求めます。σ はレジストの色（基準の厚みを垂直に往復したときの透過率）から決めます。顔料による散乱の割合も厚みに応じて変えるので、縁の薄いところでは下の銅の色が透けて明るく見えます。

**FR4 の透光性**：むき出しの基材（とレジストの下の基材）では、一部の光（`subsurface_weight` 0.15）が拡散して基材の中へ入り、`subsurface_color` / `subsurface_radius` で吸収されながら進み、反対側の面やふちから拡散して出ます（出るときは、その面のレジストの吸収を受ける）。銅の面では中へはね返ります。白い炉テスト（吸収のない白）で 0.9994。

**織り目・側面・穴の内壁**：基材の上面の織り目、側面の層、めっきされた穴の内壁（表面処理の金属）は「表面の微細な凹凸」を参照。

### 表面の微細な凹凸（シェーダ）

材質ごとに、法線を揺らすバンプと粗さのむらを付けて、のっぺりした CG らしさを抑えています（`shaders/noise.glsl`、`pathtrace.comp` の `surfaceAt`）。

| 材質 | 模様 |
|---|---|
| レジスト | ゆず肌状のゆるやかな凹凸。基材の上では、ガラス繊維の織り目がうっすら透ける |
| FR4 | 平織りの織り目（ピッチ 0.45 mm）と、糸・樹脂による色のむら。側面と穴の内壁は、水平に重なった層 |
| ENIG / HASL | 細かい粒状感 / はんだのゆるやかな盛り上がり |
| シルク | インクのざらつき |
| 部品（`model_map.toml` のプリセット） | 軽いむら |

- 強さは `board.toml` の `[surface] bump_intensity` で一括して調整できます（既定 0.25 で、近づいて分かる程度。0 で無効）。レジストは光沢が強く、わずかな凹凸でも映り込みがまだらに揺れるので、個別にさらに弱くしています。
- 個別の材質では、`bump_pattern` / `bump_strength` / `bump_scale` / `roughness_variation` で調整できます。これは OpenPBR にはない krbr 独自のパラメータです。
- 模様はインスタンスごとにずらしているので、同じ部品が並んでも同じ模様にはなりません。

### 積分器：PT / BDPT / VCM（フェーズ6）

`--integrator pt|bdpt|vcm`（設定 `[render] integrator`、ビューアのパネルの「積分器」）で選びます。既定は PT です。

| | 向いている場面 | 速さ（1280×720 の基板、1 サンプル） |
|---|---|---|
| PT | ふつうの基板。ノイズ除去と相性がよい | 約 2.9 ms |
| BDPT | 小さな光源が、拡散面を経由して照らす室内のような場面 | 約 6.7 ms |
| VCM | 透明な筐体越しの基板、レンズで集まった光（集光模様） | 約 7.5 ms |

- **BDPT / VCM**：Georgiev ほか（2012）と SmallVCM の再帰的な MIS（balance heuristic）。1 回の繰り返しで、まず全画素ぶんの光源側の経路を作って、拡散・光沢の頂点を保存し、各頂点をカメラへ直接つなぎます（画面のバッファへ float の不可分加算で足し込む）。次に各画素のカメラ側の経路で、光源に当たる・光源への直接サンプリング・同じ番号の光源側の経路の全頂点への接続・（VCM のみ）近くの光源側の頂点とのまとめを、重みをつけて足します。
- **近傍のまとめ**：光源側の頂点を、セルの大きさ 2r の格子（ハッシュ、セルごとの連結リスト）に入れて探します。半径 r は繰り返すたびに r₀·i^(−0.125) で縮めます（α = 0.75）。r₀ は既定でシーンの大きさの 0.3%（`[render] vcm_radius` で指定）。
- **ほぼ鏡面の面**（透明な材質、粗さ 0.05 未満の金属）では、接続もまとめもせず、サンプリングだけで通り抜けます。
- **基材の中を通る光（subsurface）**は、カメラ側の経路だけで扱います（そこから先は PT と同じ方法で続ける）。光源側の経路は、基材の中へ入ったところで打ち切ります。
- **分光モード**：BDPT は経路ごと（同じ番号の光源側とカメラ側で共通）、VCM は繰り返しごとに全画素で共通の 4 波長を使います（別の経路の頂点とまとめるため）。代表波長以外を打ち切ったかどうかは両側の経路で持ち、つないだ寄与ではどちらかが打ち切っていれば 1 回だけ扱います。
- 光源側の頂点は、1 本の経路あたり平均 4 個、全体で 512 MB まで保存します（1080p で約 5.6M 個）。
- 光源の選び方（PT と共通）：環境光か発光面かを、シーンに届く放射束の見積もりに比例させて選びます（どちらも 10〜90% の範囲）。環境光は、シーンを囲む球を覆う円盤から入る光として、光源側の経路を始めます。
- ふつうの基板では、VCM はサンプル数が少ないうちは近傍のまとめによるまだらなムラが残り、ノイズ除去でも消えにくいので、PT の方が向いています。

**検証シーン**（`tests/test_integrators.cpp`）：アクリルの板（筐体のふた）越しに、アクリルのボールレンズが LED（3 mm 角の発光面）の光を床に集める様子を見ます。カメラ → 板 → 床 → レンズ → LED という、鏡面に挟まれた拡散面の経路（SDS）で、PT・BDPT ではほとんど収束しません。集光点のまわりで、種を変えて 2 回描いた画像の差（ノイズ）を比べます（256 spp / 回）。

| | ノイズ（相対 RMS） |
|---|---|
| PT | 0.93 |
| BDPT | 1.65 |
| VCM | 0.066 |

VCM（1024 回）の集光点のまわりの平均は 0.2023 で、PT（16384 spp）の 0.1974 と 2.5% 以内で一致します。画像は `out/p6_caustic_vcm_1024.png`、`out/p6_caustic_pt_16384.png` などです。

### カメラとレンズ（フェーズ7）

`environment.toml` の `[lens]`、`--lens`、ビューアのパネルの「レンズ」で選びます。既定はピンホールです。ビューアでは **Ctrl + クリック**した点にピントを合わせます（CPU で全三角形と交差を調べる）。

| 種類 | 内容 |
|---|---|
| pinhole | 被写界深度なし |
| thin | 薄レンズ。焦点距離・F 値（絞りの半径 = 焦点距離 / 2F）・ピントの距離。絞りの形は羽根の枚数・回転・丸み（多角形と円の間）。周辺減光（cos⁴θ）、口径食、軸上色収差・倍率色収差（分光モード） |
| realistic | 実際のレンズ設計データ（球面の並び、pbrt と同じ書式。内蔵は double Gauss 50mm f/2）で光線を追跡。最終出力向け |

- **薄レンズ**：絞りの中の点を一様に選び（円からの棄却法）、ピントの面（前方に垂直、距離 = ピントの距離）の点へ向かう光線にします。レンズ上の点を固定すると、画像の 1 画素あたりの確率密度はピンホールと同じ形（像面までの距離² / cos³θ）になるので、BDPT / VCM の「光源側の頂点をカメラへ直接つなぐ」戦略は、同じ分布でレンズ上の点を選び、ピントの面を経由して写る画素を求めるだけで使えます。
- **口径食**：絞りの前に、同じ大きさの円形の開口を置きます。画面の隅で、2 つの開口が絞りの直径 × `optical_vignetting` だけずれる距離です。画面の端のボケは 2 つの円の重なり（レモン形、キャッツアイ）になります。
- **軸上色収差**：焦点距離を f(λ) = f · (nd − 1) / (n(λ) − 1)（ガラスは nd 1.5168、アッベ数は `abbe_number`）とし、フィルムの位置は d 線で決めます。波長ごとにピントの距離が変わります。
- **倍率色収差**：像の大きさを 1 + k · (n(λ) − nd) / (nF − nC) 倍にします（k = `lateral_ca`）。
- 色収差は波長で光線が変わるので、カメラで代表波長以外を打ち切ります（色の粒が増えるので、サンプル数を多めに）。
- **実レンズ**：フィルムの点から後ろの面の有効径の中の点へ光線を出し、各面で屈折させ（有効径の外や全反射は光が通らない）、明るさは cos⁴θ · 面積 / 距離² を、フィルムの中央の平均で割って、ピンホールと同じ明るさにそろえます。ピントは光軸の近くの光線を追って求め、フィルムの大きさは、ピントの距離での倍率で設定の画角になるように決めます。BDPT / VCM では、カメラへ直接つなぐ戦略を使いません（その分の MIS の項を 0 にする）。

**検証**（`tests/test_camera.cpp`）：

| 項目 | 結果 |
|---|---|
| 羽根 6 枚のボケ：頂点の方向 / 辺の方向の半径（正六角形で 1.155） | 1.13（回転 30° でも頂点の向きが追従）、円形の絞りでは 1.00 |
| 画面の隅のボケ：中心方向 / 直交方向の広がり | 口径食 0.6 で 0.63（レモン形）、口径食なしで 1.00 |
| 自然な周辺減光：隅 / 中央 | 0.6718（cos⁴ の理論値 0.6716） |
| 軸上色収差（アッベ数 20）：青 / 赤のボケの半径 | 1.29（なしで 1.00） |
| 倍率色収差 0.01：青と赤の像のずれ（画面の端） | 3.5 画素（なしで 0.00） |
| 薄レンズでの積分器の一致：平均 | PT 0.07913 / BDPT 0.07924 / VCM 0.07924 |
| 実レンズ：一様な環境光での中央の明るさ | 0.99 |

完了条件の確認用の画像は `out/p7_showcase.png`（7 枚羽根、口径食、周辺減光、軸上・倍率色収差。ボケは暗いので +4 EV で保存）です。

### 分光レンダリング（フェーズ5）

`--spectral`（設定 `[render] spectral = true`、ビューアのパネルの「色の計算」）で切り替えます。既定は RGB です。

- **代表波長方式**：1 本のパスに 4 波長を載せます。代表波長を可視域に重みをつけて選び（pbrt-v4 と同じ分布、360〜830 nm）、残りの 3 つは 1/4 ずつずらして層化します。
- **分散**：分散のある透明な材質（アッベ数、または Sellmeier）に当たったら、代表波長の屈折率で方向を決め、残りの 3 波長を打ち切ります（代表波長の重みを 4 倍にするので偏りはない）。屈折率は Cauchy の式 n(λ) = A + B/λ²（nF・nC・nd から求める）で計算します。
- **RGB → スペクトル**：Jakob & Hanika（2019）のシグモイド多項式。係数の表（64³ × 3）は初回に約 1.5 秒で作り、キャッシュ（`%LOCALAPPDATA%\krbr\cache\spectral`）に置きます。材質の色は CPU で係数にして GPU へ送り、環境マップ（HDR）の色はシェーダで表を引きます。発光と環境光は上限のない色として扱います（最大の成分で割ってから変換し、倍率をかける）。
- **出力**：CIE 1931 の等色関数（Wyman ほか 2013 の解析的な近似）で XYZ にして累積します。白色点は等エネルギー白色（E）とし、表示のリニア sRGB（D65）へは Bradford の色順応で変換します。RGB(1,1,1) は平坦なスペクトルになり、そのまま白に戻ります。
- **ノイズ除去**は XYZ のままかけ（アルベドも XYZ にして渡す）、表示・保存の直前に sRGB に変換します。
- 速さは RGB モードとほぼ同じです（1280×720 の基板で 1 サンプル約 2.5 ms）。基板の見た目はほとんど同じですが、レジストの吸収（Beer–Lambert）を波長ごとに計算するので、緑がわずかに鮮やかになります。

**検証シーン**（`tests/test_prism.cpp`）：頂角 12° のくさび形のプリズム（上がアクリル nd 1.491・アッベ数 57、下がポリカーボネート nd 1.585・アッベ数 30）越しに、遠くの縦縞の光源を見ます。結果は `out/p5_prisms_spectral.png`（分光）と `out/p5_prisms_rgb.png`（RGB）です。

| | 赤と青のずれ（画素） |
|---|---|
| RGB アクリル / ポリカーボネート | 0.02 / 0.00 |
| 分光 アクリル | 13.5 |
| 分光 ポリカーボネート | 29.9 |
| 比 | 2.22（(nF − nC) の比からの期待値 2.26） |

### ホタル対策

小さく明るい光源（HDR の中の電球など）が、拡散反射から光沢反射を経由して作る光の粒（ホタル）を抑えるため、**間接光だけ**に放射輝度の上限をかけています（既定 10、`--clamp` / `radiance_clamp`）。直接光のハイライトは制限しません。`0` にすると、偏りのない結果になります。

### 3D モデルが無い・読めない場合

モデルが見つからない、STEP の読み込みや分割に失敗した、のいずれでも処理は止めません。
部品の Courtyard（無ければパッドを囲む範囲）の大きさで、縁を面取りした透明な水晶（石英：nd 1.544、アッベ数 70）を置きます。下のパッドや配線が透けて見えます（高さは `board.toml` の `[placeholder] height_mm`、色は `color`：中を 5 mm 進んだときの透過の色）。面取りの幅はいちばん短い辺の 20%（最大 0.6 mm）です。PT では透明な物体は光源の直接サンプリングをさえぎるので、水晶の下はノイズが多めになります。
終了時に一覧をログ・`missing_models.txt`・`missing_models.json` に出します。

## 将来の拡張ポイント

| 拡張 | 入り口 |
|---|---|
| **積分器の追加** | `render/integrator.hpp` の `Integrator` を実装し、`createIntegrator()` に名前を足します。計算シェーダと記述子の組は `render/kernel` の `ComputeKernel` / `sceneBindings` で作れます。 |
| **KiCad の保存を監視してリアルタイム更新** | ビューア（`app/viewer.cpp`）のループに、ファイルの変更検知を足します。解析（`kicad::loadBoard`）→ `scene::buildScene` → `GpuScene::build` は分かれていて、STEP の分割はキャッシュされます。変更時はシーンを作り直して `Progressive::setFrame` で累積を 0 からやり直せば済みます。 |
| **モデル形式の追加（VRML など）** | `model::IModelLoader` を実装して `ModelLibrary` に登録します。キャッシュはそのまま使えます。 |
| **材質の対応表の拡充** | `model_map.toml` の `[presets.*]` と `[[rules]]`（ファイル名のワイルドカード、HSV の条件）。 |
| **ノイズ除去** | `*_albedo.exr` / `*_normal.exr` を出しているので、OIDN などを後段に追加できます。 |

## 既知の制限（フェーズ1）

- 3D モデルは STEP のみ読み込みます。`.wrl` の指定は、同名の `.step` があれば差し替えます。無ければ代替の箱になります。
- パッドの `padstack`（層ごとに形が違うパッド）は既定の形だけを使います。内層は描きません。
- 線フォント（KiCad 標準のストロークフォント）の文字は `render_cache` を持たないため、kicad-cli の SVG 出力から取り込みます。kicad-cli が無ければ、その文字は描かれません（警告を出します）。
- レジストの縁の丸みは法線（見え方）だけで表し、形状は垂直な段差のままです。
- PT では、透明な物体は光源の直接サンプリングの光をさえぎります。透明な物体越しの光（集光模様など）は収束が遅いので、VCM を使ってください。
- 透明な物体は、閉じた形状で三角形の巻き方が外向き（表から見て反時計回り）であることを前提にしています。
- プロセスの終了時に、NVIDIA ドライバの GPU コンテキストの解放で数秒かかることがあります。この間、CPU と GPU はほとんど使いません。
