# KRB Renderer 開発者向け

ビルド・テスト・配布の作り方・コードの拡張の入り口です。内部の仕組みは [internals.md](internals.md)、使い方の詳細は [usage.md](usage.md)。

## ビルド

配布の zip を使うだけなら、以下はどれも要りません。

前提（いずれも PATH や環境変数で見つかること）:

| もの | 確認したもの | 用途・備考 |
|---|---|---|
| Rust（MSVC の stable） | `rust/rust-toolchain.toml` で指定 | 本体 |
| Vulkan SDK（`glslc`） | 1.4.350（`VULKAN_SDK`） | シェーダ（GLSL → SPIR-V）。`krbr-render` の `build.rs` が、環境変数 `GLSLC`、`%VULKAN_SDK%\Bin`、PATH の順に探す。GPU と共有する定数（結合の番号・上限など）は `shader_consts.rs` が正本で、`build.rs` が `-DKRBR_*` として glslc に渡すので、シェーダは `build.rs` を通してしかコンパイルできない（glslc を直接呼ぶと定数が無くてエラーになる） |
| Visual Studio 2022/2026 の C++ ツール | VS 2026 Build Tools 18.10（MSVC 14.51） | Rust のリンカと、STEP 変換器 DLL。`scripts\build.cmd` が vswhere で探す。Community でも Build Tools でもよい |
| CMake 3.25 以上、Ninja | 4.2、1.13（`winget install Ninja-build.Ninja`） | STEP 変換器 DLL だけ。Ninja は VS 同梱 → PATH → winget の順で探す（Build Tools には同梱されない） |
| vcpkg | `VCPKG_ROOT`（無ければ `%USERPROFILE%\vcpkg`） | STEP 変換器 DLL の OpenCASCADE と glm。マニフェストの baseline のコミットがローカルの vcpkg にあること（無ければ `git -C %VCPKG_ROOT% pull`） |
| KiCad 10 | `C:\Program Files\KiCad\10.0` | 3D モデルの既定フォルダと kicad-cli（線フォントの文字）。実行時に使う |

```
cd rust
cargo build --release                    # krbr.exe と、シェーダ（rust\target\release\shaders）
cd ..
scripts\build.cmd                        # STEP 変換器 DLL（build\release\step）。初回は vcpkg が OpenCASCADE をビルドするので時間がかかる
scripts\build.cmd --clean                # build\release を消してやり直す（それ以外の引数は cmake --build に渡す）。MSVC を更新した後はこれが要ることがある（古いコンパイラの設定が CMake のキャッシュに残るため）
powershell -File scripts\get_oidn.ps1    # ノイズ除去（OIDN 2.5.1 の公式バイナリ）を third_party\oidn へ。無くても動く
rust\target\release\krbr.exe probe
```

STEP 変換器 DLL と OIDN が無くても krbr.exe は動きます（STEP のモデルは代替の箱、ノイズ除去は使えない）。

| ライブラリ | 用途 |
|---|---|
| ash, gpu-allocator | Vulkan（ローダーは動的読み込み） |
| winit, egui（egui-winit, egui-ash-renderer）, rfd | ビューアのウィンドウ・入力と操作パネル、ファイルを開くダイアログ |
| clipper2, earcut | 多角形の和・差・オフセット、三角形分割 |
| exr, image | EXR/HDR の入出力、PNG 出力 |
| toml, serde_json, clap, glam, xxhash-rust | 設定・レポート・コマンドライン・数学・キャッシュのキー |
| libloading | STEP 変換器 DLL と OIDN を実行時に読む |
| flate2 | VRML の `.wrz`（gzip）の展開（`png` も使っているので、同じ版が 1 つ入る） |
| Intel Open Image Denoise 2.5.1 | ノイズ除去。公式のビルド済みバイナリ（Apache-2.0）を `third_party/oidn` に置く（`scripts/get_oidn.ps1`、git には含めない）。実行時に読む |
| OpenCASCADE 8.0（TKDESTEP / TKXCAF / TKMesh / TKRWMesh） | STEP の読み込みと三角形分割（面の色つき）。**変換器 DLL `krbr_step.dll` の中だけで使い**、本体はリンクしない（下記） |

## STEP 変換器 DLL（`krbr_step.dll`）

STEP の読み込み・三角形分割だけが OpenCASCADE（C++ のライブラリ）を必要とするので、その部分を C の API 1 つの DLL に切り出しています。リポジトリの C++ はこれだけです（`step_dll/`、`CMakeLists.txt`、`vcpkg.json`）。

- `step_dll/krbr_step.cpp`：`krbr_step_to_mesh_file(step, 弦の許容誤差, 角度の許容誤差, 出力.krbm, err, len)`。OpenCASCADE に触るのはこのファイルだけ
- 出力は `.krbm`（`step_dll/mesh_file.hpp`、Rust 側は `krbr-model` の `mesh_file.rs`。頂点・法線・添字・三角形ごとの部位番号・部位の名前と色）。メッシュキャッシュと同じ形式
- `scripts\build.cmd` で `build\release\step\` に、DLL と OpenCASCADE の DLL 一式（24 本）を作ります
- krbr.exe（`krbr-model` の `library.rs`）は、`dll\step\krbr_step.dll`、`step\krbr_step.dll`（どちらも exe と同じフォルダ）、開発中は `<リポジトリ>\build\release\step\krbr_step.dll` の順に探し（exe のフォルダから上へたどるので、`deps\` に置かれるテストの実行ファイルからも見つかる）、必要になったときに読み込みます。無ければ STEP を「読めない」扱い（代替の箱）で動きます。API と形式の版を照合します

## 配布

**配布用の一式**：`powershell -ExecutionPolicy Bypass -File scripts\make_dist.ps1`（ビルド済みなら `-NoBuild`。Rust のクレートのライセンスの全文を作るのに `cargo install cargo-about --locked --features cli` が要る。設定は `rust\about.toml`、書式は `scripts\rust_licenses.hbs`）で、`dist\KRB-Renderer-<版>-win64\` と、その zip を作ります（約 100 MB、zip で約 65 MB。`dist/` は git に入れない）。集めるのは `rust\target\release`（krbr.exe とシェーダ）、`build\release\step`（STEP 変換器）、`third_party\oidn\bin`（OIDN）、Visual Studio の再頒布用の Visual C++ ランタイムです。フォルダごとどこへ置いても動きます（exe と同じフォルダのものだけを使う）。中身は次のとおりです。

| 中身 | 内容 |
|---|---|
| `krbr.exe` | 本体（アイコン付き） |
| `shaders\` | シェーダ（SPIR-V） |
| `dll\step\` | STEP 変換器（`krbr_step.dll`）と OpenCASCADE の DLL |
| `dll\oidn\` | ノイズ除去（`OpenImageDenoise*.dll`、`tbb12.dll`。CPU と CUDA のデバイス） |
| `dll\vcruntime\` | Visual C++ ランタイム（`msvcp140*.dll`、`vcruntime140*.dll`、`concrt140.dll` の再頒布用ファイル。インストール不要） |
| `KRB-Renderer.exe` | コンソールを出さずにビューアを開く起動役 |
| `bench\` | `krbr bench` の基板と、その 3D モデル |
| `README.txt`、`licenses\` | 使い方と、同梱物のライセンス（本体・OIDN・oneTBB・OpenCASCADE、krbr.exe に入る Rust のクレートの全文） |

直下には DLL を置きません。`krbr.exe` は Visual C++ ランタイムを静的に組み込み（`rust\.cargo\config.toml` の `+crt-static`）、起動の直後に `dll\vcruntime` を DLL の探し先に足してから、`dll\step`・`dll\oidn` の DLL を読みます（どちらも無ければ、従来の `step\`・exe の横、開発中の `build\release\step`・`third_party\oidn\bin` を探す）。

リポジトリの外に展開した zip だけで、空のキャッシュからの STEP の分割、ノイズ除去（CUDA）、kicad-cli のシルク、ビューア、プラグインのインストールが動くことを確かめています（リポジトリの中では、開発用の場所を探してしまうので、足りないファイルに気づけない）。

**シェーダの探し方**：環境変数 `KRBR_SHADER_DIR`、exe と同じフォルダの `shaders\`、ビルドの出力（`rust\target\<profile>\shaders`。`deps\` に置かれるテストの実行ファイル用）の順です。

**アイコン**：`krbr.exe` とビューアのウィンドウ、KiCad のツールバーのボタンは同じ絵です（元絵 `rust/krbr/assets/krbr_icon.png` から `scripts/make_icon.py` で各サイズを作り、`rust/krbr/assets/krbr.ico` を `build.rs` で exe に埋め込む）。

## テスト

```
cd rust
cargo test                                                                   # GPU を使わないもの
cargo test --release -p krbr-render -- --ignored --nocapture                 # GPU の検証（下記。約 6 分）
cargo test --release -p krbr-app --test watch -- --ignored --nocapture       # 保存の監視（GPU と KiCad の 3D モデルが要る）
cargo test --release -p krbr-app --test material -- --ignored --nocapture    # 材質の CLI（KiCad の 3D モデルが要る）
cargo test --release -p krbr-app --test denoise -- --ignored --nocapture     # ノイズ除去（GPU と OIDN の DLL が要る）
cargo test -p krbr-app --lib kicad_cli -- --ignored --nocapture             # 線フォントの文字の書き出し（kicad-cli が要る）
cargo test --release -p krbr-app --lib preview -- --ignored --nocapture      # 材質の見本を out/material_swatches.png に描く（目で確かめる）
cargo test --release -p krbr-model --test vrml_vs_step -- --ignored --nocapture # VRML と同名の STEP の比べ合わせ（下記）
cargo run --release -- material bake-swatches                                # 組み込みの材質の見本を焼き直す（下記）
```

組み込みの材質（`builtin_materials.toml`）・`Material` の項目・見本の描き方（`preview.rs`）を変えたら、`krbr material bake-swatches` で見本を焼き直します（RGB と分光の 106 枚を 16384 サンプル・OIDN で描いて `rust/crates/krbr-app/swatches/` に書き、使われなくなった画像を消す。RTX 3080 Ti で約 4 分。`--spp` で変えられる）。焼き直していないと `cargo test` の `baked_swatches_cover_the_builtin_materials` が失敗します（ビューアは、焼き付けが無い材質をその場で描くので動く）。

krbr-app のテスト（watch / material / denoise）が使う基板は、既定では `<リポジトリ>\kicad\mini_dsp_radio.kicad_pcb`（git に入れていない）で、環境変数 `KRBR_TEST_PCB` で差し替えられます。部品番号や座標は基板の中から選びます（KiCad 標準の色を持つ IC、`C_0603_1608Metric` のコンデンサ）。基板は読むだけで、書き換えるテストは一時フォルダのコピーを使います。

`vrml_vs_step` は、KiCad 8 の 3D ライブラリ（kicad-packages3D の 8.0.0。KiCad 9 以降は .wrl を配っていない）から取ってきた .wrl のフォルダを環境変数 `KRBR_TEST_VRML_DIR` で渡し、インストールした KiCad の同名の STEP と、外接箱（単位と向き）と KiCad の色の名前がそろうことを確かめます（STEP 変換器 DLL も要る）。

GPU・KiCad の 3D モデル・OIDN が要るテストは `#[ignore]` にしてあり、`--ignored` を付けたときだけ走ります。どれもウィンドウを開きません。GPU の検証のうち時間のかかる 2 つ（集光模様の PT 16384 spp の参照、カメラの確認用の画像 32768 spp）は別のテストに分けてあり、名前で外せます。画像は `out/p5_*.png`、`out/p6_*.png`、`out/p7_*.png` に書きます。

GPU を使わないもの（各 crate のユニットテスト）：

- 材質の TOML の往復、アッベ数からの屈折率、材質ライブラリの重ね方
- 金属のフレネルの式（正面・かすめる角度・k = 0 で誘電体と同じ）、測定値の表の引き方と色（金は赤・緑が高く青が低い、クロムは灰色に近い）、`conductor` / `category` の読み書き
- 組み込みの材質：どれにも分類があり、金属（錫を除く）は測定値、分散式の nd と `specular_ior` が合っていること。変種（`+color=` / `+rough=`）の組み立てと読み直し、`user.toml` の書き換え（ほかの材質を残す）
- ビューア：組み込みの材質・分類・測定値の金属・物質に英語の表示名があること。材質のパネルを描いただけでは値が変わらないこと（組み込みの材質のコピーが勝手にできない）、種類の切り替え、元に戻す・やり直す。組み込みの材質の見本がすべて焼き付けてあること
- 解決順序の5段階、パス・ハッシュでの照合、部位ごとの指定、全体設定と overrides の保存と読み込み
- S 式・基板の解析、差分、角の丸い四角形と白抜きの文字
- 距離場：半平面と 1 点からの距離、8bit への詰め方と範囲の上限
- 分光の土台：等色関数と白色点、RGB → スペクトルの往復（誤差 0.008 以下、値が 0..1 に収まること）
- VRML の読み込み（`vrml.rs`）：単位、DEF / USE、入れ子の Transform（center を含む）、凹んだ多角形の分割、creaseAngle の法線、ccw と鏡に映す配置での向き、ファイルの法線、PROTO などの読み飛ばし、VRML 1.0・壊れたファイルのエラー、.wrz。ライブラリのキャッシュ、resolver の候補の順
- はんだフィレット（`solder.rs`）：チップ部品の盛りの形と体積、部品を突き抜けないこと、モデル・ペースト・接地が無いときは作らないこと、裏面、スルーホールの円すい

GPU の検証（`rust/crates/krbr-render/tests/`。基準の値は以前の C++ 版から引き継いだもの）：

- **BDPT / VCM の白い炉テスト**：上の主な材質（拡散・金属・誘電体・subsurface・ガラス・分光のアクリル）で、PT と同じ値（差 0.004 以下）
- **分光モードの色の保存**：色のついた拡散を白い炉の中で RGB / 分光の両方で描き、チャンネルごとの差が 0.02 + 3% 以内であること（実測 0.0015 以下）
- **測定値の金属**：金・銅・クロム・オスミウム・真鍮（粗さ 0.3）を白い炉の中で RGB（F0 と F82-tint）・分光（n, k から厳密なフレネル）・分光の BDPT で描き、チャンネルごとの差が 0.01 + 3% 以内であること（実測：金の R で 3%、ほかは 1% 程度）。F82-tint を 0.7 にすると Schlick より暗くなること
- **積分器の一致**（`integrators.rs`）：面光源・相互反射・光沢のある金属球のシーンを PT（2048 spp）・BDPT・VCM（1024 回）で描いて比べます。平均は 0.09821 / 0.09815 / 0.09820、4x4 画素ごとの相対的な差は PT どうし（種だけ違う）0.010、BDPT 0.009、VCM 0.017
- **集光模様の収束**（`integrators.rs`）：[内部の仕組み](internals.md)の「積分器」
- **プリズムの分散**（`prism.rs`）：[内部の仕組み](internals.md)の「分光レンダリング」
- **カメラとレンズ**（`camera.rs`）：[内部の仕組み](internals.md)の「カメラとレンズ」
- **背景を透明にする**（`background.rs`、`glow.rs`）：PT・BDPT・VCM で、四隅は色もアルファも 0、物の上はアルファ 1 で背景を描いたときと同じ色、縁は半透明。保存の PNG（CPU）と表示（GPU）のアルファ付きのトーンマッピングが 2/255 以内で一致。画像は `out/bg_*.png`

**白い炉テスト**（`furnace.rs`）：一様な白い環境光の中の球をオフスクリーンで描き、吸収のない材質で放射輝度が 1 になることを確かめます。エネルギーが増える（1 を超える）のは誤りとして扱います。粗さ 1 の金属は、単一散乱の GGX の理論値（数値積分で 0.409）と一致することを確かめます。

| 材質 | 平均 | 備考 |
|---|---|---|
| Lambert（鏡面なし） | 0.9997 | 理想値 1 |
| 完全な鏡 | 0.9998 | 理想値 1 |
| 金属 粗さ 0.3 | 0.977 | 単一散乱による損失 |
| 金属 粗さ 1.0 | 0.413 | 理論値 0.409（多重散乱の補償は今後の予定。`plan.md`） |
| 拡散の透過（吸収なし） | 1.0004 | 理想値 1（基材の透光性の仕組み） |
| ガラス（粗さ 0） | 0.9993 | 理想値 1（反射＋屈折） |
| ガラス 粗さ 0.3 | 0.917 | 単一散乱による損失（中から当たったときの全反射が多い） |
| Lambert（分光） | 1.0003 | 理想値 1 |
| アクリル（分光、分散あり） | 0.9991 | 理想値 1（代表波長以外を打ち切っても偏りがない） |
| 白い誘電体 粗さ 0.3 | 0.906 | 層の重ね方の近似による損失 |
| コートつき Lambert | 0.905 | 同上 |

## 拡張ポイント

今後の予定は `plan.md` にあります。コードの拡張の入り口は次のとおりです。

| 拡張 | 入り口 |
|---|---|
| **積分器の追加** | `krbr-scene` の `config.rs` の `IntegratorKind` に種類と名前を足し（設定ファイル・コマンドライン・ビューアの選択肢はここから作る）、`krbr-render` の `integrator.rs` の `Integrator` を実装して `create_integrator` に足します。計算シェーダと記述子の組は `kernel.rs` の `Kernel` / `scene_bindings` で作れます。シェーダは `krbr-render` の `build.rs` の一覧に足します。 |
| **モデル形式の追加** | `krbr-model` の `library.rs` の `Format`（拡張子で判定）と `load_model`・`loader_key`（キャッシュのキー）に形式を足し、`krbr-kicad` の `resolver.rs` の候補の順に拡張子を足します（VRML の `vrml.rs` が例）。キャッシュ（`.krbm`）はそのまま使えます。 |
| **材質の対応表の拡充** | `materials/*.toml` の `[material.<名前>]`（材質ライブラリ）と、`model_map.toml` の `[[rule]]`（ファイル名のワイルドカード、HSV の条件）。 |
