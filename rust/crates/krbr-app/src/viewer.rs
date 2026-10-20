//! リアルタイム表示（ウィンドウ）。オフライン描画と同じシーン・積分器で、フレームごとにサンプルを積み増して表示する。
//! 操作パネル（egui）で、環境マップ・露出・レンズ・表面の凹凸・レジストの色などをその場で変えられる。

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ash::vk;
use egui_ash_renderer::{DynamicRendering, Options as EguiOptions, RenderMode, Renderer as EguiRenderer};
use egui_winit::State as EguiState;
use glam::Vec3;
use krbr_core::{error, info, warn};
use krbr_render::image_io::{self, ImageF};
use krbr_render::tonemapper::{DisplayMode, Tonemapper};
use krbr_render::vk::{image_barrier, Buffer};
use krbr_render::studio_env::STUDIO;
use krbr_render::{build_env_distribution, environment_label, is_builtin, load_environment_map, Context, ContextOptions, EnvDistribution, GpuScene, Progressive};
use krbr_scene::config::apply_mask_color_spec;
use krbr_scene::scene::{raycast, update_mask_material};
use krbr_scene::{mask_presets, orbit_camera, LensModel, SceneOptions};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

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
    spectral: bool,
    integrator_changed: bool,
    rotating: bool,
    panning: bool,
    /// Ctrl + クリック：その点にピントを合わせる
    focus_pick: Option<(f64, f64)>,
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
            let _ = tx.send(load_environment(p));
        });
        self.env_job = Some((path, rx));
        self.env_error.clear();
    }

    fn init_gpu(&mut self, el: &ActiveEventLoop) -> Result<(), String> {
        let hidden = self.vo.hidden || self.vo.test_frames > 0;
        let (w, h) = (self.vo.common.width.unwrap_or(1600), self.vo.common.height.unwrap_or(900));
        let attrs = Window::default_attributes().with_title(self.base_title.clone()).with_inner_size(winit::dpi::PhysicalSize::new(w, h)).with_visible(!hidden).with_active(!hidden);
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
        self.gpu = Some(Gpu { ctx, gpu, swap, prog, tone, egui_renderer, cmd, acquired, fence, timestamps, ts_period_ms });
        self.window = Some(window);
        Ok(())
    }

    fn panel(&mut self, ui: &mut egui::Ui) {
        let g = self.gpu.as_ref().unwrap();
        let st = &mut self.st;
        ui.label(format!("{} / {} spp   {:.1} ms/frame (GPU {:.1} ms)", g.prog.samples(), st.max_spp, self.last_frame_ms, self.last_gpu_ms));
        ui.add(egui::ProgressBar::new((g.prog.samples() as f32 / st.max_spp.max(1) as f32).min(1.0)));
        if !self.last_update.is_empty() {
            ui.weak(&self.last_update);
        }

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
        ui.weak("ノイズ除去（OIDN）は Rust 版では未対応");

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
        ui.weak("左ドラッグ: 回転  右ドラッグ: 移動  ホイール: 拡大");
        ui.weak("F1 / Tab: このパネルの表示切り替え");
        if let Some(p) = chosen {
            self.start_env_load(p);
        }
        if open_dialog {
            self.start_open_dialog();
        }
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
                    ui.label("右 / 中ドラッグ: 移動");
                    ui.label("ホイール: 拡大・縮小");
                    ui.label("Ctrl + クリック: その点にピントを合わせる");
                    ui.separator();
                    ui.label("F1 / Tab: 設定パネル    R: 視点を戻す    S: 保存");
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
            if self.st.show_panel {
                self.side_panel(root);
            }
            central = root.available_rect_before_wrap();
        });
        if self.st.quit_requested {
            self.done = true;
            return Ok(());
        }
        let window = self.window.as_ref().unwrap();
        self.egui_state.as_mut().unwrap().handle_platform_output(window, full.platform_output);
        let primitives = egui_ctx.tessellate(full.shapes, full.pixels_per_point);

        // ---- 状態の反映（GPU は前のフレームの完了を待っているので、ここで資源を触ってよい）
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
                self.st.dirty = true;
            }
            self.st.viewport = vp;
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

        // ---- 描画
        let g = self.gpu.as_mut().unwrap();
        let dev = &g.ctx.device;
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
        let add = if g.prog.samples() >= max_spp { 0 } else { self.samples_per_frame.min(max_spp - g.prog.samples()) };
        let last_test_frame = self.vo.test_frames > 0 && self.frames + 1 >= self.vo.test_frames && self.env_job.is_none();

        // egui のテクスチャ
        for (id, deltas) in &full.textures_delta.set {
            for delta in deltas.iter() {
                g.egui_renderer.set_texture(g.ctx.queue, g.ctx.pool, *id, delta).map_err(|e| format!("egui texture: {e}"))?;
            }
        }

        let cmd = g.cmd;
        unsafe {
            dev.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty()).map_err(|e| format!("{e:?}"))?;
            dev.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)).map_err(|e| format!("{e:?}"))?;
            dev.cmd_reset_query_pool(cmd, g.timestamps, 0, 2);
            dev.cmd_write_timestamp2(cmd, vk::PipelineStageFlags2::ALL_COMMANDS, g.timestamps, 0);
        }
        g.prog.record(cmd, add);
        unsafe { dev.cmd_write_timestamp2(cmd, vk::PipelineStageFlags2::ALL_COMMANDS, g.timestamps, 1) };
        g.tone.record(cmd, self.st.exposure, self.st.mode, g.prog.xyz_output());
        let disp = g.tone.output();
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
            self.window.as_ref().unwrap().set_title(&format!("{} - {} / {} spp - {:.1} ms/frame (GPU {:.1} ms) - EV {:+.2}", self.base_title, g.prog.samples(), max_spp, self.last_frame_ms, self.last_gpu_ms, self.st.exposure));
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
                    MouseButton::Left => self.st.rotating = down,
                    MouseButton::Right | MouseButton::Middle => self.st.panning = down,
                    _ => {}
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let (dx, dy) = (position.x - self.st.last_cursor.0, position.y - self.st.last_cursor.1);
                self.st.last_cursor = (position.x, position.y);
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
        let idle = self.gpu.as_ref().map(|g| g.prog.samples() >= self.st.max_spp).unwrap_or(false) && !self.st.dirty && self.env_job.is_none() && self.vo.test_frames == 0;
        if idle {
            // 収束したら GPU を休ませ、入力を待つ
            el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(250)));
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
        spectral: cfg.spectral,
        integrator_changed: false,
        rotating: false,
        panning: false,
        focus_pick: None,
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
