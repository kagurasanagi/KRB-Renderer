//! `--init` で書き出す設定ファイルのひな形（C++ 版 app/config.cpp の kBoardToml / kEnvironmentToml と同じ文面。
//! environment.toml の hdr の説明だけ、Rust 版の決め方に合わせてある）

pub const BOARD_TOML: &str = r#"# krbr: 基板の設定（このファイルが無ければ既定値を使います）
# 色は sRGB の [r, g, b]（0..1）

[board]
finish = "auto"              # auto（stackup の copper_finish に従う）/ ENIG / HASL / OSP
# mask_color = [0.05, 0.42, 0.16]   # 省略時は stackup の色、それも無ければ緑
# mask_color = "jlcpcb-purple"       # またはプリセット名（色・粗さ・散乱をまとめて設定。krbr mask list で一覧）
mask_roughness = 0.18
mask_scatter = 0.3           # レジストの顔料自身が光を散らす割合
# silk_color = [0.93, 0.93, 0.90]   # 省略時は白。レジストのプリセットが黄・白なら黒
fr4_color = [0.62, 0.58, 0.42]
clip_silk_by_mask = true     # レジストの開口部ではシルクを印刷しない
texture_px_per_mm = 50       # 層テクスチャの解像度（50 = 20 µm/px）
include_dnp = true

[relief]                     # 銅箔・レジスト・シルクの厚みを形状として作る（段差）
enabled = true
# copper_um = 35             # 省略時は stackup の銅の厚み
mask_on_copper_um = 12
mask_on_substrate_um = 25    # 銅の間を埋めるので、銅の上より厚い
silk_um = 15
via_dimple_um = 50           # レジストでふさがれた（テンティング）ビアのくぼみの深さ。0 で平ら

[surface]
bump_intensity = 0.25        # 表面の微細な凹凸（ゆず肌・織り目・粒状感）の強さの倍率。0 で無効

[placeholder]                # 3D モデルが見つからない部品の代替（縁を面取りした透明な水晶）
height_mm = 0                # 高さ（mm）。0 なら面積が大きいほど高く（√面積 × 0.25、0.3〜5 mm）
color = [0.92, 0.96, 1.0]    # 水晶の色（中を 5 mm 進んだときの透過の色、sRGB）

[paths]
# 3D モデルのパスの変数を追加・上書きします
# vars = { KICAD10_3DMODEL_DIR = "C:/Program Files/KiCad/10.0/share/kicad/3dmodels" }
# 見つからないモデルをファイル名で探す追加フォルダ
# model_search_dirs = ["D:/kicad/3dmodels"]

[kicad_cli]
# 線フォントの文字（render_cache の無い文字）を kicad-cli の SVG 出力から取り込む
use_for_text = true
# path = "C:/Program Files/KiCad/10.0/bin/kicad-cli.exe"
"#;

pub const ENVIRONMENT_TOML: &str = r#"# krbr: 照明・カメラ・レンダリングの設定（コマンドラインの指定が優先されます）

[environment]
# hdr = "../hdr/studio.exr"   # このファイルからの相対パスも可。builtin:studio で内蔵のスタジオ
#                             # 省略時は、この基板で最後に使ったもの、無ければ内蔵のスタジオ
intensity = 1.0
rotation_deg = 0.0

[camera]
elevation_deg = 35.0         # 基板面からの仰角
azimuth_deg = 45.0           # 0 で基板の手前（下辺側）から、正の値で右回り
fov_deg = 30.0               # 縦の画角
margin = 0.04                # 画面端の余白（割合）

[render]
width = 1920
height = 1080
spp = 64                     # 1 画素あたりのサンプル数（作業中の PC 向けに控えめ）
max_depth = 8
exposure_ev = 0.7
radiance_clamp = 10.0        # 間接光の放射輝度の上限（ホタル対策）。0 で無効（偏りのない結果）
spectral = false             # true で分光レンダリング（代表波長方式。分散による色の分かれを再現する）
integrator = "pt"            # pt（パストレーサー）/ bdpt（双方向）/ vcm（透明な物体越しの集光模様も扱う）
# vcm_radius = 0.0           # VCM の近傍の半径の初期値（mm）。0 でシーンの大きさの 0.3%

[lens]
model = "pinhole"            # pinhole / thin（薄レンズ：被写界深度・ボケ）/ realistic（実レンズのデータで追跡。最終出力向け）
focal_length_mm = 50.0       # thin：焦点距離（絞りの大きさ = 焦点距離 / F 値）
f_number = 2.8
focus_distance_mm = 0.0      # ピントを合わせる距離。0 で注視点まで
blades = 0                   # 絞りの羽根の枚数（0 で円）
blade_rotation_deg = 0.0
blade_roundness = 0.0        # 0：多角形、1：円
natural_vignetting = false   # cos⁴θ の周辺減光
optical_vignetting = 0.0     # 口径食（0..1）。画面の端のボケがレモン形になる
abbe_number = 0.0            # 軸上色収差（分光モード）：レンズのアッベ数。0 で無効
lateral_ca = 0.0             # 倍率色収差（分光モード）：F 線と C 線の像の大きさの差（例 0.003）
# lens_file = "lenses/dgauss.50mm.dat"   # realistic：pbrt 形式のレンズデータ。省略で内蔵の double Gauss 50mm f/2
"#;
