//! krbr（Rust 版）のコマンドライン。サブコマンドの解析だけをして、処理は krbr-app に任せる
//! （render / view / watch / material / bench など。`krbr <基板>.kicad_pcb` は view）

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use krbr_app::prepare::{load_board_or_report, model_resolver};
use krbr_app::viewer::{parse_render_ratio, parse_render_size, RenderSize, TestOptions};
use krbr_core::{info, log};
use krbr_kicad::{load_board, Board};
use krbr_scene::config::load_project_config;
use krbr_scene::{IntegratorKind, LensModel};

#[derive(Parser)]
#[command(name = "krbr", about = "KRB-Renderer: a physically based renderer for KiCad boards")]
struct Cli {
    /// also print debug messages
    #[arg(short, long, global = true)]
    verbose: bool,
    /// run the commands without a window (render / watch / geom / models) at the normal CPU priority (default: below normal)
    #[arg(long, global = true)]
    normal_priority: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// parse a .kicad_pcb and print a summary
    Info {
        /// .kicad_pcb file
        pcb: PathBuf,
        /// resolve 3D model paths and list them
        #[arg(long)]
        models: bool,
    },
    /// render offscreen to PNG (tone mapped) and EXR (linear)
    Render(RenderArgs),
    /// real-time viewer window (default command when the first argument is a .kicad_pcb)
    View(ViewArgs),
    /// headless: re-render whenever the board is saved (no window)
    Watch(WatchArgs),
    /// list Vulkan devices and check ray tracing support
    Probe,
    /// material assignment (OpenPBR materials, per model / per reference)
    Material {
        #[command(subcommand)]
        cmd: MaterialCmd,
    },
    /// solder mask color presets
    Mask {
        #[command(subcommand)]
        cmd: MaskCmd,
    },
    /// KiCad plugin: a "Preview in KRB Render" button on the PCB editor toolbar
    KicadPlugin {
        #[command(subcommand)]
        cmd: KicadPluginCmd,
    },
    /// measure the rendering speed under fixed conditions (to compare GPUs); results are appended to bench.csv in the config folder
    Bench {
        /// .kicad_pcb file (default: the bundled benchmark board, bench\krbr_bench.kicad_pcb next to krbr.exe)
        pcb: Option<PathBuf>,
        /// samples per case (after 8 warm-up samples)
        #[arg(long, default_value_t = 64)]
        spp: u32,
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
    },
    /// build the board geometry and the relief tiles (no GPU) and print statistics
    Geom {
        /// .kicad_pcb file
        pcb: PathBuf,
        /// write the distance fields (sdf_front.bin / sdf_back.bin) and the relief area per side and material (areas.txt) here,
        /// to check that an optimization did not change the result
        #[arg(long)]
        dump: Option<PathBuf>,
    },
    /// development: compare two images (PNG / EXR) and print the mean and max difference
    Diff {
        a: PathBuf,
        b: PathBuf,
    },
    /// load every 3D model of a board (mesh cache, else the STEP converter DLL) and print statistics
    Models {
        /// .kicad_pcb file
        pcb: PathBuf,
        /// do not read or write the mesh cache
        #[arg(long)]
        no_cache: bool,
    },
}

#[derive(Subcommand)]
enum MaterialCmd {
    /// list the material library (built-in and %APPDATA%\krbr\materials\*.toml)
    List,
    /// show the material of every model part and which rule decided it
    Show {
        /// .kicad_pcb file
        pcb: PathBuf,
        /// only this reference
        #[arg(long = "ref", default_value = "")]
        reference: String,
    },
    /// assign a material per reference (the board's <name>.krbr) or per 3D model (<name>.krbr, or --global)
    Set {
        /// .kicad_pcb file
        pcb: PathBuf,
        /// reference designator (the board's <name>.krbr)
        #[arg(long = "ref", default_value = "")]
        reference: String,
        /// 3D model: path as written in the board, file name, or resolved file
        #[arg(long, default_value = "")]
        model: String,
        /// part key (XDE name / "kicad:<color name>" / "#rrggbb"); omitted or "*" for all parts
        #[arg(long, default_value = "")]
        part: String,
        /// material name (see: krbr material list)
        #[arg(long)]
        material: String,
        /// write the per-model setting to the global model_map.toml (shared by all projects)
        #[arg(long)]
        global: bool,
    },
    /// development: render the swatches of the built-in materials (RGB and spectral) with many samples and OIDN,
    /// for embedding in the exe (rebuild afterwards)
    BakeSwatches {
        /// samples per swatch
        #[arg(long, default_value_t = krbr_app::swatch_cache::BAKE_SPP)]
        spp: u32,
        /// output folder (default: the source tree's crates/krbr-app/swatches)
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum KicadPluginCmd {
    /// list the KiCad versions found and whether the plugin is installed in each
    Status,
    /// install into every KiCad 9+ version found (Documents\KiCad\<version>\scripting\plugins), or one with --kicad / --dir
    Install {
        /// only this KiCad version (e.g. 10.0)
        #[arg(long, conflicts_with = "dir")]
        kicad: Option<String>,
        /// the scripting\plugins folder to install into
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// remove the plugin from every KiCad version, or one with --kicad / --dir
    Uninstall {
        /// only this KiCad version (e.g. 10.0)
        #[arg(long, conflicts_with = "dir")]
        kicad: Option<String>,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum MaskCmd {
    /// list the solder mask color presets (KiCad color names and JLCPCB options)
    List,
}

#[derive(Args, Debug)]
struct RenderArgs {
    /// .kicad_pcb file (view: optional; without it the window opens empty and a board can be opened from the File menu)
    pcb: Option<PathBuf>,
    /// equirectangular environment map (.hdr / .exr, or builtin:studio). Remembered per board;
    /// if omitted: the one last used for this board, else [environment] hdr in <name>.krbr, else builtin:studio
    #[arg(long, default_value = "")]
    hdr: String,
    /// output PNG (render: EXR and AOVs are written next to it; default out/board.png, watch: out/watch.png,
    /// view: the images saved with S, default <board folder>\<board name>_000.png)
    #[arg(long, short)]
    out: Option<PathBuf>,
    #[arg(long)]
    spp: Option<u32>,
    #[arg(long)]
    width: Option<u32>,
    #[arg(long)]
    height: Option<u32>,
    #[arg(long)]
    max_depth: Option<u32>,
    /// camera elevation above the board (deg, default 35)
    #[arg(long)]
    elevation: Option<f64>,
    /// camera azimuth (deg, default 45)
    #[arg(long)]
    azimuth: Option<f64>,
    /// vertical field of view (deg, default 30)
    #[arg(long)]
    fov: Option<f64>,
    /// look at this board point (KiCad X,Y in mm)
    #[arg(long, value_delimiter = ',', num_args = 2)]
    look_at: Vec<f64>,
    /// zoom factor relative to the automatic framing (default 1; when given, the view remembered for the board is not used)
    #[arg(long, value_parser = parse_zoom)]
    zoom: Option<f64>,
    /// exposure (EV)
    #[arg(long)]
    exposure: Option<f32>,
    /// glow of bright light such as lit LEDs: strength (0 = off; default 1). Remembered per board
    #[arg(long)]
    glow: Option<f32>,
    /// glow: only light brighter than this glows (max of RGB after exposure, 1 = display white; default 4)
    #[arg(long)]
    glow_threshold: Option<f32>,
    /// glow: how far it spreads (0..1; small = tight around the light; default 0.7)
    #[arg(long)]
    glow_spread: Option<f32>,
    #[arg(long)]
    env_intensity: Option<f32>,
    #[arg(long)]
    env_rotation: Option<f32>,
    /// clamp indirect radiance per sample (default 10, 0 = off)
    #[arg(long)]
    clamp: Option<f32>,
    /// integrator: pt (default), bdpt, vcm ("path" is another name for pt)
    #[arg(long, value_parser = parse_integrator)]
    integrator: Option<IntegratorKind>,
    #[arg(long, default_value_t = 0)]
    seed: u32,
    /// do not read or write the mesh cache (and the kicad-cli silk cache)
    #[arg(long)]
    no_cache: bool,
    /// do not use kicad-cli for stroke-font silkscreen texts (they will be missing)
    #[arg(long)]
    no_kicad_cli: bool,
    /// write the template of the board's settings file <name>.krbr (an existing file only gets the missing sections)
    #[arg(long)]
    init: bool,
    /// denoise with OIDN (default: on for watch, off for render/view)
    #[arg(long, overrides_with = "no_denoise")]
    denoise: bool,
    /// do not denoise
    #[arg(long)]
    no_denoise: bool,
    /// mix of the denoised and the noisy image (0..1; default 1 = OIDN result as is)
    #[arg(long)]
    denoise_strength: Option<f32>,
    /// enable the Vulkan validation layer
    #[arg(long)]
    validate: bool,
    /// spectral rendering (hero wavelengths)
    #[arg(long)]
    spectral: bool,
    /// RGB rendering (overrides the spectral mode remembered for the board or set in <name>.krbr)
    #[arg(long, conflicts_with = "spectral")]
    rgb: bool,
    /// camera lens: pinhole (default), thin, realistic
    #[arg(long, value_parser = parse_lens)]
    lens: Option<LensModel>,
    #[arg(long)]
    f_number: Option<f32>,
    #[arg(long)]
    focal_length: Option<f32>,
    #[arg(long)]
    focus_distance: Option<f32>,
    #[arg(long)]
    blades: Option<i32>,
    /// solder mask: preset name (see: krbr mask list) or r,g,b in sRGB
    #[arg(long, default_value = "")]
    mask_color: String,
    /// substrate material: a name from the material library (krbr material list), e.g. glass_frosted; fr4 = the default
    #[arg(long, default_value = "")]
    substrate: String,
}

#[derive(Args, Debug)]
struct ViewArgs {
    #[command(flatten)]
    render: RenderArgs,
    /// development: render N frames in a hidden window, then exit
    #[arg(long, default_value_t = 0)]
    test_frames: u32,
    /// development: save the displayed image (with --test-frames)
    #[arg(long)]
    screenshot: Option<PathBuf>,
    /// do not show the window
    #[arg(long)]
    hidden: bool,
    /// development: rotate the camera by this many degrees per frame
    #[arg(long, default_value_t = 0.0)]
    test_orbit: f64,
    /// do not reload when the board file is saved
    #[arg(long)]
    no_watch: bool,
    /// development: reload the board once during the test
    #[arg(long)]
    test_reload: bool,
    /// development: click this pixel of the 3D view (x y) during the test to select a part
    #[arg(long, num_args = 2)]
    test_pick: Vec<f64>,
    /// development: assign this material to all parts of the part selected by --test-pick (this part only)
    #[arg(long, default_value = "")]
    test_assign: String,
    /// development: choose this solder mask preset in the panel during the test
    #[arg(long, default_value = "")]
    test_mask: String,
    /// development: near the end of the test, "save as" to this file (.png or .exr)
    #[arg(long)]
    test_save_as: Option<PathBuf>,
    /// development: press "reset all settings" in the menu during the test
    #[arg(long)]
    test_reset_settings: bool,
    /// development: open the material editor during the test
    #[arg(long)]
    test_material_editor: bool,
    /// development: open "Settings > KiCad plugin" during the test
    #[arg(long)]
    test_kicad_plugin: bool,
    /// development: select the part with this reference during the test (instead of --test-pick)
    #[arg(long, default_value = "")]
    test_select: String,
    /// development: open this board from the File menu during the test
    #[arg(long)]
    test_open: Option<PathBuf>,
    /// development: press "Top" or "Bottom" in the toolbar during the test (top | bottom)
    #[arg(long, default_value = "")]
    test_view: String,
    /// development: hide these kinds of parts with the toolbar during the test (smd, tht or smd,tht)
    #[arg(long, default_value = "")]
    test_hide: String,
    /// rendering size: WxH (the window is resized so that the 3D view is that size / ratio) or "fit" (follow the window); default: last used
    #[arg(long, value_parser = parse_render_size)]
    render_size: Option<RenderSize>,
    /// rendering resolution / display size, 0.1 .. 4 (e.g. 0.5 renders at half resolution, 2 renders finer and shrinks); default: last used
    #[arg(long, value_parser = parse_render_ratio)]
    render_ratio: Option<f32>,
    /// open with rendering paused (P or the toolbar button resumes); for weak GPUs
    #[arg(long)]
    paused: bool,
    /// language of the window, e.g. en, ja (also KRBR_LANG). Default: the one chosen in the View menu,
    /// else the language chosen in KiCad, else the OS language
    #[arg(long)]
    lang: Option<String>,
    /// draw the toolbar and the status bar with a WebView (WebView2) instead of the built-in UI (prototype)
    #[arg(long)]
    web_ui: bool,
}

#[derive(Args, Debug)]
struct WatchArgs {
    #[command(flatten)]
    render: RenderArgs,
    /// stop after this many updates (0 = run until stopped)
    #[arg(long, default_value_t = 0)]
    max_updates: u32,
}

impl RenderArgs {
    /// default_out：--out を省略したときの出力先
    fn to_options(&self, default_out: &str) -> krbr_app::RenderOptions {
        krbr_app::RenderOptions {
            pcb: self.pcb.clone().unwrap_or_default(),
            hdr: self.hdr.clone(),
            out: self.out.clone().unwrap_or_else(|| PathBuf::from(default_out)),
            spp: self.spp,
            width: self.width,
            height: self.height,
            max_depth: self.max_depth,
            elevation: self.elevation,
            azimuth: self.azimuth,
            fov: self.fov,
            exposure: self.exposure,
            glow: self.glow,
            glow_threshold: self.glow_threshold,
            glow_spread: self.glow_spread,
            env_intensity: self.env_intensity,
            env_rotation: self.env_rotation,
            radiance_clamp: self.clamp,
            look_at: self.look_at.clone(),
            zoom: self.zoom,
            integrator: self.integrator,
            seed: self.seed,
            no_cache: self.no_cache,
            no_kicad_cli: self.no_kicad_cli,
            init: self.init,
            denoise: if self.no_denoise { Some(false) } else if self.denoise { Some(true) } else { None },
            denoise_strength: self.denoise_strength.map(|v| v.clamp(0.0, 1.0)),
            ignore_remembered: false,
            builtin_settings_only: false,
            validate: self.validate,
            spectral: if self.spectral { Some(true) } else if self.rgb { Some(false) } else { None },
            lens: self.lens,
            f_number: self.f_number,
            focal_length: self.focal_length,
            focus_distance: self.focus_distance,
            blades: self.blades,
            mask_color: self.mask_color.clone(),
            substrate: self.substrate.clone(),
        }
    }
}

fn parse_integrator(s: &str) -> Result<IntegratorKind, String> {
    s.parse()
}

fn parse_lens(s: &str) -> Result<LensModel, String> {
    s.parse()
}

fn parse_zoom(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(z) if z > 0.0 && z.is_finite() => Ok(z),
        _ => Err(format!("'{s}' is not a positive number")),
    }
}

fn cmd_diff(a: &PathBuf, b: &PathBuf) -> anyhow::Result<()> {
    use krbr_render::image_io::load_hdr_image;
    let load = |p: &PathBuf| -> anyhow::Result<krbr_render::ImageF> {
        if p.extension().map(|e| e == "exr").unwrap_or(false) {
            return load_hdr_image(p).map_err(|e| anyhow::anyhow!(e));
        }
        let img = image::open(p)?.to_rgba8();
        let (w, h) = (img.width(), img.height());
        Ok(krbr_render::ImageF { width: w, height: h, rgba: img.into_raw().iter().map(|&v| v as f32 / 255.0).collect() })
    };
    let (ia, ib) = (load(a)?, load(b)?);
    if (ia.width, ia.height) != (ib.width, ib.height) {
        anyhow::bail!("size differs: {}x{} vs {}x{}", ia.width, ia.height, ib.width, ib.height);
    }
    let n = (ia.width * ia.height) as usize;
    let (mut sum, mut max, mut big, mut ma, mut mb) = (0.0f64, 0.0f32, 0usize, 0.0f64, 0.0f64);
    for i in 0..n {
        let mut px_max = 0.0f32;
        for c in 0..3 {
            let (x, y) = (ia.rgba[i * 4 + c], ib.rgba[i * 4 + c]);
            let d = (x - y).abs();
            sum += d as f64;
            max = max.max(d);
            px_max = px_max.max(d);
            ma += x as f64;
            mb += y as f64;
        }
        if px_max > 8.0 / 255.0 {
            big += 1;
        }
    }
    info!("{}x{}: mean |diff| {:.5}, max {:.4}, pixels differing by > 8/255: {:.2}%, mean A {:.5}, mean B {:.5}", ia.width, ia.height, sum / (3.0 * n as f64), max, big as f64 * 100.0 / n as f64, ma / (3.0 * n as f64), mb / (3.0 * n as f64));
    Ok(())
}

fn cmd_geom(pcb: &PathBuf, dump: Option<&std::path::Path>) -> anyhow::Result<()> {
    use krbr_geom::{build_board_geometry, tiles::build_relief_tiles, BoardBuildOptions, LayerArtOptions, ReliefParams, ReliefTileCache};
    let _all_cores = krbr_core::platform::AllCores::new(); // 読み込みと同じ並列の数で測る
    let board = load_board(pcb)?;
    let mut rp = ReliefParams::default();
    if board.stackup.copper_thickness > 0.0 {
        rp.copper = board.stackup.copper_thickness;
    }
    let opts = BoardBuildOptions { core_half: rp.core_half(board.stackup.thickness) };
    let geo = build_board_geometry(&board, &opts);
    let mut cache = ReliefTileCache::default();
    let rebuilt = build_relief_tiles(&mut cache, &board, &geo, &rp, &LayerArtOptions::default());
    let tris: usize = (0..2).map(|s| cache.tiles[s].iter().map(|m| m.triangle_count()).sum::<usize>()).sum();
    info!("geom: {} tiles rebuilt, {} relief triangles, sdf {}x{} px", rebuilt, tris, cache.sdf_grid.width, cache.sdf_grid.height);
    if let Some(dir) = dump {
        std::fs::create_dir_all(dir)?;
        for (side, name) in [(0, "front"), (1, "back")] {
            std::fs::write(dir.join(format!("sdf_{name}.bin")), &cache.sdf_image[side])?;
        }
        // 面と材質ごとの凹凸の面積（三角形の分け方が変わっても、形が同じなら同じ値になる）
        let mut areas = std::collections::BTreeMap::<(usize, u32), f64>::new();
        for side in 0..2 {
            for m in &cache.tiles[side] {
                for (t, tri) in m.indices.chunks_exact(3).enumerate() {
                    let [a, b, c] = [0, 1, 2].map(|k| m.positions[tri[k] as usize].as_dvec3());
                    *areas.entry((side, m.tri_material.get(t).copied().unwrap_or(0))).or_default() += 0.5 * (b - a).cross(c - a).length();
                }
            }
        }
        let text: String = areas.iter().map(|((s, mat), a)| format!("{s} {mat} {a:.6}\n")).collect();
        std::fs::write(dir.join("areas.txt"), text)?;
        info!("geom: wrote {}", dir.display());
    }
    Ok(())
}

/// 基板を読み（要約をログに出す）、3D モデルのパスの解決を用意する（基板の設定ファイルの [paths] も使う。render と同じ）
fn board_and_resolver(pcb: &PathBuf) -> anyhow::Result<(Board, krbr_kicad::ModelResolver)> {
    let board = load_board_or_report(pcb).ok_or_else(|| anyhow::anyhow!("cannot load {}", pcb.display()))?;
    let resolver = model_resolver(&board, &load_project_config(&board.file));
    Ok((board, resolver))
}

fn cmd_models(pcb: &PathBuf, no_cache: bool) -> anyhow::Result<()> {
    use krbr_model::{library::step_support, MeshParams, ModelLibrary};
    let (board, resolver) = board_and_resolver(pcb)?;
    match step_support() {
        Ok(()) => info!("STEP converter: available"),
        Err(e) => krbr_core::warn!("STEP converter unavailable ({e}); STEP models will be placeholders"),
    }
    let library = ModelLibrary::new(krbr_core::platform::cache_dir(), MeshParams::default(), !no_cache);
    let t0 = std::time::Instant::now();
    let (mut tris, mut parts, mut missing) = (0usize, 0usize, 0usize);
    for fp in &board.footprints {
        for m in &fp.models {
            let r = resolver.resolve(&m.path);
            if !r.found {
                missing += 1;
                continue;
            }
            let res = library.get(&r.path);
            if let Some(mesh) = &res.mesh {
                tris += mesh.mesh.triangle_count();
                parts += mesh.parts.len();
            }
        }
    }
    let st = library.stats();
    info!(
        "models: {} loaded, {} cache hits, {} failed, {} unresolved; {} triangles, {} parts, {:.2} s",
        st.loaded,
        st.cache_hits,
        st.failed,
        missing,
        tris,
        parts,
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}

fn cmd_info(pcb: &PathBuf, models: bool) -> anyhow::Result<()> {
    let (board, resolver) = board_and_resolver(pcb)?;
    info!(
        "stackup: thickness {:.3} mm, finish '{}', mask '{}'",
        board.stackup.thickness, board.stackup.copper_finish, board.stackup.mask_color_front
    );
    info!("stroke-font texts / text boxes on the silkscreen (need kicad-cli): {}", board.stroke_silk_count());
    if !models {
        return Ok(());
    }
    let (mut found, mut missing, mut no_model) = (0, 0, 0);
    for fp in &board.footprints {
        if fp.models.is_empty() {
            no_model += 1;
            continue;
        }
        for m in &fp.models {
            let r = resolver.resolve(&m.path);
            if r.found {
                found += 1;
                info!("  {:<6} OK      {}{}", fp.reference, r.path.display(), if r.substituted { " (substituted)" } else { "" });
            } else {
                missing += 1;
                info!("  {:<6} MISSING {} [{}]", fp.reference, m.path, r.reason);
            }
        }
    }
    info!("models: {} found, {} missing, {} footprints without model", found, missing, no_model);
    Ok(())
}

/// エクスプローラーから起動されたか（exe のアイコンへのドラッグ＆ドロップ、ダブルクリック）。
/// そのときはコンソールを使っているのがこのプロセスだけになる
#[cfg(windows)]
fn launched_from_explorer() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
    }
    let mut ids = [0u32; 4];
    // 安全性：ids の長さを渡している
    unsafe { GetConsoleProcessList(ids.as_mut_ptr(), ids.len() as u32) == 1 }
}

#[cfg(not(windows))]
fn launched_from_explorer() -> bool {
    false
}

/// エクスプローラーから起動したときは、終わるとコンソールがすぐ閉じてエラーが読めないので、Enter を待つ
fn exit_with(code: i32, explorer: bool) -> ! {
    if explorer && code != 0 {
        eprintln!("\nPress Enter to close");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    std::process::exit(code)
}

fn main() {
    // 配布用の一式の dll\vcruntime を DLL の探し先に足す（STEP 変換器・OIDN を読む前に）
    krbr_core::platform::add_dll_search_dirs();
    // サブコマンドを省略して `krbr board.kicad_pcb ...` と書いたら view とみなす
    // （exe のアイコンに .kicad_pcb をドラッグ＆ドロップしたときもこの形になる）
    let mut args: Vec<String> = std::env::args().collect();
    let explorer = launched_from_explorer();
    if let Some(i) = args.iter().skip(1).position(|a| !a.starts_with('-')) {
        let i = i + 1;
        if args[i].to_lowercase().ends_with(".kicad_pcb") {
            // 作業フォルダは変えない：view の保存先は基板のフォルダから決め（--out が無ければ）、エクスプローラーからの起動では
            // 相対パスの指定も無い。ファイルのダイアログも基板・環境マップのフォルダを最初に開く
            args.insert(i, "view".into());
        }
    }
    // 基板を指定せずに起動したら（引数なし、または --spectral のようなオプションだけ）、最後にビューアで開いた基板を開く
    let help = args.iter().skip(1).any(|a| matches!(a.as_str(), "-h" | "--help" | "-V" | "--version"));
    if !help && args.iter().skip(1).all(|a| a.starts_with('-')) {
        match krbr_app::remember::last_board() {
            Some(pcb) if pcb.is_file() => {
                eprintln!("opening the last board: {}", pcb.display());
                args.splice(1..1, ["view".to_string(), pcb.to_string_lossy().into_owned()]);
            }
            // 覚えた基板が無い（消えた）ときは、基板なしでビューアを開く（「ファイル → 開く...」で開ける）
            Some(pcb) => {
                eprintln!("the last board {} is not found; opening the viewer without a board", pcb.display());
                args.insert(1, "view".to_string());
            }
            None => args.insert(1, "view".to_string()),
        }
    }
    let cli = match Cli::try_parse_from(args) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            exit_with(e.exit_code(), explorer)
        }
    };
    if cli.verbose {
        log::set_level(log::Level::Debug);
    }
    // ウィンドウを持たない重いコマンドは、プロセス全体を低優先度にする。ビューアはウィンドウのスレッドを通常のままにして、
    // 裏の処理のスレッドだけを下げる（krbr_core::platform::lower_thread_priority）
    if !cli.normal_priority && matches!(cli.command, Command::Render(_) | Command::Watch(_) | Command::Geom { .. } | Command::Models { .. }) {
        krbr_core::platform::lower_process_priority();
    }
    krbr_core::debug!("process priority: {}", if krbr_core::platform::process_priority_is_low() { "below normal" } else { "normal" });
    let result = match &cli.command {
        Command::Info { pcb, models } => cmd_info(pcb, *models),
        Command::Render(args) if args.pcb.is_none() => Err(anyhow::anyhow!("give a .kicad_pcb file (krbr render <board>.kicad_pcb)")),
        Command::Render(args) => krbr_app::cmd_render(&args.to_options("out/board.png")),
        Command::View(args) => {
            let mut common = args.render.to_options("out/board.png");
            // S で保存する画像は、--out を付けなければ基板のフォルダに <基板の名前>_000.png（.exr）
            if let (None, Some(pcb)) = (&args.render.out, &args.render.pcb) {
                let pcb = std::path::absolute(pcb).unwrap_or_else(|_| pcb.clone());
                let stem = pcb.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "board".into());
                common.out = krbr_scene::board_file::base_dir(&pcb).join(format!("{stem}.png"));
            }
            let vo = krbr_app::ViewOptions {
                common,
                hidden: args.hidden,
                watch: !args.no_watch,
                render_size: args.render_size,
                render_ratio: args.render_ratio,
                paused: args.paused,
                language: args.lang.clone(),
                web_ui: args.web_ui,
                test: TestOptions {
                    frames: args.test_frames,
                    screenshot: args.screenshot.clone(),
                    orbit_deg: args.test_orbit,
                    reload: args.test_reload,
                    pick: (args.test_pick.len() == 2).then(|| (args.test_pick[0], args.test_pick[1])),
                    assign: args.test_assign.clone(),
                    mask: args.test_mask.clone(),
                    save_as: args.test_save_as.clone(),
                    reset_settings: args.test_reset_settings,
                    material_editor: args.test_material_editor,
                    kicad_plugin: args.test_kicad_plugin,
                    select: args.test_select.clone(),
                    open: args.test_open.clone(),
                    view: args.test_view.clone(),
                    hide: args.test_hide.clone(),
                },
            };
            krbr_app::run_viewer(vo).map(|rc| {
                if rc != 0 {
                    exit_with(rc, explorer);
                }
            })
        }
        Command::Watch(args) if args.render.pcb.is_none() => Err(anyhow::anyhow!("give a .kicad_pcb file (krbr watch <board>.kicad_pcb)")),
        Command::Watch(args) => krbr_app::cmd_watch(&krbr_app::WatchOptions { common: args.render.to_options("out/watch.png"), max_updates: args.max_updates }),
        Command::Probe => krbr_app::material_cmd::cmd_probe(),
        Command::Material { cmd } => match cmd {
            MaterialCmd::List => krbr_app::material_cmd::cmd_material_list(),
            MaterialCmd::Show { pcb, reference } => krbr_app::material_cmd::cmd_material_show(pcb, reference),
            MaterialCmd::Set { pcb, reference, model, part, material, global } => krbr_app::material_cmd::cmd_material_set(&krbr_app::material_cmd::MaterialSetOptions {
                pcb: pcb.clone(),
                reference: reference.clone(),
                model: model.clone(),
                part: part.clone(),
                material: material.clone(),
                global: *global,
            }),
            MaterialCmd::BakeSwatches { spp, out } => match out.clone().or_else(krbr_app::swatch_cache::default_bake_dir) {
                Some(out) => krbr_app::swatch_cache::bake_builtin(&out, *spp).map_err(|e| anyhow::anyhow!(e)),
                None => Err(anyhow::anyhow!("give --out (the source tree's crates/krbr-app/swatches was not found)")),
            },
        },
        Command::Mask { cmd: MaskCmd::List } => krbr_app::material_cmd::cmd_mask_list(),
        Command::KicadPlugin { cmd: KicadPluginCmd::Status } => krbr_app::kicad_plugin::cmd_status(),
        Command::KicadPlugin { cmd: KicadPluginCmd::Install { kicad, dir } } => krbr_app::kicad_plugin::cmd_install(dir.as_deref(), kicad.as_deref()),
        Command::KicadPlugin { cmd: KicadPluginCmd::Uninstall { kicad, dir } } => krbr_app::kicad_plugin::cmd_uninstall(dir.as_deref(), kicad.as_deref()),
        Command::Bench { pcb, spp, width, height } => krbr_app::bench::cmd_bench(&krbr_app::bench::BenchOptions { pcb: pcb.clone(), width: *width, height: *height, spp: *spp }),
        Command::Geom { pcb, dump } => cmd_geom(pcb, dump.as_deref()),
        Command::Diff { a, b } => cmd_diff(a, b),
        Command::Models { pcb, no_cache } => cmd_models(pcb, *no_cache),
    };
    // 凹凸のタイルのディスクのキャッシュを裏で書いていれば、書き終えるまで待つ
    krbr_geom::tile_file::wait_for_writes();
    if let Err(e) = result {
        krbr_core::error!("{e}");
        exit_with(1, explorer);
    }
}
