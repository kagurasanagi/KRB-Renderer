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
use krbr_render::tonemapper::{Background, DisplayMode, Tonemapper};
use krbr_render::vk::{image_barrier, memory_barrier, Buffer};
use krbr_render::studio_env::STUDIO;
use krbr_render::{build_env_distribution, describe_environment, is_builtin, load_environment_map, Context, ContextOptions, DenoiseQuality, Denoiser, EnvDistribution, EnvironmentName, GpuScene, Progressive};
use krbr_scene::config::apply_mask_color_spec;
use krbr_scene::scene::{load_material_setup, raycast_pick_visible, MaterialFilesWatch, update_mask_material, update_substrate_material, visibility, MaterialAssignment};
use krbr_scene::{mask_presets, orbit_camera, Camera, InstanceOwner, IntegratorKind, LensModel, MaterialLibrary, MaterialStage, Scene, SceneOptions};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::fl;

mod video_job;
mod web_about;
mod web_menu;
mod web_panel;
use crate::i18n;
use crate::kicad_cli::StrokeTextJob;
use crate::kicad_plugin::{self, PluginWindow};
use crate::material_cmd::{clear_material, forget_material, set_material, MaterialSetOptions};
use crate::selection::{unproject, Outline, Selected};
use krbr_scene::board_file;
use krbr_scene::material_library::{save_user_material, split_variant, with_emission, with_smooth, Emission, Variant};
use crate::material_editor::{self, Action as EditorAction, MaterialEditor, PartInfo, PartRow};
use crate::prepare::{ReloadStats, SceneSource};
use crate::remember;
use crate::prepare::{frame_params, framed_camera, initial_camera, prepare_empty, prepare_scene, progressive_or_pt, trace_method_name, PreparedScene, RenderOptions, ENV_DIST_RESOLUTION};
use crate::swapchain::Swapchain;

#[derive(Debug, Clone, Default)]
pub struct ViewOptions {
    pub common: RenderOptions,
    /// ウィンドウを表示しない（test.frames 指定時は自動）
    pub hidden: bool,
    /// .kicad_pcb の保存を監視して描き直す（--no-watch で無効）
    pub watch: bool,
    /// レンダリングを一時停止した状態で開く（GPU の弱い PC 向け）
    pub paused: bool,
    /// 描画のサイズ（--render-size）。None なら覚えたもの
    pub render_size: Option<RenderSize>,
    /// レンダリングの比率（レンダリングの解像度 ÷ 表示の大きさ）。None なら覚えたもの
    pub render_ratio: Option<f32>,
    /// 表示の言語（--lang。"ja" など）。None なら環境変数 KRBR_LANG、メニューで選んだもの、KiCad、OS の順
    pub language: Option<String>,
    /// メニュー・ボタン列・パネル・材質エディタを WebView（WebView2）で描く。隠したウィンドウ（テスト）では作らない。
    /// 作れなければ 3D 表示とキーの操作だけになる（WebView2 Runtime の入れ方を知らせる）
    pub web_ui: bool,
    /// 開発用のテストの操作（--test-*）
    pub test: TestOptions,
}

/// 開発用（--test-*）：表示しないウィンドウで決まった数のフレームを描き、その途中で UI の操作をしたことにする
#[derive(Debug, Clone, Default)]
pub struct TestOptions {
    /// >0: このフレーム数を描いたら screenshot を保存して終了
    pub frames: u32,
    /// frames 用の保存先（表示中の画像そのもの）
    pub screenshot: Option<PathBuf>,
    /// 毎フレームこの角度だけカメラを回す
    pub orbit_deg: f64,
    /// テスト中に一度だけ基板を読み直す（保存の代わり）
    pub reload: bool,
    /// テスト中にこの画素（3D 表示の中の位置）をクリックしたことにする
    pub pick: Option<(f64, f64)>,
    /// pick で選んだ部品の全部位に、この材質を割り当てる（この部品だけ）
    pub assign: String,
    /// テスト中に、パネルでこのレジストの色（プリセット）を選んだことにする
    pub mask: String,
    /// テストの終わり近くに「名前を付けて保存」でこのファイル（.png / .exr）を選んだことにする
    pub save_as: Option<PathBuf>,
    /// テスト中に、メニューの「設定をすべて既定に戻す」を押したことにする
    pub reset_settings: bool,
    /// テストの初めから材質エディタを開いておく
    pub material_editor: bool,
    /// テストの初めから「設定 → KiCad プラグイン」の窓を開いておく
    pub kicad_plugin: bool,
    /// テスト中にこの部品番号の部品を選ぶ（クリックの代わり。pick より後）
    pub select: String,
    /// テスト中に「ファイル → 開く...」でこの基板を選んだことにする
    pub open: Option<PathBuf>,
    /// テスト中にボタン列の「真上から」（"top"）・「真裏から」（"bottom"）を押したことにする
    pub view: String,
    /// テスト中にボタン列で隠す部品の種類（"smd" / "tht" / "smd,tht"）
    pub hide: String,
    /// 基板を開いたら、短い小さな動画（320x180、1 秒、10 fps、16 spp）をこのファイルに書き出す（開発用。テストのフレーム数とは別に、見えるウィンドウで使う）
    pub video: Option<PathBuf>,
}

/// テストのパネルの操作（レジストの色・すべて既定に戻す）をするフレーム（0 から数える）。
/// 最初のフレームで 3D 表示の大きさが決まって視点を合わせ直し、パネルも描き終えた後
const TEST_PANEL_FRAME: u32 = 3;
/// テストの読み直しとクリックをするフレーム（これ以降で、裏の読み直しが終わっているとき）。パネルの操作の結果が反映した後
const TEST_SCENE_FRAME: u32 = 5;
/// テストの「名前を付けて保存」は、終わりのこのフレーム数前（保存したフレームの後に、最後のフレームを描いて終わる）
const TEST_SAVE_AS_BEFORE_END: u32 = 2;

/// テストの操作の進み具合。本番の処理は test_* を見ず、ここが UI の要求（ViewState::req）として流し込む
#[derive(Default)]
struct TestHooks {
    o: TestOptions,
    open_done: bool,
    /// --test-open の基板に切り替えたフレーム（その後も何フレームか描いてから終える）
    opened_at: Option<u32>,
    reload_started: bool,
    pick_done: bool,
}

impl TestHooks {
    /// テストで動かしているか（ウィンドウを表示せず、状態も覚えない）
    fn active(&self) -> bool {
        self.o.frames > 0
    }

    /// フレームの初めに、このフレーム（frame 番目、0 から）でする操作を要求として入れる
    fn inject(&mut self, frame: u32, st: &mut ViewState, save_as_target: &mut Option<(SaveFormat, PathBuf)>) {
        if self.o.orbit_deg != 0.0 {
            st.azimuth += self.o.orbit_deg;
            st.changed.frame = true;
        }
        if frame == TEST_PANEL_FRAME {
            if let Some(i) = mask_presets().iter().position(|p| !self.o.mask.is_empty() && p.name == self.o.mask) {
                st.mask_choice = Some(i);
                st.changed.mask = true;
            }
            st.req.reset_all |= self.o.reset_settings;
            st.req.material_editor |= self.o.material_editor;
            st.req.kicad_plugin |= self.o.kicad_plugin;
            match self.o.view.as_str() {
                "top" => st.req.view_preset = Some(TOP_VIEW),
                "bottom" => st.req.view_preset = Some(BOTTOM_VIEW),
                "fit" => st.req.fit_view = true,
                _ => {}
            }
            for kind in self.o.hide.split(',').map(str::trim) {
                match kind {
                    "smd" => st.show_smd = false,
                    "tht" => st.show_tht = false,
                    "solder" => st.show_solder = false,
                    _ => continue,
                }
                st.changed.visibility = true;
            }
        }
        if let Some(p) = self.o.open.clone().filter(|_| !self.open_done && frame >= TEST_SCENE_FRAME) {
            self.open_done = true;
            st.req.open_path = Some(p);
        }
        if self.o.reload && !self.reload_started && frame >= TEST_SCENE_FRAME {
            self.reload_started = true;
            st.req.reload = true;
        }
        if !self.o.select.is_empty() && !self.pick_done && frame >= TEST_SCENE_FRAME {
            self.pick_done = true;
            st.req.select_reference = Some(self.o.select.clone());
            if !self.o.assign.is_empty() {
                st.req.assign_all = Some(self.o.assign.clone());
            }
        }
        if let Some((x, y)) = self.o.pick.filter(|_| !self.pick_done && frame >= TEST_SCENE_FRAME) {
            self.pick_done = true;
            st.req.part_pick = Some((x + st.viewport.x as f64, y + st.viewport.y as f64));
            if !self.o.assign.is_empty() {
                st.req.assign_all = Some(self.o.assign.clone());
            }
        }
        if let Some(p) = self.o.save_as.as_ref().filter(|_| self.active() && frame + TEST_SAVE_AS_BEFORE_END == self.o.frames) {
            let format = if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exr")) { SaveFormat::Exr } else { SaveFormat::Png };
            *save_as_target = Some((format, p.clone()));
        }
    }

    /// このフレーム（frame 番目）で終えてよいか：決めたフレーム数に達し、裏の処理（busy）が無く、頼んだ読み直し・クリックが済んだ
    fn finished(&self, frame: u32, busy: bool, saved_reloads: u32) -> bool {
        self.active() && frame + 1 >= self.o.frames && !busy && (!self.o.reload || saved_reloads > 0) && ((self.o.pick.is_none() && self.o.select.is_empty()) || self.pick_done)
            && (self.o.open.is_none() || self.opened_at.is_some_and(|f| frame >= f + 10))
    }

    fn screenshot(&self) -> Option<&PathBuf> {
        self.o.screenshot.as_ref()
    }
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

/// 環境マップの表示名：内蔵のものは翻訳した名前（i18n の env-builtin-<ID>。無ければ「<ID>（内蔵）」）、ほかはファイル名
fn env_label(spec: &str) -> String {
    match describe_environment(spec) {
        EnvironmentName::Builtin(id) => i18n::get(&format!("env-builtin-{id}")).unwrap_or_else(|| fl!("env-builtin", id = id)),
        EnvironmentName::File(name) => name,
    }
}

/// 環境マップの一覧のヒント：内蔵のものは説明（env-builtin-<ID>-tip）、ほかはパス
fn env_tip(spec: &str) -> String {
    match describe_environment(spec) {
        EnvironmentName::Builtin(id) => i18n::get(&format!("env-builtin-{id}-tip")).unwrap_or_default(),
        EnvironmentName::File(_) => spec.to_string(),
    }
}

/// レンダリングの比率の候補（レンダリングの解像度 ÷ 表示の大きさ）。メニューとパネルで選べるもの
const RATIOS: [f32; 5] = [0.25, 0.5, 1.0, 1.5, 2.0];
/// --render-ratio と覚えた比率で使える範囲。0.1 未満は粗すぎて形が分からず、4 を超えると画素数（16 倍）と時間に見合う違いが無い。
/// メニューの候補（RATIOS）に無い値も、コマンドラインからは使える
pub const RENDER_RATIO_RANGE: std::ops::RangeInclusive<f32> = 0.1..=4.0;

/// 描画のサイズの指定（--render-size）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderSize {
    /// ウィンドウに合わせる（"fit"）
    Fit,
    /// 決まったサイズ（"WxH"）
    Fixed(u32, u32),
}

/// --render-size の値を読む："fit" か "WxH"（"1920x1080"。"×" も可）。読めない・範囲外なら Err（黙って「ウィンドウに合わせる」にしない）
pub fn parse_render_size(s: &str) -> Result<RenderSize, String> {
    let lower = s.trim().to_lowercase();
    if lower == "fit" {
        return Ok(RenderSize::Fit);
    }
    let bad = || format!("'{s}' is not a rendering size (WxH such as 1920x1080, or fit)");
    let (w, h) = lower.split_once(['x', '×']).ok_or_else(bad)?;
    let (w, h) = (w.trim().parse::<i64>().map_err(|_| bad())?, h.trim().parse::<i64>().map_err(|_| bad())?);
    if !remember::valid_render_size(w, h) {
        return Err(format!("rendering size {w}x{h} is out of range (1 .. {} per side)", remember::MAX_RENDER_SIZE));
    }
    Ok(RenderSize::Fixed(w as u32, h as u32))
}

/// --render-ratio の値を読む（RENDER_RATIO_RANGE の中の数）
pub fn parse_render_ratio(s: &str) -> Result<f32, String> {
    let r: f32 = s.trim().parse().map_err(|_| format!("'{s}' is not a number"))?;
    if !RENDER_RATIO_RANGE.contains(&r) {
        return Err(format!("rendering ratio {r} is out of range ({} .. {})", RENDER_RATIO_RANGE.start(), RENDER_RATIO_RANGE.end()));
    }
    Ok(r)
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
    /// kicad-cli で書き出し直した線フォントの文字を反映した
    text: bool,
    /// 材質の設定を読み直した
    materials: bool,
}

/// UI（メニュー・パネル・キー・マウス）とテストからの要求。フレームの中でまとめて処理し、処理したものは戻す
#[derive(Default)]
struct Requests {
    /// S キー・メニュー：番号を付けて PNG と EXR を保存する
    save: bool,
    /// メニュー「PNG / EXR として保存...」：保存のダイアログを開く
    save_as: Option<SaveFormat>,
    /// メニュー：基板の設定ファイルの置き場所を変える・ファイルを表示する
    settings_location: Option<board_file::Location>,
    reveal_settings: bool,
    /// 視点を戻す（R）
    reset_view: bool,
    /// ボタン列・メニュー：この仰角・方位（度）にして、基板全体が入る構図にする（真上から・真裏から）
    view_preset: Option<(f64, f64)>,
    /// ボタン列・メニュー・F：今の向きのまま、基板全体が入る構図にする（全体を表示）
    fit_view: bool,
    /// 「すべて既定に戻す」の後：視点を戻した後で、覚える値の基準を今の値に取り直す（戻した値は覚えない）
    rebase_view: bool,
    auto_exposure: bool,
    /// メニュー：パネルの設定をすべて既定（設定ファイルとコマンドライン）に戻し、覚えたものを忘れる
    reset_all: bool,
    /// 最初のフレーム：3D 表示の大きさが決まったので、最初の視点（覚えた視点、無ければ自動の構図）を合わせ直す
    start_view: bool,
    quit: bool,
    /// 決まったサイズに合わせてウィンドウの大きさを変える
    fit_window: bool,
    /// メニューで表示の言語を選んだ（Some(None)：自動）
    language: Option<Option<String>>,
    /// D キー／ボタン：ノイズ除去が CPU のとき、手動で 1 回かける
    denoise_now: bool,
    /// Ctrl + クリック：その点にピントを合わせる
    focus_pick: Option<(f64, f64)>,
    /// クリック：その点の部品を選ぶ
    part_pick: Option<(f64, f64)>,
    /// 保存を待たずに基板を読み直す（テスト）
    reload: bool,
    /// 選んだ部品の全部位にこの材質を割り当てる（テスト）
    assign_all: Option<String>,
    /// 材質エディタを開く（テスト）
    material_editor: bool,
    /// メニュー「設定 → KiCad プラグイン...」：その窓を開く
    kicad_plugin: bool,
    /// 「ファイル → 開く...」
    open_board: bool,
    /// この基板を開く（テスト。ダイアログの代わり）
    open_path: Option<PathBuf>,
    /// この部品番号の部品を選ぶ（テスト）
    select_reference: Option<String>,
}

/// 変わったもの（GPU へ送るまで立てておく）
#[derive(Default)]
struct Changes {
    /// カメラ・照明・レンズ・色の計算が変わった（累積をやり直す）
    frame: bool,
    /// 材質（凹凸の強さ・レジストの色）を GPU へ送り直す
    materials: bool,
    /// パネルでレジストの色を選び直した
    mask: bool,
    integrator: bool,
    /// 部品の表示・非表示（表面実装・スルーホール）を変えた
    visibility: bool,
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
    /// レジストの色：None = 設定どおり、Some = プリセットの添字
    mask_choice: Option<usize>,
    /// レンダリングを一時停止する（サンプルを積み足さない。GPU の弱い PC で操作を軽くする）
    paused: bool,
    /// 描画ロック：今の累積を守る。視点の操作は効かず、設定の変更・ファイルの保存による読み直しは、解除するまで待たせる
    /// （時間をかけた画像を、うっかり触ってやり直しにしない）。表示だけの設定（露出・グロー・ノイズ除去）は効く
    locked: bool,
    /// ピントを合わせる点（ワールド座標）。None なら注視点（自動）。カメラを動かしても、この点までの距離にピントを合わせ続ける
    focus_point: Option<Vec3>,
    /// 表面実装の部品・スルーホールの部品を表示する（どちらでもない部品と基板はいつも表示）
    show_smd: bool,
    show_tht: bool,
    /// はんだフィレットを表示する（形状は作ったまま。隠すと光線が通り抜ける）
    show_solder: bool,
    spectral: bool,
    /// 背景を透明にする（表示は市松模様、保存する PNG はアルファ付き）。動画の書き出しの間は使わない
    transparent: bool,
    rotating: bool,
    panning: bool,
    /// 左ボタンを押した位置と、そこから動いた量（ほとんど動かさずに離したらクリック）
    press_at: Option<(f64, f64)>,
    press_moved: f64,
    /// ノイズ除去（OIDN）した画像を表示する。強さは元の画像と混ぜる割合（1 で OIDN の結果そのまま）
    denoise: bool,
    denoise_strength: f32,
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
    tan_half_fov_y: f32,
    req: Requests,
    changed: Changes,
}

impl ViewState {
    fn glow_params(&self) -> GlowParams {
        GlowParams { strength: self.glow, threshold: self.glow_threshold, spread: self.glow_spread }
    }

    /// 見せるインスタンスの種類（krbr_scene::visibility のビット）
    fn visible_mask(&self) -> u8 {
        let mut m = visibility::ALL;
        // はんだフィレットは、その部品を隠したときも隠す
        if !self.show_smd {
            m &= !(visibility::SMD | visibility::SOLDER_SMD);
        }
        if !self.show_tht {
            m &= !(visibility::THROUGH_HOLE | visibility::SOLDER_THROUGH_HOLE);
        }
        if !self.show_solder {
            m &= !visibility::SOLDER;
        }
        m
    }
}

/// ビューアだけの設定（パネルの表示の欄など）の初めの値：コマンドライン、覚えた値、既定の順に決める。
/// 起動時は覚えた値で、「すべて既定に戻す」では空の値で作る（どちらも同じ決め方になる）
struct ViewerSettings {
    max_spp: u32,
    focus_point: Option<Vec3>,
    show_smd: bool,
    show_tht: bool,
    show_solder: bool,
    denoise: bool,
    denoise_strength: f32,
    mode: DisplayMode,
    size_mode: Option<(u32, u32)>,
    render_ratio: f32,
}

/// 最大のサンプル数の既定値
const DEFAULT_MAX_SPP: u32 = 4096;

/// --web-ui のボタン列（上端のメニューの見出しを含む）とステータスバーの高さ（egui のポイント）
const WEB_TOOLBAR_HEIGHT: f32 = 106.0;
const WEB_STATUS_HEIGHT: f32 = 48.0;
/// --web-ui の警告の帯の高さ（3D 表示の上端に重ねる）
const WEB_BANNER_HEIGHT: f32 = 40.0;

/// --web-ui の開いているメニュー
struct WebMenu {
    /// どのメニューか（"file" など）
    id: String,
    /// プルダウンの左上（ボタン列の中の位置。egui のポイント = CSS の px）
    at: egui::Pos2,
    /// 中身の大きさ（ページが測って知らせてくる。それまでは隠しておく）：見せているところと、どのサブメニューを開いても収まる大きさ。
    /// WebView は後者の大きさにして、前者だけを見せる（サブメニューを開くたびに WebView の大きさを変えると、描き直す間ちらつく）
    size: Option<(egui::Vec2, egui::Vec2)>,
    /// キーボードの入力をプルダウンへ渡したか（見せたときに一度だけ）
    focused: bool,
}

/// --web-ui の説明の吹き出し
struct WebTip {
    /// 説明のある要素のページ
    pane: crate::web_ui::Pane,
    /// その要素の位置（ページの中。egui のポイント = CSS の px）
    target: egui::Rect,
    text: String,
    /// 吹き出しの大きさ（ページが測って知らせてくる。それまでは隠しておく）
    size: Option<egui::Vec2>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusState {
    Rendering,
    Paused,
    Done,
}

/// ステータスバーの文字（訳したもの）
struct StatusTexts {
    state: StatusState,
    state_text: String,
    samples: u32,
    max_spp: u32,
    elapsed: String,
    elapsed_tip: String,
    average: Option<String>,
    average_tip: String,
    remaining: Option<String>,
    /// 2 行目：描画の条件（文字と、マウスを載せたときの説明）
    chips: Vec<(String, Option<String>)>,
    /// 2 行目の終わり：読み直しなどの知らせ（作業中なら true）
    notes: Vec<(String, bool)>,
}

impl StatusTexts {
    /// WebView へ送る JSON（status.html の krbr.status）
    fn json(&self) -> String {
        let mut chips: Vec<serde_json::Value> = self.chips.iter().map(|(t, tip)| serde_json::json!({ "text": t, "tip": tip })).collect();
        chips.extend(self.notes.iter().map(|(t, busy)| serde_json::json!({ "text": t, "note": true, "busy": busy })));
        serde_json::json!({
            "state": match self.state { StatusState::Rendering => "rendering", StatusState::Paused => "paused", StatusState::Done => "done" },
            "stateText": self.state_text,
            "samples": self.samples, "maxSpp": self.max_spp,
            "elapsed": self.elapsed, "elapsedTip": self.elapsed_tip,
            "average": self.average, "averageTip": self.average_tip,
            "remaining": self.remaining,
            "chips": chips,
        })
        .to_string()
    }
}

/// WebView のキー（KeyboardEvent.code）を、ビューアのキーの割り当てに使う KeyCode にする（割り当てのあるキーだけ）
fn key_code_of_js(code: &str) -> Option<KeyCode> {
    let k = match code {
        "F1" => KeyCode::F1,
        "Tab" => KeyCode::Tab,
        "KeyR" => KeyCode::KeyR,
        "KeyF" => KeyCode::KeyF,
        "KeyS" => KeyCode::KeyS,
        "KeyP" => KeyCode::KeyP,
        "KeyL" => KeyCode::KeyL,
        "KeyD" => KeyCode::KeyD,
        "KeyM" => KeyCode::KeyM,
        "KeyO" => KeyCode::KeyO,
        "Equal" => KeyCode::Equal,
        "Minus" => KeyCode::Minus,
        "NumpadAdd" => KeyCode::NumpadAdd,
        "NumpadSubtract" => KeyCode::NumpadSubtract,
        "BracketLeft" => KeyCode::BracketLeft,
        "BracketRight" => KeyCode::BracketRight,
        "Digit1" => KeyCode::Digit1,
        "Digit2" => KeyCode::Digit2,
        "Digit3" => KeyCode::Digit3,
        "Escape" => KeyCode::Escape,
        _ => return None,
    };
    Some(k)
}

/// 長さ（mm）を、大きさに合った単位と桁数の文字にする（単位も付ける）：1 mm 未満は µm、1 m 以上は m、1 km 以上は km。
/// 有効数字はおよそ 3 桁（カメラの距離のように、1 mm 未満から数 km まで変わる値を読めるように）
fn format_length(mm: f64) -> String {
    if !mm.is_finite() {
        return format!("{mm} mm");
    }
    let a = mm.abs();
    let three = |v: f64, unit: &str| {
        let d = if v.abs() >= 100.0 { 0 } else if v.abs() >= 10.0 { 1 } else { 2 };
        format!("{v:.d$} {unit}")
    };
    if a == 0.0 {
        "0 mm".into()
    } else if a < 1.0 {
        three(mm * 1000.0, "µm")
    } else if a < 1000.0 {
        three(mm, "mm")
    } else if a < 1.0e6 {
        three(mm / 1000.0, "m")
    } else {
        three(mm / 1.0e6, "km")
    }
}

/// ピントの距離：カメラの前方向に測った、点までの距離（mm）。点がカメラの後ろや真横なら 1 mm（レンズの計算が壊れないように）
fn focus_distance_to(point: Vec3, cam: &Camera) -> f32 {
    (point - cam.position).dot(cam.forward).max(1.0)
}

/// 基板が画面に写っているか
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoardInView {
    Visible,
    /// 画面の外（カメラの後ろ・画面の外側）
    Outside,
    /// 写っているが、遠すぎて点のように小さい
    TooSmall,
}

/// これより小さく（画面の高さに対する割合で）写っていたら「遠すぎる」
const TOO_SMALL_EXTENT: f32 = 0.02;

/// カメラの前にある点の、画面の上での広がり（縦横の大きい方。画面の高さを 1 とした割合）。前に点が無ければ 0
fn projected_extent(cam: &Camera, pts: &[Vec3]) -> f32 {
    let (mut lo, mut hi) = (glam::Vec2::splat(f32::MAX), glam::Vec2::splat(f32::MIN));
    for p in pts {
        let v = *p - cam.position;
        let z = v.dot(cam.forward);
        if z <= 1e-3 {
            continue;
        }
        let q = glam::Vec2::new(v.dot(cam.right), v.dot(cam.up)) / (z * 2.0 * cam.tan_half_fov_y);
        lo = lo.min(q);
        hi = hi.max(q);
    }
    if lo.x > hi.x {
        0.0
    } else {
        (hi - lo).max_element()
    }
}

/// 点がすべて画面の外か：すべてカメラの後ろ、またはすべて画面の同じ辺の外側（一部でも写っていれば false）
fn points_out_of_view(cam: &Camera, pts: &[Vec3]) -> bool {
    let (mut left, mut right, mut above, mut below, mut behind) = (true, true, true, true, true);
    let tx = cam.tan_half_fov_y * cam.aspect;
    for p in pts {
        let v = *p - cam.position;
        let z = v.dot(cam.forward);
        if z <= 1e-3 {
            continue;
        }
        behind = false;
        let (x, y) = (v.dot(cam.right) / (z * tx), v.dot(cam.up) / (z * cam.tan_half_fov_y));
        left &= x < -1.0;
        right &= x > 1.0;
        below &= y < -1.0;
        above &= y > 1.0;
    }
    behind || left || right || above || below
}

/// 「真上から」「真裏から」の仰角・方位（度）。真上・方位 0° は KiCad の画面と同じ向き、真下・方位 180° は左右が反転した、
/// KiCad で裏から見た向き（krbr-scene の camera.rs の basis）。その後ドラッグしても向きが跳ばない
const TOP_VIEW: (f64, f64) = (90.0, 0.0);
const BOTTOM_VIEW: (f64, f64) = (-90.0, 180.0);

impl ViewerSettings {
    fn new(vo: &ViewOptions, remembered: &remember::ViewValues) -> Self {
        let r = remember::ViewerValues::of(remembered);
        let size_mode = match vo.render_size {
            Some(RenderSize::Fit) => None,
            Some(RenderSize::Fixed(w, h)) => Some((w, h)),
            None => remember::render_size_of(remembered),
        };
        Self {
            max_spp: pow2_spp(vo.common.spp.or(r.max_spp).unwrap_or(DEFAULT_MAX_SPP)),
            focus_point: r.focus_point.map(|p| Vec3::new(p[0] as f32, p[1] as f32, p[2] as f32)),
            show_smd: r.show_smd.unwrap_or(true),
            show_tht: r.show_tht.unwrap_or(true),
            show_solder: r.show_solder.unwrap_or(true),
            denoise: vo.common.denoise.or(r.denoise).unwrap_or(false),
            denoise_strength: vo.common.denoise_strength.or(r.denoise_strength).unwrap_or(1.0),
            mode: r.display_mode.as_deref().map(display_mode_of).unwrap_or(DisplayMode::Beauty),
            size_mode,
            render_ratio: vo.render_ratio.or(r.render_ratio).map(|x| x.clamp(*RENDER_RATIO_RANGE.start(), *RENDER_RATIO_RANGE.end())).unwrap_or(1.0),
        }
    }

    fn apply(&self, st: &mut ViewState) {
        st.max_spp = self.max_spp;
        st.focus_point = self.focus_point;
        st.show_smd = self.show_smd;
        st.show_tht = self.show_tht;
        st.show_solder = self.show_solder;
        st.changed.visibility = true;
        st.denoise = self.denoise;
        st.denoise_strength = self.denoise_strength;
        st.mode = self.mode;
        st.size_mode = self.size_mode;
        st.render_ratio = self.render_ratio;
        st.req.fit_window = self.size_mode.is_some();
    }
}

fn mask_choice_of(s: &SceneOptions) -> Option<usize> {
    if s.mask_preset.is_empty() {
        None
    } else {
        mask_presets().iter().position(|p| p.name == s.mask_preset)
    }
}

/// 選んだ部品の材質を書き込む先
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssignScope {
    /// この部品だけ（<名前>.krbr の [ref.<部品番号>]）
    Part,
    /// このモデル（このプロジェクトの <名前>.krbr）
    ModelProject,
    /// このモデル（全体設定の model_map.toml）
    ModelGlobal,
}

/// キー操作
#[derive(Debug, Clone, Copy, PartialEq)]
enum KeyAction {
    Quit,
    TogglePanel,
    ResetView,
    FitView,
    Save,
    Pause,
    DenoiseNow,
    /// 露出を変える（EV）
    Exposure(f32),
    /// 環境マップを回す（度）
    RotateEnvironment(f32),
    Mode(DisplayMode),
    MaterialEditor,
    OpenBoard,
    Lock,
}

/// キーの割り当ての 1 行（ヘルプの 1 行）
struct KeyBinding {
    /// 表示するキーの名前
    label: &'static str,
    keys: &'static [(KeyCode, KeyAction)],
    /// ヘルプの説明（i18n の id）と、起動時のログの説明（英語）
    help: &'static str,
    log: &'static str,
}

/// +/- で変える露出（EV）と、[ / ] で回す環境マップの角度（度）
const EXPOSURE_STEP_EV: f32 = 0.25;
const ENV_ROTATION_STEP_DEG: f32 = 15.0;

/// キーの割り当て。キー処理・ヘルプのメニュー・起動時のログは、どれもこの表から作る
const KEY_BINDINGS: &[KeyBinding] = &[
    KeyBinding { label: "F1 / Tab", keys: &[(KeyCode::F1, KeyAction::TogglePanel), (KeyCode::Tab, KeyAction::TogglePanel)], help: "key-panel", log: "panel" },
    KeyBinding { label: "R", keys: &[(KeyCode::KeyR, KeyAction::ResetView)], help: "key-reset-view", log: "reset view" },
    KeyBinding { label: "F", keys: &[(KeyCode::KeyF, KeyAction::FitView)], help: "key-fit-view", log: "fit the board in the view" },
    KeyBinding { label: "S", keys: &[(KeyCode::KeyS, KeyAction::Save)], help: "key-save", log: "save" },
    KeyBinding { label: "P", keys: &[(KeyCode::KeyP, KeyAction::Pause)], help: "key-pause", log: "pause" },
    KeyBinding { label: "L", keys: &[(KeyCode::KeyL, KeyAction::Lock)], help: "key-lock", log: "render lock" },
    KeyBinding { label: "D", keys: &[(KeyCode::KeyD, KeyAction::DenoiseNow)], help: "key-denoise", log: "denoise now (CPU)" },
    KeyBinding {
        label: "+ / -",
        keys: &[
            (KeyCode::Equal, KeyAction::Exposure(EXPOSURE_STEP_EV)),
            (KeyCode::NumpadAdd, KeyAction::Exposure(EXPOSURE_STEP_EV)),
            (KeyCode::Minus, KeyAction::Exposure(-EXPOSURE_STEP_EV)),
            (KeyCode::NumpadSubtract, KeyAction::Exposure(-EXPOSURE_STEP_EV)),
        ],
        help: "key-exposure",
        log: "exposure",
    },
    KeyBinding {
        label: "[ / ]",
        keys: &[(KeyCode::BracketLeft, KeyAction::RotateEnvironment(-ENV_ROTATION_STEP_DEG)), (KeyCode::BracketRight, KeyAction::RotateEnvironment(ENV_ROTATION_STEP_DEG))],
        help: "key-env-rotation",
        log: "rotate environment",
    },
    KeyBinding {
        label: "1 / 2 / 3",
        keys: &[(KeyCode::Digit1, KeyAction::Mode(DisplayMode::Beauty)), (KeyCode::Digit2, KeyAction::Mode(DisplayMode::Albedo)), (KeyCode::Digit3, KeyAction::Mode(DisplayMode::Normal))],
        help: "key-display-mode",
        log: "image/albedo/normal",
    },
    KeyBinding { label: "M", keys: &[(KeyCode::KeyM, KeyAction::MaterialEditor)], help: "key-material-editor", log: "material editor" },
    KeyBinding { label: "O", keys: &[(KeyCode::KeyO, KeyAction::OpenBoard)], help: "key-open-board", log: "open a board" },
    KeyBinding { label: "Esc", keys: &[(KeyCode::Escape, KeyAction::Quit)], help: "key-quit", log: "quit" },
];

fn key_action(code: KeyCode) -> Option<KeyAction> {
    KEY_BINDINGS.iter().flat_map(|b| b.keys).find(|(k, _)| *k == code).map(|(_, a)| *a)
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

/// 読み込み中の環境マップ
struct EnvJob {
    path: String,
    rx: mpsc::Receiver<EnvData>,
    /// 読み終えたら、この基板で使ったものとして覚える（「すべて既定に戻す」で設定ファイルのものに戻すときは覚えない）
    remember: bool,
}

fn load_environment(path: String) -> EnvData {
    match load_environment_map(&path) {
        Ok(image) => {
            let dist = build_env_distribution(&image, ENV_DIST_RESOLUTION);
            EnvData { path, image, dist, error: String::new() }
        }
        Err(e) => EnvData { path, image: ImageF::default(), dist: EnvDistribution::default(), error: e },
    }
}

fn is_env_file(p: &Path) -> bool {
    matches!(p.extension().map(|e| e.to_string_lossy().to_lowercase()).as_deref(), Some("hdr") | Some("exr"))
}

/// 選べる環境マップ：内蔵のスタジオ、最近使ったもの、今のものと同じフォルダの .hdr / .exr（重なりは除く）
fn list_environments(current: &str) -> Vec<Vec<String>> {
    let mut groups: Vec<Vec<String>> = vec![vec![STUDIO.to_string()]];
    let mut seen: Vec<String> = vec![STUDIO.to_string()];
    let mut add = |groups: &mut Vec<Vec<String>>, items: Vec<String>| {
        let items: Vec<String> = items.into_iter().filter(|p| !seen.iter().any(|s| remember::same_hdr(s, p))).collect();
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

/// 流したバッチ（サンプルを積んでトーンマッピングするコマンド）
#[derive(Clone, Copy)]
struct Batch {
    /// タイムラインセマフォの値（Gpu::submitted）
    value: u64,
    /// 積むサンプルの数
    add: u32,
    /// トーンマッピングの出力の番号
    slot: usize,
    t0: Instant,
}

/// 表示用のトーンマッピングの出力の枚数。表示が読んでいる画像を、流しているバッチが書かないように順に使う
const TONE_SLOTS: usize = 3;

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
    /// サンプルを積むコマンド。表示とは別のキュー（ctx.render_queue。優先度が低い）へ流し、完了を待たずに次のフレームへ進む
    /// （重いシーンでも画面と操作が止まらない）。終わるまでは累積の画像に触らない。
    /// 最後にトーンマッピングまで済ませ（tone の slot 枚目の出力）、表示はその画像を貼るので、流している間も新しい画像が出る
    render_cmd: vk::CommandBuffer,
    /// サンプルを積むバッチの完了（タイムラインセマフォ。値は流したバッチの通し番号）。表示はこれを待ってから出力を読む
    timeline: vk::Semaphore,
    /// 最後に流したバッチの番号と、そのトーンマッピングの出力
    submitted: u64,
    submitted_slot: usize,
    /// 終わった最新のバッチの番号と、そのトーンマッピングの出力（表示が貼る画像）
    completed: u64,
    shown_slot: usize,
    /// 流していて、まだ終わっていないバッチ
    in_flight: Option<Batch>,
    /// サンプルを積む GPU 時間（render_cmd の最初と最後。キューが時刻を測れなければ使わない）
    timestamps: vk::QueryPool,
    timestamps_ok: bool,
    ts_period_ms: f64,
}

struct App {
    vo: ViewOptions,
    prep: PreparedScene,
    st: ViewState,
    scene_as_configured: SceneOptions,
    window: Option<Window>,
    /// --web-ui：ボタン列とステータスバーの WebView、イベントループを起こすための口、置き場所（egui のポイント）
    web: Option<crate::web_ui::WebUi>,
    /// --web-ui：WebView を作り始めた時刻（作っている間。フレームの合間に 1 つずつ作る）
    web_started: Option<Instant>,
    /// WebView を作れなかった回数と、次に試す時刻（前に開いていた krbr の WebView2 が終わりきる前だと、作れないことがある）
    web_failures: (u32, Option<Instant>),
    /// WebView2 を作れなかった（メニュー・パネルは出せない。3D 表示に WebView2 Runtime の入れ方を出す）
    web_missing: bool,
    /// 起動中の画面（GPU・画面・基板の用意ができるまで、ウィンドウ全体に進み具合のバーと今の段階を出す）を出している
    splash: bool,
    /// 最近使った基板（メニューを開いたときに読み直す）
    recent_boards: Vec<PathBuf>,
    /// 動画の書き出し（している間）
    video: Option<video_job::VideoJob>,
    video_dialog: video_job::VideoDialog,
    /// --test-video：書き出しを終えたら閉じる
    video_test_exit: bool,
    /// WebView の操作と基板の保存の通知で、眠っているイベントループを起こす
    proxy: Option<EventLoopProxy<crate::web_ui::Wake>>,
    /// ボタン列・ステータスバー・設定パネル・警告の帯の置き場所（egui のポイント。NOTHING なら出さない）
    web_rects: [egui::Rect; 8],
    /// 「KRB Renderer について」を開いている。中身（JSON。開くたびに作り直す。ライセンスのファイルを読むので毎フレームは作らない）
    about_open: bool,
    about_json: Option<String>,
    /// --web-ui：材質エディタの窓の場所（egui のポイント。動かす・大きさを変えると変わる。None なら初めの場所）
    editor_rect: Option<egui::Rect>,
    /// --web-ui：開いているメニューのプルダウン
    web_menu: Option<WebMenu>,
    /// 最後にプルダウンを閉じたメニューと時刻（外をクリックして閉じたのが、同じ見出しのクリックだったら開き直さない）
    web_menu_closed: Option<(String, Instant)>,
    /// --web-ui：出している説明の吹き出し
    web_tip: Option<WebTip>,
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
    env_job: Option<EnvJob>,
    /// 「ファイル → 開く...」：基板を選ぶダイアログ（別スレッド）と、選んだ基板の読み込み（別スレッド）
    board_dialog: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// 読み込み中の基板。3 つ目は起動時の基板か（コマンドラインの指定をその基板に使う）
    board_job: Option<(PathBuf, mpsc::Receiver<Option<PreparedScene>>, bool)>,
    env_error: String,
    /// 保存の監視（--no-watch なら None）
    watcher: Option<FileWatcher>,
    reload_job: Option<mpsc::Receiver<ReloadDone>>,
    /// 保存による読み直しを終えた回数（読めなかったときも数える）
    saved_reloads: u32,
    /// 前回シーンを組み立てたときの、裏で分割し終えた 3D モデルの数（増えたら組み直す）
    models_seen: usize,
    /// 線フォントの文字の書き出し（kicad-cli、裏のスレッド）
    text_job: StrokeTextJob,
    /// 新しい線フォントの文字を受け取った（凹凸タイルを全部作り直す）
    text_dirty: bool,
    /// クリックで選んだ部品と、その輪郭線
    selected: Option<Selected>,
    outline: Option<Outline>,
    /// 材質の割り当てを書き換えた（材質の設定を読み直して組み直す）
    materials_dirty_files: bool,
    /// 材質の設定のファイルを、ほかから書き換えたか（手で直した設定ファイル、別に実行した krbr material set）
    material_files: MaterialFilesWatch,
    /// 選んだ部品の材質を書き込む先
    assign_scope: AssignScope,
    /// 材質ライブラリの写し（パネルと材質エディタの一覧。自分の材質を書いたらここも直す）
    library: MaterialLibrary,
    material_error: String,
    /// 材質エディタ（別の窓）と、そこからの頼みごと（次のフレームの初めに反映する）
    editor: MaterialEditor,
    editor_actions: Vec<EditorAction>,
    /// 「設定 → KiCad プラグイン」の窓
    plugin_window: PluginWindow,
    /// シーンを組み直した・材質を差し替えた回数（材質エディタの部品のプレビューを作り直す目安）
    scene_epoch: u64,
    /// 選んだ部品の輪郭線を重ねるか
    show_outline: bool,
    /// 発光の色・明るさの編集中の値（ドラッグ中に毎回書き込まないよう、止まってから書く）
    emit_edit: Option<EmitEdit>,
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
    /// S キーの保存の次の番号の候補（実際の番号は、既存のファイルと重ならないところまで進める）
    save_index: u32,
    /// 今の累積の計測：サンプルを描いたフレームの時間の合計（一時停止・完了の間は増えない）、GPU がサンプルを描いた時間の合計、そのサンプル数
    accum_wall: Duration,
    accum_gpu_ms: f64,
    accum_measured: u32,
    /// 実効のサンプルの速さを測る区間の始まりと、そのときのサンプル数（log_throughput）
    rate_since: Instant,
    rate_samples: u32,
    last_change_at: Instant,
    last_frame_ms: f64,
    /// 最後に表示した時刻と、表示の間隔の下限（画面の書き換えの間隔から。1 秒ごとに確かめ直す）
    last_present_at: Instant,
    frame_interval: Duration,
    refresh_checked: Instant,
    last_gpu_ms: f64,
    /// パネルの設定：最後に見た値、最後に覚えた（<名前>.krbr に書いた）値、最後に変わった時刻。変わってから 1 秒たったら書く
    view_seen: remember::ViewValues,
    view_saved: remember::ViewValues,
    view_changed_at: Instant,
    /// ウィンドウの状態（基板によらず 1 つ）：起動時に読んだもの・最後に見たもの・最後に覚えたもの・最後に変わった時刻
    win_start: remember::WindowState,
    win_seen: remember::WindowState,
    win_saved: remember::WindowState,
    win_changed_at: Instant,
    /// 設定パネルの今の幅（egui のポイント）
    panel_width: f32,
    /// 設定パネルのグループ・詳細の開閉（開閉したものだけ。ほかは既定。ウィンドウの状態と一緒に覚える）
    panel_open: std::collections::BTreeMap<String, bool>,
    /// 決まったサイズに合わせてウィンドウの大きさを変えている途中：あと何フレーム合わせ直すか
    fit_frames: u32,
    /// 表示の言語（決まった言語タグ）と、メニューで選んだもの（None：自動）
    language: String,
    language_choice: Option<String>,
    out_dir: PathBuf,
    stem: String,
    /// 開発用のテストの操作
    test: TestHooks,
    rc: i32,
    done: bool,
}

/// egui の UI を描いた結果（テクスチャの更新、描くもの、3D 表示の領域）
struct UiFrame {
    textures: egui::TexturesDelta,
    primitives: Vec<egui::ClippedPrimitive>,
    pixels_per_point: f32,
    central: egui::Rect,
}

/// 表示したフレームの結果
struct Presented {
    /// ノイズ除去した画像を表示した
    denoised: bool,
    /// テストの最後のフレームで読み戻した、表示した画像（スワップチェーンの画像）
    screenshot: Option<Buffer>,
    extent: vk::Extent2D,
}

/// S キーで保存する 1 回分のファイル
struct SavePaths {
    png: PathBuf,
    exr: PathBuf,
    denoised_png: PathBuf,
    denoised_exr: PathBuf,
}

impl SavePaths {
    fn any_exists(&self) -> bool {
        [&self.png, &self.exr, &self.denoised_png, &self.denoised_exr].iter().any(|p| p.exists())
    }
}

/// 表示している画像（リニア sRGB）を読み戻す：denoised ならノイズ除去して元の画像と混ぜたもの、それ以外は累積。GPU が待機中に呼ぶこと
fn read_image(g: &Gpu, denoised: bool) -> Result<ImageF, String> {
    match g.denoiser.as_ref().filter(|_| denoised) {
        Some(d) => {
            let mut img = d.read_output()?;
            if g.prog.xyz_output() {
                krbr_render::integrator::xyz_to_linear_srgb(&mut img);
            }
            // 背景が透明なら、累積のアルファを付ける（アルファはノイズが少ないので、ノイズ除去しない）
            if g.prog.frame().transparent_background {
                img.copy_alpha_from(&g.prog.readback()?.color);
            }
            Ok(img)
        }
        None => Ok(g.prog.readback()?.color),
    }
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
/// 実効のサンプルの速さをログに出す間隔（log_throughput）
const RATE_WINDOW: Duration = Duration::from_secs(10);

/// 決まったサイズに合わせてウィンドウの大きさを変えるとき、合わせ直す最大のフレーム数。大きさを変えると、メニューの折り返しや
/// パネル・ステータスバーの大きさも変わり、1 回では 3D 表示の領域が目標にならないことがある。目標に達したらそこで止める（通常 1〜2 フレーム）
const FIT_RETRY_FRAMES: u32 = 6;
/// 画面のうちウィンドウに使ってよい割合（幅, 高さ）。winit は作業領域（タスクバーを除いた領域）を返さないので、
/// 画面全体からの割合で近似する（下端のタスクバー 40〜48 px と、上下左右の余白を見込む）
const MONITOR_USABLE_FRACTION: (f32, f32) = (0.96, 0.92);
/// 前回の位置を使うのは、ウィンドウの左上からこの点（タイトルバーの左寄り）が、今つながっている画面に入っているとき
/// （画面を外した後に、つかんで動かせない所へ出さない）
const TITLE_BAR_PROBE: (i32, i32) = (40, 10);
/// 覚えた大きさが無いときのウィンドウの中の大きさ（物理ピクセル）と、設定パネルの幅（egui のポイント）
const DEFAULT_WINDOW_SIZE: (u32, u32) = (1600, 900);
const DEFAULT_PANEL_WIDTH: f32 = 340.0;
/// 設定パネルで初めは畳んでおく「詳細」（羽根・周辺減光・収差、グローのしきい値と広がり）
const DEFAULT_CLOSED_SECTIONS: [&str; 2] = ["lens.details", "finish.glow"];

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
        c.transparent_background = st.transparent;
        c.bump_intensity = st.bump_intensity;
        c.fov_deg = st.fov;
        c.elevation_deg = st.elevation;
        c.azimuth_deg = st.azimuth;
        c.render_size = st.size_mode;
        let mut v = c.values();
        remember::set_camera(&mut v, st.target, st.distance);
        remember::ViewerValues {
            denoise: Some(st.denoise),
            denoise_strength: Some(st.denoise_strength),
            max_spp: Some(st.max_spp),
            display_mode: Some(display_mode_name(st.mode).into()),
            render_ratio: Some(st.render_ratio),
            show_smd: Some(st.show_smd),
            show_tht: Some(st.show_tht),
            show_solder: Some(st.show_solder),
            focus_point: st.focus_point.map(|p| [p.x as f64, p.y as f64, p.z as f64]),
        }
        .write(&mut v);
        v
    }

    /// 決まったサイズのとき、3D 表示の領域が「描画のサイズ ÷ 比率」になるようにウィンドウの大きさを変える。
    /// 画面に入らなければ、入るまで比率を上げる。メニュー・パネル・ステータスバーの大きさが変わることがあるので、
    /// 目標に達するまで（最大 FIT_RETRY_FRAMES フレーム）合わせ直す
    fn fit_window(&mut self) {
        let Some((w, h)) = self.st.size_mode else {
            self.st.req.fit_window = false;
            self.fit_frames = 0;
            return;
        };
        let Some(window) = self.window.as_ref() else { return };
        let inner = window.inner_size();
        let vp = self.st.viewport;
        // 領域以外（メニュー・パネル・ステータスバー）の大きさ
        let (extra_w, extra_h) = (inner.width.saturating_sub(vp.w), inner.height.saturating_sub(vp.h));
        if self.st.req.fit_window {
            self.st.req.fit_window = false;
            if window.is_maximized() {
                window.set_maximized(false);
            }
            // 画面に入るまで比率を上げる（枠の分も見込む）
            if let Some(m) = window.current_monitor() {
                let outer = window.outer_size();
                let (deco_w, deco_h) = (outer.width.saturating_sub(inner.width), outer.height.saturating_sub(inner.height));
                let (mw, mh) = (m.size().width as f32 * MONITOR_USABLE_FRACTION.0, m.size().height as f32 * MONITOR_USABLE_FRACTION.1);
                let fits = |r: f32| (w as f32 / r) + (extra_w + deco_w) as f32 <= mw && (h as f32 / r) + (extra_h + deco_h) as f32 <= mh;
                krbr_core::debug!("fit window: monitor {}x{}, inner {}x{}, outer {}x{}, 3D view {}x{}", m.size().width, m.size().height, inner.width, inner.height, outer.width, outer.height, vp.w, vp.h);
                let cur = self.st.render_ratio;
                if !fits(cur) {
                    // 候補の中で入る最小の比率。どれも入らなければ候補の最大（今の比率がそれより大きければ今のまま。比率を下げると大きくなる）
                    let r = RATIOS.iter().copied().find(|&r| r > cur && fits(r)).unwrap_or(RATIOS[RATIOS.len() - 1].max(cur));
                    self.last_update = fl!("fit-too-large", size = format!("{w}×{h}"), ratio = r.to_string(), percent = format!("{:.0}", 100.0 / r));
                    self.st.render_ratio = r;
                }
            }
            self.fit_frames = FIT_RETRY_FRAMES;
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
        Some(self.with_panel_state(w))
    }

    /// ウィンドウの状態のうち、設定パネルのもの（表示・幅・グループの開閉）を今の値にする
    fn with_panel_state(&self, mut w: remember::WindowState) -> remember::WindowState {
        w.panel_shown = self.st.show_panel;
        w.sections = self.panel_open.clone();
        if self.st.show_panel {
            w.panel_width = Some(self.panel_width.round());
        }
        w
    }

    /// パネルの設定とウィンドウの状態が変わっていたら覚える（最後に変わってから 1 秒たったら。now なら待たずに）
    fn save_view(&mut self, now: bool) {
        // ウィンドウ（表示しないテストのときは覚えない）
        if !(self.vo.hidden || self.test.active()) {
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
        if cur != self.view_seen {
            self.view_seen = cur;
            self.view_changed_at = Instant::now();
        }
        // テストでは基板の設定ファイルに書かない（後の krbr render が、テストの操作の値で描くことになる）
        if self.has_board() && !self.test.active() && self.view_seen != self.view_saved && (now || self.view_changed_at.elapsed() >= Duration::from_secs(1)) {
            remember::remember_view(&self.prep.board.file, &self.view_seen, &self.prep.config_view);
            self.view_saved = self.view_seen.clone();
        }
    }

    /// パネルの設定をすべて既定（設定ファイルとコマンドライン）に戻し、この基板について覚えたものを忘れる
    fn reset_all_settings(&mut self) {
        if self.has_board() && !self.test.active() {
            remember::forget_board(&self.prep.board.file);
        }
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
        st.transparent = c.transparent_background;
        // ビューアだけの項目は、起動時と同じ決め方（コマンドライン、既定）で。覚えた値は使わない
        ViewerSettings::new(&self.vo, &remember::ViewValues::new()).apply(st);
        st.mask_choice = mask_choice_of(&c.scene);
        st.changed.materials = true;
        st.changed.frame = true;
        if self.prep.cfg.integrator != c.integrator {
            self.prep.cfg.integrator = c.integrator;
            st.changed.integrator = true;
        }
        self.prep.cfg.lens = c.lens.clone();
        self.apply_mask_choice();
        if self.prep.cfg.scene.substrate != c.scene.substrate {
            self.prep.cfg.scene.substrate = c.scene.substrate.clone();
            update_substrate_material(&mut self.prep.scene, &self.prep.cfg.scene, &self.library);
        }
        // 視点（自動の構図）は描画サイズの縦横比で決まるので、戻したサイズで表示の矩形を決め直した後
        // （apply_view_requests）に合わせる。覚える値の基準もそこで取り直す
        self.st.req.reset_view = true;
        self.st.req.rebase_view = true;
        // 環境マップ：--hdr、設定ファイルの [environment] hdr、内蔵のスタジオ（読み終えても覚えない）
        let hdr = if !self.vo.common.hdr.is_empty() {
            remember::normalize_hdr(&self.vo.common.hdr)
        } else if !self.prep.config_hdr.is_empty() {
            remember::normalize_hdr(&self.prep.config_hdr)
        } else {
            STUDIO.to_string()
        };
        if self.env_job.is_none() && !remember::same_hdr(&hdr, &self.env_path) {
            self.start_env_load(hdr, false);
        }
        self.last_update = fl!("reset-all-done");
        info!("panel settings reset to the configuration files");
    }

    /// パネルで選んだレジストの色を、設定とシーンの材質に反映する（GPU へは changed.materials で送る）
    fn apply_mask_choice(&mut self) {
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
                let name = mask_presets()[i].name;
                if let Err(e) = apply_mask_color_spec(&mut self.prep.cfg.scene, name) {
                    warn!("solder mask {name}: {e}");
                }
            }
        }
        update_mask_material(&mut self.prep.scene, &self.prep.board, &self.prep.cfg.scene);
        self.st.changed.materials = true;
    }

    /// 設定ファイル・コマンドラインのピントの距離（mm）を、今の視線の上の点にする（覚えたピントの点が無いとき）
    fn adopt_configured_focus(&mut self) {
        let d = self.prep.cfg.lens.focus_distance_mm;
        if self.st.focus_point.is_none() && d > 0.0 {
            let c = self.view_camera();
            self.st.focus_point = Some(c.position + c.forward * d);
        }
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
        self.st.changed.frame = true;
        self.adopt_configured_focus();
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
        self.st.changed.frame = true;
        // ピントも設定ファイル・コマンドラインのもの（無ければ注視点）に戻す
        self.st.focus_point = None;
        self.prep.cfg.lens.focus_distance_mm = self.prep.configured.lens.focus_distance_mm;
        self.adopt_configured_focus();
    }

    /// 仰角・方位を決めて、基板全体が入る構図にする（ボタン列の「真上から」「真裏から」。画角は今のまま）
    fn preset_camera(&mut self, elevation: f64, azimuth: f64, w: u32, h: u32) {
        let mut cfg = self.prep.configured.clone();
        cfg.elevation_deg = elevation;
        cfg.azimuth_deg = azimuth;
        cfg.fov_deg = self.st.fov;
        // コマンドラインの注視点・拡大率は使わない（基板全体を入れる）
        let c = framed_camera(&cfg, &self.prep, &RenderOptions::default(), w, h);
        self.st.target = c.target;
        self.st.distance = c.distance;
        self.st.elevation = elevation;
        self.st.azimuth = azimuth;
        self.st.tan_half_fov_y = c.tan_half_fov_y;
        self.st.changed.frame = true;
        // 収め直した基板にピントが合うように、ピントは注視点（自動）に戻す
        self.st.focus_point = None;
    }

    fn current_frame(&mut self, w: u32, h: u32) -> krbr_render::FrameParams {
        let mut c = self.prep.cfg.clone();
        c.env_rotation_deg = self.st.env_rotation_deg;
        c.env_intensity = self.st.env_intensity;
        c.spectral = self.st.spectral;
        // MP4 は透明にできないので、動画の書き出しの間は背景を描く
        c.transparent_background = self.st.transparent && self.video.is_none();
        let cam = orbit_camera(self.st.target, self.st.distance, self.st.elevation, self.st.azimuth, self.st.fov, w as f32 / h.max(1) as f32);
        self.st.tan_half_fov_y = cam.tan_half_fov_y;
        // ピント：決めた点があれば、今のカメラからその点までの距離（カメラを動かしても合わせ続ける）。無ければ注視点（0 = 自動）
        let focus = self.st.focus_point.map(|p| focus_distance_to(p, &cam)).unwrap_or(0.0);
        self.prep.cfg.lens.focus_distance_mm = focus;
        c.lens.focus_distance_mm = focus;
        frame_params(&c, cam, self.vo.common.seed)
    }

    /// 環境マップを裏のスレッドで読み込む。remember：読み終えたら、この基板で使ったものとして覚える
    fn start_env_load(&mut self, path: String, remember: bool) {
        if self.env_job.is_some() || remember::same_hdr(&path, &self.env_path) {
            return;
        }
        info!("loading environment {}", path);
        let (tx, rx) = mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || {
            krbr_core::platform::lower_thread_priority();
            let _ = tx.send(load_environment(p));
        });
        self.env_job = Some(EnvJob { path, rx, remember });
        self.env_error.clear();
    }

    fn init_gpu(&mut self, el: &ActiveEventLoop) -> Result<(), String> {
        let hidden = self.vo.hidden || self.test.active();
        let sized = self.vo.common.width.is_some() || self.vo.common.height.is_some();
        let saved = if hidden || sized { remember::WindowState::default() } else { self.win_start.clone() };
        let (sw, sh) = saved.size.unwrap_or(DEFAULT_WINDOW_SIZE);
        let (w, h) = (self.vo.common.width.unwrap_or(sw), self.vo.common.height.unwrap_or(sh));
        // ウィンドウのアイコン（exe に埋め込んだものと同じ絵。scripts/make_icon.py で作る 64x64 の RGBA）
        let icon = winit::window::Icon::from_rgba(include_bytes!("../../../krbr/assets/krbr_64.rgba").to_vec(), 64, 64).ok();
        let attrs = Window::default_attributes().with_window_icon(icon).with_title(self.base_title.clone()).with_inner_size(winit::dpi::PhysicalSize::new(w, h)).with_visible(!hidden).with_active(!hidden).with_maximized(saved.maximized);
        // 子ウィンドウ（WebView）の下にも 3D 表示を描く（WS_CLIPCHILDREN を外す）。外さないと WebView の下は描かれず、
        // メニュー・説明の吹き出しが消えた瞬間、次のフレームが出るまでそこが白く光る
        #[cfg(windows)]
        let attrs = {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs.with_clip_children(false)
        };
        // 前回の位置：今つながっている画面のどれかに、タイトルバーの左寄りの点が入っているときだけ
        let attrs = match saved.position {
            Some((x, y)) if el.available_monitors().any(|m| {
                let (p, s) = (m.position(), m.size());
                let (px, py) = (x + TITLE_BAR_PROBE.0, y + TITLE_BAR_PROBE.1);
                px >= p.x && py >= p.y && px < p.x + s.width as i32 && py < p.y + s.height as i32
            }) => attrs.with_position(winit::dpi::PhysicalPosition::new(x, y)),
            _ => attrs,
        };
        let window = el.create_window(attrs).map_err(|e| format!("window: {e}"))?;
        // GPU の準備ができて最初のフレームを出すまで（1 秒ほど）、白い窓を見せない
        if !hidden {
            if let Ok(raw_window_handle::RawWindowHandle::Win32(h)) = window.window_handle().map(|h| h.as_raw()) {
                krbr_core::platform::paint_placeholder(h.hwnd.get());
            }
        }
        let display = window.display_handle().map_err(|e| format!("{e}"))?.as_raw();
        let wh = window.window_handle().map_err(|e| format!("{e}"))?.as_raw();
        let exts = ash_window::enumerate_required_extensions(display).map_err(|e| format!("{e:?}"))?.to_vec();
        let factory: krbr_render::vk::SurfaceFactory = Box::new(move |entry, instance| unsafe { ash_window::create_surface(entry, instance, display, wh, None) }.map_err(|e| format!("surface: {e:?}")));
        let ctx: &'static Context = Box::leak(Box::new(Context::new(&ContextOptions { validation: self.vo.common.validate, low_queue_priority: true, instance_extensions: exts, create_surface: Some(factory), ..Default::default() })?));
        // 積分器が GpuScene への参照を持つので、ムーブしてもアドレスが変わらないよう Box に入れる
        let mut gpu = Box::new(GpuScene::new(ctx));
        gpu.build(&self.prep.scene, &self.prep.env, &self.prep.dist, self.st.bump_intensity).map_err(|e| format!("GPU scene: {e}"))?;
        self.prep.env.rgba = Vec::new();
        if !self.prep.scene.missing.is_empty() {
            warn!("{} 3D model(s) missing or unreadable (see missing_models.txt of a render)", self.prep.scene.missing.len());
        }
        let size = window.inner_size();
        let (fbw, fbh) = (size.width.max(1), size.height.max(1));
        self.start_camera(fbw, fbh);
        // 3D 表示の大きさはメニューとパネルを置いてから決まるので、最初のフレームで視点を合わせ直す（覚えた視点があればそれ）
        self.st.req.start_view = true;
        let swap = Swapchain::new(ctx, fbw, fbh)?;
        let frame = self.current_frame(swap.extent.width, swap.extent.height);
        // 安全性：Box の中身はムーブされず、Gpu を捨てるときに prog を gpu より先に捨てる（run_viewer）
        let gpu_ref: &'static GpuScene<'static> = unsafe { &*(&*gpu as *const GpuScene<'static>) };
        let prog = progressive_or_pt(ctx, gpu_ref, self.prep.cfg.integrator, swap.extent.width, swap.extent.height, frame)?;
        self.prep.cfg.integrator = prog.integrator_kind(); // PT に代えたら、パネルにもそう出す
        let tone = Tonemapper::with_outputs(ctx, prog.targets(), TONE_SLOTS)?;
        let egui_renderer = EguiRenderer::with_gpu_allocator(ctx.allocator.clone(), ctx.device.clone(), RenderMode::DynamicRendering(DynamicRendering { color_attachment_format: swap.format, depth_attachment_format: None, stencil_attachment_format: None }), EguiOptions { in_flight_frames: 1, srgb_framebuffer: false, ..Default::default() })
            .map_err(|e| format!("egui renderer: {e}"))?;
        let dev = &ctx.device;
        let cmd = unsafe { dev.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(ctx.pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1)) }.map_err(|e| format!("{e:?}"))?[0];
        let acquired = unsafe { dev.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }.map_err(|e| format!("{e:?}"))?;
        let fence = unsafe { dev.create_fence(&vk::FenceCreateInfo::default(), None) }.map_err(|e| format!("{e:?}"))?;
        let render_cmd = unsafe { dev.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(ctx.render_pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1)) }.map_err(|e| format!("{e:?}"))?[0];
        let mut timeline_type = vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE).initial_value(0);
        let timeline = unsafe { dev.create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut timeline_type), None) }.map_err(|e| format!("{e:?}"))?;
        let timestamps = unsafe { dev.create_query_pool(&vk::QueryPoolCreateInfo::default().query_type(vk::QueryType::TIMESTAMP).query_count(2), None) }.map_err(|e| format!("{e:?}"))?;
        let props = unsafe { ctx.instance.get_physical_device_properties(ctx.physical) };
        let ts_period_ms = props.limits.timestamp_period as f64 * 1e-6;
        let qprops = unsafe { ctx.instance.get_physical_device_queue_family_properties(ctx.physical) };
        let timestamps_ok = qprops.get(ctx.render_family as usize).is_some_and(|q| q.timestamp_valid_bits > 0);
        let keys: Vec<String> = KEY_BINDINGS.iter().map(|b| format!("{}={}", b.label, b.log)).collect();
        info!("viewer: {}x{}, up to {} spp. mouse: left drag=rotate, click=select a part, Ctrl+click=focus, right/middle drag=pan, wheel=zoom; keys: {}", swap.extent.width, swap.extent.height, self.st.max_spp, keys.join(", "));
        install_fonts(&self.egui_ctx, &self.language);
        self.egui_state = Some(EguiState::new(self.egui_ctx.clone(), egui::ViewportId::ROOT, &window, Some(window.scale_factor() as f32), None, None));
        self.gpu = Some(Gpu { ctx, gpu, swap, prog, tone, denoiser: None, tone_denoised: None, egui_renderer, cmd, acquired, fence, render_cmd, timeline, submitted: 0, submitted_slot: 0, completed: 0, shown_slot: 0, in_flight: None, timestamps, timestamps_ok, ts_period_ms });
        let hidden = self.vo.hidden || self.test.active();
        if let (true, false, Some(proxy)) = (self.vo.web_ui, hidden, self.proxy.clone()) {
            // WebView を作るのは最初のフレームを出してから（step_web_ui）
            let w = crate::web_ui::WebUi::start(proxy);
            web_about::publish_icon(w.images());
            self.web = Some(w);
            self.web_started = Some(Instant::now());
        }
        self.window = Some(window);
        Ok(())
    }

    /// 保存（または裏での 3D モデルの分割の完了）を見つけたら、裏のスレッドで基板を読み直す（C++ 版 viewer の reloadJob）
    fn start_reload_if_needed(&mut self) {
        if self.reload_job.is_some() {
            return;
        }
        let Some(src) = &self.prep.source else { return };
        // 保存を待たずに読み直す要求（テスト）も、保存と同じに扱う
        let saved = self.watcher.as_mut().is_some_and(|w| w.poll()) | std::mem::take(&mut self.st.req.reload);
        let models_done = src.library.completed();
        if !self.materials_dirty_files && self.material_files.poll(&self.prep.board.file, &src.materials).is_some() {
            info!("material settings changed on disk; rebuilding the scene");
            self.materials_dirty_files = true;
        }
        let text = self.text_dirty;
        let materials = self.materials_dirty_files;
        if !saved && !text && !materials && models_done == self.models_seen {
            return;
        }
        self.models_seen = models_done;
        self.text_dirty = false;
        self.materials_dirty_files = false;
        let mut source = self.prep.source.take().unwrap();
        let old = self.prep.board.clone();
        let options = self.prep.cfg.scene.clone();
        let saved_at = if saved { self.watcher.as_ref().and_then(|w| w.saved_at()) } else { None };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            krbr_core::platform::lower_thread_priority(); // 読み直しは低優先度（ウィンドウのスレッドは通常のまま）
            if text {
                source.tiles.invalidate_all(); // シルクが変わったので、どのタイルも作り直す
            }
            if materials {
                source.materials = load_material_setup(&old.file); // 材質の割り当て（<名前>.krbr / model_map.toml）を書き換えた
            }
            let result = if saved { source.reload(&old, &options).map(|(b, s, st)| (Some(b), s, st)) } else { Ok((None, source.build(&old, &options, true), ReloadStats::default())) };
            let _ = tx.send(ReloadDone { source, saved, result, saved_at, text, materials });
        });
        self.reload_job = Some(rx);
    }

    /// ノイズ除去を用意する（デバイスの準備に時間がかかるので、有効にしたときに初めて行う。失敗したら再び試さない）
    fn init_denoiser(&mut self) {
        self.denoiser_tried = true;
        let g = self.gpu.as_mut().unwrap();
        g.ctx.wait_idle();
        let t = g.prog.targets();
        let made = Denoiser::new(g.ctx, t, DenoiseQuality::Fast).and_then(|d| Tonemapper::with_color(g.ctx, d.output().view, t).map(|tm| (d, tm)));
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

    /// 3D 表示のカメラ（画像を置いた矩形 display の縦横比）。クリック・ピント合わせ・輪郭線はこれで換算する
    fn view_camera(&self) -> Camera {
        let vp = self.st.display;
        orbit_camera(self.st.target, self.st.distance, self.st.elevation, self.st.azimuth, self.st.fov, vp.w as f32 / vp.h.max(1) as f32)
    }

    /// 基板が画面に写っているか（見失ったときに「全体を表示」を案内する）
    fn board_visibility(&self) -> BoardInView {
        let pts = &self.prep.scene.framing_points;
        if pts.is_empty() {
            return BoardInView::Visible;
        }
        let cam = self.view_camera();
        if points_out_of_view(&cam, pts) {
            BoardInView::Outside
        } else if projected_extent(&cam, pts) < TOO_SMALL_EXTENT {
            BoardInView::TooSmall
        } else {
            BoardInView::Visible
        }
    }

    /// 画面の点（物理ピクセル）を通る視線：カメラと方向
    fn view_ray(&self, p: (f64, f64)) -> (Camera, Vec3) {
        let vp = self.st.display;
        let c = self.view_camera();
        let dir = unproject(&c, (p.0 - vp.x as f64) as f32, (p.1 - vp.y as f64) as f32, vp.w.max(1) as f32, vp.h.max(1) as f32);
        (c, dir)
    }

    /// 画面の点（物理ピクセル）の部品を選ぶ。基板や何も無い所なら選択を外す
    fn pick_part(&mut self, p: (f64, f64)) {
        self.flush_emit_edit(true);
        self.emit_edit = None;
        let (c, dir) = self.view_ray(p);
        let hit = raycast_pick_visible(&self.prep.scene, c.position, dir, self.st.visible_mask());
        self.selected = hit.and_then(|h| Selected::from_instance(&self.prep.scene, &self.prep.board, h.instance));
        self.material_error.clear();
        self.refresh_outline();
        match &self.selected {
            Some(s) => info!("selected {}{}", s.reference, if s.model.is_none() { " (placeholder)" } else { "" }),
            None => info!("selection cleared"),
        }
    }

    /// Ctrl + クリック：画面の点（物理ピクセル）にある面にピントを合わせる（ピンホールなら薄レンズにする）
    fn focus_at(&mut self, p: (f64, f64)) {
        let (c, dir) = self.view_ray(p);
        if let Some(t) = raycast_pick_visible(&self.prep.scene, c.position, dir, self.st.visible_mask()).map(|h| h.t) {
            let point = c.position + dir * t;
            self.st.focus_point = Some(point);
            if self.prep.cfg.lens.model == LensModel::Pinhole {
                self.prep.cfg.lens.model = LensModel::Thin;
            }
            self.last_update = fl!("focus-set", mm = format_length(focus_distance_to(point, &c) as f64));
            self.st.changed.frame = true;
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
        if !self.show_outline || self.prep.cfg.lens.model == LensModel::Realistic || !central.is_positive() {
            return;
        }
        let vp = self.st.display;
        let ppp = root.ctx().pixels_per_point();
        let c = self.view_camera();
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
        self.assign_material_to(&sel, part, material, clear);
    }

    /// 部品 sel の部位（空なら全部位）に材質を書き込む。clear なら上書きを消す
    fn assign_material_to(&mut self, sel: &Selected, part: &str, material: &str, clear: bool) {
        // 代わりの箱にはモデルが無いので、部品番号に書く
        let scope = if sel.model.is_none() { AssignScope::Part } else { self.assign_scope };
        let model_spec = sel.model.and_then(|mi| self.prep.board.footprints.iter().find(|f| f.reference == sel.reference).and_then(|f| f.models.get(mi)).map(|m| m.path.clone()));
        let by_ref = scope == AssignScope::Part;
        let o = MaterialSetOptions {
            pcb: self.prep.board.file.clone(),
            reference: if by_ref { sel.reference.clone() } else { String::new() },
            model: if by_ref { String::new() } else { model_spec.unwrap_or_default() },
            part: part.to_string(),
            material: material.to_string(),
            global: scope == AssignScope::ModelGlobal,
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

    /// 選んだ部品の、材質エディタに見せるもの（部品のインスタンスすべてと、選んだ 3D モデルの部位）
    fn part_info(&self) -> Option<PartInfo> {
        let sel = self.selected.as_ref()?;
        let scene = &self.prep.scene;
        let fp = self.prep.board.footprints.iter().position(|f| f.reference == sel.reference)? as u32;
        let instances: Vec<usize> = scene
            .instances
            .iter()
            .enumerate()
            .filter(|(_, i)| match i.owner {
                InstanceOwner::Model { footprint, .. } | InstanceOwner::Placeholder { footprint } => footprint == fp,
                InstanceOwner::Board | InstanceOwner::Solder { .. } => false,
            })
            .map(|(k, _)| k)
            .collect();
        let mut rows = Vec::new();
        if let Some(i) = sel.find_instance(scene, &self.prep.board) {
            let inst = &scene.instances[i];
            if let InstanceOwner::Model { assignments, .. } = inst.owner {
                let a = &scene.assignments;
                let start = assignments as usize;
                if let Some(first) = a.get(start) {
                    for (k, r) in a[start..].iter().take_while(|r| r.reference == sel.reference && r.model_spec == first.model_spec).enumerate() {
                        rows.push(PartRow { key: r.part_key.clone(), material: r.material.clone(), scene_material: inst.material_base + k as u32 });
                    }
                }
            }
        }
        Some(PartInfo { reference: sel.reference.clone(), model: sel.model, instances, rows })
    }

    /// 書き込むのを待っている編集（材質エディタの調整・自分の材質、設定パネルの発光）を今すぐ書く。
    /// ビューアを終えるとき・別の基板に切り替えるとき（待っている間に終えると、編集が失われていた）
    fn flush_pending_edits(&mut self) {
        let info = self.part_info();
        let actions = self.editor.flush_now(info.as_ref());
        self.editor_actions.extend(actions);
        self.apply_editor_actions();
        self.flush_emit_edit(true);
    }

    /// 材質エディタの頼みごと（割り当て、自分の材質の保存・削除）
    fn apply_editor_actions(&mut self) {
        for a in std::mem::take(&mut self.editor_actions) {
            match a {
                EditorAction::Assign { part, material } => {
                    // 設定パネルで発光を編集中なら先に書く（同じ部位の書き込みが前後しないように）
                    self.flush_emit_edit(true);
                    self.emit_edit = None;
                    self.assign_material(&part, &material, false);
                }
                EditorAction::AssignTo { reference, model, part, material } => {
                    self.flush_emit_edit(true);
                    self.emit_edit = None;
                    self.assign_material_to(&Selected { reference, model }, &part, &material, false);
                }
                EditorAction::SaveUser { name, material, label } => self.save_user_material(&name, Some((*material, label))),
                EditorAction::DeleteUser(name) => self.save_user_material(&name, None),
            }
        }
    }

    /// 自分の材質（name は ID、label は表示名）を全体設定の materials/user.toml に書き（None なら消し）、
    /// 材質ライブラリとシーンにも反映する
    fn save_user_material(&mut self, name: &str, m: Option<(krbr_scene::Material, String)>) {
        let dir = krbr_core::platform::config_dir();
        // 消せるのは材質エディタのファイル（materials/user.toml）の材質だけ。ほかのファイルの材質は、user.toml から消しても
        // 次に起動したときに戻ってしまう
        if m.is_none() {
            let source = self.library.source_of(name).to_string();
            let from_editor = Path::new(&source).file_name().is_some_and(|f| f == krbr_scene::material_library::USER_MATERIALS_FILE);
            if !source.is_empty() && !from_editor {
                self.last_update = fl!("mated-delete-other-file", name = material_editor::material_label(&self.library, name), file = source);
                return;
            }
        }
        let shown = match &m {
            Some((_, label)) => label.clone(),
            None => material_editor::material_label(&self.library, name),
        };
        let file = match save_user_material(&dir, name, m.as_ref().map(|(m, l)| (m, l.as_str()))) {
            Ok(f) => f,
            Err(e) => {
                warn!("material {name}: {e}");
                self.last_update = fl!("mated-save-failed", error = e);
                return;
            }
        };
        let source = file.to_string_lossy().into_owned();
        match &m {
            Some((m, label)) => {
                self.library.insert(name, m.clone(), &source);
                self.library.set_label(name, label);
                info!("material {name} ({label}): saved to {source}");
                self.last_update = fl!("mated-saved", name = shown.as_str(), file = source.as_str());
            }
            None => {
                self.library.remove(name);
                info!("material {name}: removed from {source}");
                self.last_update = fl!("mated-deleted", name = shown.as_str());
                // この材質への割り当て（この基板と全体の対応表）も外す
                if self.has_board() {
                    match forget_material(&self.prep.board.file, name) {
                        Ok(files) => files.iter().for_each(|f| info!("material {name}: assignments removed from {}", f.display())),
                        Err(e) => warn!("material {name}: {e}"),
                    }
                }
            }
        }
        // 組み立ての状態のライブラリも直す（読み直し中なら、終わってからファイルを読み直す）
        match self.prep.source.as_mut() {
            Some(src) => match &m {
                Some((m, label)) => {
                    src.materials.library.insert(name, m.clone(), &source);
                    src.materials.library.set_label(name, label);
                }
                None => src.materials.library.remove(name),
            },
            None => self.materials_dirty_files = true,
        }
        if m.is_none() || !self.patch_scene_materials(name) {
            self.materials_dirty_files = true; // 部位の材質を決め直す（消した材質・モデルの色の使い方を変えた材質）
        }
    }

    /// シーンの中の、自分の材質 name を使う材質を、ライブラリの新しい値に差し替える（形状は組み直さない）。
    /// 組み直しが要る変更（3D モデルの色を使うかどうか、発光）なら false
    fn patch_scene_materials(&mut self, name: &str) -> bool {
        let mut changed = false;
        for m in &mut self.prep.scene.materials {
            // モデルの材質の名前は "<モデルのファイル>:<割り当てた材質名>"
            let Some((_, assigned)) = m.name.split_once(':') else { continue };
            if Variant::parse(assigned).0 != name {
                continue;
            }
            let Some(mut n) = self.library.find_variant(assigned) else { return false };
            if n.emission_luminance != m.emission_luminance || n.emission_color != m.emission_color {
                return false;
            }
            n.name = m.name.clone();
            *m = n;
            changed = true;
        }
        if changed {
            self.st.changed.materials = true;
            self.scene_epoch += 1;
        }
        true
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

    /// 書き出しが終わっていれば、新しいシルクを設定に入れる（次の組み立てでタイルを全部作り直す）
    fn poll_silk(&mut self) {
        if let Some(plot) = self.text_job.poll() {
            self.prep.cfg.scene.layer_art.stroke_text = plot.side;
            self.text_dirty = true;
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
        let old = std::mem::replace(&mut self.library, d.source.materials.library.clone());
        self.editor.library_reloaded(&old, &self.library);
        self.prep.source = Some(d.source);
        if d.saved {
            self.saved_reloads += 1;
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
                if stats.diff.stroke_text_changed {
                    self.text_job.request(); // 書き出し中なら、終わった後にもう一度
                }
                // 読み直しの間に変えたレジストの色も反映する
                update_mask_material(&mut scene, &self.prep.board, &self.prep.cfg.scene);
                self.prep.scene = scene;
                self.scene_epoch += 1;
                self.refresh_outline();
                let t0 = Instant::now();
                let g = self.gpu.as_mut().unwrap();
                // 送れなければ（VRAM が足りないなど）GPU の形状は作りかけになり描けないので、ビューアを終える（エラーを出して）
                g.gpu.rebuild_geometry(&self.prep.scene, self.st.bump_intensity).map_err(|e| format!("cannot send the reloaded board to the GPU: {e}"))?;
                g.prog.rebuild_integrator()?;
                let gpu_ms = t0.elapsed().as_secs_f64() * 1000.0;
                self.st.changed.frame = true;
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
                } else if d.text {
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

    /// 基板を開いているか（基板なしで起動して、まだ開いていなければ false）
    fn has_board(&self) -> bool {
        !self.prep.board.file.as_os_str().is_empty()
    }

    /// 「ファイル → 開く...」：基板を選ぶダイアログを別スレッドで開く（その間も描画を続ける）
    fn start_board_dialog(&mut self) {
        if self.board_dialog.is_some() || self.board_job.is_some() {
            return;
        }
        let mut dlg = rfd::FileDialog::new().set_title(fl!("dlg-open-board-title")).add_filter(fl!("dlg-board-filter"), &["kicad_pcb"]);
        let near = if self.has_board() { Some(self.prep.board.file.clone()) } else { remember::last_board() };
        if let Some(dir) = near.as_deref().and_then(|p| p.parent()).filter(|d| d.is_dir()) {
            dlg = dlg.set_directory(dir);
        }
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(dlg.pick_file());
        });
        self.board_dialog = Some(rx);
    }

    /// 基板を裏のスレッドで読み込む（読み終えるまで、今の基板を表示し続ける）。startup なら起動時に指定された基板で、
    /// コマンドラインの指定（環境マップ・注視点・拡大率・保存先など）をそのまま使う
    fn start_board_load(&mut self, pcb: PathBuf, startup: bool) {
        let pcb = std::path::absolute(&pcb).unwrap_or(pcb);
        info!("opening {}", pcb.display());
        // コマンドラインの指定のうち、前の基板のためのもの（環境マップ・注視点・拡大率・保存先）は持ち越さない
        let o = if startup {
            RenderOptions { pcb: pcb.clone(), ..self.vo.common.clone() }
        } else {
            RenderOptions { pcb: pcb.clone(), hdr: String::new(), look_at: Vec::new(), zoom: None, init: false, out: PathBuf::new(), ..self.vo.common.clone() }
        };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            krbr_core::platform::lower_thread_priority();
            let _ = tx.send(prepare_scene(&o));
        });
        self.last_update = fl!("board-loading", name = pcb.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
        self.board_job = Some((pcb, rx, startup));
    }

    /// 基板を選ぶダイアログ・読み込みの結果を見る。読み終えていれば、その基板に切り替える
    fn poll_board_open(&mut self) -> Result<(), String> {
        if std::mem::take(&mut self.st.req.open_board) {
            self.start_board_dialog();
        }
        if let Some(p) = self.st.req.open_path.take() {
            self.start_board_load(p, false);
        }
        if let Some(rx) = &self.board_dialog {
            match rx.try_recv() {
                Ok(picked) => {
                    self.board_dialog = None;
                    if let Some(p) = picked {
                        self.start_board_load(p, false);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.board_dialog = None,
            }
        }
        let Some((pcb, rx, startup)) = &self.board_job else { return Ok(()) };
        let startup = *startup;
        let prep = match rx.try_recv() {
            Ok(p) => p,
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        let pcb = pcb.clone();
        self.board_job = None;
        match prep {
            Some(prep) => self.switch_board(prep, startup),
            None => {
                warn!("cannot open {}", pcb.display());
                self.last_update = fl!("board-open-failed", path = pcb.display().to_string());
                Ok(())
            }
        }
    }

    /// 読み込んだ基板に切り替える：起動し直したのと同じ状態を作り、ウィンドウ・GPU・egui・材質エディタは引き継ぐ。
    /// startup（起動時に指定された基板）なら、コマンドラインの指定を持ち越す
    fn switch_board(&mut self, prep: PreparedScene, startup: bool) -> Result<(), String> {
        self.flush_pending_edits();
        self.save_view(true); // 前の基板のパネルの設定を覚える
        let pcb = prep.board.file.clone();
        if !self.test.active() {
            remember::remember_last_board(&pcb);
        }
        let stem = pcb.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "board".into());
        let mut vo = ViewOptions { test: TestOptions::default(), ..self.vo.clone() };
        vo.common = if startup {
            RenderOptions { pcb: pcb.clone(), ..self.vo.common.clone() }
        } else {
            RenderOptions { pcb: pcb.clone(), hdr: String::new(), look_at: Vec::new(), zoom: None, init: false, out: board_file::base_dir(&pcb).join(format!("{stem}.png")), ..self.vo.common.clone() }
        };
        let mut fresh = new_app(vo, prep);
        fresh.window = self.window.take();
        fresh.web = self.web.take();
        fresh.web_started = self.web_started.take();
        fresh.splash = self.splash;
        fresh.editor_rect = self.editor_rect;
        fresh.proxy = self.proxy.clone();
        fresh.web_rects = self.web_rects;
        fresh.egui_ctx = self.egui_ctx.clone();
        fresh.egui_state = self.egui_state.take();
        fresh.gpu = self.gpu.take();
        fresh.editor = std::mem::take(&mut self.editor);
        fresh.test = std::mem::take(&mut self.test);
        fresh.win_start = self.win_start.clone();
        fresh.win_seen = self.win_seen.clone();
        fresh.win_saved = self.win_saved.clone();
        fresh.panel_width = self.panel_width;
        fresh.st.show_panel = self.st.show_panel;
        fresh.frames = self.frames;
        let g = fresh.gpu.as_mut().unwrap();
        g.ctx.wait_idle();
        g.gpu.rebuild_geometry(&fresh.prep.scene, fresh.st.bump_intensity).map_err(|e| format!("cannot send the board to the GPU: {e}"))?;
        g.gpu.set_environment(&fresh.prep.env, &fresh.prep.dist).map_err(|e| format!("cannot send the environment map to the GPU: {e}"))?;
        fresh.prep.env.rgba = Vec::new();
        let kind = fresh.prep.cfg.integrator;
        if g.prog.integrator_kind() != kind {
            if let Err(e) = g.prog.set_integrator(kind) {
                warn!("integrator {kind}: {e}; using pt instead");
                fresh.prep.cfg.integrator = IntegratorKind::Pt;
                g.prog.set_integrator(IntegratorKind::Pt)?;
            }
        } else {
            g.prog.rebuild_integrator()?;
        }
        if let Some(w) = &fresh.window {
            w.set_title(&fresh.base_title);
        }
        // 3D 表示の大きさは今のウィンドウのまま。視点はこの基板で覚えたもの（無ければ全体が入る構図）
        fresh.st.req.start_view = true;
        fresh.st.req.fit_window = fresh.st.size_mode.is_some();
        fresh.st.changed.frame = true;
        fresh.last_update = fl!("board-opened", name = fresh.prep.board.project_name.as_str());
        fresh.test.opened_at = Some(fresh.frames);
        info!("opened {}", pcb.display());
        *self = fresh;
        Ok(())
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

    /// ステータスバーに出す文字（egui と WebView で共通）。GPU の準備の前は None
    fn status_texts(&self) -> Option<StatusTexts> {
        let g = self.gpu.as_ref()?;
        let st = &self.st;
        let (samples, max_spp) = (g.prog.samples(), st.max_spp);
        let done = samples >= max_spp;
        let measured = self.accum_measured.max(1) as f64;
        let wall_per = self.accum_wall.as_secs_f64() / measured;
        let info = &g.ctx.info;
        let state = if st.paused {
            StatusState::Paused
        } else if done {
            StatusState::Done
        } else {
            StatusState::Rendering
        };
        let state_text = match state {
            StatusState::Paused => fl!("status-paused"),
            StatusState::Done => fl!("status-done"),
            StatusState::Rendering => fl!("status-rendering"),
        };
        let (rw, rh) = (g.prog.targets().width, g.prog.targets().height);
        let mut notes: Vec<(String, bool)> = Vec::new();
        if self.reload_job.is_some() {
            notes.push((fl!("status-reloading"), true));
        }
        if self.text_job.busy() {
            notes.push((fl!("status-silk"), true));
        }
        if self.prep.scene.pending_models > 0 {
            notes.push((fl!("status-meshing", count = self.prep.scene.pending_models.to_string()), true));
        }
        if !self.last_update.is_empty() {
            notes.push((self.last_update.clone(), false));
        }
        Some(StatusTexts {
            state,
            state_text,
            samples,
            max_spp,
            elapsed: fl!("status-elapsed", time = format_duration(self.accum_wall.as_secs_f64())),
            elapsed_tip: fl!("status-elapsed-tip"),
            average: (self.accum_measured > 0).then(|| fl!("status-average", wall = format!("{:.1}", wall_per * 1000.0), gpu = format!("{:.1}", self.accum_gpu_ms / measured))),
            average_tip: fl!("status-average-tip"),
            remaining: (self.accum_measured > 0 && !done && !st.paused).then(|| fl!("status-remaining", time = format_duration(wall_per * (max_spp - samples) as f64))),
            chips: vec![
                (fl!("status-integrator", integrator = g.prog.integrator_kind().as_str().to_uppercase(), color = if st.spectral { fl!("status-spectral") } else { "RGB".into() }), None),
                (
                    fl!("status-render-size", width = rw.to_string(), height = rh.to_string(), percent = format!("{:.0}", 100.0 * st.display.w as f64 / rw.max(1) as f64)),
                    Some(if st.size_mode.is_some() { fl!("status-size-fixed-tip") } else { fl!("status-size-fit-tip") }),
                ),
                (format!("EV {:+.2}", st.exposure), None),
                (fl!("status-camera", distance = format_length(st.distance as f64)), Some(fl!("status-camera-tip"))),
                (fl!("status-gpu", name = info.name.as_str(), method = trace_method_name(g.ctx)), None),
            ],
            notes,
        })
    }

    /// WebView（ボタン列・ステータスバー・設定パネル）を置く場所を、egui の画面の端に取っておく（3D 表示はその残り）。
    /// WebView が無いとき（画面を出さないテスト、WebView2 を作れなかったとき）は取らず、3D 表示をウィンドウ全体にする
    fn reserve_web_areas(&mut self, root: &mut egui::Ui) {
        self.web_rects[0] = egui::Rect::NOTHING;
        self.web_rects[1] = egui::Rect::NOTHING;
        self.web_rects[2] = egui::Rect::NOTHING;
        if self.web.is_none() {
            return;
        }
        let reserve = |panel: egui::Panel, root: &mut egui::Ui| panel.frame(egui::Frame::NONE).show(root, |ui| ui.allocate_space(ui.available_size())).response.rect;
        self.web_rects[0] = reserve(egui::Panel::top("toolbar").exact_size(WEB_TOOLBAR_HEIGHT), root);
        self.web_rects[1] = reserve(egui::Panel::bottom("status").exact_size(WEB_STATUS_HEIGHT), root);
        if self.st.show_panel {
            self.web_rects[2] = reserve(egui::Panel::right("settings").exact_size(self.panel_width), root);
        }
    }

    /// 1 フレーム：UI を描き、要求を反映し、サンプルを積んで表示し、頼まれた保存をする
    fn frame(&mut self) -> Result<(), String> {
        let frame_t0 = Instant::now();
        let Some(window) = &self.window else { return Ok(()) };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(()); // 最小化中
        }
        self.test.inject(self.frames, &mut self.st, &mut self.save_as_target);
        self.apply_web_messages();
        let ui = self.run_ui();
        self.update_web(ui.pixels_per_point);
        if self.st.req.quit {
            self.done = true;
            return Ok(());
        }
        self.apply_requests(&ui, size.width, size.height)?;
        let last_test_frame = self.test.finished(self.frames, self.busy(), self.saved_reloads);
        let Some(shown) = self.record_and_present(&ui, last_test_frame, frame_t0)? else { return Ok(()) };
        self.handle_saves(shown.denoised);
        self.step_web_ui();
        self.update_splash();
        self.step_video();
        self.poll_video_dialog();
        if last_test_frame {
            self.finish_test(shown.screenshot, shown.extent);
        } else if let Some(mut b) = shown.screenshot {
            self.gpu.as_ref().unwrap().ctx.destroy_buffer(&mut b);
        }
        Ok(())
    }

    /// WebView のボタン・キーの操作を、egui のボタン・キーと同じ要求にする
    fn apply_web_messages(&mut self) {
        let Some(web) = self.web.as_mut() else { return };
        let done = self.gpu.as_ref().is_some_and(|g| g.prog.samples() >= self.st.max_spp);
        for m in web.messages() {
            match m {
                crate::web_ui::Msg::Menu { id, x, y } => {
                    self.recent_boards = remember::recent_boards();
                    let reopen = self.web_menu_closed.as_ref().is_some_and(|(c, t)| *c == id && t.elapsed() < Duration::from_millis(400));
                    if self.web_menu.as_ref().is_some_and(|m| m.id == id) || reopen {
                        // 開いている見出しをもう一度押した：閉じる
                        self.close_web_menu();
                    } else {
                        let focused = self.web_menu.as_ref().is_some_and(|m| m.focused);
                        self.web_menu = Some(WebMenu { id, at: egui::pos2(x, y), size: None, focused });
                        // 前と同じメニューでも描き直して、大きさを知らせてもらう（中身が同じだと送らないので、大きさが来ず開かない）
                        self.web.as_mut().unwrap().resend(crate::web_ui::Pane::Menu);
                    }
                }
                crate::web_ui::Msg::Tip { pane, text, rect } => {
                    let target = egui::Rect::from_min_max(egui::pos2(rect[0], rect[1]), egui::pos2(rect[2], rect[3]));
                    self.web_tip = (!text.is_empty()).then_some(WebTip { pane, target, text, size: None });
                    self.web.as_mut().unwrap().resend(crate::web_ui::Pane::Tooltip);
                }
                crate::web_ui::Msg::TipSize(w, h) => {
                    if let Some(t) = &mut self.web_tip {
                        t.size = Some(egui::vec2(w, h));
                    }
                }
                crate::web_ui::Msg::MenuSize { shown, full } => {
                    if let Some(m) = &mut self.web_menu {
                        m.size = Some((egui::vec2(shown.0, shown.1), egui::vec2(full.0, full.1)));
                    }
                }
                crate::web_ui::Msg::Click(id) if id == "menu.close" => self.close_web_menu(),
                crate::web_ui::Msg::Click(id) if id.starts_with("about.") => self.apply_about_click(&id),
                crate::web_ui::Msg::Click(id) if id.starts_with("video.") => self.apply_video_click(&id),
                crate::web_ui::Msg::Click(id) if id.starts_with("kplug.") => self.plugin_window.click(&id),
                crate::web_ui::Msg::Set(id, v) if id.starts_with("video.") => self.apply_video_set(&id, &v),
                // 書き出しの間は、ほかの操作（パネル・ボタン列・メニューの項目・材質エディタ）を受けない
                crate::web_ui::Msg::Click(id) if self.video.is_some() && id.starts_with("menu:") && id != "menu:video" && id != "menu:about" && id != "menu:quit" => self.close_web_menu(),
                crate::web_ui::Msg::Click(id) if self.video.is_some() && !id.starts_with("menu") => {}
                crate::web_ui::Msg::Set(_, _) if self.video.is_some() => {}
                crate::web_ui::Msg::Key(_) if self.video.is_some() => {}
                crate::web_ui::Msg::Click(id) if id == "unlock" => self.st.locked = false,
                crate::web_ui::Msg::Click(id) if id == "lock" => self.st.locked = !self.st.locked,
                crate::web_ui::Msg::Open(url) => krbr_core::platform::open_url(&url),
                crate::web_ui::Msg::Dropped(paths) => self.open_dropped(paths),
                // 材質エディタ
                crate::web_ui::Msg::Set(id, v) if id.starts_with("mated.") => {
                    let info = self.part_info();
                    let actions = self.editor.web_apply(&self.library, info.as_ref(), &id["mated.".len()..], &v);
                    self.editor_actions.extend(actions);
                }
                crate::web_ui::Msg::Click(id) if id.starts_with("mated.") => {
                    let info = self.part_info();
                    let actions = self.editor.web_apply(&self.library, info.as_ref(), &id["mated.".len()..], &serde_json::Value::Null);
                    self.editor_actions.extend(actions);
                }
                crate::web_ui::Msg::Set(id, v) if id == "editor.move" || id == "editor.grow" => {
                    let d = egui::vec2(v.get("dx").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32, v.get("dy").and_then(|x| x.as_f64()).unwrap_or(0.0) as f32);
                    if let Some(r) = &mut self.editor_rect {
                        if id == "editor.move" {
                            *r = r.translate(d);
                        } else {
                            r.max += d;
                        }
                    }
                }
                crate::web_ui::Msg::Click(id) if id.starts_with("menu:") => {
                    self.close_web_menu();
                    self.apply_menu_click(&id["menu:".len()..]);
                }
                crate::web_ui::Msg::Click(id) if self.apply_panel_click(&id) => {}
                crate::web_ui::Msg::Set(id, v) => self.apply_panel_set(&id, &v),
                crate::web_ui::Msg::Resize(w) => self.panel_width = w.clamp(300.0, 640.0),
                crate::web_ui::Msg::Click(id) => {
                    let st = &mut self.st;
                    match id.as_str() {
                        "pause" if !done => st.paused = !st.paused,
                        "top" => st.req.view_preset = Some(TOP_VIEW),
                        "bottom" => st.req.view_preset = Some(BOTTOM_VIEW),
                        "fit" => st.req.fit_view = true,
                        "smd" => {
                            st.show_smd = !st.show_smd;
                            st.changed.visibility = true;
                        }
                        "tht" => {
                            st.show_tht = !st.show_tht;
                            st.changed.visibility = true;
                        }
                        "denoise" => st.denoise = !st.denoise,
                        _ => {}
                    }
                }
                crate::web_ui::Msg::Key(code) => {
                    if let Some(action) = key_code_of_js(&code).and_then(key_action) {
                        if self.apply_key_action(action) {
                            self.st.req.quit = true;
                        }
                    }
                }
                crate::web_ui::Msg::Ready | crate::web_ui::Msg::Painted(_) => {}
            }
        }
    }

    /// 描画ロック（または動画の書き出し）中か
    fn render_locked(&self) -> bool {
        self.st.locked || self.video.is_some()
    }

    /// 描画ロック中に、解除したら反映するもの（保存された基板・変えた設定）があるか
    fn lock_pending(&self) -> bool {
        let c = &self.st.changed;
        c.frame || c.materials || c.mask || c.integrator || c.visibility || self.st.req.reload || self.text_dirty || self.materials_dirty_files || !self.editor_actions.is_empty() || self.env_job.is_some() || self.board_dialog.is_some() || self.st.req.open_path.is_some() || self.reload_job.is_some()
    }

    /// 描画ロック中に基板が保存された：読み直しは解除してから（そのときに読み直すよう覚えておく）
    fn note_saves_while_locked(&mut self) {
        if self.watcher.as_mut().is_some_and(|w| w.poll()) {
            self.st.req.reload = true;
            info!("render lock: the board was saved; it is read again when the lock is released");
        }
    }

    /// 落としたファイルを開く：基板（.kicad_pcb。プロジェクトの .kicad_pro なら同じ名前の基板）か、環境マップ
    fn open_dropped(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
            let pcb = match ext.as_str() {
                "kicad_pcb" => Some(path.clone()),
                "kicad_pro" => Some(path.with_extension("kicad_pcb")).filter(|p| p.is_file()),
                _ => None,
            };
            if let Some(pcb) = pcb {
                info!("dropped {}", pcb.display());
                self.st.req.open_path = Some(pcb);
                return;
            }
            if is_env_file(&path) {
                self.start_env_load(path.to_string_lossy().into_owned(), true);
                return;
            }
            self.last_update = fl!("drop-unknown", path = path.display().to_string());
        }
    }

    /// まだ作っていない WebView を 1 つ作る（フレームを出した後）。いつも見えるものを作ったら起動中の画面を終え、
    /// 残り（メニュー・説明・材質エディタなど）は画面を見せてから作る。いつも見えるものを作れなければ egui の画面にする
    fn step_web_ui(&mut self) {
        let Some(t0) = self.web_started else { return };
        if self.web_failures.1.is_some_and(|t| Instant::now() < t) {
            return;
        }
        let (Some(web), Some(window)) = (self.web.as_mut(), self.window.as_ref()) else { return };
        let core_before = web.core_ready();
        match web.step(window) {
            Ok(done) => {
                if !core_before && web.core_ready() {
                    info!("web UI: shown in {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
                }
                if done {
                    info!("web UI: WebView2 ready in {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
                    self.web_started = None;
                }
            }
            Err(e) if self.web_failures.0 < 6 => {
                // 少し待って作り直す
                self.web_failures = (self.web_failures.0 + 1, Some(Instant::now() + Duration::from_millis(500)));
                warn!("web UI: {e}; trying again ({})", self.web_failures.0);
            }
            Err(e) => {
                self.web_started = None;
                if web.core_ready() {
                    warn!("web UI: {e}; some pages are missing");
                } else {
                    warn!("web UI: {e} (after {:.0} ms); WebView2 is not available - only the 3D view and the keys work", t0.elapsed().as_secs_f64() * 1000.0);
                    self.web = None;
                    self.web_missing = true;
                }
            }
        }
    }

    /// 起動中の画面を終えるか：画面（WebView）を作り終え、起動時の基板を読み終え、最初の画像を描いたら
    fn update_splash(&mut self) {
        if !self.splash {
            return;
        }
        let board_loading = self.board_job.as_ref().is_some_and(|(_, _, startup)| *startup);
        let first = !self.has_board() || self.gpu.as_ref().is_some_and(|g| g.prog.samples() > 0);
        if self.web.as_ref().is_none_or(|w| w.core_ready()) && !board_loading && first {
            self.splash = false;
            self.st.changed.frame = true;
        }
    }

    /// 起動中の画面：進み具合のバーと、今している段階（1 行）
    fn draw_splash(&mut self, ctx: &egui::Context) {
        let screen = ctx.content_rect();
        ctx.layer_painter(egui::LayerId::background()).rect_filled(screen, 0.0, egui::Color32::from_rgb(17, 19, 22));
        let (made, total) = self.web.as_ref().map(|w| w.core_progress()).unwrap_or((1, 1));
        let board_loading = self.board_job.as_ref().filter(|(_, _, startup)| *startup).map(|(p, _, _)| p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
        let board_done = board_loading.is_none();
        let first = !self.has_board() || self.gpu.as_ref().is_some_and(|g| g.prog.samples() > 0);
        let frac = 0.15 + 0.45 * made as f32 / total.max(1) as f32 + if board_done { 0.25 } else { 0.0 } + if first && board_done { 0.15 } else { 0.0 };
        let step = if made < total {
            fl!("startup-step-ui", done = made.to_string(), total = total.to_string())
        } else if let Some(name) = &board_loading {
            fl!("startup-step-board", name = name.as_str())
        } else {
            fl!("startup-step-first")
        };
        let width = 360.0f32.min(screen.width() - 40.0);
        let bar = egui::Rect::from_center_size(screen.center(), egui::vec2(width, 6.0));
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("splash")));
        painter.rect_filled(bar, 3.0, egui::Color32::from_rgb(38, 41, 46));
        painter.rect_filled(egui::Rect::from_min_size(bar.min, egui::vec2(width * frac.min(1.0), bar.height())), 3.0, egui::Color32::from_rgb(232, 148, 90));
        // 長い基板の名前は 1 行に収まるよう末尾を切る
        let mut job = egui::text::LayoutJob::simple_singleline(step, egui::FontId::proportional(13.0), egui::Color32::from_rgb(164, 168, 176));
        job.wrap = egui::text::TextWrapping::truncate_at_width(width);
        let galley = ctx.fonts_mut(|f| f.layout_job(job));
        painter.galley(egui::pos2(screen.center().x - galley.size().x / 2.0, bar.bottom() + 12.0), galley, egui::Color32::from_rgb(164, 168, 176));
    }

    /// メニューのプルダウンを閉じる（キーボードの入力はビューアに戻す）
    fn close_web_menu(&mut self) {
        if let Some(m) = self.web_menu.take() {
            if m.focused {
                if let Some(web) = &mut self.web {
                    web.focus_parent(crate::web_ui::Pane::Menu);
                }
            }
            self.web_menu_closed = Some((m.id, Instant::now()));
        }
    }

    /// WebView を egui が空けた場所へ置き、ボタン列とステータスバーの状態を送る
    fn update_web(&mut self, ppp: f32) {
        if self.web.is_none() {
            return;
        }
        use crate::web_ui::Pane;
        let px = |r: egui::Rect| -> Option<crate::web_ui::PxRect> {
            (r.is_positive()).then(|| ((r.min.x * ppp).round() as i32, (r.min.y * ppp).round() as i32, (r.width() * ppp).round() as u32, (r.height() * ppp).round() as u32))
        };
        let mut rects = self.web_rects.map(px).to_vec();
        // メニューのプルダウン：見出しの下に、どのサブメニューを開いても収まる大きさで（右にはみ出すなら左へずらす。
        // 縦はページが入る高さに収め、入らない分は中でスクロールする）。見せるのは、今開いているところだけ
        let screen = self.egui_ctx.content_rect();
        let menu_top = self.web_menu.as_ref().map(|m| self.web_rects[0].min.y + m.at.y);
        let menu_rect = self.web_menu.as_ref().and_then(|m| {
            let (_, full) = m.size?;
            let x = (self.web_rects[0].min.x + m.at.x).min(screen.right() - full.x).max(screen.left());
            let r = egui::Rect::from_min_size(egui::pos2(x, menu_top?), full).intersect(screen);
            r.is_positive().then_some(r)
        });
        let menu_clip = self.web_menu.as_ref().and_then(|m| m.size).map(|(shown, _)| ((shown.x * ppp).ceil() as u32, (shown.y * ppp).ceil() as u32));
        rects.push(menu_rect.and_then(px));
        // 説明の吹き出し：要素の下の中央に（下に入らなければ上に。左右はウィンドウの中に収める）
        let tip_rect = self.web_tip.as_ref().and_then(|t| {
            let size = t.size?;
            let origin = match t.pane {
                Pane::Toolbar => self.web_rects[0],
                Pane::Status => self.web_rects[1],
                Pane::Panel => self.web_rects[2],
                Pane::Banner => self.web_rects[3],
                Pane::Editor => self.web_rects[4],
                Pane::About => self.web_rects[5],
                Pane::Video => self.web_rects[6],
                Pane::Plugin => self.web_rects[7],
                Pane::Menu => menu_rect?,
                Pane::Tooltip => return None,
            };
            if !origin.is_positive() {
                return None;
            }
            let target = t.target.translate(origin.min.to_vec2());
            let x = (target.center().x - size.x / 2.0).clamp(screen.left() + 4.0, (screen.right() - size.x - 4.0).max(screen.left() + 4.0));
            let below = target.bottom() + 6.0;
            let y = if below + size.y <= screen.bottom() - 4.0 { below } else { (target.top() - 6.0 - size.y).max(screen.top()) };
            Some(egui::Rect::from_min_size(egui::pos2(x, y), size))
        });
        rects.push(tip_rect.and_then(px));
        let tip = self.web_tip.as_ref().map(|t| serde_json::json!({ "text": t.text }).to_string());
        let menu = self.web_menu.as_ref().map(|m| self.menu_json(&m.id, (screen.bottom() - menu_top.unwrap_or(0.0) - 6.0).max(80.0)));
        if rects[5].is_some() && self.about_json.is_none() {
            self.about_json = Some(self.about_json());
        }
        let about = rects[5].is_some().then(|| self.about_json.clone().unwrap_or_default());
        let video = rects[6].is_some().then(|| self.video_json());
        let plugin = rects[7].is_some().then(|| self.plugin_window.json());
        let editor = rects[4].is_some().then(|| {
            let info = self.part_info();
            let mut scope = self.scope_json();
            // 書き込む先の材質が、より優先する上書きに隠れるなら知らせる（設定パネルと同じ）
            if let Some(n) = self.selected.as_ref().and_then(|s| self.scope_shadow_note(s)) {
                scope["warn"] = serde_json::Value::String(n);
            }
            self.editor.web_json(&self.library, info.as_ref(), scope)
        });
        let toolbar = self.toolbar_json();
        let status = self.status_texts().map(|t| t.json()).unwrap_or_default();
        let panel = rects[2].is_some().then(|| self.panel_json());
        let banner = rects[3].is_some().then(|| {
            if let Some(s) = self.video_status() {
                return serde_json::json!({ "text": s, "action": fl!("video-cancel"), "actionId": "video.cancel", "tone": "info" }).to_string();
            }
            if self.st.locked {
                let text = if self.lock_pending() { fl!("lock-banner-pending") } else { fl!("lock-banner") };
                return serde_json::json!({ "text": text, "action": fl!("lock-release"), "actionId": "unlock", "tone": "info" }).to_string();
            }
            let text = if self.board_visibility() == BoardInView::TooSmall { fl!("board-too-small") } else { fl!("board-out-of-view") };
            serde_json::json!({ "text": text, "action": fl!("camera-fit"), "actionId": "fit" }).to_string()
        });
        let web = self.web.as_mut().unwrap();
        web.clip(Pane::Menu, menu_clip);
        for (pane, r) in Pane::ALL.into_iter().zip(rects) {
            web.layout(pane, r);
        }
        web.send(Pane::Toolbar, &toolbar);
        web.send(Pane::Status, &status);
        if let Some(j) = panel {
            web.send(Pane::Panel, &j);
        }
        if let Some(j) = banner {
            web.send(Pane::Banner, &j);
        }
        if let Some(j) = menu {
            web.send(Pane::Menu, &j);
        }
        if let Some(j) = editor {
            web.send(Pane::Editor, &j);
        }
        if let Some(j) = about {
            web.send(Pane::About, &j);
        }
        if let Some(j) = video {
            web.send(Pane::Video, &j);
        }
        if let Some(j) = plugin {
            web.send(Pane::Plugin, &j);
        }
        if let Some(j) = tip {
            web.send(Pane::Tooltip, &j);
        }
        if let Some(m) = &mut self.web_menu {
            if menu_rect.is_some() && !m.focused {
                web.focus(Pane::Menu);
                m.focused = true;
            }
        }
    }

    /// ボタン列の状態（WebView へ送る JSON。文字は訳したもの）
    fn toolbar_json(&self) -> String {
        let st = &self.st;
        let done = self.gpu.as_ref().is_some_and(|g| g.prog.samples() >= st.max_spp);
        serde_json::json!({
            "paused": st.paused, "done": done, "board": self.has_board(), "locked": self.render_locked(), "video": self.video.is_some(),
            "smd": st.show_smd, "tht": st.show_tht, "denoise": st.denoise,
            "text": {
                "pause": fl!("toolbar-pause"), "resume": fl!("toolbar-resume"), "done": fl!("toolbar-done"), "lock": fl!("toolbar-lock"), "locked": fl!("toolbar-locked"),
                "top": fl!("toolbar-top"), "bottom": fl!("toolbar-bottom"), "fit": fl!("toolbar-fit"),
                "smd": fl!("toolbar-smd"), "tht": fl!("toolbar-tht"), "denoise": fl!("toolbar-denoise"),
            },
            "tips": {
                "pause": fl!("toolbar-pause-tip"), "done": fl!("toolbar-done-tip"), "lock": fl!("toolbar-lock-tip"),
                "top": fl!("toolbar-top-tip"), "bottom": fl!("toolbar-bottom-tip"), "fit": fl!("toolbar-fit-tip"),
                "smd": fl!("toolbar-smd-tip"), "tht": fl!("toolbar-tht-tip"), "denoise": fl!("toolbar-denoise-tip"),
            },
            "groups": { "camera": fl!("toolbar-group-camera"), "visibility": fl!("toolbar-group-visibility"), "effects": fl!("toolbar-group-effects") },
            "menus": web_menu::menu_heads(),
            "menuOpen": self.web_menu.as_ref().map(|m| m.id.as_str()),
        })
        .to_string()
    }

    /// 裏の処理（環境マップの読み込み・基板の読み直し・書き換えた材質の反映）が残っているか
    fn busy(&self) -> bool {
        self.env_job.is_some() || self.reload_job.is_some() || self.materials_dirty_files || self.board_job.is_some() || self.board_dialog.is_some()
    }

    /// メニューと操作パネルを描く（egui）。残りの領域が 3D 表示になる
    fn run_ui(&mut self) -> UiFrame {
        let raw = self.egui_state.as_mut().unwrap().take_egui_input(self.window.as_ref().unwrap());
        let egui_ctx = self.egui_ctx.clone();
        let mut central = egui::Rect::NOTHING;
        let full = egui_ctx.run_ui(raw, |root| {
            if self.splash {
                // 起動中：メニュー・パネルは出さず（WebView もまだ隠す）、進み具合のバーと今の段階だけ
                central = root.available_rect_before_wrap();
                self.web_rects = [egui::Rect::NOTHING; 8];
                self.draw_splash(root.ctx());
                return;
            }
            self.reserve_web_areas(root);
            central = root.available_rect_before_wrap();
            self.draw_outline(root, central);
            let lost = if self.has_board() { self.board_visibility() } else { BoardInView::Visible };
            // WebView のときは、警告は 3D 表示の上端の帯（WebView）に出す
            let banner = lost != BoardInView::Visible || self.render_locked();
            self.web_rects[3] = if banner { egui::Rect::from_min_size(central.min, egui::vec2(central.width(), WEB_BANNER_HEIGHT)) } else { egui::Rect::NOTHING };
            if (lost != BoardInView::Visible || self.render_locked()) && self.web.is_none() {
                let text = if let Some(v) = self.video_status() {
                    v
                } else if self.st.locked {
                    if self.lock_pending() { fl!("lock-banner-pending") } else { fl!("lock-banner") }
                } else if lost == BoardInView::TooSmall {
                    fl!("board-too-small")
                } else {
                    fl!("board-out-of-view")
                };
                let galley = root.painter().layout(text, egui::FontId::proportional(16.0), egui::Color32::from_rgb(255, 204, 77), (central.width() - 40.0).max(100.0));
                let pos = egui::pos2(central.center().x - galley.size().x / 2.0, central.top() + 16.0);
                let bg = egui::Rect::from_min_size(pos, galley.size()).expand(6.0);
                root.painter().with_clip_rect(central).rect_filled(bg, 4.0, egui::Color32::from_black_alpha(180));
                root.painter().with_clip_rect(central).galley(pos, galley, egui::Color32::from_rgb(255, 204, 77));
            }
            if !self.has_board() {
                let text = match &self.board_job {
                    Some((p, _, _)) => fl!("board-loading", name = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()),
                    None => fl!("no-board-hint"),
                };
                let galley = root.painter().layout(text, egui::FontId::proportional(18.0), egui::Color32::from_gray(220), (central.width() - 40.0).max(100.0));
                let pos = central.center() - galley.size() / 2.0;
                root.painter().with_clip_rect(central).galley(pos, galley, egui::Color32::from_gray(220));
            }
            let info = self.part_info();
            if let Some(web) = &self.web {
                // 材質エディタは 3D 表示の上の WebView（初めは左上。ウィンドウの中に収める）
                self.editor.attach_web(root.ctx(), web.images());
                let actions = self.editor.web_update(&self.library, info.as_ref());
                self.editor_actions.extend(actions);
                // 上下はボタン列とステータスバーの間（左右は設定パネルの上にも動かせる）
                let screen = root.ctx().content_rect();
                let top = self.web_rects[0].bottom().max(screen.top());
                let bottom = if self.web_rects[1].is_positive() { self.web_rects[1].top() } else { screen.bottom() };
                self.web_rects[4] = if self.editor.open {
                    let r = *self.editor_rect.get_or_insert_with(|| egui::Rect::from_min_size(central.min + egui::vec2(16.0, 16.0), egui::vec2(900.0f32.min(central.width() - 32.0), 780.0f32.min(central.height() - 32.0))));
                    let size = r.size().max(egui::vec2(560.0, 360.0)).min(egui::vec2(screen.width(), bottom - top));
                    let min = egui::pos2(r.min.x.clamp(screen.left(), (screen.right() - size.x).max(screen.left())), r.min.y.clamp(top, (bottom - size.y).max(top)));
                    let r = egui::Rect::from_min_size(min, size);
                    self.editor_rect = Some(r);
                    r
                } else {
                    egui::Rect::NOTHING
                };
                // 「について」：ウィンドウの真ん中
                self.web_rects[5] = if self.about_open {
                    let size = egui::vec2(780.0f32.min(screen.width() - 40.0), 600.0f32.min(screen.height() - 40.0)).max(egui::vec2(200.0, 200.0));
                    egui::Rect::from_center_size(screen.center(), size)
                } else {
                    egui::Rect::NOTHING
                };
                // 「動画を書き出す」：3D 表示の右上（書き出している画像が見えるように、真ん中には置かない）
                self.web_rects[6] = if self.video_dialog.open {
                    let size = egui::vec2(440.0f32.min(central.width() - 24.0), 620.0f32.min(central.height() - 24.0)).max(egui::vec2(200.0, 200.0));
                    egui::Rect::from_min_size(egui::pos2(central.right() - size.x - 12.0, central.top() + 12.0), size)
                } else {
                    egui::Rect::NOTHING
                };
                // 「KiCad プラグイン」：ウィンドウの真ん中
                self.web_rects[7] = if self.plugin_window.open {
                    let size = egui::vec2(620.0f32.min(screen.width() - 40.0), 520.0f32.min(screen.height() - 40.0)).max(egui::vec2(200.0, 200.0));
                    egui::Rect::from_center_size(screen.center(), size)
                } else {
                    egui::Rect::NOTHING
                };
            } else if self.web_missing {
                // WebView2 を作れなかった：メニュー・パネルは出せないので、入れ方を知らせる（キーの操作は使える）
                let galley = root.painter().layout(fl!("webview-missing"), egui::FontId::proportional(15.0), egui::Color32::from_rgb(255, 204, 77), (central.width() - 60.0).max(100.0));
                let pos = egui::pos2(central.center().x - galley.size().x / 2.0, central.bottom() - galley.size().y - 24.0);
                let bg = egui::Rect::from_min_size(pos, galley.size()).expand(10.0);
                root.painter().with_clip_rect(central).rect_filled(bg, 6.0, egui::Color32::from_black_alpha(200));
                root.painter().with_clip_rect(central).galley(pos, galley, egui::Color32::from_rgb(255, 204, 77));
            }
        });
        self.egui_state.as_mut().unwrap().handle_platform_output(self.window.as_ref().unwrap(), full.platform_output);
        let primitives = egui_ctx.tessellate(full.shapes, full.pixels_per_point);
        UiFrame { textures: full.textures_delta, primitives, pixels_per_point: full.pixels_per_point, central }
    }

    /// UI の要求と裏の処理の結果を反映する（GPU は前のフレームの完了を待っているので、ここで資源を触ってよい）
    fn apply_requests(&mut self, ui: &UiFrame, fbw: u32, fbh: u32) -> Result<(), String> {
        // 覚えるのは前のフレームまでに反映した値（「すべて既定に戻す」は、apply_view_requests で基準を取り直す）
        self.save_view(false);
        // 描画ロック中は、累積をやり直すもの（読み直し・基板を開く・材質・環境マップ・シーンの変更）を解除まで待たせる
        let locked = self.render_locked();
        if !locked && std::mem::take(&mut self.st.req.reset_all) {
            self.reset_all_settings();
        }
        if locked {
            self.note_saves_while_locked();
        } else {
            self.apply_reload()?;
            self.poll_board_open()?;
            self.poll_silk();
        }
        self.flush_emit_edit(false);
        if !locked {
            self.apply_editor_actions();
            self.start_reload_if_needed();
        }
        self.apply_menu_requests();
        if !locked {
            self.poll_environment()?;
            self.apply_scene_changes()?;
        }
        let ctx = self.gpu.as_ref().unwrap().ctx;
        let info = self.part_info();
        self.editor.render(ctx, &self.prep.scene, info.as_ref(), &self.library, self.st.spectral, self.scene_epoch);
        self.update_targets(ui, fbw, fbh)?;
        self.fit_window();
        self.apply_view_requests();
        if self.st.denoise && !self.denoiser_tried {
            self.init_denoiser();
        }
        Ok(())
    }

    /// メニューの要求（保存のダイアログ・表示の言語・設定ファイルの置き場所）と、開いているダイアログの結果
    fn apply_menu_requests(&mut self) {
        if let Some(f) = self.st.req.save_as.take() {
            self.start_save_dialog(f);
        }
        if let Some(choice) = self.st.req.language.take() {
            // メニューで選んだものは --lang より優先する（覚えて、次からも使う）
            remember::remember_ui_language(choice.as_deref());
            self.language = i18n::select(None, choice.as_deref());
            self.language_choice = choice;
            install_fonts(&self.egui_ctx, &self.language);
            info!("language: {}", self.language);
        }
        if let Some(loc) = self.st.req.settings_location.take() {
            self.set_settings_location(loc);
        }
        if std::mem::take(&mut self.st.req.reveal_settings) && self.has_board() {
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
                        self.start_env_load(p.to_string_lossy().into_owned(), true);
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.open_dialog = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    /// 環境マップを読み終えていれば GPU へ送る。送れなければ（VRAM が足りないなど）内蔵のスタジオに戻す
    fn poll_environment(&mut self) -> Result<(), String> {
        let Some(job) = &self.env_job else { return Ok(()) };
        let d = match job.rx.try_recv() {
            Ok(d) => d,
            Err(mpsc::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.env_error = format!("{}: the loading thread stopped", job.path);
                error!("environment {}", self.env_error);
                self.env_job = None;
                return Ok(());
            }
        };
        let remember_it = job.remember;
        self.env_job = None;
        if !d.error.is_empty() {
            self.env_error = d.error.clone();
            error!("environment {}: {}", d.path, d.error);
            return Ok(());
        }
        let g = self.gpu.as_mut().unwrap();
        g.ctx.wait_idle();
        match g.gpu.set_environment(&d.image, &d.dist) {
            Ok(()) => {
                self.env_path = remember::normalize_hdr(&d.path);
                // 基板を開いていなければ、覚える先（基板の設定ファイル）が無い
                if remember_it && !self.prep.board.file.as_os_str().is_empty() && !self.test.active() {
                    remember::remember_hdr(&self.prep.board.file, &self.env_path, &self.prep.config_hdr);
                }
                info!("environment: {} ({}x{})", self.env_path, d.image.width, d.image.height);
            }
            Err(e) => {
                // GPU の環境マップは消えているので、小さい内蔵のスタジオを送り直す（それも送れなければ描けないので終える）
                error!("environment {}: {e}; using the built-in studio", d.path);
                self.env_error = fl!("env-gpu-failed", error = e);
                let studio = load_environment(STUDIO.to_string());
                if !studio.error.is_empty() {
                    return Err(format!("environment: {}", studio.error));
                }
                g.gpu.set_environment(&studio.image, &studio.dist).map_err(|e| format!("environment: {e}"))?;
                self.env_path = STUDIO.to_string();
            }
        }
        g.prog.rebuild_integrator()?;
        self.env_list = list_environments(&self.env_path);
        self.st.changed.frame = true;
        Ok(())
    }

    /// 積分器・レジストの色・材質の変更を GPU へ送る
    fn apply_scene_changes(&mut self) -> Result<(), String> {
        if std::mem::take(&mut self.st.changed.integrator) {
            let g = self.gpu.as_mut().unwrap();
            g.ctx.wait_idle();
            let kind = self.prep.cfg.integrator;
            match g.prog.set_integrator(kind) {
                Ok(()) => info!("integrator: {kind}"),
                Err(e) => {
                    // 起動時と同じく PT に戻す（PT も作れなければ描けないので終える）
                    error!("integrator {kind}: {e}; using pt instead");
                    self.last_update = fl!("integrator-failed", integrator = kind.as_str().to_uppercase(), error = e);
                    self.prep.cfg.integrator = IntegratorKind::Pt;
                    g.prog.set_integrator(IntegratorKind::Pt)?;
                }
            }
            self.st.changed.frame = true;
        }
        if std::mem::take(&mut self.st.changed.mask) {
            self.apply_mask_choice();
        }
        if std::mem::take(&mut self.st.changed.visibility) {
            let mask = self.st.visible_mask();
            let g = self.gpu.as_mut().unwrap();
            if g.gpu.visible_mask != mask {
                g.ctx.wait_idle();
                if let Err(e) = g.gpu.set_visible_mask(mask) {
                    error!("visibility: {e}");
                }
                self.st.changed.frame = true;
            }
            // 隠した部品を選んでいたら、選択を外す
            let hidden = self.selected.as_ref().and_then(|s| s.find_instance(&self.prep.scene, &self.prep.board)).is_some_and(|i| self.prep.scene.class_of(i) & mask == 0);
            if hidden {
                self.selected = None;
                self.refresh_outline();
            }
        }
        if std::mem::take(&mut self.st.changed.materials) {
            let g = self.gpu.as_mut().unwrap();
            g.ctx.wait_idle();
            if let Err(e) = g.gpu.update_materials(&self.prep.scene.materials, self.st.bump_intensity) {
                error!("update materials: {e}");
                self.last_update = fl!("materials-failed", error = e);
            }
            self.st.changed.frame = true;
        }
        Ok(())
    }

    /// スワップチェーンをウィンドウの大きさに、累積の画像を描画のサイズに合わせ、3D 表示の領域と画像を置く矩形を決める
    fn update_targets(&mut self, ui: &UiFrame, fbw: u32, fbh: u32) -> Result<(), String> {
        let g = self.gpu.as_mut().unwrap();
        if self.need_recreate || fbw != g.swap.extent.width || fbh != g.swap.extent.height {
            g.ctx.wait_idle();
            g.swap.recreate(fbw, fbh)?;
            self.need_recreate = false;
        }
        // 3D 表示の領域（スワップチェーンがウィンドウと違う大きさのこともあるので、その中に収める）。
        // 描画の解像度は、決まったサイズか、領域 × 比率
        let vp = viewport_px(ui.central, ui.pixels_per_point, g.swap.extent.width, g.swap.extent.height);
        let (rw, rh) = self.st.size_mode.unwrap_or_else(|| (((vp.w as f32 * self.st.render_ratio).round() as u32).max(1), ((vp.h as f32 * self.st.render_ratio).round() as u32).max(1)));
        let t = g.prog.targets();
        // 描画ロック中は描画の大きさを変えない（ウィンドウの大きさを変えても、今の画像を縮めて見せる）
        let (rw, rh) = if self.st.locked && self.video.is_none() { (t.width, t.height) } else { (rw, rh) };
        if rw != t.width || rh != t.height {
            g.prog.resize(rw, rh)?;
            g.tone = Tonemapper::with_outputs(g.ctx, g.prog.targets(), TONE_SLOTS)?;
            // ノイズ除去は新しい大きさで作り直す
            if g.denoiser.is_some() {
                g.tone_denoised = None;
                g.denoiser = None;
                self.denoiser_tried = false;
            }
            self.st.changed.frame = true;
        }
        self.st.viewport = vp;
        self.st.display = fit_rect(vp, rw, rh);
        Ok(())
    }

    /// クリック・Ctrl + クリック・視点の要求を反映し、カメラなどが変わったら累積をやり直す
    fn apply_view_requests(&mut self) {
        if let Some(p) = self.st.req.part_pick.take() {
            self.pick_part(p);
        }
        if std::mem::take(&mut self.st.req.material_editor) {
            self.editor.open = true;
        }
        if std::mem::take(&mut self.st.req.kicad_plugin) {
            self.plugin_window.show();
        }
        if let Some(r) = self.st.req.select_reference.take() {
            let model = self.prep.board.footprints.iter().find(|f| f.reference == r).map(|f| if f.models.is_empty() { None } else { Some(0) });
            self.selected = model.map(|model| Selected { reference: r.clone(), model });
            self.refresh_outline();
            info!("selected {r} (test)");
        }
        if let Some(m) = self.st.req.assign_all.take() {
            self.assign_material("", &m, false);
        }
        if self.render_locked() {
            // 描画ロック中：視点の操作は捨てる（あとで動き出さないように）。設定の変更（changed.frame）は解除まで残す
            self.st.req.focus_pick = None;
            self.st.req.reset_view = false;
            self.st.req.view_preset = None;
            self.st.req.fit_view = false;
        }
        if self.render_locked() && self.video.is_none() {
            return;
        }
        if let Some(p) = self.st.req.focus_pick.take() {
            self.focus_at(p);
        }
        if !self.splash && std::mem::take(&mut self.st.req.start_view) {
            self.start_camera(self.st.display.w, self.st.display.h);
        }
        if std::mem::take(&mut self.st.req.reset_view) {
            self.reset_camera(self.st.display.w, self.st.display.h);
        }
        if let Some((el, az)) = self.st.req.view_preset.take() {
            self.preset_camera(el, az, self.st.display.w, self.st.display.h);
        }
        if std::mem::take(&mut self.st.req.fit_view) {
            let (el, az) = (self.st.elevation, self.st.azimuth);
            self.preset_camera(el, az, self.st.display.w, self.st.display.h);
        }
        if std::mem::take(&mut self.st.req.rebase_view) {
            // 戻した値は覚えない：今の値を基準にする（忘れた状態のまま。次に変えたときから、また覚える）
            self.view_seen = self.current_view();
            self.view_saved = self.view_seen.clone();
        }
        if std::mem::take(&mut self.st.changed.frame) {
            // 流しているサンプルが読むフレームの値を書き換えるので、終わるのを待つ
            self.settle_render();
            let (w, h) = {
                let t = self.gpu.as_ref().unwrap().prog.targets();
                (t.width, t.height)
            };
            let f = self.current_frame(w, h);
            self.gpu.as_mut().unwrap().prog.set_frame(f);
            self.last_change_at = Instant::now();
        }
        if std::mem::take(&mut self.st.req.auto_exposure) {
            self.settle_render();
            let g = self.gpu.as_ref().unwrap();
            if g.prog.samples() > 0 {
                match g.prog.readback() {
                    Ok(aov) => {
                        self.st.exposure = auto_exposure(&aov.color);
                        info!("auto exposure: {:+.2} EV", self.st.exposure);
                    }
                    Err(e) => {
                        error!("auto exposure: {e}");
                        self.last_update = fl!("readback-failed", error = e);
                    }
                }
            }
        }
    }

    /// 流したサンプルが終わっていれば（wait なら終わるまで待って）、かかった時間を数え、次に流す数を決める
    fn poll_render(&mut self, wait: bool) {
        let Some(g) = self.gpu.as_mut() else { return };
        let Some(Batch { value, add, slot, t0 }) = g.in_flight else { return };
        let dev = &g.ctx.device;
        // 待てない・状態を取れない（デバイスを失ったなど）ときは、終わったことにする（次の GPU の処理がエラーを出す）
        if wait {
            let (sems, values) = ([g.timeline], [value]);
            let _ = unsafe { dev.wait_semaphores(&vk::SemaphoreWaitInfo::default().semaphores(&sems).values(&values), u64::MAX) };
        }
        let done = wait || unsafe { dev.get_semaphore_counter_value(g.timeline) }.map_or(true, |v| v >= value);
        if !done {
            return;
        }
        g.in_flight = None;
        g.completed = value;
        g.shown_slot = slot;
        let wall = t0.elapsed();
        let mut ts = [0u64; 2];
        let ts_ok = g.timestamps_ok && unsafe { dev.get_query_pool_results(g.timestamps, 0, &mut ts, vk::QueryResultFlags::TYPE_64) }.is_ok();
        self.last_gpu_ms = if ts_ok { ts[1].saturating_sub(ts[0]) as f64 * g.ts_period_ms } else { wall.as_secs_f64() * 1000.0 };
        // サンプルを積む GPU 時間が目標に収まるように、1 回に流す数を調整する
        let per_sample = self.last_gpu_ms / add.max(1) as f64;
        self.samples_per_frame = (TARGET_FRAME_MS / per_sample.max(0.05)).clamp(1.0, 64.0) as u32;
        self.accum_wall += wall;
        self.accum_gpu_ms += self.last_gpu_ms;
        self.accum_measured += add;
    }

    /// 視点を止めて積んでいる間、RATE_WINDOW ごとに実効のサンプルの速さ（表示・ノイズ除去・待ちも含めて 1 秒に積めた数）を
    /// ログに出す。accum_wall は流してから完了に気づくまでの時間の合計で、次を流すまでの空きを含まないので、別に測る。
    /// 動かした・一時停止・積み終えた・動画のときは区間を始め直す（速度改善の前後の比較用。perf_design.md の M0）
    fn log_throughput(&mut self, samples: u32) {
        let still = !self.st.paused && samples < self.st.max_spp && samples >= self.rate_samples && self.video.is_none() && self.last_change_at.elapsed() >= Duration::from_millis(300);
        let elapsed = self.rate_since.elapsed();
        if still && elapsed >= RATE_WINDOW {
            let n = samples - self.rate_samples;
            let (w, h) = self.gpu.as_ref().map_or((0, 0), |g| (g.prog.targets().width, g.prog.targets().height));
            info!("viewer: {:.1} spp/s ({} spp in {:.1} s, {:.1} ms/spp, render {}x{}, {} spp per batch)", n as f64 / elapsed.as_secs_f64(), n, elapsed.as_secs_f64(), elapsed.as_secs_f64() * 1000.0 / n.max(1) as f64, w, h, self.samples_per_frame);
        }
        if !still || elapsed >= RATE_WINDOW {
            self.rate_since = Instant::now();
            self.rate_samples = samples;
        }
    }

    /// 流したサンプルが終わるまで待つ（累積の画像やフレームの値を CPU から読み書きする前に）
    fn settle_render(&mut self) {
        self.poll_render(true);
    }

    /// add サンプルをサンプル用のキューへ流す（完了は待たない。poll_render で確かめる）。
    /// 最後に、次のトーンマッピングの出力へ、今の表示の設定でトーンマッピングする
    fn submit_render(&mut self, add: u32) -> Result<(), String> {
        let (exposure, mode, glow) = (self.st.exposure, self.st.mode, self.st.glow_params());
        let g = self.gpu.as_mut().unwrap();
        let dev = &g.ctx.device;
        let cmd = g.render_cmd;
        let slot = (g.submitted_slot + 1) % TONE_SLOTS;
        let background = Background::for_display(g.prog.frame().transparent_background);
        unsafe {
            dev.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty()).map_err(|e| format!("{e:?}"))?;
            dev.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)).map_err(|e| format!("{e:?}"))?;
            if g.timestamps_ok {
                dev.cmd_reset_query_pool(cmd, g.timestamps, 0, 2);
                dev.cmd_write_timestamp2(cmd, vk::PipelineStageFlags2::ALL_COMMANDS, g.timestamps, 0);
            }
        }
        g.prog.record(cmd, add);
        unsafe {
            if g.timestamps_ok {
                dev.cmd_write_timestamp2(cmd, vk::PipelineStageFlags2::ALL_COMMANDS, g.timestamps, 1);
            }
        }
        // 積んだ累積を読んでトーンマッピングする（表示のキューは、この出力をタイムラインセマフォを待ってから貼る）
        let trace = g.ctx.trace_stages();
        memory_barrier(dev, cmd, trace | vk::PipelineStageFlags2::COMPUTE_SHADER, vk::AccessFlags2::SHADER_STORAGE_WRITE, vk::PipelineStageFlags2::COMPUTE_SHADER, vk::AccessFlags2::SHADER_STORAGE_READ);
        g.tone.record_to(cmd, slot, exposure, mode, g.prog.xyz_output(), &glow, background);
        let value = g.submitted + 1;
        unsafe {
            dev.end_command_buffer(cmd).map_err(|e| format!("{e:?}"))?;
            let (cmds, signal, values) = ([cmd], [g.timeline], [value]);
            let mut ts = vk::TimelineSemaphoreSubmitInfo::default().signal_semaphore_values(&values);
            let si = vk::SubmitInfo::default().command_buffers(&cmds).signal_semaphores(&signal).push_next(&mut ts);
            dev.queue_submit(g.ctx.render_queue, &[si], vk::Fence::null()).map_err(|e| format!("vkQueueSubmit: {e:?}"))?;
        }
        g.submitted = value;
        g.submitted_slot = slot;
        g.in_flight = Some(Batch { value, add, slot, t0: Instant::now() });
        Ok(())
    }

    /// このフレームでノイズ除去をかけたいか（流しているバッチがあればかけられないので、呼ぶ側が確かめる）。C++ 版と同じ間隔：
    /// GPU：動かしている間は毎フレーム fast、止まったら 150 ms ごとに high、収束時にもう一度。
    /// CPU：数秒かかり画面が止まるので、収束時（最大 spp に達したとき）と D キー／ボタンのときだけ high でかける
    fn denoise_wanted(&self, samples: u32) -> bool {
        let Some(g) = self.gpu.as_ref() else { return false };
        let on_cpu = g.denoiser.as_ref().is_some_and(|d| d.device_name() == "CPU");
        let moving = self.last_change_at.elapsed() < Duration::from_millis(300);
        let max_spp = self.st.max_spp;
        let due = if on_cpu { samples >= max_spp || self.st.req.denoise_now } else { moving || samples >= max_spp || self.last_denoise_at.elapsed() > Duration::from_millis(150) };
        self.st.denoise && g.denoiser.is_some() && self.st.mode == DisplayMode::Beauty && samples > 0 && (self.denoised_epoch != self.accum_epoch || self.denoised_samples != samples) && due
    }

    /// 流したサンプルが終わっていれば画像を描き直し、（必要なら）ノイズ除去して、画像と操作パネルを表示してから、次のサンプルを流す。
    /// サンプルを流している間は、前に描いた画像を見せる（累積の画像に触らない）。スワップチェーンを作り直すときは None
    fn record_and_present(&mut self, ui: &UiFrame, last_test_frame: bool, _frame_t0: Instant) -> Result<Option<Presented>, String> {
        self.poll_render(false);
        let (samples, busy) = {
            let g = self.gpu.as_ref().unwrap();
            (g.prog.samples(), g.in_flight.is_some())
        };
        self.log_throughput(samples);
        // 累積をやり直したら（サンプル数が減ったら）、前のノイズ除去の結果は使わない
        if samples < self.prev_samples {
            self.accum_epoch += 1;
        }
        if samples == 0 {
            // 累積をやり直した：経過時間と平均も 0 から
            self.accum_wall = Duration::ZERO;
            self.accum_gpu_ms = 0.0;
            self.accum_measured = 0;
        }
        let max_spp = self.st.max_spp;
        // 次に流すサンプルの数（一時停止中は積み足さない。カメラなどを変えて累積をやり直したときだけ、表示のために 1 サンプル描く）
        let mut add = if busy {
            0
        } else if self.st.paused {
            u32::from(samples == 0)
        } else if samples >= max_spp {
            0
        } else {
            self.samples_per_frame.min(max_spp - samples)
        };
        // 累積をやり直した直後（新しい視点をすぐ見せる）・動画・テストは、積み終えるのを待ってから表示する
        if add > 0 && (samples == 0 || self.video.is_some() || self.test.active()) {
            self.submit_render(add)?;
            self.settle_render();
            add = 0;
        }
        // 表示は累積の画像を読まない（バッチの最後のトーンマッピングの出力を貼る）ので、すぐ次を流す。
        // ただしノイズ除去をかけるフレームは、累積を読むので、表示の後に流す
        if add > 0 && !self.denoise_wanted(samples) {
            self.submit_render(add)?;
            add = 0;
        }
        let busy = self.gpu.as_ref().unwrap().in_flight.is_some();

        // 前の表示から画面の書き換えの間隔（の 85 %）が経つまで待つ。ノイズ除去などで遅れたフレームの後に続けて表示すると、
        // スワップチェーンの画像を使い切り、次の画像が戻るのを（サンプルを積んでいる GPU の後で）長く待つことになる
        if self.refresh_checked.elapsed() > Duration::from_secs(1) {
            self.refresh_checked = Instant::now();
            let mhz = self.window.as_ref().and_then(|w| w.current_monitor()).and_then(|m| m.refresh_rate_millihertz()).unwrap_or(60_000).max(1);
            self.frame_interval = Duration::from_secs_f64(0.85 * 1000.0 / mhz as f64);
        }
        // （テスト・隠した窓・動画の書き出しは待たない）
        let paced = !self.test.active() && !self.vo.hidden && self.video.is_none();
        if let Some(rest) = self.frame_interval.checked_sub(self.last_present_at.elapsed()).filter(|_| paced) {
            std::thread::sleep(rest);
        }
        let g = self.gpu.as_mut().unwrap();
        let loader = g.ctx.swapchain_loader.as_ref().unwrap();
        let (image_index, _) = match unsafe { loader.acquire_next_image(g.swap.handle, u64::MAX, g.acquired, vk::Fence::null()) } {
            Ok(r) => r,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                self.need_recreate = true;
                return Ok(None);
            }
            Err(e) => return Err(format!("vkAcquireNextImageKHR: {e:?}")),
        };

        // egui のテクスチャ
        for (id, deltas) in &ui.textures.set {
            for delta in deltas.iter() {
                g.egui_renderer.set_texture(g.ctx.queue, g.ctx.pool, *id, delta).map_err(|e| format!("egui texture: {e}"))?;
            }
        }

        // このフレームでノイズ除去するか（画像の表示のときだけ。サンプルを流している間はしない。denoise_wanted）
        let on_cpu = g.denoiser.as_ref().is_some_and(|d| d.device_name() == "CPU");
        let moving = self.last_change_at.elapsed() < Duration::from_millis(300);
        let samples = g.prog.samples();
        let beauty = self.st.mode == DisplayMode::Beauty;
        let want = !busy && self.denoise_wanted(samples);
        let g = self.gpu.as_mut().unwrap();
        if !busy {
            self.st.req.denoise_now = false;
        }
        let ctx = g.ctx;
        // 分光モードの累積は XYZ。OIDN にはリニア sRGB にして渡し、混ぜた結果は XYZ に戻す（トーンマッピングは XYZ として読む）
        let xyz = g.prog.xyz_output();
        if want {
            // 入力の準備を流して完了を待ち、OIDN で除去してから表示のコマンドを作る
            let d = g.denoiser.as_mut().unwrap();
            d.set_quality(if moving && !on_cpu { DenoiseQuality::Fast } else { DenoiseQuality::High });
            let d = &*d;
            ctx.submit_and_wait(|_, c| d.record_prepare(c, xyz))?;
            match d.execute() {
                Ok(ms) => {
                    self.last_denoise_ms = ms;
                    self.denoised_epoch = self.accum_epoch;
                    self.denoised_samples = samples;
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
        // サンプルを流している間は累積の画像を読まず、前に描いた画像をそのまま見せる
        // 背景を透明にしているなら、透明な所を市松模様にして見せる（累積しているフレームの設定で決める）
        let background = Background::for_display(g.prog.frame().transparent_background);
        let disp = if show_denoised {
            let t = g.tone_denoised.as_ref().unwrap();
            if !busy {
                // 結果を元の画像と強さで混ぜる（軽いので毎フレーム。強さのスライダーがすぐ効く）
                g.denoiser.as_ref().unwrap().record_output(cmd, self.st.denoise_strength, xyz);
                t.record(cmd, self.st.exposure, self.st.mode, xyz, &self.st.glow_params(), background);
            }
            t.output()
        } else {
            // 流していなければ、今の表示の設定でトーンマッピングし直す（一時停止中・積み終えた後も露出などがすぐ効く）。
            // 流している間は、終わった最新のバッチがトーンマッピングした画像をそのまま貼る
            if !busy {
                g.tone.record_to(cmd, g.shown_slot, self.st.exposure, self.st.mode, xyz, &self.st.glow_params(), background);
            }
            g.tone.output_at(g.shown_slot)
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
        g.egui_renderer.cmd_draw(cmd, extent, ui.pixels_per_point, &ui.primitives).map_err(|e| format!("egui draw: {e}"))?;
        unsafe { dev.cmd_end_rendering(cmd) };

        // テストの最後のフレーム：表示した画像（パネルも含む）を読み戻す
        let mut shot: Option<Buffer> = None;
        if last_test_frame && g.swap.can_read_back && self.test.screenshot().is_some() {
            match g.ctx.create_buffer(extent.width as u64 * extent.height as u64 * 4, vk::BufferUsageFlags::TRANSFER_DST, true, true) {
                Ok(b) => {
                    image_barrier(dev, cmd, swap_img, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT, vk::AccessFlags2::COLOR_ATTACHMENT_WRITE, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_READ, 1);
                    let region = vk::BufferImageCopy::default().image_subresource(sub).image_extent(vk::Extent3D { width: extent.width, height: extent.height, depth: 1 });
                    unsafe { dev.cmd_copy_image_to_buffer(cmd, swap_img, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, b.buffer, &[region]) };
                    memory_barrier(dev, cmd, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE, vk::PipelineStageFlags2::HOST, vk::AccessFlags2::HOST_READ);
                    image_barrier(dev, cmd, swap_img, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, vk::ImageLayout::PRESENT_SRC_KHR, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_READ, vk::PipelineStageFlags2::BOTTOM_OF_PIPE, vk::AccessFlags2::NONE, 1);
                    shot = Some(b);
                }
                Err(e) => error!("screenshot: {e}"),
            }
        }
        if shot.is_none() {
            image_barrier(dev, cmd, swap_img, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL, vk::ImageLayout::PRESENT_SRC_KHR, vk::PipelineStageFlags2::COLOR_ATTACHMENT_OUTPUT, vk::AccessFlags2::COLOR_ATTACHMENT_WRITE, vk::PipelineStageFlags2::BOTTOM_OF_PIPE, vk::AccessFlags2::NONE, 1);
        }
        unsafe { dev.end_command_buffer(cmd).map_err(|e| format!("{e:?}"))? };

        // 終わった最新のバッチ（completed）を待つ：CPU で確かめた値なので待たずに通るが、描画のキューが書いた出力・累積を
        // 表示のキューが読むための、キューの間のメモリの依存になる
        let wait = [g.acquired, g.timeline];
        let wait_values = [0, g.completed];
        let stages = [vk::PipelineStageFlags::TRANSFER, vk::PipelineStageFlags::TRANSFER | vk::PipelineStageFlags::COMPUTE_SHADER];
        let cmds = [cmd];
        let signal = [g.swap.render_done[image_index as usize]];
        let signal_values = [0];
        let mut ts = vk::TimelineSemaphoreSubmitInfo::default().wait_semaphore_values(&wait_values).signal_semaphore_values(&signal_values);
        let si = vk::SubmitInfo::default().wait_semaphores(&wait).wait_dst_stage_mask(&stages).command_buffers(&cmds).signal_semaphores(&signal).push_next(&mut ts);
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
        self.last_present_at = Instant::now();
        self.frames += 1;
        for id in &ui.textures.free {
            let _ = g.egui_renderer.free_texture(*id);
        }
        // 表示が累積の画像を読み終えたので、次のサンプルを流す（完了は待たない）
        if add > 0 {
            self.submit_render(add)?;
        }
        self.prev_samples = self.gpu.as_ref().unwrap().prog.samples();
        Ok(Some(Presented { denoised: show_denoised, screenshot: shot, extent }))
    }

    /// S キー（番号を付けた PNG と EXR）と「名前を付けて保存」
    fn handle_saves(&mut self, show_denoised: bool) {
        if !self.has_board() {
            // 基板が無いと保存先（基板のフォルダ）が決まらない
            if std::mem::take(&mut self.st.req.save) || self.save_as_target.take().is_some() {
                self.last_update = fl!("save-no-board");
            }
            return;
        }
        if self.st.req.save || self.save_as_target.is_some() {
            // 累積の画像を読み戻すので、流しているサンプルが終わるのを待つ
            self.settle_render();
        }
        if std::mem::take(&mut self.st.req.save) {
            self.save_numbered(show_denoised);
        }
        if let Some((format, path)) = self.save_as_target.take() {
            self.save_as(format, &path, show_denoised);
        }
    }

    /// index 番の S キーの保存のファイル
    fn save_paths(&self, index: u32) -> SavePaths {
        let file = |suffix: &str| self.out_dir.join(format!("{}_{index:03}{suffix}", self.stem));
        SavePaths { png: file(".png"), exr: file(".exr"), denoised_png: file("_denoised.png"), denoised_exr: file("_denoised.exr") }
    }

    /// 次の保存の番号：前の保存の次から、どのファイル（PNG・EXR・ノイズ除去版）も無い最小の番号（前に起動したときの画像を上書きしない）
    fn next_save_index(&self) -> u32 {
        (self.save_index..).find(|&i| !self.save_paths(i).any_exists()).unwrap_or(self.save_index)
    }

    /// 累積を番号を付けた PNG と EXR に書く。ノイズ除去した画像を表示しているなら、それも（表示と同じ強さで混ぜたもの）
    fn save_numbered(&mut self, show_denoised: bool) {
        let index = self.next_save_index();
        self.save_index = index + 1;
        let paths = self.save_paths(index);
        let g = self.gpu.as_ref().unwrap();
        let spp = g.prog.samples();
        let (exposure, glow) = (self.st.exposure, self.st.glow_params());
        let write = |img: &ImageF, png: &Path, exr: &Path| -> Result<(), String> {
            if let Some(dir) = png.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            image_io::save_png_tonemapped(png, img, exposure, &glow)?;
            image_io::save_exr(exr, img)
        };
        match read_image(g, false).and_then(|img| write(&img, &paths.png, &paths.exr)) {
            Ok(()) => {
                info!("saved {} / {} ({} spp)", paths.png.display(), paths.exr.display(), spp);
                let shown = std::path::absolute(&paths.png).unwrap_or(paths.png.clone());
                self.last_update = fl!("saved", path = shown.display().to_string(), spp = spp.to_string());
            }
            Err(e) => {
                error!("save failed: {}", e);
                self.last_update = fl!("save-failed", error = e);
            }
        }
        if show_denoised {
            match read_image(g, true).and_then(|img| write(&img, &paths.denoised_png, &paths.denoised_exr)) {
                Ok(()) => {
                    info!("saved {} / {} (denoised, strength {:.2})", paths.denoised_png.display(), paths.denoised_exr.display(), self.st.denoise_strength);
                    self.last_update.push_str(&fl!("saved-denoised-too"));
                }
                Err(e) => error!("save failed: {}", e),
            }
        }
    }

    /// 表示している画像（ノイズ除去した画像を表示していればそれ）を、選ばれた場所に書く
    fn save_as(&mut self, format: SaveFormat, path: &Path, show_denoised: bool) {
        let g = self.gpu.as_ref().unwrap();
        let spp = g.prog.samples();
        let r = read_image(g, show_denoised).and_then(|img| match format {
            SaveFormat::Png => image_io::save_png_tonemapped(path, &img, self.st.exposure, &self.st.glow_params()),
            SaveFormat::Exr => image_io::save_exr(path, &img),
        });
        match r {
            Ok(()) => {
                info!("saved {} ({} spp{})", path.display(), spp, if show_denoised { ", denoised" } else { "" });
                self.last_update = fl!("saved-as", path = path.display().to_string(), spp = spp.to_string(), denoised = if show_denoised { fl!("saved-as-denoised") } else { String::new() });
            }
            Err(e) => {
                error!("save failed: {} ({})", e, path.display());
                self.last_update = fl!("save-failed", error = e);
            }
        }
    }

    /// テストの最後のフレーム：計測をログに出し、表示した画像を保存して終える
    fn finish_test(&mut self, shot: Option<Buffer>, extent: vk::Extent2D) {
        let g = self.gpu.as_ref().unwrap();
        info!("test: {} frames, {} spp, last frame {:.1} ms (GPU {:.1} ms, {} spp/frame), swapchain {}x{} format {:?}, environment {}", self.frames, g.prog.samples(), self.last_frame_ms, self.last_gpu_ms, self.samples_per_frame, extent.width, extent.height, g.swap.format, self.env_path);
        info!("test: render {}x{}, 3D view {}x{}, display {}x{} at ({}, {}), ratio {}", g.prog.targets().width, g.prog.targets().height, self.st.viewport.w, self.st.viewport.h, self.st.display.w, self.st.display.h, self.st.display.x, self.st.display.y, self.st.render_ratio);
        info!("test: rendering {:.2} s for {} spp, average {:.1} ms/spp (GPU {:.1} ms/spp)", self.accum_wall.as_secs_f64(), self.accum_measured, self.accum_wall.as_secs_f64() * 1000.0 / self.accum_measured.max(1) as f64, self.accum_gpu_ms / self.accum_measured.max(1) as f64);
        let Some(p) = self.test.screenshot().cloned() else {
            if let Some(mut b) = shot {
                g.ctx.destroy_buffer(&mut b);
            }
            self.done = true;
            return;
        };
        let r = match shot {
            Some(mut b) => {
                // 確保した大きさは求めた大きさより大きいことがあるので、画像の分（mapped_bytes）だけを使う
                let px = b.mapped_bytes().map(<[u8]>::to_vec);
                g.ctx.destroy_buffer(&mut b);
                px.ok_or_else(|| "the readback buffer is not host-visible".to_string()).and_then(|mut px| {
                    if g.swap.format == vk::Format::B8G8R8A8_UNORM {
                        for c in px.chunks_exact_mut(4) {
                            c.swap(0, 2);
                        }
                    }
                    for c in px.chunks_exact_mut(4) {
                        c[3] = 255;
                    }
                    if let Some(dir) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    image_io::save_png_rgba8(&p, extent.width, extent.height, &px)
                })
            }
            None => Err("the displayed image could not be read back".into()),
        };
        match r {
            Ok(()) => info!("wrote {}", p.display()),
            Err(e) => {
                error!("{}: {}", p.display(), e);
                self.rc = 1;
            }
        }
        self.done = true;
    }
}

impl ApplicationHandler<crate::web_ui::Wake> for App {
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
            WindowEvent::DroppedFile(path) => self.open_dropped(vec![path]),
            WindowEvent::ModifiersChanged(m) => self.st.ctrl = m.state().control_key(),
            WindowEvent::MouseInput { state, button, .. } => {
                let down = state == ElementState::Pressed;
                // メニューのプルダウンの外（3D 表示）を押した：閉じるだけにする（WebView のフォーカスはこのウィンドウへ移らないので、ページは気づかない）
                if down && self.web_menu.is_some() {
                    self.close_web_menu();
                    return;
                }
                // 「について」の外を押した：閉じるだけ
                if down && self.about_open && self.web.is_some() {
                    self.about_open = false;
                    return;
                }
                if down && (response.consumed || !self.st.viewport.contains(self.st.last_cursor)) {
                    return; // メニュー・パネルの上での操作はカメラに渡さない
                }
                if down && button == MouseButton::Left && self.st.ctrl {
                    if !self.render_locked() {
                        self.st.req.focus_pick = Some(self.st.last_cursor);
                    }
                    return;
                }
                match button {
                    MouseButton::Left => {
                        self.st.rotating = down;
                        if down {
                            self.st.press_at = Some(self.st.last_cursor);
                            self.st.press_moved = 0.0;
                        } else if self.st.press_at.take().is_some() && self.st.press_moved < 4.0 {
                            self.st.req.part_pick = Some(self.st.last_cursor);
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
                if self.render_locked() {
                    // 描画ロック中は視点を動かさない（クリックで部品を選ぶのはできる）
                } else if self.st.rotating {
                    self.st.azimuth -= dx * 0.3;
                    self.st.elevation = (self.st.elevation + dy * 0.3).clamp(-90.0, 90.0);
                    self.st.changed.frame = true;
                } else if self.st.panning {
                    let c = orbit_camera(self.st.target, self.st.distance, self.st.elevation, self.st.azimuth, self.st.fov, 1.0);
                    let scale = 2.0 * self.st.distance * self.st.tan_half_fov_y / self.st.display.h.max(1) as f32;
                    self.st.target += (-c.right * dx as f32 + c.up * dy as f32) * scale;
                    self.st.changed.frame = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if response.consumed || !self.st.viewport.contains(self.st.last_cursor) || self.render_locked() {
                    return;
                }
                let yoff = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as f64,
                    MouseScrollDelta::PixelDelta(p) => p.y / 40.0,
                };
                self.st.distance = (self.st.distance * 0.9f64.powf(yoff) as f32).max(1.0);
                self.st.changed.frame = true;
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed || response.consumed {
                    return;
                }
                let PhysicalKey::Code(code) = event.physical_key else { return };
                let Some(action) = key_action(code) else { return };
                if self.video.is_some() && action != KeyAction::Quit {
                    return;
                }
                if self.apply_key_action(action) {
                    el.exit();
                }
            }
            WindowEvent::RedrawRequested => {} // 描画は about_to_wait で行う
            _ => {}
        }
    }

    fn user_event(&mut self, _el: &ActiveEventLoop, _wake: crate::web_ui::Wake) {
        // WebView の操作か基板の保存の通知で起きた。描画は about_to_wait で行う（そこで操作・保存を受け取る）
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        // 保存の通知が届いたら、待っている間でもすぐ起きる（開き直した基板の見張りにも付ける）
        if let (Some(w), Some(p)) = (self.watcher.as_mut(), self.proxy.as_ref()) {
            if w.uses_events() && !w.has_waker() {
                let p = p.clone();
                w.set_waker(move || {
                    let _ = p.send_event(crate::web_ui::Wake);
                });
            }
        }
        self.about_to_wait_impl(el);
    }
}

impl App {
    /// キーの操作をする。終了なら true
    fn apply_key_action(&mut self, action: KeyAction) -> bool {
        let st = &mut self.st;
        {
            {
                match action {
                    KeyAction::Quit => return true,
                    KeyAction::TogglePanel => st.show_panel = !st.show_panel,
                    KeyAction::ResetView => st.req.reset_view = true,
                    KeyAction::FitView => st.req.fit_view = true,
                    KeyAction::Save => st.req.save = true,
                    KeyAction::Pause => st.paused = !st.paused,
                    KeyAction::DenoiseNow => st.req.denoise_now = true,
                    KeyAction::Exposure(ev) => st.exposure += ev, // 露出は表示だけなので累積はやり直さない
                    KeyAction::RotateEnvironment(deg) => {
                        st.env_rotation_deg += deg;
                        st.changed.frame = true;
                    }
                    KeyAction::Mode(m) => st.mode = m,
                    KeyAction::MaterialEditor => self.editor.open = !self.editor.open,
                    KeyAction::OpenBoard => st.req.open_board = true,
                    KeyAction::Lock => st.locked = !st.locked,
                }
            }
        }
        false
    }

    fn about_to_wait_impl(&mut self, el: &ActiveEventLoop) {
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
        let idle = self.gpu.as_ref().map(|g| g.in_flight.is_none() && (g.prog.samples() >= self.st.max_spp || (self.st.paused && g.prog.samples() > 0))).unwrap_or(false) && !self.st.changed.frame && self.env_job.is_none() && self.reload_job.is_none() && !self.text_job.busy() && !self.text_dirty && !self.test.active() && !self.editor.busy()
            && self.web.as_ref().is_none_or(|w| w.moved().elapsed() > Duration::from_millis(300));
        if idle {
            // 収束したら GPU を休ませ、入力と保存を待つ（保存の通知で起きられなければ、100 ms ごとに起きて調べる）
            let wait = if self.watcher.as_ref().is_some_and(|w| !w.has_waker()) || self.prep.scene.pending_models > 0 { 100 } else { 250 };
            el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(wait)));
        } else {
            el.set_control_flow(ControlFlow::Poll);
        }
    }
}

/// 基板（または基板なし）の状態からビューアを作る（ウィンドウと GPU は、まだ無い）
fn new_app(mut vo: ViewOptions, prep: PreparedScene) -> App {
    let splash = !(vo.hidden || vo.test.frames > 0);
    let cfg = &prep.cfg;
    let win = remember::window_state();
    let mut st = ViewState {
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
        max_spp: DEFAULT_MAX_SPP,
        mode: DisplayMode::Beauty,
        show_panel: win.panel_shown,
        mask_choice: mask_choice_of(&cfg.scene),
        paused: vo.paused,
        locked: false,
        focus_point: None,
        show_smd: true,
        show_tht: true,
        show_solder: true,
        spectral: cfg.spectral,
        transparent: cfg.transparent_background,
        rotating: false,
        panning: false,
        press_at: None,
        press_moved: 0.0,
        denoise: false,
        denoise_strength: 1.0,
        last_cursor: (0.0, 0.0),
        ctrl: false,
        viewport: Viewport { x: 0, y: 0, w: 1, h: 1 },
        display: Viewport { x: 0, y: 0, w: 1, h: 1 },
        size_mode: None,
        render_ratio: 1.0,
        tan_half_fov_y: 0.27,
        req: Requests::default(),
        changed: Changes { frame: true, ..Default::default() },
    };
    // 描画のサイズ・比率・最大 spp・ノイズ除去・表示の種類：コマンドライン、覚えたもの、既定の順
    ViewerSettings::new(&vo, &prep.remembered_view).apply(&mut st);
    let language_choice = remember::ui_language();
    let language = i18n::select(vo.language.as_deref(), language_choice.as_deref());
    let out_png = vo.common.out.clone();
    let out_dir = out_png.parent().filter(|p| !p.as_os_str().is_empty()).map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let stem = out_png.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "view".into());
    let env_path = cfg.hdr.clone();
    let env_list = list_environments(&env_path);
    let watcher = (vo.watch && !prep.board.file.as_os_str().is_empty()).then(|| FileWatcher::with_defaults(&prep.board.file));
    if watcher.is_some() {
        info!("watching {} for saves", prep.board.file.display());
    }
    let models_seen = prep.source.as_ref().map(|s| s.library.completed()).unwrap_or(0);
    let material_files = MaterialFilesWatch::new(&prep.board.file);
    let library = prep.source.as_ref().map(|s| s.materials.library.clone()).unwrap_or_else(MaterialLibrary::builtin);
    let text_job = StrokeTextJob::new(prep.kicad_cli.clone(), &prep.board.file, &prep.board.project_name);
    let test = TestHooks { o: std::mem::take(&mut vo.test), ..Default::default() };
    let mut app = App {
        base_title: if prep.board.file.as_os_str().is_empty() { "KRB Renderer".to_string() } else { format!("KRB Renderer - {}", prep.board.project_name) },
        scene_as_configured: prep.configured.scene.clone(),
        vo,
        prep,
        st,
        window: None,
        web: None,
        web_started: None,
        web_failures: (0, None),
        web_missing: false,
        splash,
        recent_boards: Vec::new(),
        video: None,
        video_dialog: video_job::VideoDialog::default(),
        video_test_exit: false,
        proxy: None,
        web_rects: [egui::Rect::NOTHING; 8],
        about_open: false,
        about_json: None,
        editor_rect: None,
        web_menu: None,
        web_menu_closed: None,
        web_tip: None,
        egui_ctx: egui::Context::default(),
        egui_state: None,
        gpu: None,
        env_path,
        env_list,
        open_dialog: None,
        save_dialog: None,
        save_as_target: None,
        env_job: None,
        board_dialog: None,
        board_job: None,
        env_error: String::new(),
        watcher,
        reload_job: None,
        saved_reloads: 0,
        models_seen,
        text_job,
        text_dirty: false,
        selected: None,
        outline: None,
        materials_dirty_files: false,
        material_files,
        assign_scope: AssignScope::Part,
        library,
        editor: MaterialEditor::default(),
        editor_actions: Vec::new(),
        plugin_window: PluginWindow::default(),
        scene_epoch: 0,
        show_outline: true,
        material_error: String::new(),
        emit_edit: None,
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
        rate_since: Instant::now(),
        rate_samples: 0,
        last_change_at: Instant::now(),
        last_frame_ms: 0.0,
        last_present_at: Instant::now(),
        frame_interval: Duration::ZERO,
        refresh_checked: Instant::now().checked_sub(Duration::from_secs(2)).unwrap_or_else(Instant::now),
        last_gpu_ms: 0.0,
        view_seen: remember::ViewValues::new(),
        view_saved: remember::ViewValues::new(),
        view_changed_at: Instant::now(),
        win_start: win.clone(),
        win_seen: win.clone(),
        win_saved: win.clone(),
        win_changed_at: Instant::now(),
        panel_width: win.panel_width.unwrap_or(DEFAULT_PANEL_WIDTH),
        panel_open: win.sections.clone(),
        fit_frames: 0,
        language,
        language_choice,
        out_dir,
        stem,
        test,
        rc: 0,
        done: false,
    };
    app.view_seen = app.current_view();
    app.view_saved = app.view_seen.clone();
    app
}

pub fn run_viewer(vo: ViewOptions) -> anyhow::Result<i32> {
    // 基板があれば、ウィンドウを先に出し、基板は裏で読む（読み込みの間に、ウィンドウと GPU の準備も進む）。
    // テストモードは、決まったフレームで操作を流し込むので、これまでどおり読み終えてから始める
    let pcb = vo.common.pcb.clone();
    let background = !pcb.as_os_str().is_empty() && vo.test.frames == 0 && !vo.hidden;
    // 登録済みの KiCad のプラグインを直す（フォルダを移した・krbr.exe を更新した）。テストでは KiCad のフォルダに触らない
    if vo.test.frames == 0 && !vo.hidden {
        std::thread::spawn(kicad_plugin::repair_installed);
    }
    let prep = if background {
        // 読み込みの間は仮の状態（--hdr は基板を読んだときに使う）
        Some(crate::prepare::prepare_placeholder(&RenderOptions { pcb: PathBuf::new(), ..vo.common.clone() }))
    } else if pcb.as_os_str().is_empty() {
        prepare_empty(&RenderOptions { pcb: PathBuf::new(), ..vo.common.clone() })
    } else {
        prepare_scene(&vo.common)
    };
    let Some(prep) = prep else { anyhow::bail!("scene preparation failed") };
    // 引数なしで起動したときに開く基板（テストでは覚えない）
    if vo.test.frames == 0 && !vo.hidden && !prep.board.file.as_os_str().is_empty() {
        remember::remember_last_board(&prep.board.file);
    }
    let mut app = new_app(vo, prep);
    if background {
        app.start_board_load(pcb, true);
    }
    let event_loop = EventLoop::<crate::web_ui::Wake>::with_user_event().build().map_err(|e| anyhow::anyhow!("event loop: {e}"))?;
    // WebView の操作と、基板の保存の通知で、眠っているイベントループを起こす
    app.proxy = Some(event_loop.create_proxy());
    event_loop.run_app(&mut app).map_err(|e| anyhow::anyhow!("event loop: {e}"))?;
    app.flush_pending_edits();
    // GPU の資源はコンテキストより先に捨てる
    if let Some(g) = app.gpu.take() {
        let ctx = g.ctx;
        ctx.wait_idle();
        unsafe {
            ctx.device.destroy_query_pool(g.timestamps, None);
            ctx.device.destroy_semaphore(g.timeline, None);
            ctx.device.destroy_fence(g.fence, None);
            ctx.device.destroy_semaphore(g.acquired, None);
        }
        app.editor.release_gpu();
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
    app.save_view(true);
    Ok(app.rc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_size_and_ratio_options() {
        assert_eq!(parse_render_size("fit"), Ok(RenderSize::Fit));
        assert_eq!(parse_render_size(" 1920x1080 "), Ok(RenderSize::Fixed(1920, 1080)));
        assert_eq!(parse_render_size("1080×1350"), Ok(RenderSize::Fixed(1080, 1350)));
        assert!(parse_render_size("1920x").is_err());
        assert!(parse_render_size("0x100").is_err());
        assert!(parse_render_size(&format!("{}x10", remember::MAX_RENDER_SIZE + 1)).is_err());
        assert_eq!(parse_render_ratio("0.5"), Ok(0.5));
        assert!(parse_render_ratio("8").is_err() && parse_render_ratio("x").is_err());
        assert!(RATIOS.iter().all(|r| RENDER_RATIO_RANGE.contains(r)));
    }

    /// キーの割り当て：同じキーを 2 つの操作に割り当てていない。ヘルプの説明が英語の .ftl にある
    #[test]
    fn key_bindings_are_unique_and_have_help() {
        let keys: Vec<KeyCode> = KEY_BINDINGS.iter().flat_map(|b| b.keys.iter().map(|(k, _)| *k)).collect();
        for (i, k) in keys.iter().enumerate() {
            assert!(!keys[i + 1..].contains(k), "{k:?} is bound twice");
        }
        for b in KEY_BINDINGS {
            assert!(i18n::english_has(b.help), "{} is not in the English .ftl", b.help);
        }
        assert_eq!(key_action(KeyCode::KeyF), Some(KeyAction::FitView));
        assert_eq!(key_action(KeyCode::KeyR), Some(KeyAction::ResetView));
        assert_eq!(key_action(KeyCode::NumpadSubtract), Some(KeyAction::Exposure(-EXPOSURE_STEP_EV)));
        assert_eq!(key_action(KeyCode::KeyZ), None);
    }

    /// 長さの表示：大きさに合わせて単位と桁数を変える
    #[test]
    fn length_formatting() {
        assert_eq!(format_length(0.35), "350 µm");
        assert_eq!(format_length(0.0042), "4.20 µm");
        assert_eq!(format_length(4.25), "4.25 mm");
        assert_eq!(format_length(42.5), "42.5 mm");
        assert_eq!(format_length(186.2), "186 mm");
        assert_eq!(format_length(2350.0), "2.35 m");
        assert_eq!(format_length(12.5e6), "12.5 km");
        assert_eq!(format_length(-1.5), "-1.50 mm");
        assert_eq!(format_length(0.0), "0 mm");
    }

    /// ピントの点はカメラを動かしても同じ点：距離はカメラの前方向に測る。基板が画面の外かの判定
    #[test]
    fn focus_point_distance_and_out_of_view() {
        let cam = orbit_camera(Vec3::ZERO, 100.0, 90.0, 0.0, 30.0, 1.5);
        assert!((focus_distance_to(Vec3::ZERO, &cam) - 100.0).abs() < 1e-3);
        assert!((focus_distance_to(Vec3::new(0.0, 20.0, 0.0), &cam) - 80.0).abs() < 1e-3);
        assert_eq!(focus_distance_to(Vec3::new(0.0, 500.0, 0.0), &cam), 1.0); // カメラの後ろ
        let near = [Vec3::new(-10.0, 0.0, -10.0), Vec3::new(10.0, 0.0, 10.0)];
        assert!(!points_out_of_view(&cam, &near));
        let aside = [Vec3::new(500.0, 0.0, 0.0), Vec3::new(520.0, 0.0, 10.0)];
        assert!(points_out_of_view(&cam, &aside));
        // 画面より大きく、両側にはみ出す（真ん中は写っている）なら外ではない
        let around = [Vec3::new(-500.0, 0.0, 0.0), Vec3::new(500.0, 0.0, 0.0)];
        assert!(!points_out_of_view(&cam, &around));
        // 遠くから見ると小さい（基板 20 mm を 10 m から：画面の高さの約 0.4 %）
        let far = orbit_camera(Vec3::ZERO, 10_000.0, 90.0, 0.0, 30.0, 1.5);
        assert!(projected_extent(&far, &near) < TOO_SMALL_EXTENT && projected_extent(&cam, &near) > 0.2);
        let behind = orbit_camera(Vec3::new(0.0, 0.0, 0.0), 100.0, -90.0, 180.0, 30.0, 1.5);
        assert!(points_out_of_view(&behind, &[Vec3::new(0.0, -200.0, 0.0)]));
    }

    /// 内蔵の環境マップの表示名とヒントが英語の .ftl にある
    #[test]
    fn builtin_environment_has_a_name() {
        let EnvironmentName::Builtin(id) = describe_environment(STUDIO) else { panic!("{STUDIO} is not built in") };
        assert!(i18n::english_has(&format!("env-builtin-{id}")) && i18n::english_has(&format!("env-builtin-{id}-tip")));
    }

    /// パネルの部品（panel.html に渡す JSON）のうち、値を持つもの（id, 部品）と、ボタンの id
    fn panel_controls(json: &str) -> (Vec<(String, serde_json::Value)>, Vec<String>) {
        fn walk(v: &serde_json::Value, ctl: &mut Vec<(String, serde_json::Value)>, btn: &mut Vec<String>) {
            match v {
                serde_json::Value::Object(o) => {
                    let t = o.get("t").and_then(|t| t.as_str()).unwrap_or("");
                    if let (true, Some(id)) = (matches!(t, "slider" | "check" | "radio" | "select"), o.get("id").and_then(|i| i.as_str())) {
                        ctl.push((id.to_string(), v.clone()));
                    }
                    if t == "buttons" {
                        btn.extend(o["items"].as_array().into_iter().flatten().filter_map(|b| b["id"].as_str().map(str::to_string)));
                    }
                    o.values().for_each(|c| walk(c, ctl, btn));
                }
                serde_json::Value::Array(a) => a.iter().for_each(|c| walk(c, ctl, btn)),
                _ => {}
            }
        }
        let (mut ctl, mut btn) = (Vec::new(), Vec::new());
        walk(&serde_json::from_str(json).unwrap(), &mut ctl, &mut btn);
        (ctl, btn)
    }

    /// 部品の今の値と違う値（スライダーは範囲の中ほど、チェックは反対、選択肢は別のもの）。違う値が無ければ None
    fn another_value(c: &serde_json::Value) -> Option<serde_json::Value> {
        let options = || -> Vec<serde_json::Value> {
            match c["t"].as_str() {
                Some("radio") => c["options"].as_array().cloned().unwrap_or_default().into_iter().map(|o| o["v"].clone()).collect(),
                _ => c["groups"].as_array().into_iter().flatten().flat_map(|g| g["options"].as_array().cloned().unwrap_or_default()).map(|o| o["v"].clone()).collect(),
            }
        };
        match c["t"].as_str()? {
            "slider" => {
                let (lo, hi, v) = (c["min"].as_f64()?, c["max"].as_f64()?, c["value"].as_f64()?);
                let at = |f: f64| if c["log"].as_bool() == Some(true) { lo * (hi / lo).powf(f) } else { lo + (hi - lo) * f };
                let pick = |f: f64| if c["int"].as_bool() == Some(true) { at(f).round() } else { at(f) };
                let x = if (pick(0.6) - v).abs() > 1e-6 * hi.abs().max(1.0) { pick(0.6) } else { pick(0.3) };
                Some(serde_json::json!(x))
            }
            "check" => Some(serde_json::json!(!c["value"].as_bool()?)),
            _ => options().into_iter().find(|o| *o != c["value"]),
        }
    }

    /// --web-ui の設定パネル：出ている部品をすべて操作すると、パネルの値が変わり、描き直すものは GPU へ送る印（changed）が立つ。
    /// 処理し忘れた部品（apply_panel_set に無い id）は値が変わらないので見つかる。最後に、変えた値を覚えて読み直すと同じ値に戻ること
    #[test]
    fn every_web_panel_control_is_applied_and_remembered() {
        let _g = crate::config_lock();
        let ws = std::env::temp_dir().join(format!("krbr_web_panel_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::env::set_var("KRBR_CONFIG_DIR", ws.join("config"));
        let prep = crate::prepare::prepare_placeholder(&RenderOptions::default());
        let mut app = new_app(ViewOptions::default(), prep);

        // 描き直すもの：その id を変えたら立つ印
        let expect = |id: &str| -> &'static str {
            match id {
                "env.intensity" | "env.rotation" | "env.transparent" | "camera.fov" | "color.spectral" => "frame",
                id if id.starts_with("lens.") => "frame",
                "surface.bump" | "surface.substrate" => "materials",
                "surface.mask" => "mask",
                "integrator" => "integrator",
                _ => "", // 表示だけのもの（露出・グロー・ノイズ除去・表示の種類・描画のサイズ・最大 spp）は、表示するときに読む
            }
        };
        let mut tested = std::collections::BTreeSet::new();
        let mut buttons = std::collections::BTreeSet::new();
        // 部品は選んだものに応じて増える（薄レンズの細かい設定、羽根の枚数で出る項目など）ので、新しい部品が無くなるまで繰り返す
        for _ in 0..6 {
            let (controls, btn) = panel_controls(&app.panel_json());
            buttons.extend(btn);
            let new: Vec<_> = controls.into_iter().filter(|(id, _)| !tested.contains(id)).collect();
            if new.is_empty() {
                break;
            }
            for (id, c) in new {
                tested.insert(id.clone());
                let Some(v) = another_value(&c) else { continue }; // 選択肢が 1 つだけ（環境マップの一覧など）
                app.st.changed = Changes::default();
                app.apply_panel_set(&id, &v);
                let flag = match expect(&id) {
                    "frame" => app.st.changed.frame,
                    "materials" => app.st.changed.materials,
                    "mask" => app.st.changed.mask,
                    "integrator" => app.st.changed.integrator,
                    _ => true,
                };
                assert!(flag, "{id}: the change is not sent to the GPU ({})", expect(&id));
                let (after, _) = panel_controls(&app.panel_json());
                let Some((_, now)) = after.iter().find(|(i, _)| *i == id) else { continue }; // 変えた値でその部品が隠れた
                let same = match (now["value"].as_f64(), v.as_f64()) {
                    (Some(a), Some(b)) => (a - b).abs() <= 1e-3 * b.abs().max(1.0),
                    _ => now["value"] == v,
                };
                assert!(same, "{id}: set {v}, the panel shows {}", now["value"]);
            }
        }
        println!("web panel: {} controls, {} buttons: {:?}", tested.len(), buttons.len(), tested);
        for id in ["lens.f", "lens.focus", "lens.blades", "lens.blade_rot", "env.intensity", "glow.threshold", "surface.mask", "display.max_spp", "integrator"] {
            assert!(tested.contains(id), "{id} was never shown");
        }

        // ボタン：env.open はファイルを選ぶダイアログを開くので押さない（id は処理されている）
        for id in &buttons {
            if id == "env.open" {
                continue;
            }
            assert!(app.apply_panel_click(id), "{id}: the button is not handled");
        }
        assert!(app.st.req.reset_view && app.st.req.fit_view && app.st.req.save && app.st.focus_point.is_none() && app.st.exposure == 0.0);

        // スライダーはどれも［−］［＋］の最小単位（step）と Shift の単位（big）を持つ。露出は 1/3 段、Shift で 1 段
        let json: serde_json::Value = serde_json::from_str(&app.panel_json()).unwrap();
        let (controls, _) = panel_controls(&app.panel_json());
        for (id, c) in controls.iter().filter(|(_, c)| c["t"] == "slider") {
            let (step, big, range) = (c["step"].as_f64().unwrap_or(0.0), c["big"].as_f64().unwrap_or(0.0), c["max"].as_f64().unwrap() - c["min"].as_f64().unwrap());
            assert!(step > 0.0 && big >= step && step <= range / 10.0, "{id}: step {step}, big {big}, range {range}");
        }
        let ev = &controls.iter().find(|(id, _)| id == "exposure.ev").unwrap().1;
        assert!((ev["step"].as_f64().unwrap() - 1.0 / 3.0).abs() < 1e-9 && ev["big"] == 1.0);

        // グループは作業の流れの順。グループと詳細は開閉でき、開閉は覚える（ウィンドウの状態）
        let ids: Vec<&str> = json["sections"].as_array().unwrap().iter().filter_map(|s| s["id"].as_str()).collect();
        assert_eq!(ids, ["board", "lighting", "camera", "lens", "finish", "render", "help"]);
        assert!(app.section_open("board") && !app.section_open("lens.details") && !app.section_open("finish.glow"));
        assert!(app.apply_panel_click("section:board") && app.apply_panel_click("section:lens.details"));
        assert!(!app.section_open("board") && app.section_open("lens.details"));
        let json: serde_json::Value = serde_json::from_str(&app.panel_json()).unwrap();
        assert_eq!(json["sections"][0]["open"], false);
        let w = app.with_panel_state(app.win_seen.clone());
        assert_eq!((w.sections.get("board"), w.sections.get("lens.details")), (Some(&false), Some(&true)));

        // 覚える値の往復：覚えた値を既定の設定に当てると、同じ値になる
        let v = app.current_view();
        let mut cfg = krbr_scene::ProjectConfig::default();
        remember::apply_view_values(&mut cfg, &v);
        for (k, val) in remember::ViewSettings::of(&cfg).values() {
            if let Some(saved) = v.get(&k) {
                assert_eq!(saved, &val, "{k}: remembered {saved}, read back {val}");
            }
        }
        let viewer = remember::ViewerValues::of(&v);
        assert_eq!((viewer.max_spp, viewer.denoise, viewer.render_ratio), (Some(app.st.max_spp), Some(app.st.denoise), Some(app.st.render_ratio)));
        assert_eq!(viewer.display_mode.as_deref(), Some(display_mode_name(app.st.mode)));
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// テストモード（--test-frames）では、パネルの設定を基板の設定ファイルに書かない（以前は書いて、後の krbr render の結果を変えていた）。
    /// テストでなければ書く
    #[test]
    fn test_runs_do_not_remember_the_panel() {
        let _g = crate::config_lock();
        let ws = std::env::temp_dir().join(format!("krbr_test_mode_remember_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).unwrap();
        std::env::set_var("KRBR_CONFIG_DIR", ws.join("config"));
        let pcb = ws.join("b.kicad_pcb");
        for test_mode in [true, false] {
            let mut prep = crate::prepare::prepare_placeholder(&RenderOptions::default());
            prep.board.file = pcb.clone();
            let vo = ViewOptions { test: TestOptions { frames: u32::from(test_mode) * 10, ..Default::default() }, ..Default::default() };
            let mut app = new_app(vo, prep);
            app.st.exposure = 2.5;
            app.save_view(true);
            let text = std::fs::read_to_string(board_file::resolve(&pcb)).unwrap_or_default();
            assert_eq!(text.contains("exposure_ev = 2.5"), !test_mode, "test mode {test_mode}: {text}");
        }
        let _ = std::fs::remove_dir_all(&ws);
    }
}
