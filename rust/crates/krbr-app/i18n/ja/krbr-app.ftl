# KRB-Renderer のビューア：日本語（id は英語の krbr-app.ftl と同じものをそろえる）

## メニュー：ファイル
menu-file = ファイル
menu-save-image = 画像を保存
menu-save-image-tip = 基板のフォルダ（--out を付けたらその場所）に、番号を付けて PNG と EXR を保存する
menu-save-png = PNG として保存...
menu-save-png-tip = 場所と名前を選んで保存する（露出・グロー・トーンマッピング済み）
menu-save-exr = EXR として保存...
menu-save-exr-tip = 場所と名前を選んで保存する（リニアの放射輝度。露出・グローはかけない）
menu-settings-location = 基板の設定ファイルの置き場所
loc-next-to-board = 基板の横（<名前>.krbr）
loc-next-to-board-tip = KiCad の基板と同じフォルダに置く。基板と一緒に持ち運べる
loc-config-dir = 全体設定のフォルダ（%APPDATA%\krbr\boards）
loc-config-dir-tip = KiCad のフォルダにファイルを増やさない
loc-hint = すべての基板に効く。ほかの基板のファイルは、次に開いたときに移す
menu-reveal-settings = 基板の設定ファイルを表示
menu-reveal-settings-tip = この基板の <名前>.krbr をエクスプローラーで表示する（まだ無ければ、置き場所のフォルダを開く）
menu-quit = 終了

## メニュー：表示
menu-view = 表示
menu-settings-panel = 設定パネル
menu-pause = レンダリングを一時停止
menu-render-size = 描画のサイズ
menu-render-ratio = レンダリングの比率
menu-reset-view = 視点を戻す
menu-auto-exposure = 露出を自動で合わせる
menu-reset-all = 設定をすべて既定に戻す
menu-reset-all-tip = パネルの設定（環境マップ・露出・積分器・レンズ・色の計算・表面など）を設定ファイルとコマンドラインの値に戻し、この基板について覚えたものを忘れる
menu-language = 言語
language-auto = 自動
language-auto-tip = KiCad で選んだ言語、無ければ OS の言語

## メニュー：ヘルプ
menu-help = ヘルプ
help-rotate = 左ドラッグ: 回転
help-select = クリック: 部品を選ぶ（設定パネルで材質を変えられる）
help-pan = 右 / 中ドラッグ: 移動
help-zoom = ホイール: 拡大・縮小
help-focus = Ctrl + クリック: その点にピントを合わせる
# キー（キーの名前はプログラムが出す。ここは説明）
key-panel = 設定パネル
key-reset-view = 視点を戻す
key-save = 画像を保存
key-pause = 一時停止
key-denoise = 今ノイズ除去をかける（ノイズ除去が CPU のとき）
key-exposure = 露出
key-env-rotation = 環境マップの回転
key-display-mode = 画像 / アルベド / 法線
key-quit = 終了

## ツールバー
toolbar-done = ✔ 完了
toolbar-done-tip = 最大 spp に達したので、描き終えて止まっている。続けて描くなら、設定パネルで最大 spp を上げる
toolbar-resume = ▶ 再開
toolbar-pause = ■ 一時停止
toolbar-pause-tip =
    レンダリングを一時停止する（P）。サンプルを積み足さないので GPU の負荷がほぼ無くなり、操作が軽くなる。
    カメラを動かしたときは 1 サンプルだけ描いて止まる

## ステータスバー
status-paused = 一時停止中
status-done = 完了
status-rendering = レンダリング中
status-elapsed = 経過 { $time }
status-elapsed-tip = 今の累積を始めてからレンダリングにかかった時間（一時停止の間と完了後は数えない。視点などを変えると 0 から）
status-average = 平均 { $wall } ms/spp（GPU { $gpu } ms）
status-average-tip = 1 サンプルあたりの時間：経過時間 ÷ サンプル数（括弧内は GPU がサンプルを描いた時間だけ）
status-remaining = 残り 約 { $time }
status-integrator = { $integrator }・{ $color }
status-spectral = 分光
status-gpu = { $name }（{ $method }）
status-render-size = 描画 { $width }×{ $height }（表示 { $percent }%）
status-size-fixed-tip = 決まったサイズ（メニューの「表示」→「描画のサイズ」）
status-size-fit-tip = ウィンドウに合わせる（3D 表示の領域 × レンダリングの比率）
status-reloading = 基板を読み直し中...
status-silk = 線フォントの文字を書き出し中（kicad-cli）...
status-meshing = 3D モデルを分割中: { $count }（それまでは箱で表示）
duration-seconds = { $seconds } 秒

## 設定パネル
panel-settings = 設定
panel-close-tip = 閉じる（メニューの「表示」や F1 / Tab で再表示）
panel-help-mouse = 左ドラッグ: 回転  右ドラッグ: 移動  ホイール: 拡大  クリック: 部品を選ぶ
panel-help-toggle = F1 / Tab: このパネルの表示切り替え

panel-environment = 環境マップ
env-builtin = { $id }（内蔵）
env-builtin-studio = スタジオ（内蔵）
env-builtin-studio-tip = 灰色の撮影ブースと大きなソフトボックス
env-gpu-failed = 環境マップを GPU へ送れなかったので、内蔵のスタジオにした：{ $error }
env-open = 開く...
env-open-tip = .hdr / .exr を選ぶ（ウィンドウへのドラッグ＆ドロップでも可）
env-remember-hint = 選んだものは、この基板の次回の表示にも使う
env-loading = 読み込み中: { $path }
env-intensity = 強さ
env-rotation = 回転
env-not-an-environment = 環境マップ（.hdr / .exr）ではない: { $path }
dlg-open-env-title = 環境マップを開く
dlg-env-filter = 環境マップ (*.hdr, *.exr)

panel-exposure = 露出
exposure-auto = 自動
glow = グロー
glow-tip = 点灯した LED など、とても明るい光が周りへにじむ強さ（0 でなし）。保存する画像にも入る
glow-threshold = しきい値
glow-threshold-tip = これより明るい光だけがにじむ（露出をかけた後の RGB の最大。1 が画面の白）。白い基板や部品までにじむときは上げる
glow-spread = 広がり
glow-spread-tip = にじみの広がり。小さいと光のすぐ近くに締まり、1 で画面の大きさ近くまで薄く広がる
denoise = ノイズ除去 (OIDN)
denoise-now = 今かける (D)
denoise-unavailable = 使用できません
denoise-strength = 強さ
denoise-strength-tip = 元の画像と混ぜる割合（1 で OIDN の結果そのまま。下げると細部やざらつきが少し残る）
denoise-cpu-hint = CPU のため、収束時と D キーのときだけかけます

panel-integrator = 積分器
integrator-pt-hint = 単方向（速い）
integrator-bdpt-hint = 双方向（光源側の経路とつなぐ）
integrator-vcm-hint = 双方向＋近傍のまとめ（透明な物体越しの集光模様も収束）
integrator-failed = { $integrator } が使えないので、PT にした：{ $error }

panel-lens = レンズ
lens-pinhole = ピンホール
lens-thin = 薄レンズ
lens-realistic = 実レンズ
lens-f-number = F 値
lens-focus = ピント (mm)
lens-focus-hint = Ctrl + クリック：その点にピントを合わせる
lens-focal-length = 焦点距離 (mm)
lens-blades = 羽根の枚数
lens-blade-rotation = 羽根の回転
lens-blade-roundness = 羽根の丸み
lens-natural-vignetting = 周辺減光 (cos⁴)
lens-optical-vignetting = 口径食
lens-axial-ca = 軸上色収差 (アッベ数)
lens-lateral-ca = 倍率色収差
lens-ca-hint = 色収差は分光モードで有効
focus-set = ピントを { $mm } mm に合わせた

panel-color = 色の計算
color-spectral = 分光（4 波長）

panel-surface = 表面
bump-intensity = 凹凸の強さ
mask-as-configured = 設定どおり
mask-hint = レジストの色（プリセット）
mask-note-green = KiCad / 一般的な緑
mask-note-jlcpcb-green = JLCPCB Green：やや明るい緑、光沢
mask-note-jlcpcb-purple = JLCPCB Purple：青みの紫
mask-note-jlcpcb-white = JLCPCB White：ほぼ不透明
mask-note-jlcpcb-black = JLCPCB Black：光沢の黒

panel-display = 表示
size-fit-window = ウィンドウに合わせる
size-preset = { $size }（{ $name }）
size-full-hd = フル HD
size-square = 正方形
size-hint = 描画のサイズ（決まったサイズでは、ウィンドウの大きさを合わせる）
ratio-label = ×{ $ratio }（{ $what }）
ratio-coarse = 粗く速い
ratio-fine = 細かく描いて縮小
ratio-native = 等倍
ratio-hint = レンダリングの比率（レンダリングの解像度 ÷ 表示の大きさ）
fit-too-large = { $size } は画面に入らないので、レンダリングの比率を ×{ $ratio } にした（表示 { $percent }%）
mode-image = 画像
mode-albedo = アルベド
mode-normal = 法線
max-spp = 最大 spp

panel-camera = カメラ
camera-fov = 画角
camera-angles = 仰角 { $elevation }°  方位 { $azimuth }°
camera-reset = 視点を戻す (R)
camera-save = 保存 (S)

## 選んだ部品
sel-title = 選んだ部品
sel-none-hint = 部品をクリックすると、材質を変えられる
sel-deselect = 選択を外す
sel-no-outline = 実レンズでは輪郭線を表示しない
sel-meshing = 3D モデルを分割中（終わるまで箱で表示）
sel-missing = 3D モデルが無いので、代わりの箱で表示している（材質は変えられない）
sel-missing-model =
    { $spec }
    → { $resolved }（{ $reason }）
sel-missing-hint = 見つける場所は、基板の設定ファイル <名前>.krbr の [paths] model_search_dirs で足せる
scope-part = この部品
scope-part-tip = この基板のこの部品だけ（基板の設定ファイル <名前>.krbr の [ref.<部品番号>]）
scope-model = このモデル
scope-model-tip = この基板で同じ 3D モデルを使う部品すべて（<名前>.krbr の [[model]]）
scope-all-boards = 全基板
scope-all-boards-tip = 同じ 3D モデルを使うすべての基板（%APPDATA%\krbr\model_map.toml）
sel-shadowed-ref = この部品には部品番号の上書きがあり、そちらが優先される（「この部品」で上書きを消すと反映される）
sel-shadowed-project = このプロジェクトにモデルの設定があり、そちらが優先される（「このモデル」で上書きを消すと反映される）
part-whole = （全体）
part-source-tip =
    { $keys }
    決まった段階: { $source }
part-pattern = 模様
part-pattern-tip = 表面の模様（細かな凹凸と粗さのむら）。外すと、なめらかな面になる（材質名に +smooth が付く）
part-emit = 発光
part-emit-tip = この部位を光らせる（LED の点灯など）。色と明るさは下の行で変える（材質名に +emit=#rrggbb,<cd/m²> が付く）
emit-color-tip = 発光の色
emit-luminance-tip = 発光の明るさ（輝度）。ドラッグか入力で変える。露出をかけて画面の白（1）の何倍になるかが、グローのしきい値を超えるとにじむ（5000 で露出 0.7 EV なら約 8 倍）。上げるほど芯が白っぽくなる
parts-all = 全部位
parts-choose-all = まとめて選ぶ
parts-pattern-all-tip = 全部位の表面の模様をまとめて切り替える
sel-source = 決まった段階: { $source }
sel-source-varies = 決まった段階は部位ごとに違う（部位の名前にカーソルを乗せると出る）
sel-clear-override = 上書きを消す（規則どおりに戻す）
sel-clear-override-tip = 選んだ書き込み先の、この部品（モデル）の設定を消す
material-set-all = { $reference } の全部位を { $material } にした（{ $file }）
material-set-part = { $reference } の { $part } を { $material } にした（{ $file }）
material-cleared-all = { $reference } の全部位の上書きを消した（{ $file }）
material-cleared-part = { $reference } の { $part } の上書きを消した（{ $file }）
material-nothing-to-clear = 消す上書きが無い（規則どおりになっている）

## 知らせ
reset-all-done = 設定をすべて既定に戻した
reload-thread-failed = 読み直しに失敗したので、保存の監視を止めた
reload-error = 読み直せなかった（次の保存を待つ）: { $error }
reload-done = 読み直し：変更 { $changed }・追加 { $added }・削除 { $removed }{ $full }、タイル { $tiles } 枚（解析 { $parse } ms、組み立て { $build } ms、GPU { $gpu } ms{ $since }）
reload-full = （全体）
reload-since = 、保存から { $seconds } 秒
silk-applied = 線フォントの文字（kicad-cli）を反映した
models-applied = 3D モデルを反映した（残り { $pending }）
models-all-applied = 3D モデルをすべて反映した
settings-location-failed = 置き場所を変えられなかった：{ $error }
settings-location-changed = 基板の設定ファイルの置き場所を変えた：{ $file }
settings-location-not-yet = 基板の設定ファイルの置き場所を変えた（この基板はまだ無い。書くときに { $file } に作る）
dlg-save-png-title = PNG として保存
dlg-png-filter = PNG 画像 (*.png)
dlg-save-exr-title = EXR として保存
dlg-exr-filter = OpenEXR（リニア） (*.exr)
saved = 保存した：{ $path }（.exr も、{ $spp } spp）
saved-denoised-too = 。ノイズ除去した画像も保存した（_denoised）
saved-as = 保存した：{ $path }（{ $spp } spp{ $denoised }）
saved-as-denoised = 、ノイズ除去済み
save-failed = 保存できなかった：{ $error }
readback-failed = GPU から画像を読み戻せなかった：{ $error }
materials-failed = 材質を GPU へ送れなかった：{ $error }
