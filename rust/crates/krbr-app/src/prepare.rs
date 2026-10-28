//! render：基板の読み込み、設定、シーン構築、環境マップ、オフスクリーン描画、ファイル出力（C++ 版 prepare.cpp + cmdRender）

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use krbr_core::{debug, info, warn};
use krbr_geom::tiles::ReliefTileCache;
use krbr_kicad::{diff_boards, load_board, Board, BoardDiff, ModelResolver};
use krbr_model::{MeshParams, ModelLibrary};
use krbr_render::studio_env::{studio_environment, STUDIO};
use krbr_render::{build_env_distribution, image_io, DenoiseQuality, is_builtin, load_environment_map, render_offscreen, Context, ContextOptions, FrameParams, GpuScene, Progressive, RenderSettings};
use krbr_scene::config::{apply_mask_color_spec, load_project_config, write_default_configs, ProjectConfig};
use krbr_scene::scene::{build_scene, load_material_setup, MaterialSetup, SceneContext};
use krbr_scene::{frame_camera, orbit_camera, Camera, CameraParams, IntegratorKind, LensModel, MaterialResolver, Scene, SceneOptions};

use crate::kicad_cli::find_kicad_cli;
pub use crate::kicad_cli::KicadCliText;
use crate::remember;

/// 環境マップの重点サンプリングの分布の幅（画素）。これより大きい環境マップは縮小して分布を作る（放射輝度は元の解像度のまま）。
/// 4K の環境マップでも分布の表が数十 MB に収まり、太陽のような小さく明るい光源も 1 画素以上に残る大きさ
pub const ENV_DIST_RESOLUTION: u32 = 2048;

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
    /// グロー（明るい光のにじみ）：強さ（0 でなし）、しきい値、広がり
    pub glow: Option<f32>,
    pub glow_threshold: Option<f32>,
    pub glow_spread: Option<f32>,
    pub env_intensity: Option<f32>,
    pub env_rotation: Option<f32>,
    pub radiance_clamp: Option<f32>,
    /// 注視点（KiCad の基板座標 X, Y、mm）
    pub look_at: Vec<f64>,
    /// 自動の構図からの拡大率（正の値）。None なら指定なし（覚えた視点があればそれ、無ければ自動の構図のまま）
    pub zoom: Option<f64>,
    /// None なら設定ファイル（または覚えたもの）のまま
    pub integrator: Option<IntegratorKind>,
    pub seed: u32,
    pub no_cache: bool,
    pub validate: bool,
    pub spectral: Option<bool>,
    /// 背景を透明にする。None なら設定ファイル（または覚えたもの）のまま
    pub transparent_background: Option<bool>,
    /// はんだフィレットを作るか。None なら設定ファイルのまま
    pub solder_fillet: Option<bool>,
    /// None なら設定ファイル（または覚えたもの）のまま
    pub lens: Option<LensModel>,
    pub f_number: Option<f32>,
    pub focal_length: Option<f32>,
    pub focus_distance: Option<f32>,
    pub blades: Option<i32>,
    pub mask_color: String,
    /// 基材の材質（材質ライブラリの名前。空なら設定どおり）
    pub substrate: String,
    /// kicad-cli で線フォントの文字（シルク）を描かない
    pub no_kicad_cli: bool,
    /// 基板の設定ファイル <名前>.krbr のひな形を書く（あれば、無い表だけを足す）
    pub init: bool,
    /// ノイズ除去（OIDN）。None ならコマンドの既定（render・view は無効、watch は有効）
    pub denoise: Option<bool>,
    /// ノイズ除去した画像と元の画像を混ぜる強さ（0..1、1 で OIDN の結果そのまま）。None なら 1（ビューアは覚えた値）
    pub denoise_strength: Option<f32>,
    /// この基板で覚えた設定（パネルの設定・視点・環境マップ）を使わない（krbr bench：同じ条件で測るため）
    pub ignore_remembered: bool,
    /// 設定ファイル（基板の <名前>.krbr、全体設定の材質ライブラリと対応表）を読まず、既定値と組み込みの材質だけで作る
    /// （krbr bench の同梱の基板：どの PC でも同じシーンにするため）
    pub builtin_settings_only: bool,
}

pub struct PreparedScene {
    pub board: Board,
    pub cfg: ProjectConfig,
    pub scene: Scene,
    pub env: image_io::ImageF,
    pub dist: krbr_render::EnvDistribution,
    /// 設定ファイル（<名前>.krbr）の [environment] hdr（空なら指定なし）。選んだ環境マップを覚えるときに一緒に残す
    pub config_hdr: String,
    /// 設定ファイルとコマンドラインだけの設定（覚えたパネルの設定を当てる前）。ビューアの「設定どおり」「すべて既定に戻す」で使う
    pub configured: ProjectConfig,
    /// 設定ファイルだけのパネルの設定の値。パネルの設定を覚えるときに一緒に残す
    pub config_view: remember::ViewValues,
    /// この基板で覚えていたパネルの設定（設定ファイルが変わった項目を除く。ビューアだけの項目も入っている）
    pub remembered_view: remember::ViewValues,
    /// 線フォントの文字の書き出しに使う kicad-cli（無効・見つからなければ None）
    pub kicad_cli: Option<KicadCliText>,
    /// シーンを組み立て直すための状態。裏のスレッドで読み直している間は None（スレッドが持っている）
    pub source: Option<SceneSource>,
}

/// kicad-cli を探し、線フォントの文字・文字の枠（銅箔・マスク・シルク）があれば書き出して cfg.scene.layer_art.stroke_text に入れる
fn setup_stroke_text(board: &Board, cfg: &mut ProjectConfig, o: &RenderOptions, cache_dir: &Path) -> Option<KicadCliText> {
    let n = board.stroke_text_count();
    if !cfg.use_kicad_cli_text || o.no_kicad_cli {
        if n > 0 {
            warn!("{} stroke-font text(s) / text box(es) on copper, mask and silkscreen will be missing (kicad-cli disabled)", n);
        }
        return None;
    }
    let Some(cli) = find_kicad_cli(&cfg.kicad_cli) else {
        if n > 0 {
            warn!("kicad-cli not found; {} stroke-font text(s) / text box(es) on copper, mask and silkscreen will be missing (set [kicad_cli] path in <name>.krbr)", n);
        }
        return None;
    };
    let k = KicadCliText { cli, cache_dir: cache_dir.to_path_buf(), use_cache: !o.no_cache };
    if n > 0 {
        let t0 = Instant::now();
        let plot = k.plot(board);
        info!("kicad-cli text: {} stroke-font text(s) / text box(es), {:.0} ms{} ({})", n, t0.elapsed().as_secs_f64() * 1000.0, if plot.from_cache { ", from cache" } else { "" }, k.cli.display());
        cfg.scene.layer_art.stroke_text = plot.side;
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
    /// はんだフィレットのキャッシュ。パッドもモデルも変わらない部品は計算し直さない
    pub solder: krbr_scene::solder::SolderCache,
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
        build_scene(SceneContext { board, resolver: &self.resolver, library: &self.library, materials: &material_resolver, options, relief_cache: Some(&mut self.tiles), solder_cache: Some(&mut self.solder), async_models })
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
            if let Some(m) = b.unfilled_zones_message() {
                warn!("{m}");
            }
            Some(b)
        }
        Err(e) => {
            krbr_core::error!("{}", e);
            None
        }
    }
}

/// コマンドラインの指定を設定に重ねる（環境マップ（--hdr）は別に扱う）
fn apply_command_line(cfg: &mut ProjectConfig, o: &RenderOptions) {
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
    if let Some(v) = o.glow {
        cfg.glow = v.max(0.0);
    }
    if let Some(v) = o.glow_threshold {
        cfg.glow_threshold = v.max(0.0);
    }
    if let Some(v) = o.glow_spread {
        cfg.glow_spread = v.clamp(0.0, 1.0);
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
    if let Some(v) = o.transparent_background {
        cfg.transparent_background = v;
    }
    if let Some(v) = o.solder_fillet {
        cfg.scene.solder_fillet = v;
    }
    if !o.substrate.is_empty() {
        cfg.scene.substrate = if o.substrate == "fr4" { String::new() } else { o.substrate.clone() };
    }
    if !o.mask_color.is_empty() {
        if let Err(e) = apply_mask_color_spec(&mut cfg.scene, &o.mask_color) {
            warn!("--mask-color: {}", e);
        }
    }
    if let Some(m) = o.lens {
        cfg.lens.model = m;
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
    if let Some(k) = o.integrator {
        cfg.integrator = k;
    }
}

pub fn prepare_scene(o: &RenderOptions) -> Option<PreparedScene> {
    // 読み込みは利用者が待っているので、凹凸のタイルなどの並列の処理に論理コアをすべて使う（保存のたびの読み直しは半分のまま）
    let _all_cores = krbr_core::platform::AllCores::new();
    let board = load_board_or_report(&o.pcb)?;
    if o.init {
        write_default_configs(&board.file);
    }
    // 設定ファイル → この基板で覚えたパネルの設定 → コマンドライン の順に重ねる
    let files = if o.builtin_settings_only {
        info!("config: built-in defaults only (board and global settings are not read)");
        ProjectConfig::default()
    } else {
        load_project_config(&board.file)
    };
    let config_view = remember::ViewSettings::of(&files).values();
    let remembered_view = if o.ignore_remembered { remember::ViewValues::new() } else { remember::remembered_view(&board.file, &config_view) };
    let mut configured = files.clone();
    apply_command_line(&mut configured, o);
    let mut cfg = files;
    if !remembered_view.is_empty() {
        info!("panel settings: {} item(s) last used for this board", remembered_view.len());
        remember::apply_view_values(&mut cfg, &remembered_view);
    }
    apply_command_line(&mut cfg, o);
    debug!("solder mask: {}", { let m = remember::mask_spec(&cfg.scene); if m.is_empty() { "stackup / default".to_string() } else { m } });
    // 環境マップ：--hdr、この基板で最後に使ったもの、設定ファイルの [environment] hdr、内蔵のスタジオ、の順
    let config_hdr = cfg.hdr.clone();
    let from_cli = !o.hdr.is_empty();
    if from_cli {
        cfg.hdr = o.hdr.clone();
    } else if let Some(r) = remember::remembered_hdr(&board.file).filter(|_| !o.ignore_remembered) {
        if r.config_hdr != config_hdr {
            info!("[environment] hdr in the board's settings changed since the last run; using it");
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
    let materials = if o.builtin_settings_only { MaterialSetup::builtin_only() } else { load_material_setup(&board.file) };
    let resolver = model_resolver(&board, &cfg);
    let library = Arc::new(ModelLibrary::new(cache_dir.clone(), MeshParams::default(), !o.no_cache));
    info!("mesh cache: {}{}", cache_dir.join("meshes").display(), if o.no_cache { " (disabled)" } else { "" });
    // 互いに待たない 3 つを同時に進める：環境マップ（読み込みと重点サンプリングの表）、3D モデルの先読み、
    // kicad-cli のシルク → 凹凸のタイルと組み立て（シルクの文字はタイルに入るので、この 2 つは順に）
    let hdr = cfg.hdr.clone();
    let (env, kicad_cli, scene, source) = std::thread::scope(|sc| {
        let env_job = sc.spawn(|| {
            krbr_core::platform::lower_thread_priority();
            let env = match load_environment_map(&hdr) {
                Ok(e) => (e, hdr.clone()),
                // コマンドラインで指定したものは誤りとして止める。設定ファイルのものは内蔵のスタジオで代える
                Err(e) if from_cli || hdr == STUDIO => {
                    krbr_core::error!("environment map {}: {}", hdr, e);
                    return None;
                }
                Err(e) => {
                    warn!("environment map {}: {} (using the built-in studio instead)", hdr, e);
                    (load_environment_map(STUDIO).ok()?, STUDIO.to_string())
                }
            };
            let dist = build_env_distribution(&env.0, ENV_DIST_RESOLUTION);
            // GPU へ送るときに使う RGB → スペクトルの表（プロセスで 1 回だけ読む）も、ここで読んでおく
            krbr_scene::spectrum::rgb2spec_table();
            Some((env, dist))
        });
        let include_dnp = cfg.scene.include_dnp;
        let (board, resolver, library) = (&board, &resolver, &library);
        sc.spawn(move || {
            krbr_core::platform::lower_thread_priority();
            prefetch_models(board, resolver, library, include_dnp);
        });
        let kicad_cli = setup_stroke_text(board, &mut cfg, o, &cache_dir);
        let mut tiles = ReliefTileCache::default();
        // 凹凸のタイルのディスクのキャッシュ（基板のパスごと。変えていない基板を開き直すときに作り直さない）
        if !o.no_cache {
            let key = krbr_core::to_hex(krbr_core::xxh64(krbr_scene::board_file::board_key(&board.file).as_bytes()));
            tiles.disk_file = Some(cache_dir.join("relief").join(format!("{key}.krbt")));
        }
        let mut source = SceneSource { resolver: resolver.clone(), library: Arc::clone(library), materials, tiles, solder: Default::default() };
        let scene = source.build(board, &cfg.scene, false);
        (env_job.join().ok().flatten(), kicad_cli, scene, source)
    });
    let st = source.library.stats();
    info!("models: {} loaded+meshed, {} from cache, {} failed", st.loaded, st.cache_hits, st.failed);
    for a in &scene.assignments {
        debug!("material: {} {} [{}] -> {} ({})", a.reference, a.model_file, a.part_keys, a.material, a.source);
    }
    let ((env, hdr), dist) = env?;
    cfg.hdr = hdr;
    info!("environment: {} ({}x{})", cfg.hdr, env.width, env.height);
    if from_cli && !o.ignore_remembered {
        cfg.hdr = remember::normalize_hdr(&cfg.hdr);
        remember::remember_hdr(&board.file, &cfg.hdr, &config_hdr);
    }
    Some(PreparedScene { board, cfg, scene, env, dist, config_hdr, configured, config_view, remembered_view, kicad_cli, source: Some(source) })
}

/// prepare_scene と、ウィンドウを持たない Vulkan の準備（0.3〜0.5 秒）を同時に進める（render・watch・bench）。
/// GPU の準備に失敗しても、シーンの準備は最後まで行う（エラーは呼ぶ側で）
pub fn prepare_scene_and_context(o: &RenderOptions, validation: bool) -> (Option<PreparedScene>, anyhow::Result<Context>) {
    std::thread::scope(|sc| {
        let ctx = sc.spawn(move || Context::new(&ContextOptions { validation, low_queue_priority: true, ..Default::default() }).map_err(|e| anyhow::anyhow!(e)));
        let prep = prepare_scene(o);
        (prep, ctx.join().unwrap_or_else(|_| Err(anyhow::anyhow!("GPU initialization panicked"))))
    })
}

/// 基板を開かずに起動したビューアの状態（環境マップだけ。基板は「ファイル → 開く...」で読む）
pub fn prepare_empty(o: &RenderOptions) -> Option<PreparedScene> {
    let mut configured = ProjectConfig::default();
    apply_command_line(&mut configured, o);
    let mut cfg = configured.clone();
    cfg.hdr = if o.hdr.is_empty() { STUDIO.into() } else { o.hdr.clone() };
    let env = match load_environment_map(&cfg.hdr) {
        Ok(e) => e,
        Err(e) => {
            warn!("environment map {}: {} (using the built-in studio instead)", cfg.hdr, e);
            cfg.hdr = STUDIO.into();
            load_environment_map(STUDIO).ok()?
        }
    };
    let dist = build_env_distribution(&env, ENV_DIST_RESOLUTION);
    let config_view = remember::ViewSettings::of(&ProjectConfig::default()).values();
    // 構図：原点のまわりの 10 cm（基板を開いたら、その基板に合わせ直す）
    let scene = Scene { framing_points: vec![glam::Vec3::splat(-50.0), glam::Vec3::splat(50.0)], ..Default::default() };
    info!("no board; open one from the File menu");
    Some(PreparedScene { board: Board::default(), cfg, scene, env, dist, config_hdr: String::new(), configured, config_view, remembered_view: remember::ViewValues::new(), kicad_cli: None, source: None })
}

/// 基板を裏で読んでいる間の仮の状態（起動中の画面に隠れて見えないので、環境マップは小さな内蔵のスタジオ。
/// 2048 の本物を作ると、ウィンドウを出す前に 0.5 秒かかる）
pub fn prepare_placeholder(o: &RenderOptions) -> PreparedScene {
    let mut configured = ProjectConfig::default();
    apply_command_line(&mut configured, o);
    let mut cfg = configured.clone();
    cfg.hdr = STUDIO.into();
    let env = studio_environment(64);
    let dist = build_env_distribution(&env, 64);
    let config_view = remember::ViewSettings::of(&ProjectConfig::default()).values();
    let scene = Scene { framing_points: vec![glam::Vec3::splat(-50.0), glam::Vec3::splat(50.0)], ..Default::default() };
    PreparedScene { board: Board::default(), cfg, scene, env, dist, config_hdr: String::new(), configured, config_view, remembered_view: remember::ViewValues::new(), kicad_cli: None, source: None }
}

/// 組み立てで使う 3D モデルを先に読んでおく（ModelLibrary が覚えるので、組み立てのときはすぐ返る）。
/// 凹凸のタイルと同時に進めるためのもの。STEP の分割は 1 本ずつ（OCCT の読み込みを並べない）
fn prefetch_models(board: &Board, resolver: &ModelResolver, library: &ModelLibrary, include_dnp: bool) {
    let mut seen = std::collections::HashSet::new();
    for fp in board.footprints.iter().filter(|f| include_dnp || !f.dnp) {
        for m in fp.models.iter().filter(|m| !m.hidden && m.opacity > 0.0) {
            let r = resolver.resolve(&m.path);
            if r.found && seen.insert(r.path.clone()) {
                library.get(&r.path);
            }
        }
    }
}

/// 3D モデルのパスの解決：基板のフォルダと、設定ファイル（<名前>.krbr）の [paths] の変数・探す場所
pub fn model_resolver(board: &Board, cfg: &ProjectConfig) -> ModelResolver {
    ModelResolver::new(&board.project_dir, &cfg.path_vars, &cfg.model_search_dirs)
}

/// 累積の描画を用意する。BDPT / VCM を作れなければ（GPU の機能が足りないなど）警告して PT で作る。
/// ビューアのパネルで選び直したときも同じ扱い（どちらでも、描けないまま止めない）
pub fn progressive_or_pt<'a>(ctx: &'a Context, gpu: &'a GpuScene<'a>, kind: IntegratorKind, width: u32, height: u32, frame: FrameParams) -> Result<Progressive<'a>, String> {
    match Progressive::new(ctx, gpu, kind, width, height, frame.clone()) {
        Err(e) if kind != IntegratorKind::Pt => {
            warn!("integrator {kind}: {e}; using pt instead");
            Progressive::new(ctx, gpu, IntegratorKind::Pt, width, height, frame)
        }
        r => r,
    }
}

/// GPU が光線と三角形の交差をどの方式で求めるか（ステータスバーと krbr bench の表示。技術用語なので訳さない）
pub fn trace_method_name(ctx: &Context) -> &'static str {
    if ctx.use_rt_pipeline {
        "ray tracing pipeline"
    } else {
        "ray query"
    }
}

pub fn camera_params(cfg: &ProjectConfig, width: u32, height: u32) -> CameraParams {
    CameraParams { elevation_deg: cfg.elevation_deg, azimuth_deg: cfg.azimuth_deg, fov_deg: cfg.fov_deg, margin: cfg.margin, width, height }
}

/// 最初のカメラ：この基板で覚えた視点（--look-at / --zoom を指定しなければ）。覚えていなければ自動の構図
pub fn initial_camera(prep: &PreparedScene, o: &RenderOptions, width: u32, height: u32) -> Camera {
    let cfg = &prep.cfg;
    if o.look_at.is_empty() && o.zoom.is_none() {
        if let Some((target, distance)) = remember::camera_of(&prep.remembered_view) {
            debug!("camera: last used for this board (distance {:.1} mm, elevation {:.1}, azimuth {:.1})", distance, cfg.elevation_deg, cfg.azimuth_deg);
            return orbit_camera(target, distance, cfg.elevation_deg, cfg.azimuth_deg, cfg.fov_deg, width as f32 / height.max(1) as f32);
        }
    }
    framed_camera(cfg, prep, o, width, height)
}

/// 自動の構図に --look-at / --zoom を反映したカメラ（cfg の仰角・方位・画角で）。ビューアの「視点を戻す」もこれ
pub fn framed_camera(cfg: &ProjectConfig, prep: &PreparedScene, o: &RenderOptions, width: u32, height: u32) -> Camera {
    let cam = frame_camera(&camera_params(cfg, width, height), &prep.scene.framing_points);
    let mut target = cam.target;
    if o.look_at.len() == 2 {
        let b = &prep.scene.board;
        target = krbr_geom::board_mesh::board_to_world(glam::DVec2::new(o.look_at[0], o.look_at[1]), b.thickness / 2.0, b.center);
    }
    // 0 以下の拡大率は使わない（指定なしと同じ。カメラが無限に遠くならないように）
    let zoom = o.zoom.filter(|z| *z > 0.0).unwrap_or(1.0);
    let dist = cam.distance / zoom as f32;
    orbit_camera(target, dist, cfg.elevation_deg, cfg.azimuth_deg, cfg.fov_deg, cam.aspect)
}

pub fn frame_params(cfg: &ProjectConfig, cam: Camera, seed: u32) -> FrameParams {
    FrameParams {
        camera: cam,
        env_intensity: cfg.env_intensity,
        env_rotation_rad: cfg.env_rotation_deg.to_radians(),
        radiance_clamp: cfg.radiance_clamp,
        spectral: cfg.spectral,
        transparent_background: cfg.transparent_background,
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

    let (prep, ctx) = prepare_scene_and_context(o, o.validate);
    let Some(mut prep) = prep else { anyhow::bail!("scene preparation failed") };
    let ctx = ctx?;
    info!("project: {} ({})", prep.board.project_name, prep.board.project_dir.display());
    let cfg = &prep.cfg;
    let cam = initial_camera(&prep, o, cfg.width, cfg.height);
    info!("camera: elevation {:.1} deg, azimuth {:.1} deg, fov {:.1} deg, position ({:.1}, {:.1}, {:.1}) mm", cfg.elevation_deg, cfg.azimuth_deg, cfg.fov_deg, cam.position.x, cam.position.y, cam.position.z);

    let result = {
        let mut gpu = GpuScene::new(&ctx);
        gpu.build(&prep.scene, &prep.env, &prep.dist, cfg.scene.bump_intensity).map_err(|e| anyhow::anyhow!("GPU scene: {e}"))?;
        prep.env.rgba = Vec::new();
        let denoise = o.denoise.unwrap_or(false).then_some((DenoiseQuality::High, o.denoise_strength.unwrap_or(1.0)));
        let rs = RenderSettings { width: cfg.width, height: cfg.height, spp: cfg.spp, integrator: cfg.integrator, frame: frame_params(cfg, cam, o.seed), denoise };
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
    report(image_io::save_png_tonemapped(&png, &result.color, cfg.exposure_ev, &glow_params(&cfg)), &png);
    let exr = out_dir.join(format!("{stem}.exr"));
    report(image_io::save_exr(&exr, &result.color), &exr);
    let alb = out_dir.join(format!("{stem}_albedo.exr"));
    report(image_io::save_exr(&alb, &result.albedo), &alb);
    let nrm = out_dir.join(format!("{stem}_normal.exr"));
    report(image_io::save_exr(&nrm, &result.normal), &nrm);
    if let Some(d) = &result.denoised {
        let png = out_dir.join(format!("{stem}_denoised.png"));
        report(image_io::save_png_tonemapped(&png, d, cfg.exposure_ev, &glow_params(&cfg)), &png);
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

/// 設定のグロー
pub fn glow_params(cfg: &ProjectConfig) -> krbr_render::glow::GlowParams {
    krbr_render::glow::GlowParams { strength: cfg.glow, threshold: cfg.glow_threshold, spread: cfg.glow_spread }
}
