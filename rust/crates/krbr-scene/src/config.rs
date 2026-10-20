//! プロジェクト設定（<プロジェクト名>.krbr/board.toml, environment.toml）。C++ 版と同じファイル・同じ項目

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use glam::Vec3;
use toml::Value;

use crate::lens::{LensModel, LensParams};
use crate::mask_presets::{find_mask_preset, mask_presets};
use crate::scene::SceneOptions;

#[derive(Debug, Clone)]
pub struct ProjectConfig {
    // board.toml
    pub scene: SceneOptions,
    /// ${VAR} の追加・上書き
    pub path_vars: BTreeMap<String, String>,
    pub model_search_dirs: Vec<PathBuf>,
    pub use_kicad_cli_text: bool,
    /// 空なら自動検出
    pub kicad_cli: String,

    // environment.toml
    pub hdr: String,
    pub env_intensity: f32,
    pub env_rotation_deg: f32,
    pub elevation_deg: f64,
    pub azimuth_deg: f64,
    pub fov_deg: f64,
    pub margin: f64,
    pub width: u32,
    pub height: u32,
    pub spp: u32,
    pub max_depth: u32,
    pub exposure_ev: f32,
    /// 間接光だけに効く上限（ホタル対策）
    pub radiance_clamp: f32,
    /// 分光モード（代表波長方式、4 波長）
    pub spectral: bool,
    pub integrator: String,
    /// VCM の近傍の半径の初期値（mm）。0 で自動
    pub vcm_radius: f32,
    pub lens: LensParams,

    pub loaded_files: Vec<PathBuf>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            scene: SceneOptions::default(),
            path_vars: BTreeMap::new(),
            model_search_dirs: Vec::new(),
            use_kicad_cli_text: true,
            kicad_cli: String::new(),
            hdr: String::new(),
            env_intensity: 1.0,
            env_rotation_deg: 0.0,
            elevation_deg: 35.0,
            azimuth_deg: 45.0,
            fov_deg: 30.0,
            margin: 0.04,
            width: 1920,
            height: 1080,
            spp: 64,
            max_depth: 8,
            exposure_ev: 0.7,
            radiance_clamp: 10.0,
            spectral: false,
            integrator: "pt".into(),
            vcm_radius: 0.0,
            lens: LensParams::default(),
            loaded_files: Vec::new(),
        }
    }
}

pub fn project_config_dir(pcb: &Path) -> PathBuf {
    let stem = pcb.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    pcb.parent().unwrap_or(Path::new(".")).join(format!("{stem}.krbr"))
}

/// レジストの色の指定（プリセット名 "jlcpcb-purple" など、または "r,g,b"）を SceneOptions に反映する
pub fn apply_mask_color_spec(o: &mut SceneOptions, spec: &str) -> Result<(), String> {
    if let Some(p) = find_mask_preset(spec) {
        o.mask_color = p.srgb;
        o.mask_color_set = true;
        o.mask_roughness = p.roughness;
        o.mask_scatter = p.scatter;
        o.mask_preset = p.name.to_string();
        if !o.silk_color_set {
            o.silk_color = p.silk_srgb; // 黄・白のレジストでは黒いシルク
        }
        return Ok(());
    }
    let v: Vec<f32> = spec.split(',').filter_map(|t| t.trim().parse().ok()).collect();
    if v.len() == 3 {
        o.mask_color = Vec3::new(v[0], v[1], v[2]);
        o.mask_color_set = true;
        o.mask_preset.clear();
        return Ok(());
    }
    let names: Vec<&str> = mask_presets().iter().map(|p| p.name).collect();
    Err(format!("'{spec}' is not a mask preset or r,g,b. Presets: {}", names.join(" ")))
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Float(f) => Some(*f),
        Value::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

fn get<'a>(t: &'a toml::Table, path: &[&str]) -> Option<&'a Value> {
    let mut cur: &Value = t.get(path[0])?;
    for k in &path[1..] {
        cur = cur.as_table()?.get(*k)?;
    }
    Some(cur)
}

fn num<T: From<f32>>(t: &toml::Table, path: &[&str], dst: &mut T)
where
    T: Copy,
{
    if let Some(v) = get(t, path).and_then(as_f64) {
        *dst = T::from(v as f32);
    }
}

fn num_f64(t: &toml::Table, path: &[&str], dst: &mut f64) {
    if let Some(v) = get(t, path).and_then(as_f64) {
        *dst = v;
    }
}

fn num_u32(t: &toml::Table, path: &[&str], dst: &mut u32) {
    if let Some(v) = get(t, path).and_then(as_f64) {
        *dst = v as u32;
    }
}

fn num_i32(t: &toml::Table, path: &[&str], dst: &mut i32) {
    if let Some(v) = get(t, path).and_then(as_f64) {
        *dst = v as i32;
    }
}

fn boolean(t: &toml::Table, path: &[&str], dst: &mut bool) {
    if let Some(Value::Boolean(b)) = get(t, path) {
        *dst = *b;
    }
}

fn string(t: &toml::Table, path: &[&str], dst: &mut String) {
    if let Some(Value::String(s)) = get(t, path) {
        *dst = s.clone();
    }
}

fn color(t: &toml::Table, path: &[&str], dst: &mut Vec3, set: Option<&mut bool>) {
    if let Some(Value::Array(a)) = get(t, path) {
        if a.len() == 3 {
            if let (Some(r), Some(g), Some(b)) = (as_f64(&a[0]), as_f64(&a[1]), as_f64(&a[2])) {
                *dst = Vec3::new(r as f32, g as f32, b as f32);
                if let Some(s) = set {
                    *s = true;
                }
            }
        }
    }
}

fn parse_file(p: &Path) -> Option<toml::Table> {
    let text = std::fs::read_to_string(p).ok()?;
    match toml::from_str::<toml::Table>(&text) {
        Ok(t) => Some(t),
        Err(e) => {
            krbr_core::warn!("{}: {} - ignored", p.display(), e.message());
            None
        }
    }
}

fn resolve_rel(dir: &Path, s: &str) -> PathBuf {
    let p = PathBuf::from(s);
    if p.is_relative() {
        dir.join(p)
    } else {
        p
    }
}

/// 既定値に、見つかった設定ファイルの内容を重ねる。失敗したファイルは警告して無視
/// `--init`：<名前>.krbr\board.toml と environment.toml のひな形を書く（あるものは上書きしない。C++ 版と同じ）
pub fn write_default_configs(pcb: &Path) {
    let dir = project_config_dir(pcb);
    for (name, text) in [("board.toml", crate::config_templates::BOARD_TOML), ("environment.toml", crate::config_templates::ENVIRONMENT_TOML)] {
        let p = dir.join(name);
        if p.exists() {
            krbr_core::info!("--init: keeping existing {}", p.display());
            continue;
        }
        match std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&p, text)) {
            Ok(()) => krbr_core::info!("--init: wrote {}", p.display()),
            Err(e) => krbr_core::warn!("--init: cannot write {}: {}", p.display(), e),
        }
    }
}

pub fn load_project_config(pcb: &Path) -> ProjectConfig {
    let mut c = ProjectConfig::default();
    let dir = project_config_dir(pcb);
    let board_toml = dir.join("board.toml");
    if let Some(t) = parse_file(&board_toml) {
        c.loaded_files.push(board_toml);
        let o = &mut c.scene;
        string(&t, &["board", "finish"], &mut o.finish);
        match get(&t, &["board", "mask_color"]) {
            Some(Value::String(name)) => {
                if let Err(e) = apply_mask_color_spec(o, name) {
                    krbr_core::warn!("board.toml mask_color: {}", e);
                }
            }
            _ => color(&t, &["board", "mask_color"], &mut o.mask_color, Some(&mut o.mask_color_set)),
        }
        num(&t, &["board", "mask_roughness"], &mut o.mask_roughness);
        num(&t, &["board", "mask_scatter"], &mut o.mask_scatter);
        color(&t, &["board", "silk_color"], &mut o.silk_color, Some(&mut o.silk_color_set));
        color(&t, &["board", "fr4_color"], &mut o.fr4_color, None);
        boolean(&t, &["board", "clip_silk_by_mask"], &mut o.layer_art.clip_silk_by_mask);
        num_f64(&t, &["board", "texture_px_per_mm"], &mut o.layer_art.px_per_mm);
        boolean(&t, &["board", "include_dnp"], &mut o.include_dnp);
        boolean(&t, &["relief", "enabled"], &mut o.relief);
        if let Some(v) = get(&t, &["relief", "copper_um"]).and_then(as_f64) {
            o.relief_params.copper = v / 1000.0;
            o.relief_copper_from_stackup = false;
        }
        for (key, dst) in [("mask_on_copper_um", &mut o.relief_params.mask_on_copper), ("mask_on_substrate_um", &mut o.relief_params.mask_on_substrate), ("silk_um", &mut o.relief_params.silk), ("via_dimple_um", &mut o.relief_params.via_dimple)] {
            if let Some(v) = get(&t, &["relief", key]).and_then(as_f64) {
                *dst = v / 1000.0;
            }
        }
        num(&t, &["surface", "bump_intensity"], &mut o.bump_intensity);
        num_f64(&t, &["placeholder", "height_mm"], &mut o.placeholder_height);
        color(&t, &["placeholder", "color"], &mut o.placeholder_color, None);
        if let Some(Value::Table(vars)) = get(&t, &["paths", "vars"]) {
            for (k, v) in vars {
                if let Value::String(s) = v {
                    c.path_vars.insert(k.clone(), s.clone());
                }
            }
        }
        if let Some(Value::Array(dirs)) = get(&t, &["paths", "model_search_dirs"]) {
            for d in dirs {
                if let Value::String(s) = d {
                    c.model_search_dirs.push(resolve_rel(dir.parent().unwrap_or(Path::new(".")), s));
                }
            }
        }
        boolean(&t, &["kicad_cli", "use_for_text"], &mut c.use_kicad_cli_text);
        string(&t, &["kicad_cli", "path"], &mut c.kicad_cli);
    }
    let env_toml = dir.join("environment.toml");
    if let Some(t) = parse_file(&env_toml) {
        c.loaded_files.push(env_toml);
        if let Some(Value::String(h)) = get(&t, &["environment", "hdr"]) {
            // "builtin:studio" などの内蔵の環境マップはパスではない
            c.hdr = if h.starts_with("builtin:") { h.clone() } else { resolve_rel(&dir, h).to_string_lossy().into_owned() };
        }
        num(&t, &["environment", "intensity"], &mut c.env_intensity);
        num(&t, &["environment", "rotation_deg"], &mut c.env_rotation_deg);
        num_f64(&t, &["camera", "elevation_deg"], &mut c.elevation_deg);
        num_f64(&t, &["camera", "azimuth_deg"], &mut c.azimuth_deg);
        num_f64(&t, &["camera", "fov_deg"], &mut c.fov_deg);
        num_f64(&t, &["camera", "margin"], &mut c.margin);
        num_u32(&t, &["render", "width"], &mut c.width);
        num_u32(&t, &["render", "height"], &mut c.height);
        num_u32(&t, &["render", "spp"], &mut c.spp);
        num_u32(&t, &["render", "max_depth"], &mut c.max_depth);
        num(&t, &["render", "exposure_ev"], &mut c.exposure_ev);
        num(&t, &["render", "radiance_clamp"], &mut c.radiance_clamp);
        boolean(&t, &["render", "spectral"], &mut c.spectral);
        string(&t, &["render", "integrator"], &mut c.integrator);
        num(&t, &["render", "vcm_radius"], &mut c.vcm_radius);
        if let Some(Value::String(m)) = get(&t, &["lens", "model"]) {
            c.lens.model = match m.as_str() {
                "thin" => LensModel::Thin,
                "realistic" => LensModel::Realistic,
                _ => LensModel::Pinhole,
            };
        }
        num(&t, &["lens", "focal_length_mm"], &mut c.lens.focal_length_mm);
        num(&t, &["lens", "f_number"], &mut c.lens.f_number);
        num(&t, &["lens", "focus_distance_mm"], &mut c.lens.focus_distance_mm);
        num_i32(&t, &["lens", "blades"], &mut c.lens.blades);
        num(&t, &["lens", "blade_rotation_deg"], &mut c.lens.blade_rotation_deg);
        num(&t, &["lens", "blade_roundness"], &mut c.lens.blade_roundness);
        boolean(&t, &["lens", "natural_vignetting"], &mut c.lens.natural_vignetting);
        num(&t, &["lens", "optical_vignetting"], &mut c.lens.optical_vignetting);
        num(&t, &["lens", "abbe_number"], &mut c.lens.abbe_number);
        num(&t, &["lens", "lateral_ca"], &mut c.lens.lateral_ca);
        if let Some(Value::String(f)) = get(&t, &["lens", "lens_file"]) {
            if !f.is_empty() {
                c.lens.lens_file = resolve_rel(&dir, f).to_string_lossy().into_owned();
            }
        }
    }
    for f in &c.loaded_files {
        krbr_core::info!("config: {}", f.display());
    }
    if c.loaded_files.is_empty() {
        krbr_core::info!("config: no {} found, using defaults", dir.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_templates_are_the_defaults_and_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("krbr_rust_init_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let pcb = dir.join("b.kicad_pcb");
        write_default_configs(&pcb);
        let cfg_dir = dir.join("b.krbr");
        assert!(cfg_dir.join("board.toml").is_file() && cfg_dir.join("environment.toml").is_file());
        // ひな形の値は既定値と同じ（書いても見た目が変わらない）
        let (c, d) = (load_project_config(&pcb), ProjectConfig::default());
        assert_eq!(c.loaded_files.len(), 2);
        assert_eq!((c.width, c.height, c.spp, c.max_depth), (d.width, d.height, d.spp, d.max_depth));
        assert_eq!((c.exposure_ev, c.radiance_clamp, c.env_intensity), (d.exposure_ev, d.radiance_clamp, d.env_intensity));
        assert_eq!((c.elevation_deg, c.azimuth_deg, c.fov_deg, c.margin), (d.elevation_deg, d.azimuth_deg, d.fov_deg, d.margin));
        assert_eq!((c.integrator.as_str(), c.spectral, c.hdr.as_str()), (d.integrator.as_str(), d.spectral, ""));
        assert_eq!((c.lens.model, c.lens.f_number, c.lens.focal_length_mm), (d.lens.model, d.lens.f_number, d.lens.focal_length_mm));
        assert_eq!((c.scene.mask_roughness, c.scene.mask_scatter, c.scene.bump_intensity), (d.scene.mask_roughness, d.scene.mask_scatter, d.scene.bump_intensity));
        assert_eq!((c.scene.placeholder_height, c.scene.placeholder_color), (d.scene.placeholder_height, d.scene.placeholder_color));
        assert_eq!(c.scene.relief_params.via_dimple, d.scene.relief_params.via_dimple);
        assert_eq!(c.use_kicad_cli_text, d.use_kicad_cli_text);
        // 既にあるファイルは上書きしない
        std::fs::write(cfg_dir.join("board.toml"), "[surface]\nbump_intensity = 0.5\n").unwrap();
        write_default_configs(&pcb);
        assert_eq!(std::fs::read_to_string(cfg_dir.join("board.toml")).unwrap(), "[surface]\nbump_intensity = 0.5\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_both_files() {
        let dir = std::env::temp_dir().join("krbr_rust_config_test");
        let cfg_dir = dir.join("board.krbr");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("board.toml"), "[board]\nmask_color = \"jlcpcb-white\"\nmask_scatter = 0.5\n[relief]\nvia_dimple_um = 30\n[paths]\nvars = { X = \"y\" }\n").unwrap();
        std::fs::write(cfg_dir.join("environment.toml"), "[environment]\nhdr = \"../studio.exr\"\n[render]\nspp = 16\nwidth = 640\n[lens]\nmodel = \"thin\"\nblades = 6\n").unwrap();
        let c = load_project_config(&dir.join("board.kicad_pcb"));
        assert_eq!(c.scene.mask_preset, "jlcpcb-white");
        assert_eq!(c.scene.mask_scatter, 0.5); // プリセットの後の指定が勝つ
        assert_eq!(c.scene.silk_color, Vec3::new(0.08, 0.08, 0.08));
        assert!((c.scene.relief_params.via_dimple - 0.03).abs() < 1e-9);
        assert_eq!(c.path_vars["X"], "y");
        assert!(c.hdr.ends_with("studio.exr"));
        assert_eq!((c.spp, c.width, c.height), (16, 640, 1080));
        assert_eq!(c.lens.model, LensModel::Thin);
        assert_eq!(c.lens.blades, 6);
        assert_eq!(c.loaded_files.len(), 2);
    }
}
