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

use crate::kicad_cli::{plot_stroke_silk, SilkPlot};
use crate::material_cmd::{clear_material, set_material, MaterialSetOptions};
use crate::selection::{Outline, Selected};
use krbr_scene::material_library::{split_variant, with_smooth};
use crate::prepare::{ReloadStats, SceneSource};
use crate::remember;
use crate::prepare::{frame_params, initial_camera, prepare_scene, PreparedScene, RenderOptions};
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
    /// レンダリングを一時停止した状態で開く（GPU の弱い PC 向け）
    pub paused: bool,
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
    reset_requested: bool,
    auto_exposure_requested: bool,
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
    /// 3D 表示の領域（ウィンドウのうちメニューとパネルを除いた所、物理ピクセル）。描画の解像度もこれ
    viewport: Viewport,
    tan_half_fov_y: f32,
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

/// 日本語を表示できる Windows のフォント（無ければ egui の既定のフォント）
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for f in ["C:/Windows/Fonts/YuGothM.ttc", "C:/Windows/Fonts/meiryo.ttc", "C:/Windows/Fonts/msgothic.ttc"] {
        if let Ok(bytes) = std::fs::read(f) {
            fonts.font_data.insert("jp".into(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
            fonts.families.entry(egui::FontFamily::Proportional).or_default().insert(0, "jp".into());
            fonts.families.entry(egui::FontFamily::Monospace).or_default().push("jp".into());
            break;
        }
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
    last_title: Instant,
    last_change_at: Instant,
    last_frame_ms: f64,
    last_gpu_ms: f64,
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

impl App {
    fn reset_camera(&mut self, w: u32, h: u32) {
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
        let (w, h) = (self.vo.common.width.unwrap_or(1600), self.vo.common.height.unwrap_or(900));
        // ウィンドウのアイコン（exe に埋め込んだものと同じ絵。scripts/make_icon.py で作る 64x64 の RGBA）
        let icon = winit::window::Icon::from_rgba(include_bytes!("../../../krbr/assets/krbr_64.rgba").to_vec(), 64, 64).ok();
        let attrs = Window::default_attributes().with_window_icon(icon).with_title(self.base_title.clone()).with_inner_size(winit::dpi::PhysicalSize::new(w, h)).with_visible(!hidden).with_active(!hidden);
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
        self.reset_camera(fbw, fbh);
        // 3D 表示の大きさはメニューとパネルを置いてから決まるので、最初のフレームで視点を合わせ直す
        self.st.reset_requested = true;
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
        install_fonts(&self.egui_ctx);
        self.egui_state = Some(EguiState::new(self.egui_ctx.clone(), egui::ViewportId::ROOT, &window, Some(window.scale_factor() as f32), None, None));
        self.gpu = Some(Gpu { ctx, gpu, swap, prog, tone, denoiser: None, tone_denoised: None, egui_renderer, cmd, acquired, fence, timestamps, ts_period_ms });
        self.window = Some(window);
        Ok(())
    }

    fn panel(&mut self, ui: &mut egui::Ui) {
        let (samples, max_spp) = (self.gpu.as_ref().unwrap().prog.samples(), self.st.max_spp);
        ui.label(format!("{} / {} spp   {:.1} ms/frame (GPU {:.1} ms)", samples, max_spp, self.last_frame_ms, self.last_gpu_ms));
        ui.add(egui::ProgressBar::new((samples as f32 / max_spp.max(1) as f32).min(1.0)));
        if self.reload_job.is_some() {
            ui.colored_label(egui::Color32::from_rgb(255, 204, 77), "基板を読み直し中...");
        }
        if self.silk_job.is_some() {
            ui.colored_label(egui::Color32::from_rgb(255, 204, 77), "線フォントの文字を書き出し中（kicad-cli）...");
        }
        if self.prep.scene.pending_models > 0 {
            ui.colored_label(egui::Color32::from_rgb(255, 204, 77), format!("3D モデルを分割中: {}（それまでは箱で表示）", self.prep.scene.pending_models));
        }
        if !self.last_update.is_empty() {
            ui.weak(&self.last_update);
        }

        ui.separator();
        self.selection_panel(ui);

        let st = &mut self.st;
        ui.separator();
        ui.strong("環境マップ");
        let mut chosen: Option<String> = None;
        let mut open_dialog = false;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("hdr").width(220.0).selected_text(environment_label(&self.env_path)).show_ui(ui, |ui| {
                for (gi, group) in self.env_list.iter().enumerate() {
                    if gi > 0 {
                        ui.separator();
                    }
                    for p in group {
                        let r = ui.selectable_label(same_env(p, &self.env_path), environment_label(p));
                        let r = if is_builtin(p) { r.on_hover_text("灰色の撮影ブースと大きなソフトボックス") } else { r.on_hover_text(p) };
                        if r.clicked() {
                            chosen = Some(p.clone());
                        }
                    }
                }
            });
            if ui.add_enabled(self.open_dialog.is_none(), egui::Button::new("開く...")).on_hover_text(".hdr / .exr を選ぶ（ウィンドウへのドラッグ＆ドロップでも可）").clicked() {
                open_dialog = true;
            }
        });
        ui.weak("選んだものは、この基板の次回の表示にも使う");
        if let Some((p, _)) = &self.env_job {
            ui.colored_label(egui::Color32::from_rgb(255, 204, 77), format!("読み込み中: {p}"));
        }
        if !self.env_error.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(255, 102, 102), &self.env_error);
        }
        if ui.add(egui::Slider::new(&mut st.env_intensity, 0.05..=8.0).logarithmic(true).text("強さ")).changed() {
            st.dirty = true;
        }
        if ui.add(egui::Slider::new(&mut st.env_rotation_deg, -180.0..=180.0).suffix("°").text("回転")).changed() {
            st.dirty = true;
        }

        ui.separator();
        ui.strong("露出");
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut st.exposure, -6.0..=6.0).text("EV"));
            if ui.button("自動").clicked() {
                st.auto_exposure_requested = true;
            }
            if ui.button("0").clicked() {
                st.exposure = 0.0;
            }
        });
        {
            let on_cpu = self.gpu.as_ref().and_then(|g| g.denoiser.as_ref()).is_some_and(|d| d.device_name() == "CPU");
            ui.horizontal(|ui| {
                ui.checkbox(&mut st.denoise, "ノイズ除去 (OIDN)");
                if let Some(d) = self.gpu.as_ref().and_then(|g| g.denoiser.as_ref()) {
                    ui.weak(format!("{}, {:.0} ms", d.device_name(), self.last_denoise_ms));
                    if on_cpu && ui.button("今かける (D)").clicked() {
                        st.denoise_requested = true;
                    }
                } else if self.denoiser_tried && st.denoise {
                    ui.weak("使用できません");
                }
            });
            if st.denoise {
                ui.add(egui::Slider::new(&mut st.denoise_strength, 0.0..=1.0).text("強さ")).on_hover_text("元の画像と混ぜる割合（1 で OIDN の結果そのまま。下げると細部やざらつきが少し残る）");
                if on_cpu {
                    ui.weak("CPU のため、収束時と D キーのときだけかけます");
                }
                if !self.denoise_error.is_empty() {
                    ui.colored_label(egui::Color32::from_rgb(255, 102, 102), &self.denoise_error);
                }
            }
        }

        ui.separator();
        ui.strong("積分器");
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
            0 => "単方向（速い）",
            1 => "双方向（光源側の経路とつなぐ）",
            _ => "双方向＋近傍のまとめ（透明な物体越しの集光模様も収束）",
        });

        ui.separator();
        ui.strong("レンズ");
        {
            let l = &mut self.prep.cfg.lens;
            let mut ch = false;
            ui.horizontal(|ui| {
                ch |= ui.radio_value(&mut l.model, LensModel::Pinhole, "ピンホール").changed();
                ch |= ui.radio_value(&mut l.model, LensModel::Thin, "薄レンズ").changed();
                ch |= ui.radio_value(&mut l.model, LensModel::Realistic, "実レンズ").changed();
            });
            if l.model != LensModel::Pinhole {
                ch |= ui.add(egui::Slider::new(&mut l.f_number, 0.7..=22.0).logarithmic(true).text("F 値")).changed();
                let mut focus = if l.focus_distance_mm > 0.0 { l.focus_distance_mm } else { st.distance };
                if ui.add(egui::Slider::new(&mut focus, 5.0..=5000.0).logarithmic(true).text("ピント (mm)")).changed() {
                    l.focus_distance_mm = focus;
                    ch = true;
                }
                ui.weak("Ctrl + クリック：その点にピントを合わせる");
            }
            if l.model == LensModel::Thin {
                ch |= ui.add(egui::Slider::new(&mut l.focal_length_mm, 10.0..=300.0).logarithmic(true).text("焦点距離 (mm)")).changed();
                ch |= ui.add(egui::Slider::new(&mut l.blades, 0..=12).text("羽根の枚数")).changed();
                if l.blades >= 3 {
                    ch |= ui.add(egui::Slider::new(&mut l.blade_rotation_deg, 0.0..=360.0).suffix("°").text("羽根の回転")).changed();
                    ch |= ui.add(egui::Slider::new(&mut l.blade_roundness, 0.0..=1.0).text("羽根の丸み")).changed();
                }
                ch |= ui.checkbox(&mut l.natural_vignetting, "周辺減光 (cos⁴)").changed();
                ch |= ui.add(egui::Slider::new(&mut l.optical_vignetting, 0.0..=1.0).text("口径食")).changed();
                ch |= ui.add(egui::Slider::new(&mut l.abbe_number, 0.0..=80.0).text("軸上色収差 (アッベ数)")).changed();
                ch |= ui.add(egui::Slider::new(&mut l.lateral_ca, 0.0..=0.02).text("倍率色収差")).changed();
                if (l.abbe_number > 0.0 || l.lateral_ca > 0.0) && !st.spectral {
                    ui.weak("色収差は分光モードで有効");
                }
            }
            if ch {
                st.dirty = true;
            }
        }

        ui.separator();
        ui.strong("色の計算");
        let mut spectral = st.spectral;
        ui.horizontal(|ui| {
            ui.radio_value(&mut spectral, false, "RGB");
            ui.radio_value(&mut spectral, true, "分光（4 波長）");
        });
        if spectral != st.spectral {
            st.spectral = spectral;
            st.dirty = true;
        }

        ui.separator();
        ui.strong("表面");
        if ui.add(egui::Slider::new(&mut st.bump_intensity, 0.0..=2.0).text("凹凸の強さ")).changed() {
            st.materials_dirty = true;
        }
        {
            let presets = mask_presets();
            let current = st.mask_choice.map(|i| presets[i].name).unwrap_or("設定どおり");
            egui::ComboBox::from_id_salt("mask").width(200.0).selected_text(current).show_ui(ui, |ui| {
                if ui.selectable_label(st.mask_choice.is_none(), "設定どおり").clicked() && st.mask_choice.is_some() {
                    st.mask_choice = None;
                    st.mask_changed = true;
                }
                for (i, p) in presets.iter().enumerate() {
                    let sel = st.mask_choice == Some(i);
                    if ui.selectable_label(sel, p.name).on_hover_text(p.note).clicked() && !sel {
                        st.mask_choice = Some(i);
                        st.mask_changed = true;
                    }
                }
            });
            ui.weak("レジストの色（プリセット）");
        }

        ui.separator();
        ui.strong("表示");
        ui.horizontal(|ui| {
            ui.radio_value(&mut st.mode, DisplayMode::Beauty, "画像");
            ui.radio_value(&mut st.mode, DisplayMode::Albedo, "アルベド");
            ui.radio_value(&mut st.mode, DisplayMode::Normal, "法線");
        });
        {
            let mut e = 4u32;
            while (1u32 << e) < st.max_spp && e < 16 {
                e += 1;
            }
            if ui.add(egui::Slider::new(&mut e, 4..=16).custom_formatter(|v, _| format!("2^{} = {}", v as u32, 1u32 << (v as u32))).text("最大 spp")).changed() {
                st.max_spp = 1 << e;
            }
        }

        ui.separator();
        ui.strong("カメラ");
        let mut fov = st.fov as f32;
        if ui.add(egui::Slider::new(&mut fov, 5.0..=90.0).suffix("°").text("画角")).changed() {
            st.fov = fov as f64;
            st.dirty = true;
        }
        ui.label(format!("仰角 {:.0}°  方位 {:.0}°", st.elevation, st.azimuth.rem_euclid(360.0)));
        ui.horizontal(|ui| {
            if ui.button("視点を戻す (R)").clicked() {
                st.reset_requested = true;
            }
            if ui.button("保存 (S)").clicked() {
                st.save_requested = true;
            }
        });
        ui.separator();
        ui.weak("左ドラッグ: 回転  右ドラッグ: 移動  ホイール: 拡大  クリック: 部品を選ぶ");
        ui.weak("F1 / Tab: このパネルの表示切り替え");
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
                source.materials = load_material_setup(&old.file); // 材質の割り当て（overrides.toml / model_map.toml）を書き換えた
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
        let vp = self.st.viewport;
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
        let vp = self.st.viewport;
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
        let r = if clear { clear_material(&o).map(|f| f.map(|f| (f, "上書きを消した".to_string()))) } else { set_material(&o).map(|f| Some((f, format!("{} にした", material)))) };
        match r {
            Ok(Some((file, what))) => {
                let target = if part.is_empty() { format!("{} の全部位", sel.reference) } else { format!("{} の {}", sel.reference, part) };
                self.last_update = format!("{target}を{what}（{}）", file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
                info!("material: {} {} -> {} ({})", sel.reference, if part.is_empty() { "*" } else { part }, if clear { "(cleared)" } else { material }, file.display());
                self.material_error.clear();
                self.materials_dirty_files = true;
            }
            Ok(None) => self.material_error = "消す上書きが無い（規則どおりになっている）".into(),
            Err(e) => {
                warn!("material: {e}");
                self.material_error = e;
            }
        }
    }

    /// 選んだ部品の欄：部位ごとの材質と、決まった段階。材質を選ぶと設定ファイルに書いて描き直す
    fn selection_panel(&mut self, ui: &mut egui::Ui) {
        ui.strong("選んだ部品");
        let Some(sel) = self.selected.clone() else {
            ui.weak("部品をクリックすると、材質を変えられる");
            return;
        };
        let fp = self.prep.board.footprints.iter().find(|f| f.reference == sel.reference);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&sel.reference).strong().color(egui::Color32::from_rgb(255, 170, 40)));
            if ui.small_button("選択を外す").clicked() {
                self.selected = None;
                self.outline = None;
            }
        });
        // 長い名前はパネルの幅で切り詰める（カーソルを乗せると全体が出る）
        let lib_id = fp.map(|f| f.lib_id.clone()).unwrap_or_default();
        ui.add(egui::Label::new(egui::RichText::new(&lib_id).weak()).truncate()).on_hover_text(&lib_id);
        if self.selected.is_none() {
            return;
        }
        if self.prep.cfg.lens.model == LensModel::Realistic {
            ui.weak("実レンズでは輪郭線を表示しない");
        }
        let Some(mi) = sel.model else {
            // 代わりの箱：3D モデルが無い理由
            let missing: Vec<_> = self.prep.scene.missing.iter().filter(|m| m.reference == sel.reference).collect();
            if missing.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(255, 204, 77), "3D モデルを分割中（終わるまで箱で表示）");
            } else {
                ui.colored_label(egui::Color32::from_rgb(255, 204, 77), "3D モデルが無いので、代わりの箱で表示している（材質は変えられない）");
                for m in missing {
                    ui.weak(format!("{}\n→ {}（{}）", m.spec, m.resolved, m.reason));
                }
                ui.weak("見つける場所は board.toml の [paths] model_search_dirs で足せる");
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
            ui.radio_value(&mut self.assign_scope, 0, "この部品").on_hover_text("この基板のこの部品だけ（<名前>.krbr\\overrides.toml の部品番号）");
            ui.radio_value(&mut self.assign_scope, 1, "このモデル").on_hover_text("この基板で同じ 3D モデルを使う部品すべて（overrides.toml のモデル）");
            ui.radio_value(&mut self.assign_scope, 2, "全基板").on_hover_text("同じ 3D モデルを使うすべての基板（%APPDATA%\\krbr\\model_map.toml）");
        });
        // 書き込む先より優先される上書きがあると、書いても見た目は変わらない
        let shadowed_by_ref = self.assign_scope >= 1 && rows.iter().any(|r| r.source.starts_with("ref "));
        let shadowed_by_project = self.assign_scope == 2 && rows.iter().any(|r| r.source == "model (project)");
        if shadowed_by_ref {
            ui.colored_label(egui::Color32::from_rgb(255, 204, 77), "この部品には部品番号の上書きがあり、そちらが優先される（「この部品」で上書きを消すと反映される）");
        } else if shadowed_by_project {
            ui.colored_label(egui::Color32::from_rgb(255, 204, 77), "このプロジェクトにモデルの設定があり、そちらが優先される（「このモデル」で上書きを消すと反映される）");
        }
        // 書き込むもの：(部位（空なら全部位）, 材質, 上書きを消すか)
        let mut actions: Vec<(String, String, bool)> = Vec::new();
        let names = self.material_names.clone();
        let combo = |ui: &mut egui::Ui, id: String, current: &str, label: &str| -> Option<String> {
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
        let pattern_tip = "表面の模様（細かな凹凸と粗さのむら）。外すと、なめらかな面になる（材質名に +smooth が付く）";
        egui::Grid::new("parts").num_columns(3).spacing([8.0, 4.0]).show(ui, |ui| {
            for (i, r) in rows.iter().enumerate() {
                let key = r.part_keys.split(' ').next().unwrap_or("").to_string();
                let (base, smooth) = split_variant(&r.material);
                // 表示では "kicad:" を省く（カーソルを乗せると全体が出る）
                let shown = if key.is_empty() { "（全体）".to_string() } else { key.strip_prefix("kicad:").unwrap_or(&key).to_string() };
                ui.label(shown).on_hover_text(format!("{}\n決まった段階: {}", r.part_keys, r.source));
                // 材質を選び直しても、模様のあり・なしは保つ
                if let Some(m) = combo(ui, format!("part{i}"), base, base) {
                    actions.push((key.clone(), with_smooth(&m, smooth), false));
                }
                let mut pattern = !smooth;
                if ui.checkbox(&mut pattern, "模様").on_hover_text(pattern_tip).changed() {
                    actions.push((key.clone(), with_smooth(&r.material, !pattern), false));
                }
                ui.end_row();
            }
            let all_smooth = !rows.is_empty() && rows.iter().all(|r| split_variant(&r.material).1);
            ui.label("全部位");
            if let Some(m) = combo(ui, "all".into(), "", "まとめて選ぶ") {
                actions.push((String::new(), with_smooth(&m, all_smooth), false));
            }
            let mut pattern = !all_smooth;
            if ui.checkbox(&mut pattern, "模様").on_hover_text("全部位の表面の模様をまとめて切り替える").changed() {
                for r in &rows {
                    let key = r.part_keys.split(' ').next().unwrap_or("").to_string();
                    actions.push((key, with_smooth(&r.material, !pattern), false));
                }
            }
            ui.end_row();
        });
        // 決まった段階（部位ごとに違えば、部位の名前にカーソルを乗せると出る）
        let mut sources: Vec<&str> = rows.iter().map(|r| r.source.as_str()).collect();
        sources.dedup();
        if let [one] = sources.as_slice() {
            ui.weak(format!("決まった段階: {one}"));
        } else if !sources.is_empty() {
            ui.weak("決まった段階は部位ごとに違う（部位の名前にカーソルを乗せると出る）");
        }
        if ui.button("上書きを消す（規則どおりに戻す）").on_hover_text("選んだ書き込み先の、この部品（モデル）の設定を消す").clicked() {
            actions.push((String::new(), String::new(), true));
        }
        if !self.material_error.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(255, 102, 102), &self.material_error);
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
                self.last_update = "読み直しに失敗したので、保存の監視を止めた".into();
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
                self.last_update = format!("読み直せなかった（次の保存を待つ）: {e}");
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
                    let since = d.saved_at.and_then(|t| t.elapsed().ok()).map(|e| format!("、保存から {:.2} 秒", e.as_secs_f64())).unwrap_or_default();
                    let df = &stats.diff;
                    let full = if df.full { "（全体）" } else { "" };
                    self.last_update = format!("読み直し：変更 {}・追加 {}・削除 {}{}、タイル {} 枚（解析 {:.0} ms、組み立て {:.0} ms、GPU {:.0} ms{}）", df.changed, df.added, df.removed, full, stats.tiles_rebuilt, stats.parse_ms, stats.build_ms, gpu_ms, since);
                    let meshing = if pending > 0 { format!(", {pending} model(s) still meshing") } else { String::new() };
                    info!("reload: {} changed, {} added, {} removed{}, {} tile(s) rebuilt; parse {:.0} ms, build {:.0} ms, GPU {:.0} ms{}", df.changed, df.added, df.removed, if df.full { " (full)" } else { "" }, stats.tiles_rebuilt, stats.parse_ms, stats.build_ms, gpu_ms, meshing);
                } else if d.materials {
                    info!("material assignment updated; scene rebuilt");
                } else if d.silk {
                    self.last_update = "線フォントの文字（kicad-cli）を反映した".into();
                    info!("stroke-font silkscreen updated; scene rebuilt");
                } else {
                    self.last_update = if pending > 0 { format!("3D モデルを反映した（残り {pending}）") } else { "3D モデルをすべて反映した".into() };
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
        let mut dlg = rfd::FileDialog::new().set_title("環境マップを開く").add_filter("環境マップ (*.hdr, *.exr)", &["hdr", "exr"]);
        if let Some(dir) = Path::new(&self.env_path).parent().filter(|d| !is_builtin(&self.env_path) && d.is_dir()) {
            dlg = dlg.set_directory(dir);
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(dlg.pick_file());
        });
        self.open_dialog = Some(rx);
    }

    /// ウィンドウ上端のメニュー
    /// メニューの下のボタン列（レンダリングの一時停止）
    fn tool_bar(&mut self, root: &mut egui::Ui) {
        let st = &mut self.st;
        let samples = self.gpu.as_ref().map(|g| g.prog.samples()).unwrap_or(0);
        egui::Panel::top("toolbar").show(root, |ui| {
            ui.horizontal(|ui| {
                let label = if st.paused { "▶ 再開" } else { "■ 一時停止" };
                let tip = "レンダリングを一時停止する（P）。サンプルを積み足さないので GPU の負荷がほぼ無くなり、操作が軽くなる。\nカメラを動かしたときは 1 サンプルだけ描いて止まる";
                if ui.add(egui::Button::new(label).selected(st.paused)).on_hover_text(tip).clicked() {
                    st.paused = !st.paused;
                }
                if st.paused {
                    ui.colored_label(egui::Color32::from_rgb(255, 204, 77), format!("一時停止中（{} spp）", samples));
                }
            });
        });
    }

    fn menu_bar(&mut self, root: &mut egui::Ui) {
        let st = &mut self.st;
        egui::Panel::top("menu").show(root, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("ファイル", |ui| {
                    if ui.add(egui::Button::new("画像を保存").shortcut_text("S")).clicked() {
                        st.save_requested = true;
                        ui.close();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("終了").shortcut_text("Esc")).clicked() {
                        st.quit_requested = true;
                        ui.close();
                    }
                });
                ui.menu_button("表示", |ui| {
                    if ui.add(egui::Button::new("設定パネル").selected(st.show_panel).shortcut_text("F1 / Tab")).clicked() {
                        st.show_panel = !st.show_panel;
                        ui.close();
                    }
                    if ui.add(egui::Button::new("レンダリングを一時停止").selected(st.paused).shortcut_text("P")).clicked() {
                        st.paused = !st.paused;
                        ui.close();
                    }
                    ui.separator();
                    for (mode, name, key) in [(DisplayMode::Beauty, "画像", "1"), (DisplayMode::Albedo, "アルベド", "2"), (DisplayMode::Normal, "法線", "3")] {
                        if ui.add(egui::Button::new(name).selected(st.mode == mode).shortcut_text(key)).clicked() {
                            st.mode = mode;
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.add(egui::Button::new("視点を戻す").shortcut_text("R")).clicked() {
                        st.reset_requested = true;
                        ui.close();
                    }
                    if ui.button("露出を自動で合わせる").clicked() {
                        st.auto_exposure_requested = true;
                        ui.close();
                    }
                });
                ui.menu_button("ヘルプ", |ui| {
                    ui.label("左ドラッグ: 回転");
                    ui.label("クリック: 部品を選ぶ（設定パネルで材質を変えられる）");
                    ui.label("右 / 中ドラッグ: 移動");
                    ui.label("ホイール: 拡大・縮小");
                    ui.label("Ctrl + クリック: その点にピントを合わせる");
                    ui.separator();
                    ui.label("F1 / Tab: 設定パネル    R: 視点を戻す    S: 保存    P: 一時停止");
                    ui.label("+ / -: 露出    [ / ]: 環境マップの回転    1 / 2 / 3: 画像 / アルベド / 法線");
                    ui.label("Esc: 終了");
                });
            });
        });
    }

    /// 右端に上下いっぱいで固定する設定パネル（幅は境目のドラッグで変えられる、× で閉じる）
    fn side_panel(&mut self, root: &mut egui::Ui) {
        let mut close = false;
        egui::Panel::right("settings").resizable(true).default_size(340.0).size_range(300.0..=640.0).show(root, |ui| {
            ui.horizontal(|ui| {
                ui.strong("設定");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if close_button(ui).on_hover_text("閉じる（メニューの「表示」や F1 / Tab で再表示）").clicked() {
                        close = true;
                    }
                });
            });
            ui.separator();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.panel(ui));
        });
        if close {
            self.st.show_panel = false;
        }
    }

    fn frame(&mut self) -> Result<(), String> {
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
        self.apply_reload()?;
        self.poll_silk();
        self.start_reload_if_needed();
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
                if d.error.is_empty() {
                    let g = self.gpu.as_mut().unwrap();
                    g.ctx.wait_idle();
                    g.gpu.set_environment(&d.image, &d.dist);
                    g.prog.rebuild_integrator()?;
                    self.env_path = remember::normalize_hdr(&d.path);
                    remember::remember_hdr(&self.prep.board.file, &self.env_path, &self.prep.config_hdr);
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
            // 描画の解像度は 3D 表示の領域に合わせる（スワップチェーンがウィンドウと違う大きさのこともあるので、その中に収める）
            let vp = viewport_px(central, full.pixels_per_point, g.swap.extent.width, g.swap.extent.height);
            let t = g.prog.targets();
            if vp.w != t.width || vp.h != t.height {
                g.prog.resize(vp.w, vp.h)?;
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
            let vp = self.st.viewport;
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
                self.last_update = format!("ピントを {:.1} mm に合わせた", self.prep.cfg.lens.focus_distance_mm);
                self.st.dirty = true;
            }
        }
        if self.st.reset_requested {
            self.st.reset_requested = false;
            self.reset_camera(self.st.viewport.w, self.st.viewport.h);
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
            t.record(cmd, self.st.exposure, self.st.mode, g.prog.xyz_output());
            t.output()
        } else {
            g.tone.record(cmd, self.st.exposure, self.st.mode, g.prog.xyz_output());
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
        let vp = self.st.viewport;
        let sub = vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 };
        let blit = vk::ImageBlit {
            src_subresource: sub,
            src_offsets: [vk::Offset3D::default(), vk::Offset3D { x: disp.width as i32, y: disp.height as i32, z: 1 }],
            dst_subresource: sub,
            dst_offsets: [vk::Offset3D { x: vp.x as i32, y: vp.y as i32, z: 0 }, vk::Offset3D { x: (vp.x + vp.w) as i32, y: (vp.y + vp.h) as i32, z: 1 }],
        };
        unsafe { dev.cmd_blit_image(cmd, disp.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, swap_img, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[blit], vk::Filter::NEAREST) };
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
        }
        if self.last_title.elapsed().as_secs_f64() > 0.3 {
            let paused = if self.st.paused { " (一時停止)" } else { "" };
            self.window.as_ref().unwrap().set_title(&format!("{} - {} / {} spp{} - {:.1} ms/frame (GPU {:.1} ms) - EV {:+.2}", self.base_title, g.prog.samples(), max_spp, paused, self.last_frame_ms, self.last_gpu_ms, self.st.exposure));
            self.last_title = Instant::now();
        }

        if self.st.save_requested {
            self.st.save_requested = false;
            let aov = g.prog.readback();
            let png = self.out_dir.join(format!("{}_{:03}.png", self.stem, self.save_index));
            let exr = self.out_dir.join(format!("{}_{:03}.exr", self.stem, self.save_index));
            self.save_index += 1;
            let _ = std::fs::create_dir_all(&self.out_dir);
            match image_io::save_png_tonemapped(&png, &aov.color, self.st.exposure).and_then(|_| image_io::save_exr(&exr, &aov.color)) {
                Ok(()) => info!("saved {} / {} ({} spp)", png.display(), exr.display(), g.prog.samples()),
                Err(e) => error!("save failed: {}", e),
            }
            // ノイズ除去した画像を表示しているなら、それも書く（表示と同じ強さで混ぜたもの）
            if let (true, Some(d)) = (show_denoised, g.denoiser.as_ref()) {
                let mut img = d.read_output();
                if g.prog.xyz_output() {
                    krbr_render::integrator::xyz_to_linear_srgb(&mut img);
                }
                let png = png.with_file_name(format!("{}_{:03}_denoised.png", self.stem, self.save_index - 1));
                let exr = png.with_extension("exr");
                match image_io::save_png_tonemapped(&png, &img, self.st.exposure).and_then(|_| image_io::save_exr(&exr, &img)) {
                    Ok(()) => info!("saved {} / {} (denoised, strength {:.2})", png.display(), exr.display(), self.st.denoise_strength),
                    Err(e) => error!("save failed: {}", e),
                }
            }
        }

        if last_test_frame {
            info!("test: {} frames, {} spp, last frame {:.1} ms (GPU {:.1} ms, {} spp/frame), swapchain {}x{} format {:?}, environment {}", self.frames, g.prog.samples(), self.last_frame_ms, self.last_gpu_ms, self.samples_per_frame, extent.width, extent.height, g.swap.format, self.env_path);
            if let Some(p) = &self.vo.screenshot {
                let ok = if let Some(mut b) = shot.take() {
                    let mut px = b.mapped_ref().unwrap().to_vec();
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
                    self.last_update = format!("環境マップ（.hdr / .exr）ではない: {}", path.display());
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
                    let scale = 2.0 * self.st.distance * self.st.tan_half_fov_y / self.st.viewport.h.max(1) as f32;
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
    // 最大のサンプル数は 2 の累乗にそろえる（--spp の値は、それ以上で最も近い 2 の累乗に切り上げる）
    let mut max_spp = 4096u32;
    if let Some(spp) = vo.common.spp {
        max_spp = 16;
        while max_spp < spp && max_spp < (1 << 16) {
            max_spp <<= 1;
        }
    }
    let mask_choice = if cfg.scene.mask_preset.is_empty() { None } else { mask_presets().iter().position(|p| p.name == cfg.scene.mask_preset) };
    let st = ViewState {
        target: Vec3::ZERO,
        distance: 100.0,
        elevation: cfg.elevation_deg,
        azimuth: cfg.azimuth_deg,
        fov: cfg.fov_deg,
        exposure: cfg.exposure_ev,
        env_rotation_deg: cfg.env_rotation_deg,
        env_intensity: cfg.env_intensity,
        bump_intensity: cfg.scene.bump_intensity,
        max_spp,
        mode: DisplayMode::Beauty,
        show_panel: true,
        dirty: true,
        materials_dirty: false,
        mask_choice,
        mask_changed: false,
        save_requested: false,
        reset_requested: false,
        auto_exposure_requested: false,
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
        denoise: vo.common.denoise.unwrap_or(false),
        denoise_strength: vo.common.denoise_strength,
        denoise_requested: false,
        last_cursor: (0.0, 0.0),
        ctrl: false,
        viewport: Viewport { x: 0, y: 0, w: 1, h: 1 },
        tan_half_fov_y: 0.27,
    };
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
        scene_as_configured: cfg.scene.clone(),
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
        last_title: Instant::now(),
        last_change_at: Instant::now(),
        last_frame_ms: 0.0,
        last_gpu_ms: 0.0,
        out_dir,
        stem,
        rc: 0,
        done: false,
    };
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
    Ok(app.rc)
}
