# KRB-Renderer

KiCad の基板を、GPU でリアルタイムに写真のように描くビューアです。

![ビューアの画面](docs/images/viewer.png)

<p>
  <img src="docs/images/usbhub_cm1.jpg" width="49%" alt="レンダリングの例：白いレジストの USB ハブ基板">
  <img src="docs/images/usbhub_cm2.jpg" width="49%" alt="レンダリングの例：同じ基板に寄った画">
</p>

KiCad の 3D ビューアは編集の邪魔をしない簡素な表示なので、もっときれいに眺めたくて作りました。

## できること

- KiCad で保存すると、すぐ描き直す
- レジスト・金メッキ・シルクの質感、LED の発光まで写実的に描く
- 部品をクリックして材質を変える
- 画像（PNG / EXR）と、回転する動画（MP4）を書き出す
- KiCad のツールバーのボタンから開く

## 動作環境

- Windows 10 / 11（64 bit）
- レイトレーシング対応の GPU（NVIDIA GeForce GTX 10 系以降など）
- KiCad 9 / 10（3D モデルと一部の文字に使う）

## インストール

1. [Releases](https://github.com/kagurasanagi/KRB-Renderer/releases) から zip をダウンロード
2. 好きな場所に展開（インストールは不要）
3. `KRB-Renderer.exe` を起動

「PC が保護されました」と出たら、「詳細情報」→「実行」で起動できます（署名のないアプリのため）。

## 使い方

1. `.kicad_pcb` を `KRB-Renderer.exe` かウィンドウにドラッグ＆ドロップ
2. KiCad で編集して保存すると、自動で描き直す
3. `S` で画像を保存（基板のフォルダへ）。動画は「ファイル → 動画を書き出す」

**KiCad から開く**：「設定 → KiCad プラグイン」で登録し、KiCad を開き直すと、PCB エディタのツールバーに「KRB Render でプレビュー」が出ます。

| 操作 | 内容 |
|---|---|
| 左ドラッグ | 回転 |
| 右／中ドラッグ | 移動 |
| ホイール | 拡大・縮小 |
| クリック | 部品を選ぶ（材質を変える） |
| Ctrl ＋クリック | ピントを合わせる |
| `F` | 全体を表示 |
| `P` | 一時停止 |

## 困ったとき

- **部品が箱になる**：3D モデル（STEP）が見つかりません。KiCad の 3D モデルのライブラリを入れてください。
- **文字や QR コードが出ない**：KiCad 付属の `kicad-cli` が見つかりません。KiCad を標準の場所に入れてください。
- **操作が重い**：`P` で一時停止すると軽くなります。
- **メニューが出ない**：[WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/) を入れてください。

## 詳しい説明

- [使い方の詳細](docs/usage.md)：全ての操作、コマンドライン、設定ファイル、材質
- [開発者向け](docs/development.md)：ビルド、テスト、配布の作り方
- [内部の仕組み](docs/internals.md)：処理の流れ、描画のアルゴリズム

## ライセンス

MIT。同梱のライブラリのライセンスは、zip の `licenses` フォルダにあります。
