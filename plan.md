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

## 次の作業（最優先）：C++ 版の廃止
Rust 版が C++ 版の機能（層テクスチャ方式を除く）をすべて持つので、C++ 版の本体を外し、シェーダの変更を 1 つのホストだけで済むようにする。
段階ごとに設計案を出して承認を得てから進める。
1. （済）GPU の検証テスト（白い炉、積分器の一致、プリズムの分散、カメラ、分光の色の保存）を Rust に移植し、C++ 版と同じ値で通ることを確かめる（削除より先に行う）。rust/crates/krbr-render/tests/。cargo test --release -p krbr-render -- --ignored --nocapture
2. シェーダのビルド（GLSL → SPIR-V、ray query 版と ray tracing pipeline 版）を、CMake から Rust の build.rs（glslc を呼ぶ）に移し、cargo build だけでそろうようにする
3. 削除の直前にタグ（cpp-final など）を付けてから、C++ の本体（src/ の step_dll 以外）・tests/・不要な依存（GLFW、ImGui など）を消す。CMake と vcpkg は STEP 変換器 DLL（OpenCASCADE は C++ のライブラリなので C++ のまま残す）だけの構成にする。README を整理する
- 層テクスチャ方式（[relief] enabled = false）は Rust 版に無いので、機能ごと廃止する（凹凸を形状にする方式が上位互換）

## 配布と KiCad 連携
- 配布の形を決める：ポータブル版の zip（scripts\make_dist.ps1）に加えて、インストーラ、または KiCad のプラグインマネージャ（PCM）のパッケージを作るか
- GitHub Releases への公開（公開の前に確認を取る）
- KiCad 11 で旧来の Action Plugin（SWIG の pcbnew）が無くなるので、IPC API のプラグイン（plugin.json）に移す

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
