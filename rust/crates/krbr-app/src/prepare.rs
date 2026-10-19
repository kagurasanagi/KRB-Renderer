//! render：基板の読み込み、設定、シーン構築、環境マップ、オフスクリーン描画、ファイル出力（C++ 版 prepare.cpp + cmdRender）

use std::path::{Path, PathBuf};

use krbr_core::{info, warn};
use krbr_kicad::{load_board, Board, ModelResolver};
use krbr_model::{MeshParams, ModelLibrary};
use krbr_render::{build_env_distribution, image_io, render_offscreen, Context, ContextOptions, FrameParams, GpuScene, RenderSettings};
use krbr_scene::config::{apply_mask_color_spec, load_project_config, ProjectConfig};
use krbr_scene::scene::{build_scene, load_material_setup, SceneContext};
use krbr_scene::{frame_camera, orbit_camera, Camera, CameraParams, LensModel, MaterialResolver, Scene};

#[derive(Debug, Clone, Default)]
pub struct RenderOptions {
    pub pcb: PathBuf,
    pub hdr: String,
    pub out: PathBuf,
    pub spp: Option<u32>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub max_depth: Option<u32>,
    pub elevation: Option<f64>,
    pub azimuth: Option<f64>,
    pub fov: Option<f64>,
    pub exposure: Option<f32>,
    pub env_intensity: Option<f32>,
    pub env_rotation: Option<f32>,
    pub radiance_clamp: Option<f32>,
    /// 注視点（KiCad の基板座標 X, Y、mm）
    pub look_at: Vec<f64>,
    pub zoom: f64,
    pub integrator: String,
    pub seed: u32,
    pub no_cache: bool,
    pub validate: bool,
    pub spectral: Option<bool>,
    pub lens: String,
    pub f_number: Option<f32>,
    pub focal_length: Option<f32>,
    pub focus_distance: Option<f32>,
    pub blades: Option<i32>,
    pub mask_color: String,
}

pub struct PreparedScene {
    pub board: Board,
    pub cfg: ProjectConfig,
    pub scene: Scene,
    pub env: image_io::ImageF,
    pub dist: krbr_render::EnvDistribution,
}

pub fn load_board_or_report(pcb: &Path) -> Option<Board> {
    info!("loading {}", std::path::absolute(pcb).unwrap_or(pcb.to_path_buf()).display());
    match load_board(pcb) {
        Ok(b) => {
            info!("board v{}: {} footprints, {} pads, {} tracks, {} vias, {} zone fills, {} graphics", b.version, b.footprints.len(), b.pads.len(), b.tracks.len(), b.vias.len(), b.zones.len(), b.graphics.len());
            if b.unfilled_zones > 0 {
                warn!("{} zone(s) have no fill data (refill zones in KiCad and save)", b.unfilled_zones);
            }
            Some(b)
        }
        Err(e) => {
            krbr_core::error!("{}", e);
            None
        }
    }
}

pub fn prepare_scene(o: &RenderOptions) -> Option<PreparedScene> {
    let board = load_board_or_report(&o.pcb)?;
    let mut cfg = load_project_config(&board.file);
    if let Some(v) = o.spp {
        cfg.spp = v;
    }
    if let Some(v) = o.width {
        cfg.width = v;
    }
    if let Some(v) = o.height {
        cfg.height = v;
    }
    if let Some(v) = o.max_depth {
        cfg.max_depth = v;
    }
    if let Some(v) = o.elevation {
        cfg.elevation_deg = v;
    }
    if let Some(v) = o.azimuth {
        cfg.azimuth_deg = v;
    }
    if let Some(v) = o.fov {
        cfg.fov_deg = v;
    }
    if let Some(v) = o.exposure {
        cfg.exposure_ev = v;
    }
    if let Some(v) = o.env_intensity {
        cfg.env_intensity = v;
    }
    if let Some(v) = o.env_rotation {
        cfg.env_rotation_deg = v;
    }
    if let Some(v) = o.radiance_clamp {
        cfg.radiance_clamp = v;
    }
    if let Some(v) = o.spectral {
        cfg.spectral = v;
    }
    if !o.mask_color.is_empty() {
        if let Err(e) = apply_mask_color_spec(&mut cfg.scene, &o.mask_color) {
            warn!("--mask-color: {}", e);
        }
    }
    if !o.lens.is_empty() {
        cfg.lens.model = match o.lens.as_str() {
            "thin" => LensModel::Thin,
            "realistic" => LensModel::Realistic,
            _ => LensModel::Pinhole,
        };
    }
    if let Some(v) = o.f_number {
        cfg.lens.f_number = v;
    }
    if let Some(v) = o.focal_length {
        cfg.lens.focal_length_mm = v;
    }
    if let Some(v) = o.focus_distance {
        cfg.lens.focus_distance_mm = v;
    }
    if let Some(v) = o.blades {
        cfg.lens.blades = v;
    }
    if !o.integrator.is_empty() {
        cfg.integrator = o.integrator.clone();
    }
    if !o.hdr.is_empty() {
        cfg.hdr = o.hdr.clone();
    }
    if cfg.hdr.is_empty() {
        krbr_core::error!("no environment map: pass --hdr <file.hdr|file.exr> or set [environment] hdr in environment.toml");
        return None;
    }
    if cfg.width == 0 || cfg.height == 0 || cfg.spp == 0 {
        krbr_core::error!("invalid size or spp");
        return None;
    }
    let cache_dir = krbr_core::platform::cache_dir();

    // 部品の材質：材質ライブラリ、全体の対応表、プロジェクトの上書き
    let materials = load_material_setup(&board.file);
    let stroke = board.stroke_texts.iter().filter(|t| t.layer == "F.SilkS" || t.layer == "B.SilkS").count();
    if stroke > 0 {
        warn!("{} stroke-font silkscreen text(s) will be missing (kicad-cli silk is not ported yet)", stroke);
    }
    let resolver = ModelResolver::new(&board.project_dir, &cfg.path_vars, &cfg.model_search_dirs);
    let library = ModelLibrary::new(cache_dir.clone(), MeshParams::default(), !o.no_cache);
    info!("mesh cache: {}{}", cache_dir.join("meshes").display(), if o.no_cache { " (disabled)" } else { "" });
    let scene = {
        let material_resolver = MaterialResolver::new(&materials.library, &materials.project, &materials.global);
        build_scene(SceneContext { board: &board, resolver: &resolver, library: &library, materials: &material_resolver, options: &cfg.scene, relief_cache: None, async_models: false })
    };
    let st = library.stats();
    info!("models: {} loaded+meshed, {} from cache, {} failed", st.loaded, st.cache_hits, st.failed);

    let env = match image_io::load_hdr_image(Path::new(&cfg.hdr)) {
        Ok(e) => e,
        Err(e) => {
            krbr_core::error!("environment map {}: {}", cfg.hdr, e);
            return None;
        }
    };
    info!("environment: {} ({}x{})", cfg.hdr, env.width, env.height);
    let dist = build_env_distribution(&env, 2048);
    Some(PreparedScene { board, cfg, scene, env, dist })
}

pub fn camera_params(cfg: &ProjectConfig, width: u32, height: u32) -> CameraParams {
    CameraParams { elevation_deg: cfg.elevation_deg, azimuth_deg: cfg.azimuth_deg, fov_deg: cfg.fov_deg, margin: cfg.margin, width, height }
}

/// 自動の構図に --look-at / --zoom を反映したカメラ
pub fn initial_camera(prep: &PreparedScene, o: &RenderOptions, width: u32, height: u32) -> Camera {
    let cfg = &prep.cfg;
    let cam = frame_camera(&camera_params(cfg, width, height), &prep.scene.framing_points);
    let mut target = cam.target;
    if o.look_at.len() == 2 {
        let b = &prep.scene.board;
        target = krbr_geom::board_mesh::board_to_world(glam::DVec2::new(o.look_at[0], o.look_at[1]), b.thickness / 2.0, b.center);
    }
    let dist = cam.distance / o.zoom.max(1e-3) as f32;
    orbit_camera(target, dist, cfg.elevation_deg, cfg.azimuth_deg, cfg.fov_deg, cam.aspect)
}

pub fn frame_params(cfg: &ProjectConfig, cam: Camera, seed: u32) -> FrameParams {
    FrameParams {
        camera: cam,
        env_intensity: cfg.env_intensity,
        env_rotation_rad: cfg.env_rotation_deg.to_radians(),
        radiance_clamp: cfg.radiance_clamp,
        spectral: cfg.spectral,
        vcm_radius: cfg.vcm_radius,
        lens: cfg.lens.clone(),
        max_depth: cfg.max_depth.max(1),
        seed,
        ..Default::default()
    }
}

pub fn cmd_render(o: &RenderOptions) -> anyhow::Result<()> {
    let out_dir = o.out.parent().filter(|p| !p.as_os_str().is_empty()).map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
    let stem = o.out.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "board".into());
    std::fs::create_dir_all(&out_dir)?;

    let Some(mut prep) = prepare_scene(o) else { anyhow::bail!("scene preparation failed") };
    info!("project: {} ({})", prep.board.project_name, prep.board.project_dir.display());
    let cfg = &prep.cfg;
    let cam = initial_camera(&prep, o, cfg.width, cfg.height);
    info!("camera: elevation {:.1} deg, azimuth {:.1} deg, fov {:.1} deg, position ({:.1}, {:.1}, {:.1}) mm", cfg.elevation_deg, cfg.azimuth_deg, cfg.fov_deg, cam.position.x, cam.position.y, cam.position.z);

    let ctx = Context::new(&ContextOptions { validation: o.validate, low_queue_priority: true, ..Default::default() }).map_err(|e| anyhow::anyhow!(e))?;
    let result = {
        let mut gpu = GpuScene::new(&ctx);
        gpu.build(&prep.scene, &prep.env, &prep.dist, cfg.scene.bump_intensity);
        prep.env.rgba = Vec::new();
        let rs = RenderSettings { width: cfg.width, height: cfg.height, spp: cfg.spp, integrator: cfg.integrator.clone(), frame: frame_params(cfg, cam, o.seed) };
        render_offscreen(&ctx, &gpu, &rs).map_err(|e| anyhow::anyhow!("render failed: {e}"))?
    };

    let mut failures = 0;
    let mut report = |r: Result<(), String>, p: &Path| match r {
        Ok(()) => info!("wrote {}", p.display()),
        Err(e) => {
            krbr_core::error!("{}: {}", p.display(), e);
            failures += 1;
        }
    };
    let png = out_dir.join(format!("{stem}.png"));
    report(image_io::save_png_tonemapped(&png, &result.color, cfg.exposure_ev), &png);
    let exr = out_dir.join(format!("{stem}.exr"));
    report(image_io::save_exr(&exr, &result.color), &exr);
    let alb = out_dir.join(format!("{stem}_albedo.exr"));
    report(image_io::save_exr(&alb, &result.albedo), &alb);
    let nrm = out_dir.join(format!("{stem}_normal.exr"));
    report(image_io::save_exr(&nrm, &result.normal), &nrm);
    write_missing_report(&prep.scene, &out_dir);
    if failures > 0 {
        anyhow::bail!("{failures} file(s) could not be written");
    }
    Ok(())
}

/// 不足・失敗したモデルの一覧をログに出し、missing_models.json / .txt を書く
fn write_missing_report(scene: &Scene, out_dir: &Path) {
    if scene.missing.is_empty() {
        let _ = std::fs::write(out_dir.join("missing_models.json"), "[]\n");
        let _ = std::fs::write(out_dir.join("missing_models.txt"), "");
        return;
    }
    warn!("{} 3D model(s) missing or unreadable:", scene.missing.len());
    let mut txt = String::new();
    let mut json = String::from("[\n");
    for (i, m) in scene.missing.iter().enumerate() {
        warn!("  {:<6} {} [{}]", m.reference, m.spec, m.reason);
        txt += &format!("{}\t{}\t{}\t{}\t{}\n", m.reference, m.footprint, m.spec, m.resolved, m.reason);
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        json += &format!(
            "  {{\"reference\": \"{}\", \"footprint\": \"{}\", \"value\": \"{}\", \"model\": \"{}\", \"resolved\": \"{}\", \"reason\": \"{}\"}}{}\n",
            esc(&m.reference),
            esc(&m.footprint),
            esc(&m.value),
            esc(&m.spec),
            esc(&m.resolved),
            esc(&m.reason),
            if i + 1 < scene.missing.len() { "," } else { "" }
        );
    }
    json += "]\n";
    let _ = std::fs::write(out_dir.join("missing_models.json"), json);
    let _ = std::fs::write(out_dir.join("missing_models.txt"), txt);
}
