//! リアルタイム表示（ウィンドウ）。オフライン描画と同じシーン・積分器で、フレームごとにサンプルを積み増して表示する。
//! 操作パネル（egui）で、環境マップ・露出・レンズ・表面の凹凸・レジストの色などをその場で変えられる。

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use ash::vk;
use egui_ash_renderer::{DynamicRendering, Options as EguiOptions, RenderMode, Renderer as EguiRenderer};
use egui_winit::State as EguiState;
use glam::Vec3;
use krbr_core::{error, info, warn, FileWatcher};
use krbr_kicad::Board;
use krbr_render::image_io::{self, ImageF};
use krbr_render::glow::GlowParams;
use krbr_render::tonemapper::{DisplayMode, Tonemapper};
use krbr_render::vk::{image_barrier, Buffer};
use krbr_render::studio_env::STUDIO;
use krbr_render::{build_env_distribution, environment_label, DenoiseQuality, Denoiser, is_builtin, load_environment_map, Context, ContextOptions, EnvDistribution, GpuScene, Progressive};
use krbr_scene::config::apply_mask_color_spec;
use krbr_scene::scene::{load_material_setup, raycast, raycast_pick, update_mask_material, MaterialAssignment};
use krbr_scene::{mask_presets, orbit_camera, InstanceOwner, LensModel, Scene, SceneOptions};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::fl;
use crate::i18n;
use crate::kicad_cli::{plot_stroke_silk, SilkPlot};
use crate::material_cmd::{clear_material, set_material, MaterialSetOptions};
use crate::selection::{Outline, Selected};
use krbr_scene::board_file;
use krbr_scene::material_library::{parse_variant, split_variant, with_emission, with_smooth, Emission};
use crate::prepare::{ReloadStats, SceneSource};
use crate::remember;
use crate::prepare::{frame_params, framed_camera, initial_camera, prepare_scene, PreparedScene, RenderOptions};
use crate::swapchain::Swapchain;

#[derive(Debug, Clone, Default)]
pub struct ViewOptions {
    pub common: RenderOptions,
    /// >0: このフレーム数を描いたら screenshot を保存して終了（開発用）
    pub test_frames: u32,
    /// test_frames 用の保存先（表示中の画像そのもの）
    pub screenshot: Option<PathBuf>,
    /// ウィンドウを表示しない（test_frames 指定時は自動）
    pub hidden: bool,
    /// 開発用：毎フレームこの角度だけカメラを回す
    pub test_orbit_deg: f64,
    /// .kicad_pcb の保存を監視して描き直す（--no-watch で無効）
    pub watch: bool,
    /// 開発用：テスト中に一度だけ基板を読み直す（保存の代わり）
    pub test_reload: bool,
    /// 開発用：テスト中にこの画素（3D 表示の中の位置）をクリックしたことにする
    pub test_pick: Option<(f64, f64)>,
    /// 開発用：test_pick で選んだ部品の全部位に、この材質を割り当てる（この部品だけ）
    pub test_assign: String,
    /// 開発用：テスト中に、パネルでこのレジストの色（プリセット）を選んだことにする
    pub test_mask: String,
    /// 開発用：テストの終わり近くに「名前を付けて保存」でこのファイル（.png / .exr）を選んだことにする
    pub test_save_as: Option<PathBuf>,
    /// 開発用：テスト中に、メニューの「設定をすべて既定に戻す」を押したことにする
    pub test_reset_settings: bool,
    /// レンダリングを一時停止した状態で開く（GPU の弱い PC 向け）
    pub paused: bool,
    /// 描画のサイズ："WxH"（決まったサイズ。3D 表示の領域がそれ ÷ 比率になるようウィンドウの大きさを変える）か "fit"（ウィンドウに合わせる）。空なら覚えたもの
    pub render_size: String,
    /// レンダリングの比率（レンダリングの解像度 ÷ 表示の大きさ）。None なら覚えたもの
    pub render_ratio: Option<f32>,
    /// 表示の言語（--lang。"ja" など）。None なら環境変数 KRBR_LANG、メニューで選んだもの、KiCad、OS の順
    pub language: Option<String>,
}

/// 描画のサイズの候補（幅, 高さ）
const SIZE_PRESETS: [(u32, u32); 7] = [(1280, 720), (1920, 1080), (2560, 1440), (3840, 2160), (1080, 1080), (1080, 1350), (1200, 800)];

/// 描画のサイズの表示名（"1920×1080（フル HD）" など）
fn size_label(w: u32, h: u32) -> String {
    let name = match (w, h) {
        (1280, 720) => "HD".to_string(),
        (1920, 1080) => fl!("size-full-hd"),
        (2560, 1440) => "WQHD".to_string(),
        (3840, 2160) => "4K".to_string(),
        (1080, 1080) => fl!("size-square"),
        (1080, 1350) => "4:5".to_string(),
        (1200, 800) => "3:2".to_string(),
        _ => return format!("{w}×{h}"),
    };
    fl!("size-preset", size = format!("{w}×{h}"), name = name)
}

/// 環境マップの表示名（内蔵のスタジオは翻訳した名前、ほかはファイル名）
fn env_label(spec: &str) -> String {
    if spec == STUDIO {
        fl!("env-studio")
    } else {
        environment_label(spec)
    }
}

/// レンダリングの比率の候補（レンダリングの解像度 ÷ 表示の大きさ）
const RATIOS: [f32; 5] = [0.25, 0.5, 1.0, 1.5, 2.0];

/// "WxH" を読む（"fit" や空は None）
fn parse_size(s: &str) -> Option<(u32, u32)> {
    let lower = s.to_lowercase();
    let (w, h) = lower.split_once(['x', '×'])?;
    let (w, h) = (w.trim().parse::<u32>().ok()?, h.trim().parse::<u32>().ok()?);
    (w > 0 && h > 0 && w <= 16384 && h <= 16384).then_some((w, h))
}

/// w x h の画像を、縦横比を保って領域 vp の中央に収めた矩形
fn fit_rect(vp: Viewport, w: u32, h: u32) -> Viewport {
    let s = (vp.w as f64 / w.max(1) as f64).min(vp.h as f64 / h.max(1) as f64);
    let (dw, dh) = (((w as f64 * s).round() as u32).clamp(1, vp.w.max(1)), ((h as f64 * s).round() as u32).clamp(1, vp.h.max(1)));
    Viewport { x: vp.x + (vp.w - dw) / 2, y: vp.y + (vp.h - dh) / 2, w: dw, h: dh }
}

/// 「名前を付けて保存」の形式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SaveFormat {
    /// 露出・グロー・トーンマッピング済み
    Png,
    /// リニアの放射輝度
    Exr,
}

impl SaveFormat {
    fn extension(self) -> &'static str {
        match self {
            SaveFormat::Png => "png",
            SaveFormat::Exr => "exr",
        }
    }
}

/// 発光の色・明るさの編集中の値。止まってから書き込み、組み直しが終わるまでは表示にこの値を使う
struct EmitEdit {
    reference: String,
    part: String,
    /// 書き込む材質名（"<材質>+emit=..."）
    material: String,
    changed_at: Instant,
    written: bool,
}

/// 発光を付けたときの明るさの既定値（cd/m²。露出 0.7 EV で画面の白の約 8 倍になり、グローの既定のしきい値 4 を超えてにじむ）
const DEFAULT_EMISSION_LUMINANCE: f32 = 5000.0;
/// 発光の色・明るさを最後に変えてから書き込むまでの時間
const EMIT_EDIT_DELAY: Duration = Duration::from_millis(400);

/// 発光を付けたときの色の既定値：部位の色（部位のキーの "#rrggbb"）を、色合いを保って最も明るくしたもの。色が無ければ白
fn emission_color_of_part(part_keys: &str) -> [u8; 3] {
    let Some(c) = part_keys.split(' ').find_map(|k| Emission::parse(&format!("{k},0"))) else { return [255; 3] };
    let max = *c.srgb.iter().max().unwrap();
    if max == 0 {
        return [255; 3];
    }
    c.srgb.map(|v| (v as u32 * 255 / max as u32) as u8)
}

/// 基板の読み直し（裏のスレッド）の結果
struct ReloadDone {
    source: SceneSource,
    /// 保存による読み直しか（false：裏で分割していた 3D モデルが揃ったので組み直しただけ）
    saved: bool,
    /// 新しい基板（保存のとき）、シーン、統計
    result: Result<(Option<Board>, Scene, ReloadStats), String>,
    /// 保存したファイルの更新時刻（保存から画面の更新までの時間を測る）
    saved_at: Option<SystemTime>,
    /// kicad-cli で書き出し直したシルクを反映した
    silk: bool,
    /// 材質の設定を読み直した
    materials: bool,
}

/// 入力・操作パネルで変わる表示の状態
struct ViewState {
    target: Vec3,
    distance: f32,
    elevation: f64,
    azimuth: f64,
    fov: f64,
    exposure: f32,
    /// グロー（明るい光のにじみ）：強さ（0 でなし）、しきい値、広がり。表示だけに効く（累積はやり直さない）
    glow: f32,
    glow_threshold: f32,
    glow_spread: f32,
    env_rotation_deg: f32,
    env_intensity: f32,
    bump_intensity: f32,
    /// 2 の累乗
    max_spp: u32,
    mode: DisplayMode,
    /// 右側の設定パネルを出すか（閉じてもメニューの「表示」や F1 / Tab で再表示）
    show_panel: bool,
    /// カメラ・照明が変わった（累積をやり直す）
    dirty: bool,
    materials_dirty: bool,
    /// レジストの色：None = 設定どおり、Some = プリセットの添字
    mask_choice: Option<usize>,
    mask_changed: bool,
    save_requested: bool,
    /// メニュー「PNG / EXR として保存...」：保存のダイアログを開く
    save_as_requested: Option<SaveFormat>,
    /// メニュー：基板の設定ファイルの置き場所を変える・ファイルを表示する
    settings_location_requested: Option<board_file::Location>,
    reveal_settings_requested: bool,
    reset_requested: bool,
    auto_exposure_requested: bool,
    /// メニュー：パネルの設定をすべて既定（設定ファイルとコマンドライン）に戻し、覚えたものを忘れる
    reset_all_requested: bool,
    /// 最初のフレーム：3D 表示の大きさが決まったので、最初の視点（覚えた視点、無ければ自動の構図）を合わせ直す
    start_requested: bool,
    quit_requested: bool,
    /// レンダリングを一時停止する（サンプルを積み足さない。GPU の弱い PC で操作を軽くする）
    paused: bool,
    spectral: bool,
    integrator_changed: bool,
    rotating: bool,
    panning: bool,
    /// Ctrl + クリック：その点にピントを合わせる
    focus_pick: Option<(f64, f64)>,
    /// 左ボタンを押した位置と、そこから動いた量（ほとんど動かさずに離したらクリック）
    press_at: Option<(f64, f64)>,
    press_moved: f64,
    /// クリック：その点の部品を選ぶ
    part_pick: Option<(f64, f64)>,
    /// ノイズ除去（OIDN）した画像を表示する。強さは元の画像と混ぜる割合（1 で OIDN の結果そのまま）
    denoise: bool,
    denoise_strength: f32,
    /// D キー／ボタン：ノイズ除去が CPU のとき、手動で 1 回かける
    denoise_requested: bool,
    last_cursor: (f64, f64),
    ctrl: bool,
    /// 3D 表示の領域（ウィンドウのうちメニューとパネルを除いた所、物理ピクセル）
    viewport: Viewport,
    /// 画像を置く矩形（3D 表示の領域の中に、描画の縦横比を保って収めたもの）。クリックや輪郭線の換算はこれで行う
    display: Viewport,
    /// 描画のサイズ：None = ウィンドウに合わせる（3D 表示の領域 × 比率）、Some = 決まったサイズ
    size_mode: Option<(u32, u32)>,
    /// レンダリングの比率（レンダリングの解像度 ÷ 表示の大きさ）
    render_ratio: f32,
    /// 決まったサイズに合わせてウィンドウの大きさを変える
    fit_window_requested: bool,
    /// メニューで表示の言語を選んだ（Some(None)：自動）
    language_requested: Option<Option<String>>,
    tan_half_fov_y: f32,
}

impl ViewState {
    fn glow_params(&self) -> GlowParams {
        GlowParams { strength: self.glow, threshold: self.glow_threshold, spread: self.glow_spread }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Viewport {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Viewport {
    fn contains(&self, p: (f64, f64)) -> bool {
        p.0 >= self.x as f64 && p.1 >= self.y as f64 && p.0 < (self.x + self.w) as f64 && p.1 < (self.y + self.h) as f64
    }
}

/// 環境マップの読み込み（別スレッドで行い、終わったら GPU へ差し替える）
struct EnvData {
    path: String,
    image: ImageF,
    dist: EnvDistribution,
    error: String,
}

fn load_environment(path: String) -> EnvData {
    match load_environment_map(&path) {
        Ok(image) => {
            let dist = build_env_distribution(&image, 2048);
            EnvData { path, image, dist, error: String::new() }
        }
        Err(e) => EnvData { path, image: ImageF::default(), dist: EnvDistribution::default(), error: e },
    }
}

fn is_env_file(p: &Path) -> bool {
    matches!(p.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("hdr") | Some("exr"))
}

/// 同じ環境マップか（Windows では大文字・小文字と区切り文字の違いを無視する）
fn same_env(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.replace('\\', "/").eq_ignore_ascii_case(&b.replace('\\', "/"))
    } else {
        a == b
    }
}

/// 選べる環境マップ：内蔵のスタジオ、最近使ったもの、今のものと同じフォルダの .hdr / .exr（重なりは除く）
fn list_environments(current: &str) -> Vec<Vec<String>> {
    let mut groups: Vec<Vec<String>> = vec![vec![STUDIO.to_string()]];
    let mut seen: Vec<String> = vec![STUDIO.to_string()];
    let mut add = |groups: &mut Vec<Vec<String>>, items: Vec<String>| {
        let items: Vec<String> = items.into_iter().filter(|p| !seen.iter().any(|s| same_env(s, p))).collect();
        seen.extend(items.iter().cloned());
        if !items.is_empty() {
            groups.push(items);
        }
    };
    let recent: Vec<String> = remember::recent_hdrs().into_iter().filter(|p| !is_builtin(p) && Path::new(p).is_file()).collect();
    add(&mut groups, recent);
    if !is_builtin(current) {
        let dir = Path::new(current).parent().filter(|p| !p.as_os_str().is_empty()).map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.is_file() && is_env_file(p)).map(|p| p.to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        files.sort();
        add(&mut groups, files);
    }
    groups
}

/// 画像全体の対数平均輝度から露出を決める（中間の灰色 0.18 に合わせる）
fn auto_exposure(img: &ImageF) -> f32 {
    let (mut sum, mut n) = (0.0f64, 0usize);
    for px in img.rgba.chunks_exact(4) {
        let l = 0.2126 * px[0] as f64 + 0.7152 * px[1] as f64 + 0.0722 * px[2] as f64;
        sum += l.max(1e-4).ln();
        n += 1;
    }
    if n == 0 {
        return 0.0;
    }
    let avg = (sum / n as f64).exp();
    (0.18f64 / avg).log2().clamp(-6.0, 6.0) as f32
}

/// 漢字・かなを表示できる Windows のフォント（読んだものを使い回す。無ければ None）
fn cjk_font() -> Option<&'static [u8]> {
    static FONT: std::sync::OnceLock<Option<Vec<u8>>> = std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        ["C:/Windows/Fonts/YuGothM.ttc", "C:/Windows/Fonts/meiryo.ttc", "C:/Windows/Fonts/msgothic.ttc", "C:/Windows/Fonts/msyh.ttc", "C:/Windows/Fonts/malgun.ttf"].iter().find_map(|f| std::fs::read(f).ok())
    })
    .as_deref()
}

/// 表示の言語に合わせたフォント：日本語なら日本語のフォントを先に（英字も同じ書体に）、ほかは egui の既定のフォントを先にして、
/// 漢字・かなは日本語のフォントで出す（ファイル名や部品の名前に含まれることがある）
fn install_fonts(ctx: &egui::Context, lang: &str) {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(bytes) = cjk_font() {
        fonts.font_data.insert("cjk".into(), std::sync::Arc::new(egui::FontData::from_static(bytes)));
        let proportional = fonts.families.entry(egui::FontFamily::Proportional).or_default();
        if lang == "ja" {
            proportional.insert(0, "cjk".into());
        } else {
            proportional.push("cjk".into());
        }
        fonts.families.entry(egui::FontFamily::Monospace).or_default().push("cjk".into());
    }
    ctx.set_fonts(fonts);
}

/// パネルを閉じるボタン（egui の Window の × と同じ描き方）
fn close_button(ui: &mut egui::Ui) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(ui.spacing().icon_width), egui::Sense::click());
    let vis = ui.style().interact(&resp);
    let r = rect.shrink(2.0).expand(vis.expansion);
    ui.painter().line_segment([r.left_top(), r.right_bottom()], vis.fg_stroke);
    ui.painter().line_segment([r.right_top(), r.left_bottom()], vis.fg_stroke);
    resp
}

/// GPU 側の資源一式（ウィンドウができてから作る）
struct Gpu {
    ctx: &'static Context,
    gpu: Box<GpuScene<'static>>,
    swap: Swapchain<'static>,
    prog: Progressive<'static>,
    tone: Tonemapper<'static>,
    /// ノイズ除去（有効にしたときに初めて作る）と、その結果を入力にしたトーンマッピング
    denoiser: Option<Denoiser<'static>>,
    tone_denoised: Option<Tonemapper<'static>>,
    egui_renderer: EguiRenderer<egui_ash_renderer::allocator::GpuAllocator>,
    cmd: vk::CommandBuffer,
    acquired: vk::Semaphore,
    fence: vk::Fence,
    timestamps: vk::QueryPool,
    ts_period_ms: f64,
}

struct App {
    vo: ViewOptions,
    prep: PreparedScene,
    st: ViewState,
    scene_as_configured: SceneOptions,
    window: Option<Window>,
    egui_ctx: egui::Context,
    egui_state: Option<EguiState>,
    gpu: Option<Gpu>,
    base_title: String,
    env_path: String,
    /// 一覧に出す環境マップ（区切り線ごとのまとまり）
    env_list: Vec<Vec<String>>,
    /// 「開く...」のダイアログ（別スレッド）の結果
    open_dialog: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// 「名前を付けて保存」のダイアログ（別スレッド）の結果と、選ばれた保存先（次のフレームで書く）
    save_dialog: Option<(SaveFormat, mpsc::Receiver<Option<PathBuf>>)>,
    save_as_target: Option<(SaveFormat, PathBuf)>,
    env_job: Option<(String, mpsc::Receiver<EnvData>)>,
    env_error: String,
    /// 保存の監視（--no-watch なら None）
    watcher: Option<FileWatcher>,
    reload_job: Option<mpsc::Receiver<ReloadDone>>,
    /// 前回シーンを組み立てたときの、裏で分割し終えた 3D モデルの数（増えたら組み直す）
    models_seen: usize,
    test_reload_started: bool,
    test_reload_applied: bool,
    /// 線フォントの文字の書き出し（kicad-cli、裏のスレッド）
    silk_job: Option<mpsc::Receiver<SilkPlot>>,
    /// 書き出しの間にまた文字が変わった（終わったらもう一度書き出す）
    silk_rerun: bool,
    /// 新しいシルクを受け取った（凹凸タイルを全部作り直す）
    silk_dirty: bool,
    /// クリックで選んだ部品と、その輪郭線
    selected: Option<Selected>,
    outline: Option<Outline>,
    /// 材質の割り当てを書き換えた（材質の設定を読み直して組み直す）
    materials_dirty_files: bool,
    /// 選んだ部品の材質を書き込む先：0 この部品だけ、1 このモデル（このプロジェクト）、2 このモデル（全体設定）
    assign_scope: usize,
    material_names: Vec<String>,
    material_error: String,
    /// 発光の色・明るさの編集中の値（ドラッグ中に毎回書き込まないよう、止まってから書く）
    emit_edit: Option<EmitEdit>,
    test_pick_done: bool,
    /// ノイズ除去：作ってみたか（失敗したら作り直さない）、累積をやり直した回数、どの累積をいくつのサンプルで除去したか
    denoiser_tried: bool,
    denoise_error: String,
    accum_epoch: u64,
    prev_samples: u32,
    denoised_epoch: u64,
    denoised_samples: u32,
    last_denoise_ms: f64,
    last_denoise_at: Instant,
    last_update: String,
    samples_per_frame: u32,
    need_recreate: bool,
    frames: u32,
    save_index: u32,
    /// 今の累積の計測：サンプルを描いたフレームの時間の合計（一時停止・完了の間は増えない）、GPU がサンプルを描いた時間の合計、そのサンプル数
    accum_wall: Duration,
    accum_gpu_ms: f64,
    accum_measured: u32,
    last_change_at: Instant,
    last_frame_ms: f64,
    last_gpu_ms: f64,
    /// パネルの設定：最後に見た値、最後に覚えた（state.toml に書いた）値、最後に変わった時刻。変わってから 1 秒たったら書く
    view_seen: remember::ViewValues,
    view_saved: remember::ViewValues,
    view_changed_at: Instant,
    /// 「すべて既定に戻す」の直後：戻した値が反映されたら、それを覚えずに基準にする（覚えていない状態のままにする）
    view_rebase: bool,
    /// ウィンドウの状態（基板によらず 1 つ）：起動時に読んだもの・最後に見たもの・最後に覚えたもの・最後に変わった時刻
    win_start: remember::WindowState,
    win_seen: remember::WindowState,
    win_saved: remember::WindowState,
    win_changed_at: Instant,
    /// 設定パネルの今の幅（egui のポイント）
    panel_width: f32,
    /// 決まったサイズに合わせてウィンドウの大きさを変えている途中：あと何フレーム合わせ直すか
    fit_frames: u32,
    /// 次に読み終えた環境マップを覚えない（「すべて既定に戻す」で設定ファイルのものに戻したとき）
    env_no_remember: bool,
    /// 表示の言語（決まった言語タグ）と、メニューで選んだもの（None：自動）
    language: String,
    language_choice: Option<String>,
    out_dir: PathBuf,
    stem: String,
    rc: i32,
    done: bool,
}

/// egui の座標（ポイント）の領域を、w x h の中に収めた物理ピクセルの領域にする（1 ピクセル以上）
fn viewport_px(r: egui::Rect, ppp: f32, w: u32, h: u32) -> Viewport {
    let (w, h) = (w.max(1), h.max(1));
    if !r.is_positive() {
        return Viewport { x: 0, y: 0, w, h };
    }
    let px = |v: f32, max: u32| ((v * ppp).round().max(0.0) as u32).min(max);
    let (x0, y0) = (px(r.min.x, w - 1), px(r.min.y, h - 1));
    let (x1, y1) = (px(r.max.x, w).max(x0 + 1), px(r.max.y, h).max(y0 + 1));
    Viewport { x: x0, y: y0, w: x1 - x0, h: y1 - y0 }
}

const TARGET_FRAME_MS: f64 = 10.0; // 60fps（16.7 ms）に収まるよう、表示処理の分を残す

fn ratio_label(r: f32) -> String {
    let what = if r < 1.0 {
        fl!("ratio-coarse")
    } else if r > 1.0 {
        fl!("ratio-fine")
    } else {
        fl!("ratio-native")
    };
    fl!("ratio-label", ratio = r.to_string(), what = what)
}

/// 設定パネルの「表示」の欄：描画のサイズとレンダリングの比率
fn size_controls(ui: &mut egui::Ui, st: &mut ViewState) {
    let current = match st.size_mode {
        None => fl!("size-fit-window"),
        Some((w, h)) => size_label(w, h),
    };
    egui::ComboBox::from_id_salt("render_size").width(200.0).selected_text(current).show_ui(ui, |ui| {
        if ui.selectable_label(st.size_mode.is_none(), fl!("size-fit-window")).clicked() {
            st.size_mode = None;
        }
        for (w, h) in SIZE_PRESETS {
            if ui.selectable_label(st.size_mode == Some((w, h)), size_label(w, h)).clicked() && st.size_mode != Some((w, h)) {
                st.size_mode = Some((w, h));
                st.fit_window_requested = true;
            }
        }
    });
    ui.weak(fl!("size-hint"));
    egui::ComboBox::from_id_salt("render_ratio").width(200.0).selected_text(ratio_label(st.render_ratio)).show_ui(ui, |ui| {
        for r in RATIOS {
            if ui.selectable_label(st.render_ratio == r, ratio_label(r)).clicked() && st.render_ratio != r {
                st.render_ratio = r;
                st.fit_window_requested = st.size_mode.is_some();
            }
        }
    });
    ui.weak(fl!("ratio-hint"));
}

fn display_mode_name(m: DisplayMode) -> &'static str {
    match m {
        DisplayMode::Beauty => "image",
        DisplayMode::Albedo => "albedo",
        DisplayMode::Normal => "normal",
    }
}

fn display_mode_of(name: &str) -> DisplayMode {
    match name {
        "albedo" => DisplayMode::Albedo,
        "normal" => DisplayMode::Normal,
        _ => DisplayMode::Beauty,
    }
}

/// 経過時間の表示：1 分未満は「12.3 秒」、それ以上は「m:ss」「h:mm:ss」
fn format_duration(secs: f64) -> String {
    if secs < 60.0 {
        return fl!("duration-seconds", seconds = format!("{secs:.1}"));
    }
    let t = secs.round() as u64;
    if t < 3600 {
        format!("{}:{:02}", t / 60, t % 60)
    } else {
        format!("{}:{:02}:{:02}", t / 3600, t / 60 % 60, t % 60)
    }
}

/// 最大のサンプル数は 2 の累乗にそろえる（それ以上で最も近い 2 の累乗に切り上げる。16 .. 65536）
fn pow2_spp(spp: u32) -> u32 {
    let mut m = 16u32;
    while m < spp && m < (1 << 16) {
        m <<= 1;
    }
    m
}

impl App {
    /// 今のパネルの設定の値（覚える値）
    fn current_view(&self) -> remember::ViewValues {
        let mut c = remember::ViewSettings::of(&self.prep.cfg);
        let st = &self.st;
        c.env_intensity = st.env_intensity;
        c.env_rotation_deg = st.env_rotation_deg;
        c.exposure_ev = st.exposure;
        c.glow = st.glow;
        c.glow_threshold = st.glow_threshold;
        c.glow_spread = st.glow_spread;
        c.spectral = st.spectral;
        c.bump_intensity = st.bump_intensity;
        c.fov_deg = st.fov;
        c.elevation_deg = st.elevation;
        c.azimuth_deg = st.azimuth;
        c.render_size = st.size_mode;
        let mut v = c.values();
        v.insert("render_ratio".into(), toml::Value::Float(st.render_ratio as f64));
        remember::set_camera(&mut v, st.target, st.distance);
        v.insert("display_mode".into(), toml::Value::String(display_mode_name(st.mode).into()));
        v.insert("denoise".into(), toml::Value::Boolean(st.denoise));
        v.insert("denoise_strength".into(), toml::Value::Float(st.denoise_strength as f64));
        v.insert("max_spp".into(), toml::Value::Integer(st.max_spp as i64));
        v
    }

    /// 決まったサイズのとき、3D 表示の領域が「描画のサイズ ÷ 比率」になるようにウィンドウの大きさを変える。
    /// 画面に入らなければ、入るまで比率を上げる。メニュー・パネル・ステータスバーの大きさが変わることがあるので、数フレームかけて合わせる
    fn fit_window(&mut self) {
        let Some((w, h)) = self.st.size_mode else {
            self.st.fit_window_requested = false;
            self.fit_frames = 0;
            return;
        };
        let Some(window) = self.window.as_ref() else { return };
        let inner = window.inner_size();
        let vp = self.st.viewport;
        // 領域以外（メニュー・パネル・ステータスバー）の大きさ
        let (extra_w, extra_h) = (inner.width.saturating_sub(vp.w), inner.height.saturating_sub(vp.h));
        if self.st.fit_window_requested {
            self.st.fit_window_requested = false;
            if window.is_maximized() {
                window.set_maximized(false);
            }
            // 画面に入るまで比率を上げる（枠の分も見込む）
            if let Some(m) = window.current_monitor() {
                let outer = window.outer_size();
                let (deco_w, deco_h) = (outer.width.saturating_sub(inner.width), outer.height.saturating_sub(inner.height));
                let (mw, mh) = (m.size().width as f32 * 0.96, m.size().height as f32 * 0.92);
                let fits = |r: f32| (w as f32 / r) + (extra_w + deco_w) as f32 <= mw && (h as f32 / r) + (extra_h + deco_h) as f32 <= mh;
                krbr_core::debug!("fit window: monitor {}x{}, inner {}x{}, outer {}x{}, 3D view {}x{}", m.size().width, m.size().height, inner.width, inner.height, outer.width, outer.height, vp.w, vp.h);
                if !fits(self.st.render_ratio) {
                    let r = RATIOS.iter().copied().find(|&r| r > self.st.render_ratio && fits(r)).unwrap_or(*RATIOS.last().unwrap());
                    self.last_update = fl!("fit-too-large", size = format!("{w}×{h}"), ratio = r.to_string(), percent = format!("{:.0}", 100.0 / r));
                    self.st.render_ratio = r;
                }
            }
            self.fit_frames = 6;
        }
        if self.fit_frames == 0 {
            return;
        }
        let target = (((w as f32 / self.st.render_ratio).round() as u32).max(1), ((h as f32 / self.st.render_ratio).round() as u32).max(1));
        if (vp.w, vp.h) == target {
            self.fit_frames = 0;
            return;
        }
        self.fit_frames -= 1;
        let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(target.0 + extra_w, target.1 + extra_h));
    }

    /// 今のウィンドウの状態（最大化している間は、最大化する前の大きさと位置を残す）
    fn current_window(&self) -> Option<remember::WindowState> {
        let window = self.window.as_ref()?;
        let mut w = self.win_seen.clone();
        w.maximized = window.is_maximized();
        if !w.maximized && !window.is_minimized().unwrap_or(false) {
            let size = window.inner_size();
            w.size = Some((size.width, size.height));
            w.position = window.outer_position().ok().map(|p| (p.x, p.y));
        }
        w.panel_shown = self.st.show_panel;
        if self.st.show_panel {
            w.panel_width = Some(self.panel_width.round());
        }
        Some(w)
    }

    /// パネルの設定とウィンドウの状態が変わっていたら覚える（最後に変わってから 1 秒たったら。now なら待たずに）
    fn save_view(&mut self, now: bool) {
        // ウィンドウ（表示しないテストのときは覚えない）
        if !(self.vo.hidden || self.vo.test_frames > 0) {
            if let Some(w) = self.current_window() {
                if w != self.win_seen {
                    self.win_seen = w;
                    self.win_changed_at = Instant::now();
                }
                if self.win_seen != self.win_saved && (now || self.win_changed_at.elapsed() >= Duration::from_secs(1)) {
                    remember::remember_window(&self.win_seen);
                    self.win_saved = self.win_seen.clone();
                }
            }
        }
        let cur = self.current_view();
        if self.view_rebase {
            // レジストの色などは次のフレームで設定に反映されるので、ここで基準を取り直す
            self.view_rebase = false;
            self.view_seen = cur.clone();
            self.view_saved = cur;
            return;
        }
        if cur != self.view_seen {
            self.view_seen = cur;
            self.view_changed_at = Instant::now();
        }
        if self.view_seen != self.view_saved && (now || self.view_changed_at.elapsed() >= Duration::from_secs(1)) {
            remember::remember_view(&self.prep.board.file, &self.view_seen, &self.prep.config_view);
            self.view_saved = self.view_seen.clone();
        }
    }

    /// パネルの設定をすべて既定（設定ファイルとコマンドライン）に戻し、この基板について覚えたものを忘れる
    fn reset_all_settings(&mut self) {
        remember::forget_board(&self.prep.board.file);
        let c = self.prep.configured.clone();
        let st = &mut self.st;
        st.exposure = c.exposure_ev;
        st.glow = c.glow;
        st.glow_threshold = c.glow_threshold;
        st.glow_spread = c.glow_spread;
        st.env_intensity = c.env_intensity;
        st.env_rotation_deg = c.env_rotation_deg;
        st.bump_intensity = c.scene.bump_intensity;
        st.fov = c.fov_deg;
        st.spectral = c.spectral;
        st.max_spp = self.vo.common.spp.map(pow2_spp).unwrap_or(4096);
        st.denoise = self.vo.common.denoise.unwrap_or(false);
        st.denoise_strength = self.vo.common.denoise_strength.unwrap_or(1.0);
        st.mode = DisplayMode::Beauty;
        st.reset_requested = true; // 視点も自動の構図に
        st.size_mode = None;
        st.render_ratio = 1.0;
        st.mask_choice = if c.scene.mask_preset.is_empty() { None } else { mask_presets().iter().position(|p| p.name == c.scene.mask_preset) };
        st.mask_changed = true;
        st.materials_dirty = true;
        st.dirty = true;
        if self.prep.cfg.integrator != c.integrator {
            self.prep.cfg.integrator = c.integrator.clone();
            st.integrator_changed = true;
        }
        self.prep.cfg.lens = c.lens.clone();
        // 環境マップ：--hdr、設定ファイルの [environment] hdr、内蔵のスタジオ（読み込みが終わると、それを覚え直す）
        let hdr = if !self.vo.common.hdr.is_empty() {
            remember::normalize_hdr(&self.vo.common.hdr)
        } else if !self.prep.config_hdr.is_empty() {
            remember::normalize_hdr(&self.prep.config_hdr)
        } else {
            STUDIO.to_string()
        };
        if self.env_job.is_none() && !same_env(&hdr, &self.env_path) {
            self.env_no_remember = true;
            self.start_env_load(hdr);
        }
        // 戻した値は覚えない（忘れた状態のままにする。次に変えたときから、また覚える）
        self.view_rebase = true;
        self.last_update = fl!("reset-all-done");
        info!("panel settings reset to the configuration files");
    }

    /// 最初の視点：この基板で覚えた視点、無ければ自動の構図
    fn start_camera(&mut self, w: u32, h: u32) {
        let c = initial_camera(&self.prep, &self.vo.common, w, h);
        let cfg = &self.prep.cfg;
        self.st.target = c.target;
        self.st.distance = c.distance;
        self.st.elevation = cfg.elevation_deg;
        self.st.azimuth = cfg.azimuth_deg;
        self.st.fov = cfg.fov_deg;
        self.st.tan_half_fov_y = c.tan_half_fov_y;
        self.st.dirty = true;
    }

    /// 視点を戻す（R）：設定ファイルとコマンドラインの仰角・方位・画角での自動の構図
    fn reset_camera(&mut self, w: u32, h: u32) {
        let cfg = self.prep.configured.clone();
        let c = framed_camera(&cfg, &self.prep, &self.vo.common, w, h);
        let cfg = &cfg;
        self.st.target = c.target;
        self.st.distance = c.distance;
        self.st.elevation = cfg.elevation_deg;
        self.st.azimuth = cfg.azimuth_deg;
        self.st.fov = cfg.fov_deg;
        self.st.tan_half_fov_y = c.tan_half_fov_y;
        self.st.dirty = true;
    }

    fn current_frame(&mut self, w: u32, h: u32) -> krbr_render::FrameParams {
        let mut c = self.prep.cfg.clone();
        c.env_rotation_deg = self.st.env_rotation_deg;
        c.env_intensity = self.st.env_intensity;
        c.spectral = self.st.spectral;
        let cam = orbit_camera(self.st.target, self.st.distance, self.st.elevation, self.st.azimuth, self.st.fov, w as f32 / h.max(1) as f32);
        self.st.tan_half_fov_y = cam.tan_half_fov_y;
        frame_params(&c, cam, self.vo.common.seed)
    }

    fn start_env_load(&mut self, path: String) {
        if self.env_job.is_some() || same_env(&path, &self.env_path) {
            return;
        }
        info!("loading environment {}", path);
        let (tx, rx) = mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || {
            krbr_core::platform::lower_thread_priority();
            let _ = tx.send(load_environment(p));
        });
        self.env_job = Some((path, rx));
        self.env_error.clear();
    }

    fn init_gpu(&mut self, el: &ActiveEventLoop) -> Result<(), String> {
        let hidden = self.vo.hidden || self.vo.test_frames > 0;
        let sized = self.vo.common.width.is_some() || self.vo.common.height.is_some();
        let saved = if hidden || sized { remember::WindowState::default() } else { self.win_start.clone() };
        let (sw, sh) = saved.size.unwrap_or((1600, 900));
        let (w, h) = (self.vo.common.width.unwrap_or(sw), self.vo.common.height.unwrap_or(sh));
        // ウィンドウのアイコン（exe に埋め込んだものと同じ絵。scripts/make_icon.py で作る 64x64 の RGBA）
        let icon = winit::window::Icon::from_rgba(include_bytes!("../../../krbr/assets/krbr_64.rgba").to_vec(), 64, 64).ok();
        let attrs = Window::default_attributes().with_window_icon(icon).with_title(self.base_title.clone()).with_inner_size(winit::dpi::PhysicalSize::new(w, h)).with_visible(!hidden).with_active(!hidden).with_maximized(saved.maximized);
        // 前回の位置：今つながっている画面のどれかに、ウィンドウの左上の近くが入っているときだけ（画面を外した後に見えない所へ出さない）
        let attrs = match saved.position {
            Some((x, y)) if el.available_monitors().any(|m| {
                let (p, s) = (m.position(), m.size());
                x + 40 >= p.x && y + 10 >= p.y && x + 40 < p.x + s.width as i32 && y + 10 < p.y + s.height as i32
            }) => attrs.with_position(winit::dpi::PhysicalPosition::new(x, y)),
            _ => attrs,
        };
        let window = el.create_window(attrs).map_err(|e| format!("window: {e}"))?;
        let display = window.display_handle().map_err(|e| format!("{e}"))?.as_raw();
        let wh = window.window_handle().map_err(|e| format!("{e}"))?.as_raw();
        let exts = ash_window::enumerate_required_extensions(display).map_err(|e| format!("{e:?}"))?.to_vec();
        let factory: krbr_render::vk::SurfaceFactory = Box::new(move |entry, instance| unsafe { ash_window::create_surface(entry, instance, display, wh, None) }.map_err(|e| format!("surface: {e:?}")));
        let ctx: &'static Context = Box::leak(Box::new(Context::new(&ContextOptions { validation: self.vo.common.validate, low_queue_priority: true, instance_extensions: exts, create_surface: Some(factory) })?));
        // 積分器が GpuScene への参照を持つので、ムーブしてもアドレスが変わらないよう Box に入れる
        let mut gpu = Box::new(GpuScene::new(ctx));
        gpu.build(&self.prep.scene, &self.prep.env, &self.prep.dist, self.st.bump_intensity);
        self.prep.env.rgba = Vec::new();
        if !self.prep.scene.missing.is_empty() {
            warn!("{} 3D model(s) missing or unreadable (see missing_models.txt of a render)", self.prep.scene.missing.len());
        }
        let size = window.inner_size();
        let (fbw, fbh) = (size.width.max(1), size.height.max(1));
        self.start_camera(fbw, fbh);
        // 3D 表示の大きさはメニューとパネルを置いてから決まるので、最初のフレームで視点を合わせ直す（覚えた視点があればそれ）
        self.st.start_requested = true;
        let swap = Swapchain::new(ctx, fbw, fbh)?;
        let frame = self.current_frame(swap.extent.width, swap.extent.height);
        // 安全性：Box の中身はムーブされず、Gpu を捨てるときに prog を gpu より先に捨てる（run_viewer）
        let gpu_ref: &'static GpuScene<'static> = unsafe { &*(&*gpu as *const GpuScene<'static>) };
        let prog = Progressive::new(ctx, gpu_ref, &self.prep.cfg.integrator, swap.extent.width, swap.extent.height, frame)?;
        let tone = Tonemapper::new(ctx, prog.targets())?;
        let egui_renderer = EguiRenderer::with_gpu_allocator(ctx.allocator.clone(), ctx.device.clone(), RenderMode::DynamicRendering(DynamicRendering { color_attachment_format: swap.format, depth_attachment_format: None, stencil_attachment_format: None }), EguiOptions { in_flight_frames: 1, srgb_framebuffer: false, ..Default::default() })
            .map_err(|e| format!("egui renderer: {e}"))?;
        let dev = &ctx.device;
        let cmd = unsafe { dev.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(ctx.pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1)) }.map_err(|e| format!("{e:?}"))?[0];
        let acquired = unsafe { dev.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }.map_err(|e| format!("{e:?}"))?;
        let fence = unsafe { dev.create_fence(&vk::FenceCreateInfo::default(), None) }.map_err(|e| format!("{e:?}"))?;
        let timestamps = unsafe { dev.create_query_pool(&vk::QueryPoolCreateInfo::default().query_type(vk::QueryType::TIMESTAMP).query_count(2), None) }.map_err(|e| format!("{e:?}"))?;
        let props = unsafe { ctx.instance.get_physical_device_properties(ctx.physical) };
        let ts_period_ms = props.limits.timestamp_period as f64 * 1e-6;
        info!(
            "viewer: {}x{}, up to {} spp. mouse: L=rotate, R/M=pan, wheel=zoom; keys: F1/Tab=panel, R=reset, +/-=exposure, [ ]=rotate environment, 1/2/3=image/albedo/normal, S=save, Esc=quit",
            swap.extent.width,
            swap.extent.height,
            self.st.max_spp
        );
        install_fonts(&self.egui_ctx, &self.language);
        self.egui_state = Some(EguiState::new(self.egui_ctx.clone(), egui::ViewportId::ROOT, &window, Some(window.scale_factor() as f32), None, None));
        self.gpu = Some(Gpu { ctx, gpu, swap, prog, tone, denoiser: None, tone_denoised: None, egui_renderer, cmd, acquired, fence, timestamps, ts_period_ms });
        self.window = Some(window);
        Ok(())
    }

    fn panel(&mut self, ui: &mut egui::Ui) {
        self.selection_panel(ui);

        let st = &mut self.st;
        ui.separator();
        ui.strong(fl!("panel-environment"));
        let mut chosen: Option<String> = None;
        let mut open_dialog = false;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("hdr").width(220.0).selected_text(env_label(&self.env_path)).show_ui(ui, |ui| {
                for (gi, group) in self.env_list.iter().enumerate() {
                    if gi > 0 {
                        ui.separator();
                    }
                    for p in group {
                        let r = ui.selectable_label(same_env(p, &self.env_path), env_label(p));
                        let r = if is_builtin(p) { r.on_hover_text(fl!("env-studio-tip")) } else { r.on_hover_text(p) };
                        if r.clicked() {
                            chosen = Some(p.clone());
                        }
                    }
                }
            });
            if ui.add_enabled(self.open_dialog.is_none(), egui::Button::new(fl!("env-open"))).on_hover_text(fl!("env-open-tip")).clicked() {
                open_dialog = true;
            }
        });
        ui.weak(fl!("env-remember-hint"));
        if let Some((p, _)) = &self.env_job {
            ui.colored_label(egui::Color32::from_rgb(255, 204, 77), fl!("env-loading", path = p.as_str()));
        }
        if !self.env_error.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(255, 102, 102), &self.env_error);
        }
        if ui.add(egui::Slider::new(&mut st.env_intensity, 0.05..=8.0).logarithmic(true).text(fl!("env-intensity"))).changed() {
            st.dirty = true;
        }
        if ui.add(egui::Slider::new(&mut st.env_rotation_deg, -180.0..=180.0).suffix("°").text(fl!("env-rotation"))).changed() {
            st.dirty = true;
        }

        ui.separator();
        ui.strong(fl!("panel-exposure"));
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut st.exposure, -6.0..=6.0).text("EV"));
            if ui.button(fl!("exposure-auto")).clicked() {
                st.auto_exposure_requested = true;
            }
            if ui.button("0").clicked() {
                st.exposure = 0.0;
            }
        });
        ui.add(egui::Slider::new(&mut st.glow, 0.0..=4.0).text(fl!("glow")))
            .on_hover_text(fl!("glow-tip"));
        if st.glow > 0.0 {
            ui.add(egui::Slider::new(&mut st.glow_threshold, 0.5..=20.0).logarithmic(true).text(fl!("glow-threshold")))
                .on_hover_text(fl!("glow-threshold-tip"));
            ui.add(egui::Slider::new(&mut st.glow_spread, 0.0..=1.0).text(fl!("glow-spread")))
                .on_hover_text(fl!("glow-spread-tip"));
        }
        {
            let on_cpu = self.gpu.as_ref().and_then(|g| g.denoiser.as_ref()).is_some_and(|d| d.device_name() == "CPU");
            ui.horizontal(|ui| {
                ui.checkbox(&mut st.denoise, fl!("denoise"));
                if let Some(d) = self.gpu.as_ref().and_then(|g| g.denoiser.as_ref()) {
                    ui.weak(format!("{}, {:.0} ms", d.device_name(), self.last_denoise_ms));
                    if on_cpu && ui.button(fl!("denoise-now")).clicked() {
                        st.denoise_requested = true;
                    }
                } else if self.denoiser_tried && st.denoise {
                    ui.weak(fl!("denoise-unavailable"));
                }
            });
            if st.denoise {
                ui.add(egui::Slider::new(&mut st.denoise_strength, 0.0..=1.0).text(fl!("denoise-strength"))).on_hover_text(fl!("denoise-strength-tip"));
                if on_cpu {
                    ui.weak(fl!("denoise-cpu-hint"));
                }
                if !self.denoise_error.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(255, 102, 102), &self.denoise_error);
                }
            }
        }

        ui.separator();
        ui.strong(fl!("panel-integrator"));
        let integrators = ["pt", "bdpt", "vcm"];
        let mut cur = integrators.iter().position(|i| *i == self.prep.cfg.integrator).unwrap_or(0);
        ui.horizontal(|ui| {
            ui.radio_value(&mut cur, 0, "PT");
            ui.radio_value(&mut cur, 1, "BDPT");
            ui.radio_value(&mut cur, 2, "VCM");
        });
        if integrators[cur] != self.prep.cfg.integrator {
            self.prep.cfg.integrator = integrators[cur].to_string();
            st.integrator_changed = true;
        }
        ui.weak(match cur {
            0 => fl!("integrator-pt-hint"),
            1 => fl!("integrator-bdpt-hint"),
            _ => fl!("integrator-vcm-hint"),
        });

        ui.separator();
        ui.strong(fl!("panel-lens"));
        {
            let l = &mut self.prep.cfg.lens;
            let mut ch = false;
            ui.horizontal(|ui| {
                ch |= ui.radio_value(&mut l.model, LensModel::Pinhole, fl!("lens-pinhole")).changed();
                ch |= ui.radio_value(&mut l.model, LensModel::Thin, fl!("lens-thin")).changed();
                ch |= ui.radio_value(&mut l.model, LensModel::Realistic, fl!("lens-realistic")).changed();
            });
            if l.model != LensModel::Pinhole {
                ch |= ui.add(egui::Slider::new(&mut l.f_number, 0.7..=22.0).logarithmic(true).text(fl!("lens-f-number"))).changed();
                let mut focus = if l.focus_distance_mm > 0.0 { l.focus_distance_mm } else { st.distance };
                if ui.add(egui::Slider::new(&mut focus, 5.0..=5000.0).logarithmic(true).text(fl!("lens-focus"))).changed() {
                    l.focus_distance_mm = focus;
                    ch = true;
                }
                ui.weak(fl!("lens-focus-hint"));
            }
            if l.model == LensModel::Thin {
                ch |= ui.add(egui::Slider::new(&mut l.focal_length_mm, 10.0..=300.0).logarithmic(true).text(fl!("lens-focal-length"))).changed();
                ch |= ui.add(egui::Slider::new(&mut l.blades, 0..=12).text(fl!("lens-blades"))).changed();
                if l.blades >= 3 {
                    ch |= ui.add(egui::Slider::new(&mut l.blade_rotation_deg, 0.0..=360.0).suffix("°").text(fl!("lens-blade-rotation"))).changed();
                    ch |= ui.add(egui::Slider::new(&mut l.blade_roundness, 0.0..=1.0).text(fl!("lens-blade-roundness"))).changed();
                }
                ch |= ui.checkbox(&mut l.natural_vignetting, fl!("lens-natural-vignetting")).changed();
                ch |= ui.add(egui::Slider::new(&mut l.optical_vignetting, 0.0..=1.0).text(fl!("lens-optical-vignetting"))).changed();
                ch |= ui.add(egui::Slider::new(&mut l.abbe_number, 0.0..=80.0).text(fl!("lens-axial-ca"))).changed();
                ch |= ui.add(egui::Slider::new(&mut l.lateral_ca, 0.0..=0.02).text(fl!("lens-lateral-ca"))).changed();
                if (l.abbe_number > 0.0 || l.lateral_ca > 0.0) && !st.spectral {
                    ui.weak(fl!("lens-ca-hint"));
                }
            }
            if ch {
                st.dirty = true;
            }
        }

        ui.separator();
        ui.strong(fl!("panel-color"));
        let mut spectral = st.spectral;
        ui.horizontal(|ui| {
            ui.radio_value(&mut spectral, false, "RGB");
            ui.radio_value(&mut spectral, true, fl!("color-spectral"));
        });
        if spectral != st.spectral {
            st.spectral = spectral;
            st.dirty = true;
        }

        ui.separator();
        ui.strong(fl!("panel-surface"));
        if ui.add(egui::Slider::new(&mut st.bump_intensity, 0.0..=2.0).text(fl!("bump-intensity"))).changed() {
            st.materials_dirty = true;
        }
        {
            let presets = mask_presets();
            let current = st.mask_choice.map(|i| presets[i].name.to_string()).unwrap_or_else(|| fl!("mask-as-configured"));
            egui::ComboBox::from_id_salt("mask").width(200.0).selected_text(current).show_ui(ui, |ui| {
                if ui.selectable_label(st.mask_choice.is_none(), fl!("mask-as-configured")).clicked() && st.mask_choice.is_some() {
                    st.mask_choice = None;
                    st.mask_changed = true;
                }
                for (i, p) in presets.iter().enumerate() {
                    let sel = st.mask_choice == Some(i);
                    let note = i18n::get(&format!("mask-note-{}", p.name)).unwrap_or_else(|| p.note.to_string());
                    if ui.selectable_label(sel, p.name).on_hover_text(note).clicked() && !sel {
                        st.mask_choice = Some(i);
                        st.mask_changed = true;
                    }
                }
            });
            ui.weak(fl!("mask-hint"));
        }

        ui.separator();
        ui.strong(fl!("panel-display"));
        size_controls(ui, st);
        ui.horizontal(|ui| {
            ui.radio_value(&mut st.mode, DisplayMode::Beauty, fl!("mode-image"));
            ui.radio_value(&mut st.mode, DisplayMode::Albedo, fl!("mode-albedo"));
            ui.radio_value(&mut st.mode, DisplayMode::Normal, fl!("mode-normal"));
        });
        {
            let mut e = 4u32;
            while (1u32 << e) < st.max_spp && e < 16 {
                e += 1;
            }
            if ui.add(egui::Slider::new(&mut e, 4..=16).custom_formatter(|v, _| format!("2^{} = {}", v as u32, 1u32 << (v as u32))).text(fl!("max-spp"))).changed() {
                st.max_spp = 1 << e;
            }
        }

        ui.separator();
        ui.strong(fl!("panel-camera"));
        let mut fov = st.fov as f32;
        if ui.add(egui::Slider::new(&mut fov, 5.0..=90.0).suffix("°").text(fl!("camera-fov"))).changed() {
            st.fov = fov as f64;
            st.dirty = true;
        }
        ui.label(fl!("camera-angles", elevation = format!("{:.0}", st.elevation), azimuth = format!("{:.0}", st.azimuth.rem_euclid(360.0))));
        ui.horizontal(|ui| {
            if ui.button(fl!("camera-reset")).clicked() {
                st.reset_requested = true;
            }
            if ui.button(fl!("camera-save")).clicked() {
                st.save_requested = true;
            }
        });
        ui.separator();
        ui.weak(fl!("panel-help-mouse"));
        ui.weak(fl!("panel-help-toggle"));
        if let Some(p) = chosen {
            self.start_env_load(p);
        }
        if open_dialog {
            self.start_open_dialog();
        }
    }

    /// 保存（または裏での 3D モデルの分割の完了）を見つけたら、裏のスレッドで基板を読み直す（C++ 版 viewer の reloadJob）
    fn start_reload_if_needed(&mut self) {
        if self.reload_job.is_some() {
            return;
        }
        let Some(src) = &self.prep.source else { return };
        let mut saved = self.watcher.as_mut().is_some_and(|w| w.poll());
        if self.vo.test_reload && !self.test_reload_started && self.frames >= 5 {
            saved = true;
            self.test_reload_started = true;
        }
        let models_done = src.library.completed();
        let silk = self.silk_dirty;
        let materials = self.materials_dirty_files;
        if !saved && !silk && !materials && models_done == self.models_seen {
            return;
        }
        self.models_seen = models_done;
        self.silk_dirty = false;
        self.materials_dirty_files = false;
        let mut source = self.prep.source.take().unwrap();
        let old = self.prep.board.clone();
        let options = self.prep.cfg.scene.clone();
        let saved_at = if saved { self.watcher.as_ref().and_then(|w| w.saved_at()) } else { None };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            krbr_core::platform::lower_thread_priority(); // 読み直しは低優先度（ウィンドウのスレッドは通常のまま）
            if silk {
                source.tiles.invalidate_all(); // シルクが変わったので、どのタイルも作り直す
            }
            if materials {
                source.materials = load_material_setup(&old.file); // 材質の割り当て（<名前>.krbr / model_map.toml）を書き換えた
            }
            let result = if saved { source.reload(&old, &options).map(|(b, s, st)| (Some(b), s, st)) } else { Ok((None, source.build(&old, &options, true), ReloadStats::default())) };
            let _ = tx.send(ReloadDone { source, saved, result, saved_at, silk, materials });
        });
        self.reload_job = Some(rx);
    }

    /// ノイズ除去を用意する（デバイスの準備に時間がかかるので、有効にしたときに初めて行う。失敗したら再び試さない）
    fn init_denoiser(&mut self) {
        self.denoiser_tried = true;
        let g = self.gpu.as_mut().unwrap();
        g.ctx.wait_idle();
        let t = g.prog.targets();
        let made = Denoiser::new(g.ctx, t, DenoiseQuality::Fast).and_then(|d| Tonemapper::with_views(g.ctx, d.output().view, t.albedo.view, t.normal.view, t.width, t.height).map(|tm| (d, tm)));
        match made {
            Ok((d, tm)) => {
                g.tone_denoised = Some(tm);
                g.denoiser = Some(d);
                self.denoise_error.clear();
                self.denoised_epoch = u64::MAX;
            }
            Err(e) => {
                warn!("denoiser: {e}");
                self.denoise_error = e;
            }
        }
    }

    /// 画面の点（物理ピクセル）の部品を選ぶ。基板や何も無い所なら選択を外す
    fn pick_part(&mut self, p: (f64, f64)) {
        self.flush_emit_edit(true);
        self.emit_edit = None;
        let vp = self.st.display;
        let (ww, wh) = (vp.w as f64, vp.h as f64);
        let (px, py) = (p.0 - vp.x as f64, p.1 - vp.y as f64);
        let c = orbit_camera(self.st.target, self.st.distance, self.st.elevation, self.st.azimuth, self.st.fov, (ww / wh) as f32);
        let (nx, ny) = ((2.0 * px / ww - 1.0) as f32, (2.0 * py / wh - 1.0) as f32);
        let dir = (c.forward + nx * c.tan_half_fov_y * c.aspect * c.right - ny * c.tan_half_fov_y * c.up).normalize();
        let hit = raycast_pick(&self.prep.scene, c.position, dir);
        self.selected = hit.and_then(|h| Selected::from_instance(&self.prep.scene, &self.prep.board, h.instance));
        self.material_error.clear();
        self.refresh_outline();
        match &self.selected {
            Some(s) => info!("selected {}{}", s.reference, if s.model.is_none() { " (placeholder)" } else { "" }),
            None => info!("selection cleared"),
        }
    }

    /// 選んだ部品の輪郭を作り直す（選んだとき・シーンを組み直したとき）
    fn refresh_outline(&mut self) {
        let inst = self.selected.as_ref().and_then(|s| s.find_instance(&self.prep.scene, &self.prep.board));
        self.outline = inst.map(|i| Outline::new(&self.prep.scene, i));
    }

    /// 選んだ部品の輪郭線を 3D 表示の上に重ねる（画像には入らない）。実レンズは投影が違うので描かない
    fn draw_outline(&self, root: &mut egui::Ui, central: egui::Rect) {
        let Some(o) = &self.outline else { return };
        if self.prep.cfg.lens.model == LensModel::Realistic || !central.is_positive() {
            return;
        }
        let vp = self.st.display;
        let ppp = root.ctx().pixels_per_point();
        let c = orbit_camera(self.st.target, self.st.distance, self.st.elevation, self.st.azimuth, self.st.fov, vp.w as f32 / vp.h.max(1) as f32);
        let painter = root.painter().with_clip_rect(central);
        let to_pt = |q: [f32; 2]| egui::pos2((q[0] + vp.x as f32) / ppp, (q[1] + vp.y as f32) / ppp);
        let segs = o.segments(&c, vp.w as f32, vp.h as f32);
        for (a, b) in &segs {
            painter.line_segment([to_pt(*a), to_pt(*b)], egui::Stroke::new(3.0, egui::Color32::from_black_alpha(160)));
        }
        for (a, b) in &segs {
            painter.line_segment([to_pt(*a), to_pt(*b)], egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 170, 40)));
        }
    }

    /// 選んだ部品の部位（空なら全部位）に材質を書き込む。clear なら上書きを消す
    fn assign_material(&mut self, part: &str, material: &str, clear: bool) {
        let Some(sel) = self.selected.clone() else { return };
        let scope = if sel.model.is_none() { 0 } else { self.assign_scope };
        let model_spec = sel.model.and_then(|mi| self.prep.board.footprints.iter().find(|f| f.reference == sel.reference).and_then(|f| f.models.get(mi)).map(|m| m.path.clone()));
        let o = MaterialSetOptions {
            pcb: self.prep.board.file.clone(),
            reference: if scope == 0 { sel.reference.clone() } else { String::new() },
            model: if scope == 0 { String::new() } else { model_spec.unwrap_or_default() },
            part: part.to_string(),
            material: material.to_string(),
            global: scope == 2,
        };
        let r = if clear { clear_material(&o) } else { set_material(&o).map(Some) };
        match r {
            Ok(Some(file)) => {
                let (reference, name) = (sel.reference.as_str(), file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
                self.last_update = match (clear, part.is_empty()) {
                    (false, true) => fl!("material-set-all", reference = reference, material = material, file = name),
                    (false, false) => fl!("material-set-part", reference = reference, part = part, material = material, file = name),
                    (true, true) => fl!("material-cleared-all", reference = reference, file = name),
                    (true, false) => fl!("material-cleared-part", reference = reference, part = part, file = name),
                };
                info!("material: {} {} -> {} ({})", sel.reference, if part.is_empty() { "*" } else { part }, if clear { "(cleared)" } else { material }, file.display());
                self.material_error.clear();
                self.materials_dirty_files = true;
            }
            Ok(None) => self.material_error = fl!("material-nothing-to-clear"),
            Err(e) => {
                warn!("material: {e}");
                self.material_error = e;
            }
        }
    }

    /// 発光の色・明るさの編集が止まっていれば（now なら待たずに）書き込む。
    /// 書き込んだ後は、組み直しが終わるまで表示に編集中の値を使い、終わったら捨てる
    fn flush_emit_edit(&mut self, now: bool) {
        let Some(e) = &self.emit_edit else { return };
        if e.written {
            if self.reload_job.is_none() && !self.materials_dirty_files {
                self.emit_edit = None;
            }
            return;
        }
        if !now && e.changed_at.elapsed() < EMIT_EDIT_DELAY {
            return;
        }
        let (part, material) = (e.part.clone(), e.material.clone());
        if self.selected.as_ref().is_some_and(|s| s.reference == e.reference) {
            self.assign_material(&part, &material, false);
        }
        match &mut self.emit_edit {
            Some(e) if self.material_error.is_empty() => e.written = true,
            _ => self.emit_edit = None,
        }
    }

    /// 選んだ部品の欄：部位ごとの材質と、決まった段階。材質を選ぶと設定ファイルに書いて描き直す
    fn selection_panel(&mut self, ui: &mut egui::Ui) {
        ui.strong(fl!("sel-title"));
        let Some(sel) = self.selected.clone() else {
            ui.weak(fl!("sel-none-hint"));
            return;
        };
        let mut deselect = false;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&sel.reference).strong().color(egui::Color32::from_rgb(255, 170, 40)));
            deselect = ui.small_button(fl!("sel-deselect")).clicked();
        });
        if deselect {
            self.flush_emit_edit(true);
            self.emit_edit = None;
            self.selected = None;
            self.outline = None;
        }
        let fp = self.prep.board.footprints.iter().find(|f| f.reference == sel.reference);
        // 長い名前はパネルの幅で切り詰める（カーソルを乗せると全体が出る）
        let lib_id = fp.map(|f| f.lib_id.clone()).unwrap_or_default();
        ui.add(egui::Label::new(egui::RichText::new(&lib_id).weak()).truncate()).on_hover_text(&lib_id);
        if self.selected.is_none() {
            return;
        }
        if self.prep.cfg.lens.model == LensModel::Realistic {
            ui.weak(fl!("sel-no-outline"));
        }
        let Some(mi) = sel.model else {
            // 代わりの箱：3D モデルが無い理由
            let missing: Vec<_> = self.prep.scene.missing.iter().filter(|m| m.reference == sel.reference).collect();
            if missing.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(255, 204, 77), fl!("sel-meshing"));
            } else {
                ui.colored_label(egui::Color32::from_rgb(255, 204, 77), fl!("sel-missing"));
                for m in missing {
                    ui.weak(fl!("sel-missing-model", spec = m.spec.as_str(), resolved = m.resolved.as_str(), reason = m.reason.as_str()));
                }
                ui.weak(fl!("sel-missing-hint"));
            }
            return;
        };
        let model_file = fp.and_then(|f| f.models.get(mi)).map(|m| std::path::Path::new(&m.path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()).unwrap_or_default();
        ui.add(egui::Label::new(egui::RichText::new(&model_file).weak()).truncate()).on_hover_text(&model_file);
        // このインスタンスの部位の行
        let rows: Vec<MaterialAssignment> = sel
            .find_instance(&self.prep.scene, &self.prep.board)
            .and_then(|i| match self.prep.scene.instances[i].owner {
                InstanceOwner::Model { assignments, .. } => {
                    let a = &self.prep.scene.assignments;
                    let start = assignments as usize;
                    let spec = &a.get(start)?.model_spec;
                    Some(a[start..].iter().take_while(|r| r.reference == sel.reference && &r.model_spec == spec).cloned().collect())
                }
                _ => None,
            })
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.assign_scope, 0, fl!("scope-part")).on_hover_text(fl!("scope-part-tip"));
            ui.radio_value(&mut self.assign_scope, 1, fl!("scope-model")).on_hover_text(fl!("scope-model-tip"));
            ui.radio_value(&mut self.assign_scope, 2, fl!("scope-all-boards")).on_hover_text(fl!("scope-all-boards-tip"));
        });
        // 書き込む先より優先される上書きがあると、書いても見た目は変わらない
        let shadowed_by_ref = self.assign_scope >= 1 && rows.iter().any(|r| r.source.starts_with("ref "));
        let shadowed_by_project = self.assign_scope == 2 && rows.iter().any(|r| r.source == "model (project)");
        if shadowed_by_ref {
            ui.colored_label(egui::Color32::from_rgb(255, 204, 77), fl!("sel-shadowed-ref"));
        } else if shadowed_by_project {
            ui.colored_label(egui::Color32::from_rgb(255, 204, 77), fl!("sel-shadowed-project"));
        }
        // 書き込むもの：(部位（空なら全部位）, 材質, 上書きを消すか)
        let mut actions: Vec<(String, String, bool)> = Vec::new();
        let names = self.material_names.clone();
        let combo = |ui: &mut egui::Ui, id: String, current: &str, label: String| -> Option<String> {
            let mut chosen = None;
            egui::ComboBox::from_id_salt(id).width(105.0).selected_text(label).show_ui(ui, |ui| {
                for n in &names {
                    if ui.selectable_label(n == current, n).clicked() {
                        chosen = Some(n.clone());
                    }
                }
            });
            chosen
        };
        let (pattern_tip, emit_tip) = (fl!("part-pattern-tip"), fl!("part-emit-tip"));
        // 発光の色・明るさを編集中なら、その値を表示に使う（書き込んで組み直しが終わるまで）
        let pending = self.emit_edit.as_ref().filter(|e| e.reference == sel.reference).map(|e| (e.part.clone(), e.material.clone()));
        // 色・明るさの変更：(部位, 材質)。止まってから書き込む
        let mut emit_changes: Vec<(String, String)> = Vec::new();
        egui::Grid::new("parts").num_columns(4).spacing([8.0, 4.0]).show(ui, |ui| {
            for (i, r) in rows.iter().enumerate() {
                let key = r.part_keys.split(' ').next().unwrap_or("").to_string();
                let material = match &pending {
                    Some((part, m)) if *part == key => m.clone(),
                    _ => r.material.clone(),
                };
                let (base, smooth, emission) = parse_variant(&material);
                // 表示では "kicad:" を省く（カーソルを乗せると全体が出る）
                let shown = if key.is_empty() { fl!("part-whole") } else { key.strip_prefix("kicad:").unwrap_or(&key).to_string() };
                ui.label(shown).on_hover_text(fl!("part-source-tip", keys = r.part_keys.as_str(), source = r.source.as_str()));
                // 材質を選び直しても、模様のあり・なしと発光は保つ
                if let Some(m) = combo(ui, format!("part{i}"), base, base.to_string()) {
                    actions.push((key.clone(), with_emission(&with_smooth(&m, smooth), emission), false));
                }
                let mut pattern = !smooth;
                if ui.checkbox(&mut pattern, fl!("part-pattern")).on_hover_text(&pattern_tip).changed() {
                    actions.push((key.clone(), with_smooth(&material, !pattern), false));
                }
                let mut emit = emission.is_some();
                if ui.checkbox(&mut emit, fl!("part-emit")).on_hover_text(&emit_tip).changed() {
                    let e = emit.then(|| Emission { srgb: emission_color_of_part(&r.part_keys), luminance: DEFAULT_EMISSION_LUMINANCE });
                    actions.push((key.clone(), with_emission(&material, e), false));
                }
                ui.end_row();
                if let Some(e) = emission {
                    ui.label("");
                    let mut srgb = e.srgb;
                    let mut luminance = e.luminance;
                    let mut changed = ui.color_edit_button_srgb(&mut srgb).on_hover_text(fl!("emit-color-tip")).changed();
                    let speed = (luminance * 0.01).max(1.0);
                    let drag = egui::DragValue::new(&mut luminance).range(0.0..=1.0e6).speed(speed).max_decimals(0).suffix(" cd/m²");
                    changed |= ui.add(drag).on_hover_text(fl!("emit-luminance-tip")).changed();
                    ui.label("");
                    ui.end_row();
                    if changed {
                        emit_changes.push((key.clone(), with_emission(&material, Some(Emission { srgb, luminance: luminance.round() }))));
                    }
                }
            }
            let all_smooth = !rows.is_empty() && rows.iter().all(|r| split_variant(&r.material).1);
            ui.label(fl!("parts-all"));
            if let Some(m) = combo(ui, "all".into(), "", fl!("parts-choose-all")) {
                actions.push((String::new(), with_smooth(&m, all_smooth), false));
            }
            let mut pattern = !all_smooth;
            if ui.checkbox(&mut pattern, fl!("part-pattern")).on_hover_text(fl!("parts-pattern-all-tip")).changed() {
                for r in &rows {
                    let key = r.part_keys.split(' ').next().unwrap_or("").to_string();
                    actions.push((key, with_smooth(&r.material, !pattern), false));
                }
            }
            ui.label("");
            ui.end_row();
        });
        for (part, material) in emit_changes {
            // 別の部位の編集が残っていれば先に書く
            if self.emit_edit.as_ref().is_some_and(|e| e.part != part || e.reference != sel.reference) {
                self.flush_emit_edit(true);
            }
            self.emit_edit = Some(EmitEdit { reference: sel.reference.clone(), part, material, changed_at: Instant::now(), written: false });
        }
        // 決まった段階（部位ごとに違えば、部位の名前にカーソルを乗せると出る）
        let mut sources: Vec<&str> = rows.iter().map(|r| r.source.as_str()).collect();
        sources.dedup();
        if let [one] = sources.as_slice() {
            ui.weak(fl!("sel-source", source = one.to_string()));
        } else if !sources.is_empty() {
            ui.weak(fl!("sel-source-varies"));
        }
        if ui.button(fl!("sel-clear-override")).on_hover_text(fl!("sel-clear-override-tip")).clicked() {
            actions.push((String::new(), String::new(), true));
        }
        if !self.material_error.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(255, 102, 102), &self.material_error);
        }
        if !actions.is_empty() {
            // 同じ部位の編集中の発光の値は、表示に使っていたので actions の材質名に入っている。ほかの部位のものは先に書く
            if self.emit_edit.as_ref().is_some_and(|e| !actions.iter().any(|(p, _, _)| *p == e.part)) {
                self.flush_emit_edit(true);
            }
            self.emit_edit = None;
        }
        for (part, m, clear) in actions {
            self.assign_material(&part, &m, clear);
        }
    }

    /// 線フォントの文字が変わったので、kicad-cli で裏で書き出し直す（書き出し中なら、終わった後にもう一度）
    fn request_silk(&mut self) {
        let Some(k) = self.prep.kicad_cli.clone() else { return };
        if self.silk_job.is_some() {
            self.silk_rerun = true;
            return;
        }
        let (file, name) = (self.prep.board.file.clone(), self.prep.board.project_name.clone());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(plot_stroke_silk(&k.cli, &file, &name, &k.cache_dir, k.use_cache));
        });
        self.silk_job = Some(rx);
    }

    /// 書き出しが終わっていれば、新しいシルクを設定に入れる（次の組み立てでタイルを全部作り直す）
    fn poll_silk(&mut self) {
        let Some(rx) = &self.silk_job else { return };
        match rx.try_recv() {
            Ok(silk) => {
                self.silk_job = None;
                self.prep.cfg.scene.layer_art.extra_silk = silk.side;
                self.silk_dirty = true;
                if std::mem::take(&mut self.silk_rerun) {
                    self.request_silk();
                }
            }
            Err(mpsc::TryRecvError::Disconnected) => self.silk_job = None,
            Err(mpsc::TryRecvError::Empty) => {}
        }
    }

    /// 読み直しが終わっていれば、新しいシーンに差し替える（GPU の形状と積分器を作り直す）
    fn apply_reload(&mut self) -> Result<(), String> {
        let Some(rx) = &self.reload_job else { return Ok(()) };
        let d = match rx.try_recv() {
            Ok(d) => d,
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => {
                // スレッドが異常終了した（組み立ての状態も失ったので、これ以降は読み直さない）
                self.reload_job = None;
                self.watcher = None;
                error!("board reload thread failed; watching stopped");
                self.last_update = fl!("reload-thread-failed");
                return Ok(());
            }
        };
        self.reload_job = None;
        self.prep.source = Some(d.source);
        if d.saved {
            self.test_reload_applied = true;
        }
        match d.result {
            Err(e) => {
                warn!("reload {}: {} (keeping the previous board; waiting for the next save)", self.prep.board.file.display(), e);
                self.last_update = fl!("reload-error", error = e);
            }
            Ok((board, mut scene, stats)) => {
                if let Some(b) = board {
                    self.prep.board = b;
                }
                if stats.diff.stroke_silk_changed {
                    self.request_silk();
                }
                // 読み直しの間に変えたレジストの色も反映する
                update_mask_material(&mut scene, &self.prep.board, &self.prep.cfg.scene);
                self.prep.scene = scene;
                self.refresh_outline();
                let t0 = Instant::now();
                let g = self.gpu.as_mut().unwrap();
                g.gpu.rebuild_geometry(&self.prep.scene, self.st.bump_intensity);
                g.prog.rebuild_integrator()?;
                let gpu_ms = t0.elapsed().as_secs_f64() * 1000.0;
                self.st.dirty = true;
                let pending = self.prep.scene.pending_models;
                if d.saved {
                    let since = d.saved_at.and_then(|t| t.elapsed().ok()).map(|e| fl!("reload-since", seconds = format!("{:.2}", e.as_secs_f64()))).unwrap_or_default();
                    let df = &stats.diff;
                    let full = if df.full { fl!("reload-full") } else { String::new() };
                    self.last_update = fl!(
                        "reload-done",
                        changed = df.changed.to_string(),
                        added = df.added.to_string(),
                        removed = df.removed.to_string(),
                        full = full,
                        tiles = stats.tiles_rebuilt.to_string(),
                        parse = format!("{:.0}", stats.parse_ms),
                        build = format!("{:.0}", stats.build_ms),
                        gpu = format!("{gpu_ms:.0}"),
                        since = since
                    );
                    let meshing = if pending > 0 { format!(", {pending} model(s) still meshing") } else { String::new() };
                    info!("reload: {} changed, {} added, {} removed{}, {} tile(s) rebuilt; parse {:.0} ms, build {:.0} ms, GPU {:.0} ms{}", df.changed, df.added, df.removed, if df.full { " (full)" } else { "" }, stats.tiles_rebuilt, stats.parse_ms, stats.build_ms, gpu_ms, meshing);
                } else if d.materials {
                    info!("material assignment updated; scene rebuilt");
                } else if d.silk {
                    self.last_update = fl!("silk-applied");
                    info!("stroke-font silkscreen updated; scene rebuilt");
                } else {
                    self.last_update = if pending > 0 { fl!("models-applied", pending = pending.to_string()) } else { fl!("models-all-applied") };
                    info!("models finished meshing; scene rebuilt ({} pending)", pending);
                }
            }
        }
        Ok(())
    }

    /// ファイルを選ぶダイアログを別スレッドで開く（その間も描画を続ける）
    fn start_open_dialog(&mut self) {
        if self.open_dialog.is_some() {
            return;
        }
        let mut dlg = rfd::FileDialog::new().set_title(fl!("dlg-open-env-title")).add_filter(fl!("dlg-env-filter"), &["hdr", "exr"]);
        if let Some(dir) = Path::new(&self.env_path).parent().filter(|d| !is_builtin(&self.env_path) && d.is_dir()) {
            dlg = dlg.set_directory(dir);
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(dlg.pick_file());
        });
        self.open_dialog = Some(rx);
    }

    /// 基板の設定ファイルの置き場所を変え、この基板のファイルを今すぐ移す（ほかの基板のものは次に開いたときに移る）
    fn set_settings_location(&mut self, loc: board_file::Location) {
        if let Err(e) = board_file::set_location(loc) {
            warn!("board settings location: {e}");
            self.last_update = fl!("settings-location-failed", error = e.to_string());
            return;
        }
        let file = board_file::resolve(&self.prep.board.file);
        info!("board settings location: {:?} ({})", loc, file.display());
        let shown = file.display().to_string();
        self.last_update = if file.is_file() { fl!("settings-location-changed", file = shown) } else { fl!("settings-location-not-yet", file = shown) };
    }

    /// 「名前を付けて保存」のダイアログを別スレッドで開く（その間も描画を続ける）。最初は基板のフォルダ
    fn start_save_dialog(&mut self, format: SaveFormat) {
        if self.save_dialog.is_some() {
            return;
        }
        let ext = format.extension();
        let (title, filter) = match format {
            SaveFormat::Png => (fl!("dlg-save-png-title"), fl!("dlg-png-filter")),
            SaveFormat::Exr => (fl!("dlg-save-exr-title"), fl!("dlg-exr-filter")),
        };
        let mut dlg = rfd::FileDialog::new().set_title(title).add_filter(filter, &[ext]).set_file_name(format!("{}.{ext}", self.stem));
        if let Some(dir) = self.prep.board.file.parent().filter(|d| d.is_dir()) {
            dlg = dlg.set_directory(dir);
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // 拡張子を付けずに名前を入れたときは付ける
            let _ = tx.send(dlg.save_file().map(|p| if p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext)) { p } else { p.with_extension(ext) }));
        });
        self.save_dialog = Some((format, rx));
    }

    /// ウィンドウ上端のメニュー
    /// メニューの下のボタン列（レンダリングの一時停止）
    fn tool_bar(&mut self, root: &mut egui::Ui) {
        // 最大 spp に達したら、止めるものが無いので「完了」にして押せなくする（視点などを変えて累積をやり直すと元に戻る）
        let done = self.gpu.as_ref().is_some_and(|g| g.prog.samples() >= self.st.max_spp);
        let st = &mut self.st;
        egui::Panel::top("toolbar").show(root, |ui| {
            ui.horizontal(|ui| {
                if done {
                    ui.add_enabled(false, egui::Button::new(fl!("toolbar-done"))).on_disabled_hover_text(fl!("toolbar-done-tip"));
                    return;
                }
                let label = if st.paused { fl!("toolbar-resume") } else { fl!("toolbar-pause") };
                let tip = fl!("toolbar-pause-tip");
                if ui.add(egui::Button::new(label).selected(st.paused)).on_hover_text(tip).clicked() {
                    st.paused = !st.paused;
                }
            });
        });
    }

    /// 画面の下端のステータスバー：状態、spp と進み具合、経過時間、1 サンプルあたりの平均、残りの予想、描画の条件。
    /// 読み直しなどの知らせがあれば 2 行目に出す
    fn status_bar(&mut self, root: &mut egui::Ui) {
        let Some(g) = self.gpu.as_ref() else { return };
        let st = &self.st;
        let (samples, max_spp) = (g.prog.samples(), st.max_spp);
        let done = samples >= max_spp;
        let yellow = egui::Color32::from_rgb(255, 204, 77);
        let measured = self.accum_measured.max(1) as f64;
        let wall_per = self.accum_wall.as_secs_f64() / measured;
        let info = &g.ctx.info;
        let method = if g.ctx.use_rt_pipeline { "ray tracing pipeline" } else { "ray query" };
        let mut notes: Vec<(String, bool)> = Vec::new();
        if self.reload_job.is_some() {
            notes.push((fl!("status-reloading"), true));
        }
        if self.silk_job.is_some() {
            notes.push((fl!("status-silk"), true));
        }
        if self.prep.scene.pending_models > 0 {
            notes.push((fl!("status-meshing", count = self.prep.scene.pending_models.to_string()), true));
        }
        if !self.last_update.is_empty() {
            notes.push((self.last_update.clone(), false));
        }
        egui::Panel::bottom("status").show(root, |ui| {
            ui.horizontal_wrapped(|ui| {
                if st.paused {
                    ui.colored_label(yellow, fl!("status-paused"));
                } else if done {
                    ui.colored_label(egui::Color32::from_rgb(120, 200, 120), fl!("status-done"));
                } else {
                    ui.label(fl!("status-rendering"));
                }
                ui.add(egui::ProgressBar::new((samples as f32 / max_spp.max(1) as f32).min(1.0)).desired_width(140.0).text(format!("{samples} / {max_spp} spp")));
                ui.separator();
                ui.label(fl!("status-elapsed", time = format_duration(self.accum_wall.as_secs_f64())))
                    .on_hover_text(fl!("status-elapsed-tip"));
                if self.accum_measured > 0 {
                    ui.label(fl!("status-average", wall = format!("{:.1}", wall_per * 1000.0), gpu = format!("{:.1}", self.accum_gpu_ms / measured)))
                        .on_hover_text(fl!("status-average-tip"));
                    if !done && !st.paused {
                        ui.label(fl!("status-remaining", time = format_duration(wall_per * (max_spp - samples) as f64)));
                    }
                }
            });
            // 2 行目：描画の条件と、読み直しなどの知らせ（幅が足りなければ折り返す）
            ui.horizontal_wrapped(|ui| {
                ui.weak(fl!("status-integrator", integrator = g.prog.integrator_name().to_uppercase(), color = if st.spectral { fl!("status-spectral") } else { "RGB".into() }));
                ui.separator();
                let (rw, rh) = (g.prog.targets().width, g.prog.targets().height);
                ui.weak(fl!("status-render-size", width = rw.to_string(), height = rh.to_string(), percent = format!("{:.0}", 100.0 * st.display.w as f64 / rw.max(1) as f64)))
                    .on_hover_text(if st.size_mode.is_some() { fl!("status-size-fixed-tip") } else { fl!("status-size-fit-tip") });
                ui.separator();
                ui.weak(format!("EV {:+.2}", st.exposure));
                ui.separator();
                ui.weak(fl!("status-gpu", name = info.name.as_str(), method = method));
                for (text, busy) in &notes {
                    ui.separator();
                    if *busy {
                        ui.colored_label(yellow, text);
                    } else {
                        ui.weak(text);
                    }
                }
            });
        });
    }

    fn menu_bar(&mut self, root: &mut egui::Ui) {
        let language_choice = self.language_choice.clone();
        let st = &mut self.st;
        egui::Panel::top("menu").show(root, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button(fl!("menu-file"), |ui| {
                    if ui.add(egui::Button::new(fl!("menu-save-image")).shortcut_text("S")).on_hover_text(fl!("menu-save-image-tip")).clicked() {
                        st.save_requested = true;
                        ui.close();
                    }
                    if ui.button(fl!("menu-save-png")).on_hover_text(fl!("menu-save-png-tip")).clicked() {
                        st.save_as_requested = Some(SaveFormat::Png);
                        ui.close();
                    }
                    if ui.button(fl!("menu-save-exr")).on_hover_text(fl!("menu-save-exr-tip")).clicked() {
                        st.save_as_requested = Some(SaveFormat::Exr);
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button(fl!("menu-settings-location"), |ui| {
                        let now = board_file::location();
                        let choices = [
                            (board_file::Location::NextToBoard, fl!("loc-next-to-board"), fl!("loc-next-to-board-tip")),
                            (board_file::Location::ConfigDir, fl!("loc-config-dir"), fl!("loc-config-dir-tip")),
                        ];
                        for (loc, label, tip) in choices {
                            if ui.add(egui::Button::new(label).selected(now == loc)).on_hover_text(tip).clicked() {
                                st.settings_location_requested = Some(loc);
                                ui.close();
                            }
                        }
                        ui.weak(fl!("loc-hint"));
                    });
                    if ui.button(fl!("menu-reveal-settings")).on_hover_text(fl!("menu-reveal-settings-tip")).clicked() {
                        st.reveal_settings_requested = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new(fl!("menu-quit")).shortcut_text("Esc")).clicked() {
                        st.quit_requested = true;
                        ui.close();
                    }
                });
                ui.menu_button(fl!("menu-view"), |ui| {
                    if ui.add(egui::Button::new(fl!("menu-settings-panel")).selected(st.show_panel).shortcut_text("F1 / Tab")).clicked() {
                        st.show_panel = !st.show_panel;
                        ui.close();
                    }
                    if ui.add(egui::Button::new(fl!("menu-pause")).selected(st.paused).shortcut_text("P")).clicked() {
                        st.paused = !st.paused;
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button(fl!("menu-render-size"), |ui| {
                        if ui.add(egui::Button::new(fl!("size-fit-window")).selected(st.size_mode.is_none())).clicked() {
                            st.size_mode = None;
                            ui.close();
                        }
                        for (w, h) in SIZE_PRESETS {
                            if ui.add(egui::Button::new(size_label(w, h)).selected(st.size_mode == Some((w, h)))).clicked() {
                                st.size_mode = Some((w, h));
                                st.fit_window_requested = true;
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button(fl!("menu-render-ratio"), |ui| {
                        for r in RATIOS {
                            if ui.add(egui::Button::new(ratio_label(r)).selected(st.render_ratio == r)).clicked() {
                                st.render_ratio = r;
                                st.fit_window_requested = st.size_mode.is_some();
                                ui.close();
                            }
                        }
                    });
                    ui.separator();
                    for (mode, name, key) in [(DisplayMode::Beauty, fl!("mode-image"), "1"), (DisplayMode::Albedo, fl!("mode-albedo"), "2"), (DisplayMode::Normal, fl!("mode-normal"), "3")] {
                        if ui.add(egui::Button::new(name).selected(st.mode == mode).shortcut_text(key)).clicked() {
                            st.mode = mode;
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.add(egui::Button::new(fl!("menu-reset-view")).shortcut_text("R")).clicked() {
                        st.reset_requested = true;
                        ui.close();
                    }
                    if ui.button(fl!("menu-auto-exposure")).clicked() {
                        st.auto_exposure_requested = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(fl!("menu-reset-all")).on_hover_text(fl!("menu-reset-all-tip")).clicked() {
                        st.reset_all_requested = true;
                        ui.close();
                    }
                    ui.separator();
                    ui.menu_button(fl!("menu-language"), |ui| {
                        if ui.add(egui::Button::new(fl!("language-auto")).selected(language_choice.is_none())).on_hover_text(fl!("language-auto-tip")).clicked() {
                            st.language_requested = Some(None);
                            ui.close();
                        }
                        ui.separator();
                        for (tag, name) in i18n::LANGUAGES {
                            if ui.add(egui::Button::new(*name).selected(language_choice.as_deref() == Some(*tag))).clicked() {
                                st.language_requested = Some(Some(tag.to_string()));
                                ui.close();
                            }
                        }
                    });
                });
                ui.menu_button(fl!("menu-help"), |ui| {
                    ui.label(fl!("help-rotate"));
                    ui.label(fl!("help-select"));
                    ui.label(fl!("help-pan"));
                    ui.label(fl!("help-zoom"));
                    ui.label(fl!("help-focus"));
                    ui.separator();
                    ui.label(fl!("help-keys"));
                    ui.label(fl!("help-keys-2"));
                    ui.label(fl!("help-quit"));
                });
            });
        });
    }

    /// 右端に上下いっぱいで固定する設定パネル（幅は境目のドラッグで変えられる、× で閉じる）
    fn side_panel(&mut self, root: &mut egui::Ui) {
        let mut close = false;
        let shown = egui::Panel::right("settings").resizable(true).default_size(self.panel_width).size_range(300.0..=640.0).show(root, |ui| {
            ui.horizontal(|ui| {
                ui.strong(fl!("panel-settings"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if close_button(ui).on_hover_text(fl!("panel-close-tip")).clicked() {
                        close = true;
                    }
                });
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.panel(ui));
        });
        self.panel_width = shown.response.rect.width();
        if close {
            self.st.show_panel = false;
        }
    }

    fn frame(&mut self) -> Result<(), String> {
        let frame_t0 = Instant::now();
        let Some(window) = &self.window else { return Ok(()) };
        let size = window.inner_size();
        let (fbw, fbh) = (size.width, size.height);
        if fbw == 0 || fbh == 0 {
            return Ok(()); // 最小化中
        }
        if self.vo.test_orbit_deg != 0.0 {
            self.st.azimuth += self.vo.test_orbit_deg;
            self.st.dirty = true;
        }

        // ---- メニューと操作パネル。残りの領域が 3D 表示になる
        let raw = self.egui_state.as_mut().unwrap().take_egui_input(window);
        let egui_ctx = self.egui_ctx.clone();
        let mut central = egui::Rect::NOTHING;
        let full = egui_ctx.run_ui(raw, |root| {
            self.menu_bar(root);
            self.tool_bar(root);
            self.status_bar(root);
            if self.st.show_panel {
                self.side_panel(root);
            }
            central = root.available_rect_before_wrap();
            self.draw_outline(root, central);
        });
        if self.st.quit_requested {
            self.done = true;
            return Ok(());
        }
        let window = self.window.as_ref().unwrap();
        self.egui_state.as_mut().unwrap().handle_platform_output(window, full.platform_output);
        let primitives = egui_ctx.tessellate(full.shapes, full.pixels_per_point);

        // ---- 状態の反映（GPU は前のフレームの完了を待っているので、ここで資源を触ってよい）
        if self.frames == 3 {
            if let Some(i) = mask_presets().iter().position(|p| !self.vo.test_mask.is_empty() && p.name == self.vo.test_mask) {
                self.st.mask_choice = Some(i);
                self.st.mask_changed = true;
            }
            self.st.reset_all_requested |= self.vo.test_reset_settings;
        }
        // 覚えるのは前のフレームまでに反映した値（「すべて既定に戻す」で変えた値は、反映した次のフレームで基準にする）
        self.save_view(false);
        if self.st.reset_all_requested {
            self.st.reset_all_requested = false;
            self.reset_all_settings();
        }
        self.apply_reload()?;
        self.poll_silk();
        self.flush_emit_edit(false);
        self.start_reload_if_needed();
        if let Some(f) = self.st.save_as_requested.take() {
            self.start_save_dialog(f);
        }
        if let Some(choice) = self.st.language_requested.take() {
            // メニューで選んだものは --lang より優先する（覚えて、次からも使う）
            remember::remember_ui_language(choice.as_deref());
            self.language = i18n::select(None, choice.as_deref());
            self.language_choice = choice;
            install_fonts(&self.egui_ctx, &self.language);
            info!("language: {}", self.language);
        }
        if let Some(loc) = self.st.settings_location_requested.take() {
            self.set_settings_location(loc);
        }
        if std::mem::take(&mut self.st.reveal_settings_requested) {
            krbr_core::platform::reveal_in_file_manager(&board_file::resolve(&self.prep.board.file));
        }
        if let Some((f, rx)) = &self.save_dialog {
            match rx.try_recv() {
                Ok(picked) => {
                    if let Some(p) = picked {
                        self.save_as_target = Some((*f, p));
                    }
                    self.save_dialog = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.save_dialog = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &self.open_dialog {
            match rx.try_recv() {
                Ok(picked) => {
                    self.open_dialog = None;
                    if let Some(p) = picked {
                        self.start_env_load(p.to_string_lossy().into_owned());
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.open_dialog = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some((path, rx)) = &self.env_job {
            if let Ok(d) = rx.try_recv() {
                let _ = path;
                self.env_job = None;
                let no_remember = std::mem::take(&mut self.env_no_remember);
                if d.error.is_empty() {
                    let g = self.gpu.as_mut().unwrap();
                    g.ctx.wait_idle();
                    g.gpu.set_environment(&d.image, &d.dist);
                    g.prog.rebuild_integrator()?;
                    self.env_path = remember::normalize_hdr(&d.path);
                    if !no_remember {
                        remember::remember_hdr(&self.prep.board.file, &self.env_path, &self.prep.config_hdr);
                    }
                    self.env_list = list_environments(&self.env_path);
                    self.st.dirty = true;
                    info!("environment: {} ({}x{})", self.env_path, d.image.width, d.image.height);
                } else {
                    self.env_error = d.error.clone();
                    error!("environment {}: {}", d.path, d.error);
                }
            }
        }
        if self.st.integrator_changed {
            self.st.integrator_changed = false;
            let g = self.gpu.as_mut().unwrap();
            g.ctx.wait_idle();
            if let Err(e) = g.prog.set_integrator(&self.prep.cfg.integrator) {
                error!("integrator {}: {}", self.prep.cfg.integrator, e);
                self.prep.cfg.integrator = "pt".into();
                g.prog.set_integrator("pt")?;
            } else {
                info!("integrator: {}", self.prep.cfg.integrator);
            }
            self.st.dirty = true;
        }
        if self.st.mask_changed {
            self.st.mask_changed = false;
            match self.st.mask_choice {
                None => {
                    let s = &self.scene_as_configured;
                    let o = &mut self.prep.cfg.scene;
                    o.mask_color = s.mask_color;
                    o.mask_color_set = s.mask_color_set;
                    o.mask_roughness = s.mask_roughness;
                    o.mask_scatter = s.mask_scatter;
                    o.mask_preset = s.mask_preset.clone();
                    o.silk_color = s.silk_color;
                }
                Some(i) => {
                    let _ = apply_mask_color_spec(&mut self.prep.cfg.scene, mask_presets()[i].name);
                }
            }
            update_mask_material(&mut self.prep.scene, &self.prep.board, &self.prep.cfg.scene);
            self.st.materials_dirty = true;
        }
        if self.st.materials_dirty {
            self.st.materials_dirty = false;
            let g = self.gpu.as_mut().unwrap();
            g.ctx.wait_idle();
            g.gpu.update_materials(&self.prep.scene.materials, self.st.bump_intensity);
            self.st.dirty = true;
        }
        {
            let g = self.gpu.as_mut().unwrap();
            if self.need_recreate || fbw != g.swap.extent.width || fbh != g.swap.extent.height {
                g.ctx.wait_idle();
                g.swap.recreate(fbw, fbh)?;
                self.need_recreate = false;
            }
            // 3D 表示の領域（スワップチェーンがウィンドウと違う大きさのこともあるので、その中に収める）。
            // 描画の解像度は、決まったサイズか、領域 × 比率
            let vp = viewport_px(central, full.pixels_per_point, g.swap.extent.width, g.swap.extent.height);
            let (rw, rh) = self.st.size_mode.unwrap_or_else(|| (((vp.w as f32 * self.st.render_ratio).round() as u32).max(1), ((vp.h as f32 * self.st.render_ratio).round() as u32).max(1)));
            let t = g.prog.targets();
            if rw != t.width || rh != t.height {
                g.prog.resize(rw, rh)?;
                g.tone = Tonemapper::new(g.ctx, g.prog.targets())?;
                // ノイズ除去は新しい大きさで作り直す
                if g.denoiser.is_some() {
                    g.tone_denoised = None;
                    g.denoiser = None;
                    self.denoiser_tried = false;
                }
                self.st.dirty = true;
            }
            self.st.viewport = vp;
            self.st.display = fit_rect(vp, rw, rh);
        }
        self.fit_window();
        if let Some(p) = self.vo.test_save_as.as_ref().filter(|_| self.vo.test_frames > 0 && self.frames + 2 == self.vo.test_frames) {
            let format = if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exr")) { SaveFormat::Exr } else { SaveFormat::Png };
            self.save_as_target = Some((format, p.clone()));
        }
        if self.vo.test_pick.is_some() && !self.test_pick_done && self.frames >= 5 {
            self.test_pick_done = true;
            let (x, y) = self.vo.test_pick.unwrap();
            self.st.part_pick = Some((x + self.st.viewport.x as f64, y + self.st.viewport.y as f64));
        }
        if let Some(p) = self.st.part_pick.take() {
            self.pick_part(p);
            if self.vo.test_pick.is_some() && !self.vo.test_assign.is_empty() {
                let m = self.vo.test_assign.clone();
                self.assign_material("", &m, false);
            }
        }
        if let Some((px, py)) = self.st.focus_pick.take() {
            let vp = self.st.display;
            let (ww, wh) = (vp.w as f64, vp.h as f64);
            let (px, py) = (px - vp.x as f64, py - vp.y as f64);
            let c = orbit_camera(self.st.target, self.st.distance, self.st.elevation, self.st.azimuth, self.st.fov, (ww / wh) as f32);
            let (nx, ny) = ((2.0 * px / ww - 1.0) as f32, (2.0 * py / wh - 1.0) as f32);
            let dir = (c.forward + nx * c.tan_half_fov_y * c.aspect * c.right - ny * c.tan_half_fov_y * c.up).normalize();
            if let Some(t) = raycast(&self.prep.scene, c.position, dir) {
                self.prep.cfg.lens.focus_distance_mm = t * dir.dot(c.forward);
                if self.prep.cfg.lens.model == LensModel::Pinhole {
                    self.prep.cfg.lens.model = LensModel::Thin;
                }
                self.last_update = fl!("focus-set", mm = format!("{:.1}", self.prep.cfg.lens.focus_distance_mm));
                self.st.dirty = true;
            }
        }
        if self.st.start_requested {
            self.st.start_requested = false;
            self.start_camera(self.st.display.w, self.st.display.h);
        }
        if self.st.reset_requested {
            self.st.reset_requested = false;
            self.reset_camera(self.st.display.w, self.st.display.h);
        }
        if self.st.dirty {
            let (w, h) = {
                let t = self.gpu.as_ref().unwrap().prog.targets();
                (t.width, t.height)
            };
            let f = self.current_frame(w, h);
            self.gpu.as_mut().unwrap().prog.set_frame(f);
            self.st.dirty = false;
            self.last_change_at = Instant::now();
        }
        if self.st.auto_exposure_requested {
            self.st.auto_exposure_requested = false;
            let g = self.gpu.as_ref().unwrap();
            if g.prog.samples() > 0 {
                self.st.exposure = auto_exposure(&g.prog.readback().color);
                info!("auto exposure: {:+.2} EV", self.st.exposure);
            }
        }

        if self.st.denoise && !self.denoiser_tried {
            self.init_denoiser();
        }

        // ---- 描画
        let g = self.gpu.as_mut().unwrap();
        let loader = g.ctx.swapchain_loader.as_ref().unwrap();
        let (image_index, _) = match unsafe { loader.acquire_next_image(g.swap.handle, u64::MAX, g.acquired, vk::Fence::null()) } {
            Ok(r) => r,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.need_recreate = true;
                return Ok(());
            }
            Err(e) => return Err(format!("vkAcquireNextImageKHR: {e:?}")),
        };
        let max_spp = self.st.max_spp;
        // 一時停止中は積み足さない（カメラなどを変えて累積をやり直したときだけ、表示のために 1 サンプル描く）
        let add = if self.st.paused {
            u32::from(g.prog.samples() == 0)
        } else if g.prog.samples() >= max_spp {
            0
        } else {
            self.samples_per_frame.min(max_spp - g.prog.samples())
        };
        let last_test_frame = self.vo.test_frames > 0 && self.frames + 1 >= self.vo.test_frames && self.env_job.is_none() && self.reload_job.is_none() && (!self.vo.test_reload || self.test_reload_applied) && (self.vo.test_pick.is_none() || self.test_pick_done) && !self.materials_dirty_files;

        // egui のテクスチャ
        for (id, deltas) in &full.textures_delta.set {
            for delta in deltas.iter() {
                g.egui_renderer.set_texture(g.ctx.queue, g.ctx.pool, *id, delta).map_err(|e| format!("egui texture: {e}"))?;
            }
        }

        // 累積をやり直したら（サンプル数が減ったら）、前のノイズ除去の結果は使わない
        if g.prog.samples() < self.prev_samples {
            self.accum_epoch += 1;
        }
        if g.prog.samples() == 0 {
            // 累積をやり直した：経過時間と平均も 0 から
            self.accum_wall = Duration::ZERO;
            self.accum_gpu_ms = 0.0;
            self.accum_measured = 0;
        }
        // このフレームでノイズ除去するか（画像の表示のときだけ）。C++ 版と同じ間隔：
        // GPU：動かしている間は毎フレーム fast、止まったら 150 ms ごとに high、収束時にもう一度。
        // CPU：数秒かかり画面が止まるので、収束時（最大 spp に達したとき）と D キー／ボタンのときだけ high でかける
        let on_cpu = g.denoiser.as_ref().is_some_and(|d| d.device_name() == "CPU");
        let moving = self.last_change_at.elapsed() < Duration::from_millis(300);
        let samples_after = g.prog.samples() + add;
        let due = if on_cpu { samples_after >= max_spp || self.st.denoise_requested } else { moving || samples_after >= max_spp || self.last_denoise_at.elapsed() > Duration::from_millis(150) };
        let beauty = self.st.mode == DisplayMode::Beauty;
        let want = self.st.denoise && g.denoiser.is_some() && beauty && samples_after > 0 && (self.denoised_epoch != self.accum_epoch || self.denoised_samples != samples_after) && due;
        self.st.denoise_requested = false;
        let ctx = g.ctx;
        let timestamps = g.timestamps;
        if want {
            // サンプルの追加と入力の準備を先に流して完了を待ち、OIDN で除去してから表示のコマンドを作る
            let d = g.denoiser.as_mut().unwrap();
            d.set_quality(if moving && !on_cpu { DenoiseQuality::Fast } else { DenoiseQuality::High });
            let (prog, d) = (&mut g.prog, &*d);
            ctx.submit_and_wait(|dev, c| {
                unsafe {
                    dev.cmd_reset_query_pool(c, timestamps, 0, 2);
                    dev.cmd_write_timestamp2(c, vk::PipelineStageFlags2::ALL_COMMANDS, timestamps, 0);
                }
                prog.record(c, add);
                unsafe { dev.cmd_write_timestamp2(c, vk::PipelineStageFlags2::ALL_COMMANDS, timestamps, 1) };
                d.record_prepare(c);
            });
            match d.execute() {
                Ok(ms) => {
                    self.last_denoise_ms = ms;
                    self.denoised_epoch = self.accum_epoch;
                    self.denoised_samples = g.prog.samples();
                }
                Err(e) => warn!("{e}"),
            }
            self.last_denoise_at = Instant::now();
        }
        let show_denoised = self.st.denoise && g.denoiser.is_some() && g.tone_denoised.is_some() && beauty && self.denoised_epoch == self.accum_epoch;
        let dev = &ctx.device;
        let cmd = g.cmd;
        unsafe {
            dev.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty()).map_err(|e| format!("{e:?}"))?;
            dev.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)).map_err(|e| format!("{e:?}"))?;
        }
        if !want {
            unsafe {
                dev.cmd_reset_query_pool(cmd, timestamps, 0, 2);
                dev.cmd_write_timestamp2(cmd, vk::PipelineStageFlags2::ALL_COMMANDS, timestamps, 0);
            }
            g.prog.record(cmd, add);
            unsafe { dev.cmd_write_timestamp2(cmd, vk::PipelineStageFlags2::ALL_COMMANDS, timestamps, 1) };
        }
        self.prev_samples = g.prog.samples();
        let disp = if show_denoised {
            // 結果を元の画像と強さで混ぜる（軽いので毎フレーム。強さのスライダーがすぐ効く）
            g.denoiser.as_ref().unwrap().record_output(cmd, self.st.denoise_strength);
            let t = g.tone_denoised.as_ref().unwrap();
            t.record(cmd, self.st.exposure, self.st.mode, g.prog.xyz_output(), &self.st.glow_params());
            t.output()
        } else {
            g.tone.record(cmd, self.st.exposure, self.st.mode, g.prog.xyz_output(), &self.st.glow_params());
            g.tone.output()
        };
        let swap_img = g.swap.images[image_index as usize];
        let extent = g.swap.extent;
        image_barrier(dev, cmd, disp.image, vk::ImageLayout::GENERAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::PipelineStageFlags2::COMPUTE_SHADER, vk::AccessFlags2::SHADER_STORAGE_WRITE, vk::PipelineStageFlags2::BLIT, vk::AccessFlags2::TRANSFER_READ, 1);
        image_barrier(dev, cmd, swap_img, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::PipelineStageFlags2::CLEAR, vk::AccessFlags2::NONE, vk::PipelineStageFlags2::CLEAR, vk::AccessFlags2::TRANSFER_WRITE, 1);
        // メニュー・パネルの下も含めて一度塗りつぶし、3D 表示の領域に画像を置く
        let range = vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 };
        unsafe { dev.cmd_clear_color_image(cmd, swap_img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &vk::ClearColorValue { float32: [0.1, 0.1, 0.1, 1.0] }, &[range]) };
        image_barrier(dev, cmd, swap_img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::PipelineStageFlags2::CLEAR, vk::AccessFlags2::TRANSFER_WRITE, vk::PipelineStageFlags2::BLIT, vk::AccessFlags2::TRANSFER_WRITE, 1);
        let vp = self.st.display;
        let scaled = vp.w != disp.width || vp.h != disp.height;
        let sub = vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 };
        let blit = vk::ImageBlit {
            src_subresource: sub,
            src_offsets: [vk::Offset3D::default(), vk::Offset3D { x: disp.width as i32, y: disp.height as i32, z: 1 }],
            dst_subresource: sub,
            dst_offsets: [vk::Offset3D { x: vp.x as i32, y: vp.y as i32, z: 0 }, vk::Offset3D { x: (vp.x + vp.w) as i32, y: (vp.y + vp.h) as i32, z: 1 }],
        };
        unsafe { dev.cmd_blit_image(cmd, disp.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, swap_img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[blit], if scaled { vk::Filter::LINEAR } else { vk::Filter::NEAREST }) };
        image_barrier(dev, cmd, disp.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::ImageLayout::GENERAL, vk::PipelineStageFlags2::BLIT, vk::AccessFlags2::TRANSFER_READ, vk::PipelineStageFlags2::COMPUTE_SHADER | vk::PipelineStageFlags2::COPY, vk::AccessFlags2::SHADER_STORAGE_WRITE | vk::AccessFlags2::TRANSFER_READ, 1);

        // 操作パネルを画像の上に重ねて描く（動的レンダリング）
        image_barrier(dev, cmd, swap_img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::PipelineStageFlags2::BLIT, vk::AccessFlags2::TRANSFER_WRITE, vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT, vk::AccessFlags2::COLOR_ATTACHMENT_READ | vk::AccessFlags2::COLOR_ATTACHMENT_WRITE, 1);
        let att = [vk::RenderingAttachmentInfo::default().image_view(g.swap.views[image_index as usize]).image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL).load_op(vk::AttachmentLoadOp::LOAD).store_op(vk::AttachmentStoreOp::STORE)];
        let ri = vk::RenderingInfo::default().render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent }).layer_count(1).color_attachments(&att);
        unsafe { dev.cmd_begin_rendering(cmd, &ri) };
        g.egui_renderer.cmd_draw(cmd, extent, full.pixels_per_point, &primitives).map_err(|e| format!("egui draw: {e}"))?;
        unsafe { dev.cmd_end_rendering(cmd) };

        let mut shot: Option<Buffer> = None;
        if last_test_frame && g.swap.can_read_back && self.vo.screenshot.is_some() {
            image_barrier(dev, cmd, swap_img, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT, vk::AccessFlags2::COLOR_ATTACHMENT_WRITE, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_READ, 1);
            let b = g.ctx.create_buffer(extent.width as u64 * extent.height as u64 * 4, vk::BufferUsageFlags::TRANSFER_DST, true, true);
            let region = vk::BufferImageCopy::default().image_subresource(sub).image_extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 });
            unsafe { dev.cmd_copy_image_to_buffer(cmd, swap_img, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, b.buffer, &[region]) };
            image_barrier(dev, cmd, swap_img, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::ImageLayout::PRESENT_SRC_KHR, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_READ, vk::PipelineStageFlags2::BOTTOM_OF_PIPE, vk::AccessFlags2::NONE, 1);
            shot = Some(b);
        } else {
            image_barrier(dev, cmd, swap_img, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::ImageLayout::PRESENT_SRC_KHR, vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT, vk::AccessFlags2::COLOR_ATTACHMENT_WRITE, vk::PipelineStageFlags2::BOTTOM_OF_PIPE, vk::AccessFlags2::NONE, 1);
        }
        unsafe { dev.end_command_buffer(cmd).map_err(|e| format!("{e:?}"))? };

        let wait = [g.acquired];
        let stages = [vk::PipelineStageFlags::TRANSFER];
        let cmds = [cmd];
        let signal = [g.swap.render_done[image_index as usize]];
        let si = vk::SubmitInfo::default().wait_semaphores(&wait).wait_dst_stage_mask(&stages).command_buffers(&cmds).signal_semaphores(&signal);
        let t0 = Instant::now();
        unsafe {
            dev.reset_fences(&[g.fence]).map_err(|e| format!("{e:?}"))?;
            dev.queue_submit(g.ctx.queue, &[si], g.fence).map_err(|e| format!("vkQueueSubmit: {e:?}"))?;
            let swaps = [g.swap.handle];
            let indices = [image_index];
            let pi = vk::PresentInfoKHR::default().wait_semaphores(&signal).swapchains(&swaps).image_indices(&indices);
            match loader.queue_present(g.ctx.queue, &pi) {
                Ok(suboptimal) => {
                    if suboptimal {
                        self.need_recreate = true;
                    }
                }
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => self.need_recreate = true,
                Err(e) => return Err(format!("vkQueuePresentKHR: {e:?}")),
            }
            dev.wait_for_fences(&[g.fence], true, u64::MAX).map_err(|e| format!("{e:?}"))?;
        }
        self.last_frame_ms = t0.elapsed().as_secs_f64() * 1000.0;
        self.frames += 1;
        for id in &full.textures_delta.free {
            let _ = g.egui_renderer.free_texture(*id);
        }

        // サンプルを積む GPU 時間が目標に収まるように、1 フレームに積む数を調整する
        let mut ts = [0u64; 2];
        let ts_ok = unsafe { dev.get_query_pool_results(g.timestamps, 0, &mut ts, vk::QueryResultFlags::TYPE_64) }.is_ok();
        self.last_gpu_ms = if ts_ok { (ts[1] - ts[0]) as f64 * g.ts_period_ms } else { self.last_frame_ms };
        if add > 0 {
            let per_sample = self.last_gpu_ms / add as f64;
            self.samples_per_frame = (TARGET_FRAME_MS / per_sample.max(0.05)).clamp(1.0, 64.0) as u32;
            self.accum_wall += frame_t0.elapsed();
            self.accum_gpu_ms += self.last_gpu_ms;
            self.accum_measured += add;
        }

        if self.st.save_requested {
            self.st.save_requested = false;
            let aov = g.prog.readback();
            let png = self.out_dir.join(format!("{}_{:03}.png", self.stem, self.save_index));
            let exr = self.out_dir.join(format!("{}_{:03}.exr", self.stem, self.save_index));
            self.save_index += 1;
            let _ = std::fs::create_dir_all(&self.out_dir);
            match image_io::save_png_tonemapped(&png, &aov.color, self.st.exposure, &self.st.glow_params()).and_then(|_| image_io::save_exr(&exr, &aov.color)) {
                Ok(()) => {
                    info!("saved {} / {} ({} spp)", png.display(), exr.display(), g.prog.samples());
                    let shown = std::path::absolute(&png).unwrap_or(png.clone());
                    self.last_update = fl!("saved", path = shown.display().to_string(), spp = g.prog.samples().to_string());
                }
                Err(e) => {
                    error!("save failed: {}", e);
                    self.last_update = fl!("save-failed", error = e.to_string());
                }
            }
            // ノイズ除去した画像を表示しているなら、それも書く（表示と同じ強さで混ぜたもの）
            if let (true, Some(d)) = (show_denoised, g.denoiser.as_ref()) {
                let mut img = d.read_output();
                if g.prog.xyz_output() {
                    krbr_render::integrator::xyz_to_linear_srgb(&mut img);
                }
                let png = png.with_file_name(format!("{}_{:03}_denoised.png", self.stem, self.save_index - 1));
                let exr = png.with_extension("exr");
                match image_io::save_png_tonemapped(&png, &img, self.st.exposure, &self.st.glow_params()).and_then(|_| image_io::save_exr(&exr, &img)) {
                    Ok(()) => {
                        info!("saved {} / {} (denoised, strength {:.2})", png.display(), exr.display(), self.st.denoise_strength);
                        self.last_update.push_str(&fl!("saved-denoised-too"));
                    }
                    Err(e) => error!("save failed: {}", e),
                }
            }
        }

        if let Some((format, path)) = self.save_as_target.take() {
            // 表示している画像（ノイズ除去した画像を表示していればそれ）を書く
            let img = match (show_denoised, g.denoiser.as_ref()) {
                (true, Some(d)) => {
                    let mut img = d.read_output();
                    if g.prog.xyz_output() {
                        krbr_render::integrator::xyz_to_linear_srgb(&mut img);
                    }
                    img
                }
                _ => g.prog.readback().color,
            };
            let r = match format {
                SaveFormat::Png => image_io::save_png_tonemapped(&path, &img, self.st.exposure, &self.st.glow_params()),
                SaveFormat::Exr => image_io::save_exr(&path, &img),
            };
            match r {
                Ok(()) => {
                    info!("saved {} ({} spp{})", path.display(), g.prog.samples(), if show_denoised { ", denoised" } else { "" });
                    self.last_update = fl!("saved-as", path = path.display().to_string(), spp = g.prog.samples().to_string(), denoised = if show_denoised { fl!("saved-as-denoised") } else { String::new() });
                }
                Err(e) => {
                    error!("save failed: {} ({})", e, path.display());
                    self.last_update = fl!("save-failed", error = e.to_string());
                }
            }
        }

        if last_test_frame {
            info!("test: {} frames, {} spp, last frame {:.1} ms (GPU {:.1} ms, {} spp/frame), swapchain {}x{} format {:?}, environment {}", self.frames, g.prog.samples(), self.last_frame_ms, self.last_gpu_ms, self.samples_per_frame, extent.width, extent.height, g.swap.format, self.env_path);
            info!("test: render {}x{}, 3D view {}x{}, display {}x{} at ({}, {}), ratio {}", g.prog.targets().width, g.prog.targets().height, self.st.viewport.w, self.st.viewport.h, self.st.display.w, self.st.display.h, self.st.display.x, self.st.display.y, self.st.render_ratio);
            info!("test: rendering {:.2} s for {} spp, average {:.1} ms/spp (GPU {:.1} ms/spp)", self.accum_wall.as_secs_f64(), self.accum_measured, self.accum_wall.as_secs_f64() * 1000.0 / self.accum_measured.max(1) as f64, self.accum_gpu_ms / self.accum_measured.max(1) as f64);
            if let Some(p) = &self.vo.screenshot {
                let ok = if let Some(mut b) = shot.take() {
                    // 確保した大きさは要求より大きいことがあるので、画像の分だけを使う
                    let mut px = b.mapped_ref().unwrap()[..extent.width as usize * extent.height as usize * 4].to_vec();
                    if g.swap.format == vk::Format::B8G8R8A8_UNORM {
                        for c in px.chunks_exact_mut(4) {
                            c.swap(0, 2);
                        }
                    }
                    for c in px.chunks_exact_mut(4) {
                        c[3] = 255;
                    }
                    g.ctx.destroy_buffer(&mut b);
                    if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    image_io::save_png_rgba8(p, extent.width, extent.height, &px)
                } else {
                    Err("swapchain cannot be read back".into())
                };
                match ok {
                    Ok(()) => info!("wrote {}", p.display()),
                    Err(e) => {
                        error!("{}: {}", p.display(), e);
                        self.rc = 1;
                    }
                }
            }
            self.done = true;
        }
        if let Some(mut b) = shot {
            g.ctx.destroy_buffer(&mut b);
        }
        Ok(())
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(e) = self.init_gpu(el) {
            error!("viewer failed: {}", e);
            self.rc = 1;
            el.exit();
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = &self.window else { return };
        if self.done {
            el.exit();
            return;
        }
        let response = self.egui_state.as_mut().map(|s| s.on_window_event(window, &event)).unwrap_or_default();
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::DroppedFile(path) => {
                if is_env_file(&path) {
                    self.start_env_load(path.to_string_lossy().into_owned());
                } else {
                    self.last_update = fl!("env-not-an-environment", path = path.display().to_string());
                }
            }
            WindowEvent::ModifiersChanged(m) => self.st.ctrl = m.state().control_key(),
            WindowEvent::MouseInput { state, button, .. } => {
                let down = state == ElementState::Pressed;
                if down && (response.consumed || !self.st.viewport.contains(self.st.last_cursor)) {
                    return; // メニュー・パネルの上での操作はカメラに渡さない
                }
                if down && button == MouseButton::Left && self.st.ctrl {
                    self.st.focus_pick = Some(self.st.last_cursor);
                    return;
                }
                match button {
                    MouseButton::Left => {
                        self.st.rotating = down;
                        if down {
                            self.st.press_at = Some(self.st.last_cursor);
                            self.st.press_moved = 0.0;
                        } else if self.st.press_at.take().is_some() && self.st.press_moved < 4.0 {
                            self.st.part_pick = Some(self.st.last_cursor);
                        }
                    }
                    MouseButton::Right | MouseButton::Middle => self.st.panning = down,
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (dx, dy) = (position.x - self.st.last_cursor.0, position.y - self.st.last_cursor.1);
                self.st.last_cursor = (position.x, position.y);
                if self.st.press_at.is_some() {
                    self.st.press_moved += dx.abs() + dy.abs();
                }
                if self.st.rotating {
                    self.st.azimuth -= dx * 0.3;
                    self.st.elevation = (self.st.elevation + dy * 0.3).clamp(-89.0, 89.0);
                    self.st.dirty = true;
                } else if self.st.panning {
                    let c = orbit_camera(self.st.target, self.st.distance, self.st.elevation, self.st.azimuth, self.st.fov, 1.0);
                    let scale = 2.0 * self.st.distance * self.st.tan_half_fov_y / self.st.display.h.max(1) as f32;
                    self.st.target += (-c.right * dx as f32 + c.up * dy as f32) * scale;
                    self.st.dirty = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if response.consumed || !self.st.viewport.contains(self.st.last_cursor) {
                    return;
                }
                let yoff = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 40.0,
                };
                self.st.distance = (self.st.distance * 0.9f64.powf(yoff) as f32).max(1.0);
                self.st.dirty = true;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed || response.consumed {
                    return;
                }
                let PhysicalKey::Code(code) = event.physical_key else { return };
                let st = &mut self.st;
                match code {
                    KeyCode::Escape => el.exit(),
                    KeyCode::F1 | KeyCode::Tab => st.show_panel = !st.show_panel,
                    KeyCode::KeyR | KeyCode::KeyF => st.reset_requested = true,
                    KeyCode::KeyS => st.save_requested = true,
                    KeyCode::Equal | KeyCode::NumpadAdd => st.exposure += 0.25, // 露出は表示だけなので累積はやり直さない
                    KeyCode::Minus | KeyCode::NumpadSubtract => st.exposure -= 0.25,
                    KeyCode::BracketLeft => {
                        st.env_rotation_deg -= 15.0;
                        st.dirty = true;
                    }
                    KeyCode::BracketRight => {
                        st.env_rotation_deg += 15.0;
                        st.dirty = true;
                    }
                    KeyCode::Digit1 => st.mode = DisplayMode::Beauty,
                    KeyCode::Digit2 => st.mode = DisplayMode::Albedo,
                    KeyCode::Digit3 => st.mode = DisplayMode::Normal,
                    KeyCode::KeyD => st.denoise_requested = true,
                    KeyCode::KeyP => st.paused = !st.paused,
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => {} // 描画は about_to_wait で行う
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.window.is_none() || self.gpu.is_none() || self.done {
            return;
        }
        // 描画はここで行う（非表示のウィンドウには RedrawRequested が来ないため）
        if let Err(e) = self.frame() {
            error!("viewer failed: {}", e);
            self.rc = 1;
            el.exit();
            return;
        }
        if self.done {
            el.exit();
            return;
        }
        let idle = self.gpu.as_ref().map(|g| g.prog.samples() >= self.st.max_spp || (self.st.paused && g.prog.samples() > 0)).unwrap_or(false) && !self.st.dirty && self.env_job.is_none() && self.reload_job.is_none() && self.silk_job.is_none() && !self.silk_dirty && self.vo.test_frames == 0;
        if idle {
            // 収束したら GPU を休ませ、入力と保存を待つ
            let wait = if self.watcher.is_some() || self.prep.scene.pending_models > 0 { 100 } else { 250 };
            el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(wait)));
        } else {
            el.set_control_flow(ControlFlow::Poll);
        }
    }
}

pub fn run_viewer(vo: ViewOptions) -> anyhow::Result<i32> {
    let Some(prep) = prepare_scene(&vo.common) else { anyhow::bail!("scene preparation failed") };
    let cfg = &prep.cfg;
    let rv = &prep.remembered_view;
    let win = remember::window_state();
    // 描画のサイズと比率：コマンドライン、覚えたもの、ウィンドウに合わせる・×1
    let size_mode = if vo.render_size.is_empty() { remember::render_size_of(rv) } else { parse_size(&vo.render_size) };
    let render_ratio = vo.render_ratio.or_else(|| rv.get("render_ratio").and_then(|v| v.as_float()).map(|v| v as f32)).map(|r| r.clamp(0.1, 4.0)).unwrap_or(1.0);
    let max_spp = vo.common.spp.map(pow2_spp).or_else(|| rv.get("max_spp").and_then(|v| v.as_integer()).map(|v| pow2_spp(v as u32))).unwrap_or(4096);
    let mask_choice = if cfg.scene.mask_preset.is_empty() { None } else { mask_presets().iter().position(|p| p.name == cfg.scene.mask_preset) };
    let st = ViewState {
        target: Vec3::ZERO,
        distance: 100.0,
        elevation: cfg.elevation_deg,
        azimuth: cfg.azimuth_deg,
        fov: cfg.fov_deg,
        exposure: cfg.exposure_ev,
        glow: cfg.glow,
        glow_threshold: cfg.glow_threshold,
        glow_spread: cfg.glow_spread,
        env_rotation_deg: cfg.env_rotation_deg,
        env_intensity: cfg.env_intensity,
        bump_intensity: cfg.scene.bump_intensity,
        max_spp,
        mode: rv.get("display_mode").and_then(|v| v.as_str()).map(display_mode_of).unwrap_or(DisplayMode::Beauty),
        show_panel: win.panel_shown,
        dirty: true,
        materials_dirty: false,
        mask_choice,
        mask_changed: false,
        save_requested: false,
        save_as_requested: None,
        settings_location_requested: None,
        reveal_settings_requested: false,
        reset_requested: false,
        auto_exposure_requested: false,
        reset_all_requested: false,
        start_requested: false,
        quit_requested: false,
        paused: vo.paused,
        spectral: cfg.spectral,
        integrator_changed: false,
        rotating: false,
        panning: false,
        focus_pick: None,
        press_at: None,
        press_moved: 0.0,
        part_pick: None,
        denoise: vo.common.denoise.or_else(|| rv.get("denoise").and_then(|v| v.as_bool())).unwrap_or(false),
        denoise_strength: vo.common.denoise_strength.or_else(|| rv.get("denoise_strength").and_then(|v| v.as_float()).map(|v| v as f32)).unwrap_or(1.0),
        denoise_requested: false,
        last_cursor: (0.0, 0.0),
        ctrl: false,
        viewport: Viewport { x: 0, y: 0, w: 1, h: 1 },
        display: Viewport { x: 0, y: 0, w: 1, h: 1 },
        size_mode,
        render_ratio,
        fit_window_requested: size_mode.is_some(),
        language_requested: None,
        tan_half_fov_y: 0.27,
    };
    let language_choice = remember::ui_language();
    let language = i18n::select(vo.language.as_deref(), language_choice.as_deref());
    let out_png = vo.common.out.clone();
    let out_dir = out_png.parent().filter(|p| !p.as_os_str().is_empty()).map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let stem = out_png.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "view".into());
    let env_path = cfg.hdr.clone();
    let env_list = list_environments(&env_path);
    let watcher = vo.watch.then(|| FileWatcher::with_defaults(&prep.board.file));
    if watcher.is_some() {
        info!("watching {} for saves", prep.board.file.display());
    }
    let models_seen = prep.source.as_ref().map(|s| s.library.completed()).unwrap_or(0);
    let material_names = prep.source.as_ref().map(|s| s.materials.library.names()).unwrap_or_default();
    let mut app = App {
        base_title: format!("krbr - {}", prep.board.project_name),
        scene_as_configured: prep.configured.scene.clone(),
        vo,
        prep,
        st,
        window: None,
        egui_ctx: egui::Context::default(),
        egui_state: None,
        gpu: None,
        env_path,
        env_list,
        open_dialog: None,
        save_dialog: None,
        save_as_target: None,
        env_job: None,
        env_error: String::new(),
        watcher,
        reload_job: None,
        models_seen,
        test_reload_started: false,
        test_reload_applied: false,
        silk_job: None,
        silk_rerun: false,
        silk_dirty: false,
        selected: None,
        outline: None,
        materials_dirty_files: false,
        assign_scope: 0,
        material_names,
        material_error: String::new(),
        emit_edit: None,
        test_pick_done: false,
        denoiser_tried: false,
        denoise_error: String::new(),
        accum_epoch: 0,
        prev_samples: 0,
        denoised_epoch: u64::MAX,
        denoised_samples: 0,
        last_denoise_ms: 0.0,
        last_denoise_at: Instant::now(),
        last_update: String::new(),
        samples_per_frame: 1,
        need_recreate: false,
        frames: 0,
        save_index: 0,
        accum_wall: Duration::ZERO,
        accum_gpu_ms: 0.0,
        accum_measured: 0,
        last_change_at: Instant::now(),
        last_frame_ms: 0.0,
        last_gpu_ms: 0.0,
        view_seen: remember::ViewValues::new(),
        view_saved: remember::ViewValues::new(),
        view_changed_at: Instant::now(),
        view_rebase: false,
        win_start: win.clone(),
        win_seen: win.clone(),
        win_saved: win.clone(),
        win_changed_at: Instant::now(),
        panel_width: win.panel_width.unwrap_or(340.0),
        fit_frames: 0,
        env_no_remember: false,
        language,
        language_choice,
        out_dir,
        stem,
        rc: 0,
        done: false,
    };
    app.view_seen = app.current_view();
    app.view_saved = app.view_seen.clone();
    let event_loop = EventLoop::new().map_err(|e| anyhow::anyhow!("event loop: {e}"))?;
    event_loop.run_app(&mut app).map_err(|e| anyhow::anyhow!("event loop: {e}"))?;
    // GPU の資源はコンテキストより先に捨てる
    if let Some(g) = app.gpu.take() {
        let ctx = g.ctx;
        ctx.wait_idle();
        unsafe {
            ctx.device.destroy_query_pool(g.timestamps, None);
            ctx.device.destroy_fence(g.fence, None);
            ctx.device.destroy_semaphore(g.acquired, None);
        }
        drop(g.egui_renderer);
        drop(g.tone_denoised);
        drop(g.denoiser);
        drop(g.tone);
        drop(g.prog);
        drop(g.swap);
        drop(g.gpu);
        // 安全性：Box::leak で作ったコンテキストを、参照していた資源をすべて捨てた後に解放する
        unsafe { drop(Box::from_raw(ctx as *const Context as *mut Context)) };
    }
    let _ = app.last_change_at;
    app.save_view(true);
    Ok(app.rc)
}
