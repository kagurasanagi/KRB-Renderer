//! render：基板の読み込み、設定、シーン構築、環境マップ、オフスクリーン描画、ファイル出力（C++ 版 prepare.cpp + cmdRender）

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use krbr_core::{debug, info, warn};
use krbr_geom::tiles::ReliefTileCache;
use krbr_kicad::{diff_boards, load_board, Board, BoardDiff, ModelResolver};
use krbr_model::{MeshParams, ModelLibrary};
use krbr_render::studio_env::STUDIO;
use krbr_render::{build_env_distribution, image_io, DenoiseQuality, is_builtin, load_environment_map, render_offscreen, Context, ContextOptions, FrameParams, GpuScene, RenderSettings};
use krbr_scene::config::{apply_mask_color_spec, load_project_config, write_default_configs, ProjectConfig};
use krbr_scene::scene::{build_scene, load_material_setup, MaterialSetup, SceneContext};
use krbr_scene::{frame_camera, orbit_camera, Camera, CameraParams, LensModel, MaterialResolver, Scene, SceneOptions};

use crate::kicad_cli::{find_kicad_cli, plot_stroke_silk, SilkPlot};
use crate::remember;

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
    /// kicad-cli で線フォントの文字（シルク）を描かない
    pub no_kicad_cli: bool,
    /// <名前>.krbr\board.toml と environment.toml のひな形を書く（あるものは上書きしない）
    pub init: bool,
    /// ノイズ除去（OIDN）。None ならコマンドの既定（render・view は無効、watch は有効）
    pub denoise: Option<bool>,
    /// ノイズ除去した画像と元の画像を混ぜる強さ（0..1、1 で OIDN の結果そのまま）
    pub denoise_strength: f32,
}

pub struct PreparedScene {
    pub board: Board,
    pub cfg: ProjectConfig,
    pub scene: Scene,
    pub env: image_io::ImageF,
    pub dist: krbr_render::EnvDistribution,
    /// environment.toml の hdr（空なら指定なし）。選んだ環境マップを覚えるときに一緒に残す
    pub config_hdr: String,
    /// 線フォントの文字の書き出しに使う kicad-cli（無効・見つからなければ None）
    pub kicad_cli: Option<KicadCliSilk>,
    /// シーンを組み立て直すための状態。裏のスレッドで読み直している間は None（スレッドが持っている）
    pub source: Option<SceneSource>,
}

/// 線フォントの文字（シルク）の書き出し（kicad-cli）
#[derive(Debug, Clone)]
pub struct KicadCliSilk {
    pub cli: PathBuf,
    pub cache_dir: PathBuf,
    pub use_cache: bool,
}

impl KicadCliSilk {
    pub fn plot(&self, board: &Board) -> SilkPlot {
        plot_stroke_silk(&self.cli, &board.file, &board.project_name, &self.cache_dir, self.use_cache)
    }
}

/// シルク層の線フォントの文字の数（krbr は形を作れないので kicad-cli で描く）
pub fn stroke_silk_count(board: &Board) -> usize {
    board.stroke_texts.iter().filter(|t| t.layer == "F.SilkS" || t.layer == "B.SilkS").count()
}

/// kicad-cli を探し、線フォントの文字があれば書き出して cfg.scene.layer_art.extra_silk に入れる
fn setup_stroke_silk(board: &Board, cfg: &mut ProjectConfig, o: &RenderOptions, cache_dir: &Path) -> Option<KicadCliSilk> {
    let n = stroke_silk_count(board);
    if !cfg.use_kicad_cli_text || o.no_kicad_cli {
        if n > 0 {
            warn!("{} stroke-font silkscreen text(s) will be missing (kicad-cli disabled)", n);
        }
        return None;
    }
    let Some(cli) = find_kicad_cli(&cfg.kicad_cli) else {
        if n > 0 {
            warn!("kicad-cli not found; {} stroke-font silkscreen text(s) will be missing (set [kicad_cli] path in board.toml)", n);
        }
        return None;
    };
    let k = KicadCliSilk { cli, cache_dir: cache_dir.to_path_buf(), use_cache: !o.no_cache };
    if n > 0 {
        let t0 = Instant::now();
        let silk = k.plot(board);
        info!("kicad-cli silk: {} stroke-font text(s), {:.0} ms{} ({})", n, t0.elapsed().as_secs_f64() * 1000.0, if silk.from_cache { ", from cache" } else { "" }, k.cli.display());
        cfg.scene.layer_art.extra_silk = silk.side;
    }
    Some(k)
}

/// シーンを組み立てるのに使う状態（C++ 版 PreparedScene の resolver / library / materials / tiles）。
/// 保存の監視中は、読み直しのたびに裏のスレッドへ渡し、新しいシーンと一緒に返してもらう
pub struct SceneSource {
    pub resolver: ModelResolver,
    pub library: Arc<ModelLibrary>,
    pub materials: MaterialSetup,
    /// 凹凸タイルのキャッシュ。変わった範囲のタイルだけを作り直す
    pub tiles: ReliefTileCache,
}

/// 読み直しの統計
#[derive(Debug, Clone, Default)]
pub struct ReloadStats {
    pub diff: BoardDiff,
    pub parse_ms: f64,
    pub build_ms: f64,
    /// 作り直した凹凸タイルの数（面ごとに数える）
    pub tiles_rebuilt: usize,
}

impl SceneSource {
    /// シーンを組み立てる。async_models なら、まだ分割していない STEP は裏で分割し、それまでは箱で仮表示する
    pub fn build(&mut self, board: &Board, options: &SceneOptions, async_models: bool) -> Scene {
        let material_resolver = MaterialResolver::new(&self.materials.library, &self.materials.project, &self.materials.global);
        build_scene(SceneContext { board, resolver: &self.resolver, library: &self.library, materials: &material_resolver, options, relief_cache: Some(&mut self.tiles), async_models })
    }

    /// 基板を読み直し、前の基板との差分で変わった範囲のタイルを無効にしてから組み立てる（C++ 版 PreparedScene::reload）。
    /// 読めなければ Err（タイルのキャッシュは前のまま。次の保存を待つ）
    pub fn reload(&mut self, old: &Board, options: &SceneOptions) -> Result<(Board, Scene, ReloadStats), String> {
        let mut st = ReloadStats::default();
        let t0 = Instant::now();
        let board = load_board(&old.file).map_err(|e| format!("{e}"))?;
        st.parse_ms = t0.elapsed().as_secs_f64() * 1000.0;
        st.diff = diff_boards(old, &board);
        if st.diff.full {
            self.tiles.invalidate_all();
        } else {
            self.tiles.invalidate(&st.diff.dirty);
        }
        let t1 = Instant::now();
        let scene = self.build(&board, options, true);
        st.build_ms = t1.elapsed().as_secs_f64() * 1000.0;
        st.tiles_rebuilt = self.tiles.last_rebuilt.len();
        Ok((board, scene, st))
    }
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
    if o.init {
        write_default_configs(&board.file);
    }
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
    // 環境マップ：--hdr、この基板で最後に使ったもの、environment.toml、内蔵のスタジオ、の順
    let config_hdr = cfg.hdr.clone();
    let from_cli = !o.hdr.is_empty();
    if from_cli {
        cfg.hdr = o.hdr.clone();
    } else if let Some(r) = remember::remembered_hdr(&board.file) {
        if r.config_hdr != config_hdr {
            info!("environment.toml changed since the last run; using its environment map");
        } else if is_builtin(&r.hdr) || Path::new(&r.hdr).is_file() {
            info!("environment: last used for this board");
            cfg.hdr = r.hdr;
        } else {
            warn!("last used environment map {} no longer exists", r.hdr);
        }
    }
    if cfg.hdr.is_empty() {
        cfg.hdr = STUDIO.into();
    }
    if cfg.width == 0 || cfg.height == 0 || cfg.spp == 0 {
        krbr_core::error!("invalid size or spp");
        return None;
    }
    let cache_dir = krbr_core::platform::cache_dir();

    // 部品の材質：材質ライブラリ、全体の対応表、プロジェクトの上書き
    let materials = load_material_setup(&board.file);
    let kicad_cli = setup_stroke_silk(&board, &mut cfg, o, &cache_dir);
    let resolver = ModelResolver::new(&board.project_dir, &cfg.path_vars, &cfg.model_search_dirs);
    let library = Arc::new(ModelLibrary::new(cache_dir.clone(), MeshParams::default(), !o.no_cache));
    info!("mesh cache: {}{}", cache_dir.join("meshes").display(), if o.no_cache { " (disabled)" } else { "" });
    let mut source = SceneSource { resolver, library, materials, tiles: ReliefTileCache::default() };
    let scene = source.build(&board, &cfg.scene, false);
    let st = source.library.stats();
    info!("models: {} loaded+meshed, {} from cache, {} failed", st.loaded, st.cache_hits, st.failed);
    for a in &scene.assignments {
        debug!("material: {} {} [{}] -> {} ({})", a.reference, a.model_file, a.part_keys, a.material, a.source);
    }

    let env = match load_environment_map(&cfg.hdr) {
        Ok(e) => e,
        // コマンドラインで指定したものは誤りとして止める。設定ファイルのものは内蔵のスタジオで代える
        Err(e) if from_cli || cfg.hdr == STUDIO => {
            krbr_core::error!("environment map {}: {}", cfg.hdr, e);
            return None;
        }
        Err(e) => {
            warn!("environment map {}: {} (using the built-in studio instead)", cfg.hdr, e);
            cfg.hdr = STUDIO.into();
            load_environment_map(STUDIO).ok()?
        }
    };
    info!("environment: {} ({}x{})", cfg.hdr, env.width, env.height);
    if from_cli {
        cfg.hdr = remember::normalize_hdr(&cfg.hdr);
        remember::remember_hdr(&board.file, &cfg.hdr, &config_hdr);
    }
    let dist = build_env_distribution(&env, 2048);
    Some(PreparedScene { board, cfg, scene, env, dist, config_hdr, kicad_cli, source: Some(source) })
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
        let denoise = o.denoise.unwrap_or(false).then_some((DenoiseQuality::High, o.denoise_strength));
        let rs = RenderSettings { width: cfg.width, height: cfg.height, spp: cfg.spp, integrator: cfg.integrator.clone(), frame: frame_params(cfg, cam, o.seed), denoise };
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
    if let Some(d) = &result.denoised {
        let png = out_dir.join(format!("{stem}_denoised.png"));
        report(image_io::save_png_tonemapped(&png, d, cfg.exposure_ev), &png);
        let exr = out_dir.join(format!("{stem}_denoised.exr"));
        report(image_io::save_exr(&exr, d), &exr);
    }
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
