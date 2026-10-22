//! 覚えておくもの：
//!   基板ごと   最後に使った環境マップとパネルの設定（基板の設定ファイル `<名前>.krbr` の [remembered]。krbr_scene::board_file）
//!   基板によらない  最近使った環境マップの一覧、ウィンドウの状態（`%APPDATA%\krbr\state.toml`）
//! `<名前>.krbr` の [remembered] 以外の表（手で書く設定）は書き換えない。
//! どの項目も、覚えたときの設定ファイルの値を一緒に残し、後で設定ファイルの方が書き換えられたら、そちらを優先する。
//! 以前は基板ごとのものも state.toml の [boards."<パス>"] に置いていた。見つけたら `<名前>.krbr` へ移す。

use std::path::{Path, PathBuf};

use krbr_core::{info, warn};
use krbr_scene::board_file;
use krbr_scene::config::{apply_mask_color_spec, ProjectConfig};
use krbr_scene::{IntegratorKind, LensModel, LensParams};
use toml::{Table, Value};

const MAX_RECENT: usize = 12;

/// 描画のサイズの上限（幅・高さとも）。多くのデスクトップ GPU の maxImageDimension2D（16384）に合わせる。
/// これより大きいと累積の画像を作れない（覚えた値・--render-size のどちらもこれで確かめる）
pub const MAX_RENDER_SIZE: u32 = 16384;

/// 描画のサイズとして使える大きさか（1 .. MAX_RENDER_SIZE）
pub fn valid_render_size(w: i64, h: i64) -> bool {
    (1..=MAX_RENDER_SIZE as i64).contains(&w) && (1..=MAX_RENDER_SIZE as i64).contains(&h)
}

/// 基板について覚えている環境マップ
#[derive(Debug, Clone, PartialEq)]
pub struct Remembered {
    pub hdr: String,
    /// 覚えたときの設定ファイルの [environment] hdr。後で設定ファイルの方が書き換えられたら、そちらを優先する
    pub config_hdr: String,
}

fn state_file() -> PathBuf {
    krbr_core::platform::config_dir().join("state.toml")
}

/// 環境マップの指定を、作業フォルダによらない形にする（内蔵の名前はそのまま）
pub fn normalize_hdr(spec: &str) -> String {
    if krbr_render::is_builtin(spec) {
        return spec.to_string();
    }
    std::path::absolute(spec).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| spec.to_string())
}

/// 同じ環境マップか（Windows では大文字・小文字と区切り文字の違いを無視する）。最近使った一覧の重なりの除去とビューアの一覧で使う
pub fn same_hdr(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.replace('\\', "/").eq_ignore_ascii_case(&b.replace('\\', "/"))
    } else {
        a == b
    }
}

fn load(file: &Path) -> Table {
    match std::fs::read_to_string(file) {
        Ok(s) => s.parse::<Table>().unwrap_or_else(|e| {
            warn!("{}: {}", file.display(), e);
            Table::new()
        }),
        Err(_) => Table::new(),
    }
}

fn save(file: &Path, t: &Table) {
    let tmp = file.with_extension("toml.tmp");
    let r = file
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|_| std::fs::write(&tmp, t.to_string()))
        .and_then(|_| std::fs::rename(&tmp, file));
    if let Err(e) = r {
        warn!("{}: {}", file.display(), e);
    }
}

/// この基板について覚えたもの（<名前>.krbr の [remembered]）。以前の形（state.toml の [boards."<パス>"]）にあれば移す
fn board_remembered(pcb: &Path) -> Table {
    let (_, t) = board_file::read(pcb);
    if let Some(r) = t.as_ref().and_then(|t| t.get("remembered")).and_then(|v| v.as_table()) {
        return r.clone();
    }
    let file = state_file();
    let mut st = load(&file);
    let old = st.get_mut("boards").and_then(|b| b.as_table_mut()).and_then(|b| b.remove(&board_file::board_key(pcb)));
    if let Some(Value::Table(b)) = old {
        // 書けたときだけ state.toml から消す
        if let Some(to) = write_board_remembered(pcb, &b) {
            if st.get("boards").and_then(|v| v.as_table()).is_some_and(|t| t.is_empty()) {
                st.remove("boards");
            }
            save(&file, &st);
            info!("remembered settings: moved from {} to {}", file.display(), to.display());
        }
        return b;
    }
    Table::new()
}

/// この基板について覚えたものを <名前>.krbr の [remembered] に書く（空なら [remembered] を消す）。書いたファイル
fn write_board_remembered(pcb: &Path, b: &Table) -> Option<PathBuf> {
    let mut src = Table::new();
    if !b.is_empty() {
        src.insert("remembered".into(), Value::Table(b.clone()));
    }
    // 表を作れなければ書かない（ファイルの [remembered] を消してしまわない）
    let file = board_file::resolve(pcb);
    match board_file::try_edit_toml(&file, &board_file::template(), |doc| board_file::try_replace_tables(doc, &src, &["remembered"])) {
        Ok(()) => Some(file),
        Err(e) => {
            warn!("remembered settings: {}", e);
            None
        }
    }
}

fn remembered_in(b: &Table) -> Option<Remembered> {
    let hdr = b.get("hdr")?.as_str()?.to_string();
    let config_hdr = b.get("config_hdr").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Some(Remembered { hdr, config_hdr })
}

fn remember_in(b: &mut Table, hdr: &str, config_hdr: &str) {
    b.insert("hdr".into(), Value::String(hdr.to_string()));
    b.insert("config_hdr".into(), Value::String(config_hdr.to_string()));
}

fn add_recent(t: &mut Table, hdr: &str) {
    let mut recent = recent_in(t);
    recent.retain(|r| !same_hdr(r, hdr));
    recent.insert(0, hdr.to_string());
    recent.truncate(MAX_RECENT);
    t.insert("recent_hdr".into(), Value::Array(recent.into_iter().map(Value::String).collect()));
}

fn recent_in(t: &Table) -> Vec<String> {
    t.get("recent_hdr").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// この基板で最後に使った環境マップ
pub fn remembered_hdr(pcb: &Path) -> Option<Remembered> {
    remembered_in(&board_remembered(pcb))
}

/// この基板で使った環境マップを覚える（最近使った一覧の先頭にも入れる）
pub fn remember_hdr(pcb: &Path, hdr: &str, config_hdr: &str) {
    let mut b = board_remembered(pcb);
    remember_in(&mut b, hdr, config_hdr);
    write_board_remembered(pcb, &b);
    let file = state_file();
    let mut t = load(&file);
    add_recent(&mut t, hdr);
    save(&file, &t);
}

/// 最近使った環境マップ（新しい順）
pub fn recent_hdrs() -> Vec<String> {
    recent_in(&load(&state_file()))
}

// ---------------------------------------------------------------- パネルの設定

/// パネルの設定の値（項目の名前 → 値）。設定ファイルにもある項目（ViewSettings）、視点の注視点と距離（camera_of / set_camera。
/// render も使う）、ビューアだけの項目（ViewerValues）がある
pub type ViewValues = Table;

/// ビューアだけの項目（render / watch には使わない。設定ファイルには無い）。覚えていない項目は None
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewerValues {
    pub denoise: Option<bool>,
    pub denoise_strength: Option<f32>,
    /// 最大のサンプル数
    pub max_spp: Option<u32>,
    /// 表示の種類の名前（"image" / "albedo" / "normal"）
    pub display_mode: Option<String>,
    /// レンダリングの比率
    pub render_ratio: Option<f32>,
}

impl ViewerValues {
    const DENOISE: &'static str = "denoise";
    const DENOISE_STRENGTH: &'static str = "denoise_strength";
    const MAX_SPP: &'static str = "max_spp";
    const DISPLAY_MODE: &'static str = "display_mode";
    const RENDER_RATIO: &'static str = "render_ratio";

    /// 覚えた値から読む
    pub fn of(v: &ViewValues) -> Self {
        Self {
            denoise: v.get(Self::DENOISE).and_then(|x| x.as_bool()),
            denoise_strength: v.get(Self::DENOISE_STRENGTH).and_then(|x| x.as_float()).map(|x| x as f32),
            max_spp: v.get(Self::MAX_SPP).and_then(|x| x.as_integer()).and_then(|x| u32::try_from(x).ok()),
            display_mode: v.get(Self::DISPLAY_MODE).and_then(|x| x.as_str()).map(str::to_string),
            render_ratio: v.get(Self::RENDER_RATIO).and_then(|x| x.as_float()).map(|x| x as f32),
        }
    }

    /// 値に入れる（None の項目は入れない）
    pub fn write(&self, v: &mut ViewValues) {
        if let Some(b) = self.denoise {
            v.insert(Self::DENOISE.into(), Value::Boolean(b));
        }
        if let Some(x) = self.denoise_strength {
            v.insert(Self::DENOISE_STRENGTH.into(), Value::Float(x as f64));
        }
        if let Some(n) = self.max_spp {
            v.insert(Self::MAX_SPP.into(), Value::Integer(n as i64));
        }
        if let Some(m) = &self.display_mode {
            v.insert(Self::DISPLAY_MODE.into(), Value::String(m.clone()));
        }
        if let Some(r) = self.render_ratio {
            v.insert(Self::RENDER_RATIO.into(), Value::Float(r as f64));
        }
    }
}

/// 画像に関わるパネルの設定（設定ファイル・コマンドラインにもある項目）
#[derive(Debug, Clone)]
pub struct ViewSettings<'a> {
    pub env_intensity: f32,
    pub env_rotation_deg: f32,
    pub exposure_ev: f32,
    pub glow: f32,
    pub glow_threshold: f32,
    pub glow_spread: f32,
    pub integrator: IntegratorKind,
    pub spectral: bool,
    pub bump_intensity: f32,
    /// レジストの色：プリセットの名前、r,g,b、または空（stackup の色・既定）
    pub mask: String,
    pub fov_deg: f64,
    /// 視点の仰角・方位（注視点と距離は camera_of / set_camera で別に扱う）
    pub elevation_deg: f64,
    pub azimuth_deg: f64,
    /// 描画のサイズ。None = ビューアのウィンドウに合わせる（覚えない。render は設定ファイルの大きさ）
    pub render_size: Option<(u32, u32)>,
    pub lens: &'a LensParams,
}

impl<'a> ViewSettings<'a> {
    pub fn of(cfg: &'a ProjectConfig) -> Self {
        Self {
            env_intensity: cfg.env_intensity,
            env_rotation_deg: cfg.env_rotation_deg,
            exposure_ev: cfg.exposure_ev,
            glow: cfg.glow,
            glow_threshold: cfg.glow_threshold,
            glow_spread: cfg.glow_spread,
            integrator: cfg.integrator,
            spectral: cfg.spectral,
            bump_intensity: cfg.scene.bump_intensity,
            mask: mask_spec(&cfg.scene),
            fov_deg: cfg.fov_deg,
            elevation_deg: cfg.elevation_deg,
            azimuth_deg: cfg.azimuth_deg,
            render_size: Some((cfg.width, cfg.height)),
            lens: &cfg.lens,
        }
    }

    pub fn values(&self) -> ViewValues {
        let mut t = Table::new();
        let mut f = |k: &str, v: f64| {
            t.insert(k.into(), Value::Float(v));
        };
        f("env_intensity", self.env_intensity as f64);
        f("env_rotation_deg", self.env_rotation_deg as f64);
        f("exposure_ev", self.exposure_ev as f64);
        f("glow", self.glow as f64);
        f("glow_threshold", self.glow_threshold as f64);
        f("glow_spread", self.glow_spread as f64);
        f("bump_intensity", self.bump_intensity as f64);
        f("fov_deg", self.fov_deg);
        f("elevation_deg", self.elevation_deg);
        f("azimuth_deg", self.azimuth_deg);
        let l = self.lens;
        for (k, v) in [
            ("lens_f_number", l.f_number),
            ("lens_focal_length_mm", l.focal_length_mm),
            ("lens_focus_distance_mm", l.focus_distance_mm),
            ("lens_blade_rotation_deg", l.blade_rotation_deg),
            ("lens_blade_roundness", l.blade_roundness),
            ("lens_optical_vignetting", l.optical_vignetting),
            ("lens_abbe_number", l.abbe_number),
            ("lens_lateral_ca", l.lateral_ca),
        ] {
            f(k, v as f64);
        }
        if let Some((w, h)) = self.render_size {
            t.insert("render_size".into(), Value::Array(vec![Value::Integer(w as i64), Value::Integer(h as i64)]));
        }
        t.insert("integrator".into(), Value::String(self.integrator.as_str().into()));
        t.insert("spectral".into(), Value::Boolean(self.spectral));
        t.insert("mask".into(), Value::String(self.mask.clone()));
        t.insert("lens_model".into(), Value::String(l.model.as_str().into()));
        t.insert("lens_blades".into(), Value::Integer(l.blades as i64));
        t.insert("lens_natural_vignetting".into(), Value::Boolean(l.natural_vignetting));
        t
    }
}

/// レジストの色の指定：プリセットの名前、r,g,b、または空（stackup の色・既定）
pub fn mask_spec(s: &krbr_scene::SceneOptions) -> String {
    if !s.mask_preset.is_empty() {
        s.mask_preset.clone()
    } else if s.mask_color_set {
        format!("{:.6},{:.6},{:.6}", s.mask_color.x, s.mask_color.y, s.mask_color.z)
    } else {
        String::new()
    }
}

/// 覚えた値を設定に当てる（ある項目だけ。ビューアだけの項目は当てない）
pub fn apply_view_values(cfg: &mut ProjectConfig, v: &ViewValues) {
    let f = |k: &str| v.get(k).and_then(|x| x.as_float());
    let set = |dst: &mut f32, k: &str| {
        if let Some(x) = f(k) {
            *dst = x as f32;
        }
    };
    set(&mut cfg.env_intensity, "env_intensity");
    set(&mut cfg.env_rotation_deg, "env_rotation_deg");
    set(&mut cfg.exposure_ev, "exposure_ev");
    set(&mut cfg.glow, "glow");
    set(&mut cfg.glow_threshold, "glow_threshold");
    set(&mut cfg.glow_spread, "glow_spread");
    set(&mut cfg.scene.bump_intensity, "bump_intensity");
    if let Some(x) = f("fov_deg") {
        cfg.fov_deg = x;
    }
    if let Some(x) = f("elevation_deg") {
        cfg.elevation_deg = x;
    }
    if let Some(x) = f("azimuth_deg") {
        cfg.azimuth_deg = x;
    }
    let l = &mut cfg.lens;
    set(&mut l.f_number, "lens_f_number");
    set(&mut l.focal_length_mm, "lens_focal_length_mm");
    set(&mut l.focus_distance_mm, "lens_focus_distance_mm");
    set(&mut l.blade_rotation_deg, "lens_blade_rotation_deg");
    set(&mut l.blade_roundness, "lens_blade_roundness");
    set(&mut l.optical_vignetting, "lens_optical_vignetting");
    set(&mut l.abbe_number, "lens_abbe_number");
    set(&mut l.lateral_ca, "lens_lateral_ca");
    if let Some(m) = v.get("lens_model").and_then(|x| x.as_str()) {
        match m.parse::<LensModel>() {
            Ok(m) => l.model = m,
            Err(e) => warn!("remembered lens: {e}; ignored"),
        }
    }
    if let Some(n) = v.get("lens_blades").and_then(|x| x.as_integer()) {
        l.blades = n as i32;
    }
    if let Some(b) = v.get("lens_natural_vignetting").and_then(|x| x.as_bool()) {
        l.natural_vignetting = b;
    }
    if let Some((w, h)) = render_size_of(v) {
        cfg.width = w;
        cfg.height = h;
    }
    if let Some(i) = v.get("integrator").and_then(|x| x.as_str()) {
        match i.parse::<IntegratorKind>() {
            Ok(k) => cfg.integrator = k,
            Err(e) => warn!("remembered integrator: {e}; ignored"),
        }
    }
    if let Some(b) = v.get("spectral").and_then(|x| x.as_bool()) {
        cfg.spectral = b;
    }
    if let Some(m) = v.get("mask").and_then(|x| x.as_str()).filter(|m| !m.is_empty()) {
        if let Err(e) = apply_mask_color_spec(&mut cfg.scene, m) {
            warn!("remembered mask color {}: {}", m, e);
        }
    }
}

/// 覚えた描画のサイズ（決まったサイズを選んでいたとき）
pub fn render_size_of(v: &ViewValues) -> Option<(u32, u32)> {
    let a = v.get("render_size")?.as_array()?;
    let (w, h) = (a.first()?.as_integer()?, a.get(1)?.as_integer()?);
    valid_render_size(w, h).then_some((w as u32, h as u32))
}

/// 覚えた視点の注視点（ワールド座標）と距離（mm）
pub fn camera_of(v: &ViewValues) -> Option<(glam::Vec3, f32)> {
    let t = v.get("camera_target")?.as_array()?;
    let c: Vec<f32> = t.iter().filter_map(|x| x.as_float()).map(|x| x as f32).collect();
    let d = v.get("camera_distance")?.as_float()? as f32;
    (c.len() == 3 && d > 0.0 && c.iter().all(|x| x.is_finite())).then(|| (glam::Vec3::new(c[0], c[1], c[2]), d))
}

/// 視点の注視点と距離を値に入れる
pub fn set_camera(v: &mut ViewValues, target: glam::Vec3, distance: f32) {
    v.insert("camera_target".into(), Value::Array(target.to_array().iter().map(|&x| Value::Float(x as f64)).collect()));
    v.insert("camera_distance".into(), Value::Float(distance as f64));
}

/// 覚えたパネルの設定のうち、覚えたときから設定ファイルの値が変わっていない項目（ビューアだけの項目は常に）
fn usable_view_values(b: &Table, config_now: &ViewValues) -> ViewValues {
    let Some(values) = b.get("view").and_then(|v| v.as_table()) else {
        return ViewValues::new();
    };
    let config_then = b.get("view_config").and_then(|v| v.as_table());
    values
        .iter()
        .filter(|(k, _)| config_then.and_then(|c| c.get(*k)).map(|v| canonical_config_value(k, v)) == config_now.get(*k).cloned())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// 覚えた設定ファイルの値を、今の書き方（ViewSettings::values）にそろえる。以前は積分器の名前を設定ファイルの
/// 書き方のまま（"path" など）覚えていたので、別名を正規の名前にしないと「設定ファイルが変わった」とみなしてしまう
fn canonical_config_value(key: &str, v: &Value) -> Value {
    match (key, v.as_str().map(str::parse::<IntegratorKind>)) {
        ("integrator", Some(Ok(k))) => Value::String(k.as_str().into()),
        _ => v.clone(),
    }
}

fn remember_view_in(b: &mut Table, values: &ViewValues, config: &ViewValues) {
    b.insert("view".into(), Value::Table(values.clone()));
    b.insert("view_config".into(), Value::Table(config.clone()));
}

/// この基板で覚えているパネルの設定（設定ファイルが変わった項目は除く）。config_now は今の設定ファイルの値
pub fn remembered_view(pcb: &Path, config_now: &ViewValues) -> ViewValues {
    usable_view_values(&board_remembered(pcb), config_now)
}

/// この基板のパネルの設定を覚える。config は今の設定ファイルの値（ViewSettings::of(設定ファイルだけの設定).values()）
pub fn remember_view(pcb: &Path, values: &ViewValues, config: &ViewValues) {
    let mut b = board_remembered(pcb);
    remember_view_in(&mut b, values, config);
    write_board_remembered(pcb, &b);
}

/// この基板について覚えたもの（環境マップとパネルの設定）をすべて忘れる。最近使った環境マップの一覧は残す
pub fn forget_board(pcb: &Path) {
    if !board_remembered(pcb).is_empty() {
        write_board_remembered(pcb, &Table::new());
    }
}

// ---------------------------------------------------------------- ウィンドウ（基板によらず 1 つ）

/// ビューアのウィンドウの大きさ・位置・最大化と、設定パネルを出すかとその幅
#[derive(Debug, Clone, PartialEq)]
pub struct WindowState {
    /// 最大化していないときの中の大きさ（物理ピクセル）
    pub size: Option<(u32, u32)>,
    /// 最大化していないときの外側の位置（物理ピクセル）
    pub position: Option<(i32, i32)>,
    pub maximized: bool,
    pub panel_shown: bool,
    /// 設定パネルの幅（egui のポイント）
    pub panel_width: Option<f32>,
}

impl Default for WindowState {
    fn default() -> Self {
        Self { size: None, position: None, maximized: false, panel_shown: true, panel_width: None }
    }
}

fn window_state_in(t: &Table) -> WindowState {
    let mut w = WindowState::default();
    let Some(v) = t.get("window").and_then(|v| v.as_table()) else { return w };
    let pair = |k: &str| -> Option<(i64, i64)> {
        let a = v.get(k)?.as_array()?;
        Some((a.first()?.as_integer()?, a.get(1)?.as_integer()?))
    };
    w.size = pair("size").filter(|&(x, y)| x > 0 && y > 0).map(|(x, y)| (x as u32, y as u32));
    w.position = pair("position").map(|(x, y)| (x as i32, y as i32));
    w.maximized = v.get("maximized").and_then(|x| x.as_bool()).unwrap_or(false);
    w.panel_shown = v.get("panel_shown").and_then(|x| x.as_bool()).unwrap_or(true);
    w.panel_width = v.get("panel_width").and_then(|x| x.as_float()).map(|x| x as f32);
    w
}

fn remember_window_in(t: &mut Table, w: &WindowState) {
    let mut v = Table::new();
    let pair = |a: i64, b: i64| Value::Array(vec![Value::Integer(a), Value::Integer(b)]);
    if let Some((x, y)) = w.size {
        v.insert("size".into(), pair(x as i64, y as i64));
    }
    if let Some((x, y)) = w.position {
        v.insert("position".into(), pair(x as i64, y as i64));
    }
    v.insert("maximized".into(), Value::Boolean(w.maximized));
    v.insert("panel_shown".into(), Value::Boolean(w.panel_shown));
    if let Some(pw) = w.panel_width {
        v.insert("panel_width".into(), Value::Float(pw as f64));
    }
    t.insert("window".into(), Value::Table(v));
}

/// 前回のウィンドウの状態
pub fn window_state() -> WindowState {
    window_state_in(&load(&state_file()))
}

/// ウィンドウの状態を覚える
pub fn remember_window(w: &WindowState) {
    let file = state_file();
    let mut t = load(&file);
    remember_window_in(&mut t, w);
    save(&file, &t);
}

// ---------------------------------------------------------------- 表示の言語（基板によらず 1 つ）

fn ui_language_in(t: &Table) -> Option<String> {
    t.get("ui")?.get("language")?.as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

fn remember_ui_language_in(t: &mut Table, lang: Option<&str>) {
    let ui = t.entry("ui").or_insert_with(|| Value::Table(Table::new()));
    if let Some(ui) = ui.as_table_mut() {
        match lang {
            Some(l) => ui.insert("language".into(), Value::String(l.into())),
            None => ui.remove("language"),
        };
    }
}

/// ビューアのメニューで選んだ表示の言語（state.toml の [ui] language）。None なら自動（KiCad、OS の順）
pub fn ui_language() -> Option<String> {
    ui_language_in(&load(&state_file()))
}

/// 表示の言語を覚える（None：自動に戻す）
pub fn remember_ui_language(lang: Option<&str>) {
    let file = state_file();
    let mut t = load(&file);
    remember_ui_language_in(&mut t, lang);
    save(&file, &t);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_language_round_trip() {
        let mut t = Table::new();
        assert_eq!(ui_language_in(&t), None);
        remember_ui_language_in(&mut t, Some("ja"));
        assert_eq!(ui_language_in(&t).as_deref(), Some("ja"));
        remember_ui_language_in(&mut t, None);
        assert_eq!(ui_language_in(&t), None);
    }

    #[test]
    fn remembers_hdr_and_recent() {
        let (mut a, mut b) = (Table::new(), Table::new());
        let mut st = Table::new();
        remember_in(&mut a, "/hdr/one.exr", "");
        add_recent(&mut st, "/hdr/one.exr");
        remember_in(&mut b, "/hdr/two.exr", "/cfg.exr");
        add_recent(&mut st, "/hdr/two.exr");
        remember_in(&mut a, "/hdr/two.exr", "");
        add_recent(&mut st, "/hdr/two.exr");
        // 文字列にして読み直しても同じ
        let (a, st): (Table, Table) = (a.to_string().parse().unwrap(), st.to_string().parse().unwrap());
        assert_eq!(remembered_in(&a), Some(Remembered { hdr: "/hdr/two.exr".into(), config_hdr: String::new() }));
        assert_eq!(remembered_in(&b).unwrap().config_hdr, "/cfg.exr");
        assert_eq!(remembered_in(&Table::new()), None);
        assert_eq!(recent_in(&st), vec!["/hdr/two.exr".to_string(), "/hdr/one.exr".to_string()]);
        // Windows では大文字・小文字と区切り文字だけが違うものは同じファイル（一覧に重ねない）
        if cfg!(windows) {
            let mut st = Table::new();
            add_recent(&mut st, "C:\\hdr\\One.exr");
            add_recent(&mut st, "c:/hdr/one.exr");
            assert_eq!(recent_in(&st), vec!["c:/hdr/one.exr".to_string()]);
        }
    }

    #[test]
    fn viewer_values_round_trip_and_render_size_limit() {
        let v = ViewerValues { denoise: Some(true), denoise_strength: Some(0.5), max_spp: Some(256), display_mode: Some("albedo".into()), render_ratio: Some(1.5) };
        let mut t = ViewValues::new();
        v.write(&mut t);
        let t: Table = t.to_string().parse().unwrap();
        assert_eq!(ViewerValues::of(&t), v);
        assert_eq!(ViewerValues::of(&ViewValues::new()), ViewerValues::default());
        assert!(valid_render_size(1, MAX_RENDER_SIZE as i64) && !valid_render_size(0, 10) && !valid_render_size(10, MAX_RENDER_SIZE as i64 + 1));
    }

    #[test]
    fn view_settings_round_trip_and_config_changes_win() {
        let mut cfg = ProjectConfig::default();
        let config_then = ViewSettings::of(&cfg).values();
        // パネルで変えた値
        let mut panel = cfg.clone();
        panel.exposure_ev = 1.5;
        panel.integrator = IntegratorKind::Vcm;
        panel.spectral = true;
        panel.lens.model = LensModel::Thin;
        panel.lens.blades = 6;
        panel.width = 1080;
        panel.height = 1350;
        apply_mask_color_spec(&mut panel.scene, "jlcpcb-purple").unwrap();
        let mut values = ViewSettings::of(&panel).values();
        ViewerValues { max_spp: Some(1024), ..Default::default() }.write(&mut values);

        let mut b = Table::new();
        remember_in(&mut b, "/hdr/one.exr", "");
        remember_view_in(&mut b, &values, &config_then);
        let b: Table = b.to_string().parse().unwrap();
        assert_eq!(remembered_in(&b).unwrap().hdr, "/hdr/one.exr"); // 環境マップも残っている

        // 設定ファイルが同じなら全部使う
        let got = usable_view_values(&b, &config_then);
        assert_eq!(got, values);
        apply_view_values(&mut cfg, &got);
        assert_eq!((cfg.exposure_ev, cfg.integrator, cfg.spectral, cfg.lens.model, cfg.lens.blades), (1.5, IntegratorKind::Vcm, true, LensModel::Thin, 6));
        assert_eq!(cfg.scene.mask_preset, "jlcpcb-purple");
        assert_eq!((cfg.width, cfg.height), (1080, 1350));
        assert_eq!(render_size_of(&got), Some((1080, 1350)));

        // 設定ファイルの露出が書き換えられたら、露出だけ設定ファイルを使う（ほかの項目とビューアだけの項目は覚えたもの）
        let mut edited = ProjectConfig::default();
        edited.exposure_ev = -1.0;
        let got = usable_view_values(&b, &ViewSettings::of(&edited).values());
        assert!(!got.contains_key("exposure_ev"));
        assert_eq!(got.get("integrator").and_then(|v| v.as_str()), Some("vcm"));
        assert_eq!(ViewerValues::of(&got).max_spp, Some(1024));

        // 以前の版は設定ファイルの積分器の名前を別名のまま覚えていた（"path"）。今の "pt" と同じとみなす
        let mut old = b.clone();
        old["view_config"].as_table_mut().unwrap().insert("integrator".into(), Value::String("path".into()));
        assert_eq!(usable_view_values(&old, &config_then).get("integrator").and_then(|v| v.as_str()), Some("vcm"));

        // 視点
        let mut v = ViewValues::new();
        assert_eq!(camera_of(&v), None);
        set_camera(&mut v, glam::Vec3::new(1.0, -2.0, 3.5), 120.0);
        let v: Table = Table::from_iter([("view".to_string(), Value::Table(v))]).to_string().parse::<Table>().unwrap()["view"].as_table().unwrap().clone();
        assert_eq!(camera_of(&v), Some((glam::Vec3::new(1.0, -2.0, 3.5), 120.0)));
    }

    /// 基板の設定ファイルの [remembered] に書き、手で書いた表は残す。以前の state.toml の分は移す。忘れると [remembered] だけ消える
    #[test]
    fn remembered_settings_live_in_the_board_file() {
        let ws = std::env::temp_dir().join(format!("krbr_remember_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).unwrap();
        std::env::set_var("KRBR_CONFIG_DIR", ws.join("config"));
        let (a, b) = (ws.join("a.kicad_pcb"), ws.join("b.kicad_pcb"));
        std::fs::write(ws.join("a.krbr"), "# mine\n[render]\nspp = 16   # keep\n").unwrap();
        let config = ViewSettings::of(&ProjectConfig::default()).values();
        let mut values = config.clone();
        values.insert("exposure_ev".into(), Value::Float(2.0));
        remember_view(&a, &values, &config);
        remember_hdr(&a, "/hdr/one.exr", "");
        let text = std::fs::read_to_string(ws.join("a.krbr")).unwrap();
        assert!(text.starts_with("# mine\n[render]\nspp = 16   # keep\n") && text.contains("[remembered]"), "{text}");
        assert_eq!(remembered_view(&a, &config).get("exposure_ev").and_then(|v| v.as_float()), Some(2.0));
        assert_eq!(remembered_hdr(&a).unwrap().hdr, "/hdr/one.exr");
        assert_eq!(recent_hdrs(), vec!["/hdr/one.exr".to_string()]);
        // 以前の形：state.toml の [boards."<パス>"] は、読んだときに b.krbr へ移る
        let mut st = load(&state_file());
        let mut old = Table::new();
        remember_in(&mut old, "/hdr/two.exr", "");
        st.insert("boards".into(), Value::Table(Table::from_iter([(board_file::board_key(&b), Value::Table(old))])));
        save(&state_file(), &st);
        assert_eq!(remembered_hdr(&b).unwrap().hdr, "/hdr/two.exr");
        assert!(ws.join("b.krbr").is_file());
        assert!(!load(&state_file()).contains_key("boards"));
        // 忘れる
        forget_board(&a);
        assert!(remembered_hdr(&a).is_none() && remembered_view(&a, &config).is_empty());
        assert!(std::fs::read_to_string(ws.join("a.krbr")).unwrap().contains("spp = 16   # keep"));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn window_state_round_trip() {
        let mut t = Table::new();
        assert_eq!(window_state_in(&t), WindowState::default());
        let w = WindowState { size: Some((1600, 900)), position: Some((-1200, 40)), maximized: true, panel_shown: false, panel_width: Some(420.0) };
        remember_window_in(&mut t, &w);
        let t: Table = t.to_string().parse().unwrap();
        assert_eq!(window_state_in(&t), w);
    }
}
