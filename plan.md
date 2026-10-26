# KRB-Renderer（KiCad連携 物理ベース レンダラー。コマンド名は krbr）今後の予定

## 進め方（厳守）
- 各項目の開始時に、設計案（変更するモジュール、データ構造、テスト方法）を提示して承認を得てから実装する。一度に複数の項目へ手を付けない
- 小さな単位で動作確認とコミットを行う
- 許可が別途ない限り開発やテストの最中に GUI ウィンドウを開かない。動作確認はオフスクリーン描画とファイル出力で行う
- KiCad の GUI を起動しない。.kicad_pcb、KiCad のライブラリ、KiCad の設定ファイルは読み取り専用
- 出力した画像を自動で開かない。重い処理は低優先度で実行する
- 依存ライブラリを追加する前に、方法と導入先を提示して確認を取る

## 正しさの検証（全項目共通）
- 白い炉テスト（white furnace test）：一様な環境光の中で、エネルギーが保存されることを確認する
- 積分器の一致テスト：パストレーサー、BDPT、VCM が、同じシーンで同じ値に収束することを確認する（参照画像との差（RMSE）が閾値以下であることを自動テストにする）
- 分光モードとRGBモードの比較：分散なしのシーンで、両者の結果が許容誤差内で一致することを確認する

## C++ 版の廃止（済、2026-10-20）
C++ 版の本体は git の履歴にだけ残す（タグ cpp-final は 2026-10-26 に削除）。GPU の検証テストは rust/crates/krbr-render/tests/ に移し（C++ 版と同じ値）、シェーダは krbr-render の build.rs が作る。
C++ は STEP 変換器 DLL（step_dll/、CMake・vcpkg は OpenCASCADE と glm だけ）。層テクスチャ方式（[relief] enabled = false）は廃止した。
## 配布と KiCad 連携
- 配布の形を決める：ポータブル版の zip（scripts\make_dist.ps1）に加えて、インストーラ、または KiCad のプラグインマネージャ（PCM）のパッケージを作るか
- GitHub Releases への公開（公開の前に確認を取る）
- KiCad 11 で旧来の Action Plugin（SWIG の pcbnew）が無くなるので、IPC API のプラグイン（plugin.json）に移す

## V2：Ubuntu（Linux）対応（2026-10-26 決定。最初の公開は Windows 専用）
- 画面の部品（WebView）は、まず XWayland で動かす：Linux では winit を X11 で起動し、wry の子ウィンドウ（build_as_child は X11 のみ）で今と同じ作りにする。
  要るもの：GTK の初期化とフレームごとの GTK のイベント処理、表示範囲の切り抜き（Windows の SetWindowRgn）の X11 版（GDK の shape）
- ネイティブの Wayland は将来の課題：3D を Vulkan のサブサーフェスに描いて GTK の透明なウィンドウの下に敷く（入力とイベントループを GTK に移す）か、
  WebView を画面外で描かせて Vulkan で合成する（WPE WebKit / CEF の画面外描画。ウィンドウは 1 枚）
- ほかに Linux で要るもの：ビルド（webkit2gtk の開発用パッケージ、STEP 変換器の Linux 版）、動画の書き出し（今は Windows の Media Foundation だけ）、
  KiCad プラグインの登録、配布の形。描画の確認には GPU のある実機の Ubuntu が要る（WSL の Vulkan ではレイトレーシングがほぼ使えない）

## 実装予定機能リスト（Arnold / V-Ray / Corona の系統で、公開情報のある技術）
優先度の高い順に進める。各項目で、参考にする論文や公開実装を明示すること。
1. LightMix：光源グループごとに AOV を出力し、後から合成する。LED の点灯・消灯を、描画し直さずに切り替えられるようにする
2. 多数光源のサンプリング：ライト BVH（Conty Estevez & Kulla 2018）
3. パスガイディング：Open PGL の組み込み、または Practical Path Guiding（Müller 2017）。VCM と併用する。
   ガラス越しの光源（代替の箱の下の太陽など）を少ないサンプルで正しい明るさにする候補の一つ。もう一つの候補は
   MNEE（Manifold Next Event Estimation、Hanika et al. 2015。Blender Cycles の shadow caustics）：屈折面を通って光源へつながる経路をニュートン法で解き、直接サンプリングする（滑らかな形なら任意）
4. マイクロファセットの多重散乱のエネルギー補償（Kulla & Conty 2017）
5. 適応サンプリングと、ブルーノイズによる誤差の分散
6. ランダムウォーク SSS のアルベド反転（Chiang 2016）
7. 薄膜干渉（Belcour & Barla 2017）、sheen
8. 絞りの形に基づくグレアとブルーム（絞りの形を FFT して光芒を作る。カメラの絞りの設定と共通化する）
9. 物理モデルによる空（Hosek–Wilkie、Prague sky model）
10. トーンマッピング（AgX。ACES は実装済み）
11. Cryptomatte と AOV の出力
12. プレビュー用のラディアンスキャッシュ（偏りのあるモードとして明示する）
