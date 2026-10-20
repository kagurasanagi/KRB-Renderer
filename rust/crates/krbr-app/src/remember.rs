//! 基板ごとに最後に使った環境マップとパネルの設定、最近使った環境マップの一覧を覚えておく（`%APPDATA%\krbr\state.toml`）。
//! プロジェクトのファイル（`<名前>.krbr\environment.toml` / `board.toml`）は書き換えない。
//! どの項目も、覚えたときの設定ファイルの値を一緒に残し、後で設定ファイルの方が書き換えられたら、そちらを優先する。

use std::path::{Path, PathBuf};

use krbr_core::warn;
use krbr_scene::config::{apply_mask_color_spec, ProjectConfig};
use krbr_scene::{LensModel, LensParams};
use toml::{Table, Value};

const MAX_RECENT: usize = 12;

/// 基板について覚えている環境マップ
#[derive(Debug, Clone, PartialEq)]
pub struct Remembered {
    pub hdr: String,
    /// 覚えたときの environment.toml の hdr。後で設定ファイルの方が書き換えられたら、そちらを優先する
    pub config_hdr: String,
}

fn state_file() -> PathBuf {
    krbr_core::platform::config_dir().join("state.toml")
}

/// 表を引く鍵：絶対パス、区切りは /、Windows では大文字・小文字を区別しない
fn board_key(pcb: &Path) -> String {
    let abs = std::path::absolute(pcb).unwrap_or_else(|_| pcb.to_path_buf());
    let s = abs.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// 環境マップの指定を、作業フォルダによらない形にする（内蔵の名前はそのまま）
pub fn normalize_hdr(spec: &str) -> String {
    if krbr_render::is_builtin(spec) {
        return spec.to_string();
    }
    std::path::absolute(spec).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| spec.to_string())
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

fn remembered_in(t: &Table, pcb: &Path) -> Option<Remembered> {
    let b = t.get("boards")?.as_table()?.get(&board_key(pcb))?.as_table()?;
    let hdr = b.get("hdr")?.as_str()?.to_string();
    let config_hdr = b.get("config_hdr").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Some(Remembered { hdr, config_hdr })
}

fn remember_in(t: &mut Table, pcb: &Path, hdr: &str, config_hdr: &str) {
    let b = board_table(t, pcb);
    b.insert("hdr".into(), Value::String(hdr.to_string()));
    b.insert("config_hdr".into(), Value::String(config_hdr.to_string()));

    let mut recent = recent_in(t);
    recent.retain(|r| r != hdr);
    recent.insert(0, hdr.to_string());
    recent.truncate(MAX_RECENT);
    t.insert("recent_hdr".into(), Value::Array(recent.into_iter().map(Value::String).collect()));
}

/// この基板の表（無ければ作る）。環境マップとパネルの設定は同じ表に入れる
fn board_table<'a>(t: &'a mut Table, pcb: &Path) -> &'a mut Table {
    let boards = t.entry("boards").or_insert_with(|| Value::Table(Table::new()));
    if !boards.is_table() {
        *boards = Value::Table(Table::new());
    }
    let b = boards.as_table_mut().unwrap().entry(board_key(pcb)).or_insert_with(|| Value::Table(Table::new()));
    if !b.is_table() {
        *b = Value::Table(Table::new());
    }
    b.as_table_mut().unwrap()
}

fn recent_in(t: &Table) -> Vec<String> {
    t.get("recent_hdr").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// この基板で最後に使った環境マップ
pub fn remembered_hdr(pcb: &Path) -> Option<Remembered> {
    remembered_in(&load(&state_file()), pcb)
}

/// この基板で使った環境マップを覚える（最近使った一覧の先頭にも入れる）
pub fn remember_hdr(pcb: &Path, hdr: &str, config_hdr: &str) {
    let file = state_file();
    let mut t = load(&file);
    remember_in(&mut t, pcb, hdr, config_hdr);
    save(&file, &t);
}

/// 最近使った環境マップ（新しい順）
pub fn recent_hdrs() -> Vec<String> {
    recent_in(&load(&state_file()))
}

// ---------------------------------------------------------------- パネルの設定

/// パネルの設定の値（項目の名前 → 値）。設定ファイルにもある項目と、ビューアだけの項目（VIEWER_ONLY）がある
pub type ViewValues = Table;

/// ビューアだけの項目（render / watch には使わない。設定ファイルには無い）
pub const VIEWER_ONLY: [&str; 4] = ["denoise", "denoise_strength", "max_spp", "display_mode"];

/// 画像に関わるパネルの設定（設定ファイル・コマンドラインにもある項目）
#[derive(Debug, Clone)]
pub struct ViewSettings<'a> {
    pub env_intensity: f32,
    pub env_rotation_deg: f32,
    pub exposure_ev: f32,
    pub integrator: &'a str,
    pub spectral: bool,
    pub bump_intensity: f32,
    /// レジストの色：プリセットの名前、r,g,b、または空（stackup の色・既定）
    pub mask: String,
    pub fov_deg: f64,
    /// 視点の仰角・方位（注視点と距離は camera_of / set_camera で別に扱う）
    pub elevation_deg: f64,
    pub azimuth_deg: f64,
    pub lens: &'a LensParams,
}

impl<'a> ViewSettings<'a> {
    pub fn of(cfg: &'a ProjectConfig) -> Self {
        Self {
            env_intensity: cfg.env_intensity,
            env_rotation_deg: cfg.env_rotation_deg,
            exposure_ev: cfg.exposure_ev,
            integrator: &cfg.integrator,
            spectral: cfg.spectral,
            bump_intensity: cfg.scene.bump_intensity,
            mask: mask_spec(&cfg.scene),
            fov_deg: cfg.fov_deg,
            elevation_deg: cfg.elevation_deg,
            azimuth_deg: cfg.azimuth_deg,
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
        t.insert("integrator".into(), Value::String(self.integrator.to_string()));
        t.insert("spectral".into(), Value::Boolean(self.spectral));
        t.insert("mask".into(), Value::String(self.mask.clone()));
        t.insert("lens_model".into(), Value::String(lens_model_name(l.model).into()));
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

fn lens_model_name(m: LensModel) -> &'static str {
    match m {
        LensModel::Pinhole => "pinhole",
        LensModel::Thin => "thin",
        LensModel::Realistic => "realistic",
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
        l.model = match m {
            "thin" => LensModel::Thin,
            "realistic" => LensModel::Realistic,
            _ => LensModel::Pinhole,
        };
    }
    if let Some(n) = v.get("lens_blades").and_then(|x| x.as_integer()) {
        l.blades = n as i32;
    }
    if let Some(b) = v.get("lens_natural_vignetting").and_then(|x| x.as_bool()) {
        l.natural_vignetting = b;
    }
    if let Some(i) = v.get("integrator").and_then(|x| x.as_str()) {
        cfg.integrator = i.to_string();
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
fn usable_view_values(t: &Table, pcb: &Path, config_now: &ViewValues) -> ViewValues {
    let Some(b) = t.get("boards").and_then(|b| b.as_table()).and_then(|b| b.get(&board_key(pcb))).and_then(|b| b.as_table()) else {
        return ViewValues::new();
    };
    let Some(values) = b.get("view").and_then(|v| v.as_table()) else {
        return ViewValues::new();
    };
    let config_then = b.get("view_config").and_then(|v| v.as_table());
    values.iter().filter(|(k, _)| config_then.and_then(|c| c.get(*k)) == config_now.get(*k)).map(|(k, v)| (k.clone(), v.clone())).collect()
}

fn remember_view_in(t: &mut Table, pcb: &Path, values: &ViewValues, config: &ViewValues) {
    let b = board_table(t, pcb);
    b.insert("view".into(), Value::Table(values.clone()));
    b.insert("view_config".into(), Value::Table(config.clone()));
}

fn forget_in(t: &mut Table, pcb: &Path) {
    if let Some(b) = t.get_mut("boards").and_then(|b| b.as_table_mut()) {
        b.remove(&board_key(pcb));
    }
}

/// この基板で覚えているパネルの設定（設定ファイルが変わった項目は除く）。config_now は今の設定ファイルの値
pub fn remembered_view(pcb: &Path, config_now: &ViewValues) -> ViewValues {
    usable_view_values(&load(&state_file()), pcb, config_now)
}

/// この基板のパネルの設定を覚える。config は今の設定ファイルの値（ViewSettings::of(設定ファイルだけの設定).values()）
pub fn remember_view(pcb: &Path, values: &ViewValues, config: &ViewValues) {
    let file = state_file();
    let mut t = load(&file);
    remember_view_in(&mut t, pcb, values, config);
    save(&file, &t);
}

/// この基板について覚えたもの（環境マップとパネルの設定）をすべて忘れる。最近使った環境マップの一覧は残す
pub fn forget_board(pcb: &Path) {
    let file = state_file();
    let mut t = load(&file);
    forget_in(&mut t, pcb);
    save(&file, &t);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_per_board_and_recent() {
        let mut t = Table::new();
        let (a, b) = (Path::new("/x/a.kicad_pcb"), Path::new("/x/b.kicad_pcb"));
        remember_in(&mut t, a, "/hdr/one.exr", "");
        remember_in(&mut t, b, "/hdr/two.exr", "/cfg.exr");
        remember_in(&mut t, a, "/hdr/two.exr", "");
        // 文字列にして読み直しても同じ
        let t: Table = t.to_string().parse().unwrap();
        assert_eq!(remembered_in(&t, a), Some(Remembered { hdr: "/hdr/two.exr".into(), config_hdr: String::new() }));
        assert_eq!(remembered_in(&t, b).unwrap().config_hdr, "/cfg.exr");
        assert_eq!(remembered_in(&t, Path::new("/x/c.kicad_pcb")), None);
        assert_eq!(recent_in(&t), vec!["/hdr/two.exr".to_string(), "/hdr/one.exr".to_string()]);
    }

    #[test]
    fn view_settings_round_trip_and_config_changes_win() {
        let mut cfg = ProjectConfig::default();
        let config_then = ViewSettings::of(&cfg).values();
        // パネルで変えた値
        let mut panel = cfg.clone();
        panel.exposure_ev = 1.5;
        panel.integrator = "vcm".into();
        panel.spectral = true;
        panel.lens.model = LensModel::Thin;
        panel.lens.blades = 6;
        apply_mask_color_spec(&mut panel.scene, "jlcpcb-purple").unwrap();
        let mut values = ViewSettings::of(&panel).values();
        values.insert("max_spp".into(), Value::Integer(1024));

        let (pcb, other) = (Path::new("/x/a.kicad_pcb"), Path::new("/x/b.kicad_pcb"));
        let mut t = Table::new();
        remember_in(&mut t, pcb, "/hdr/one.exr", "");
        remember_view_in(&mut t, pcb, &values, &config_then);
        remember_in(&mut t, other, "/hdr/two.exr", "");
        let mut t: Table = t.to_string().parse().unwrap();
        assert_eq!(remembered_in(&t, pcb).unwrap().hdr, "/hdr/one.exr"); // 環境マップも残っている

        // 設定ファイルが同じなら全部使う
        let got = usable_view_values(&t, pcb, &config_then);
        assert_eq!(got, values);
        apply_view_values(&mut cfg, &got);
        assert_eq!((cfg.exposure_ev, cfg.integrator.as_str(), cfg.spectral, cfg.lens.model, cfg.lens.blades), (1.5, "vcm", true, LensModel::Thin, 6));
        assert_eq!(cfg.scene.mask_preset, "jlcpcb-purple");

        // 設定ファイルの露出が書き換えられたら、露出だけ設定ファイルを使う（ほかの項目とビューアだけの項目は覚えたもの）
        let mut edited = ProjectConfig::default();
        edited.exposure_ev = -1.0;
        let got = usable_view_values(&t, pcb, &ViewSettings::of(&edited).values());
        assert!(!got.contains_key("exposure_ev"));
        assert_eq!(got.get("integrator").and_then(|v| v.as_str()), Some("vcm"));
        assert_eq!(got.get("max_spp").and_then(|v| v.as_integer()), Some(1024));

        // 視点
        let mut v = ViewValues::new();
        assert_eq!(camera_of(&v), None);
        set_camera(&mut v, glam::Vec3::new(1.0, -2.0, 3.5), 120.0);
        let v: Table = Table::from_iter([("view".to_string(), Value::Table(v))]).to_string().parse::<Table>().unwrap()["view"].as_table().unwrap().clone();
        assert_eq!(camera_of(&v), Some((glam::Vec3::new(1.0, -2.0, 3.5), 120.0)));

        // 忘れる：環境マップも含めて、この基板の分だけ消える
        forget_in(&mut t, pcb);
        assert!(usable_view_values(&t, pcb, &config_then).is_empty());
        assert_eq!(remembered_in(&t, pcb), None);
        assert!(remembered_in(&t, other).is_some());
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
