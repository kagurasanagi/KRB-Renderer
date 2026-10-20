//! krbr（Rust 版）のコマンドライン。移植の進み具合に合わせてサブコマンドを増やす。
//! フェーズ1：`info`（基板の解析と 3D モデルのパス解決。GPU は使わない）

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use krbr_core::{info, log};
use krbr_kicad::{load_board, ModelResolver};

#[derive(Parser)]
#[command(name = "krbr", about = "KRB-Renderer: KiCad 連携 物理ベース レンダラー (Rust port)")]
struct Cli {
    /// debug ログも出す
    #[arg(short, long, global = true)]
    verbose: bool,
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
    /// build the board geometry and the relief tiles (no GPU) and print statistics
    Geom {
        /// .kicad_pcb file
        pcb: PathBuf,
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
    /// assign a material per reference (project overrides.toml) or per 3D model (project, or --global)
    Set {
        /// .kicad_pcb file
        pcb: PathBuf,
        /// reference designator (project overrides.toml)
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
}

#[derive(Subcommand)]
enum MaskCmd {
    /// list the solder mask color presets (KiCad color names and JLCPCB options)
    List,
}

#[derive(Args, Debug)]
struct RenderArgs {
    /// .kicad_pcb file
    pcb: PathBuf,
    /// equirectangular environment map (.hdr / .exr, or builtin:studio). Remembered per board;
    /// if omitted: the one last used for this board, else [environment] hdr in environment.toml, else builtin:studio
    #[arg(long, default_value = "")]
    hdr: String,
    /// output PNG (render: EXR and AOVs are written next to it; default out/board.png, watch: out/watch.png)
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
    /// zoom factor relative to the automatic framing
    #[arg(long, default_value_t = 1.0)]
    zoom: f64,
    /// exposure (EV)
    #[arg(long)]
    exposure: Option<f32>,
    #[arg(long)]
    env_intensity: Option<f32>,
    #[arg(long)]
    env_rotation: Option<f32>,
    /// clamp indirect radiance per sample (default 10, 0 = off)
    #[arg(long)]
    clamp: Option<f32>,
    /// integrator: pt (default), bdpt, vcm
    #[arg(long, default_value = "")]
    integrator: String,
    #[arg(long, default_value_t = 0)]
    seed: u32,
    /// do not read or write the mesh cache (and the kicad-cli silk cache)
    #[arg(long)]
    no_cache: bool,
    /// do not use kicad-cli for stroke-font silkscreen texts (they will be missing)
    #[arg(long)]
    no_kicad_cli: bool,
    /// write template <name>.krbr\board.toml and environment.toml (existing files are kept)
    #[arg(long)]
    init: bool,
    /// enable the Vulkan validation layer
    #[arg(long)]
    validate: bool,
    /// spectral rendering (hero wavelengths)
    #[arg(long)]
    spectral: bool,
    /// camera lens: pinhole (default), thin, realistic
    #[arg(long, default_value = "")]
    lens: String,
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
            pcb: self.pcb.clone(),
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
            env_intensity: self.env_intensity,
            env_rotation: self.env_rotation,
            radiance_clamp: self.clamp,
            look_at: self.look_at.clone(),
            zoom: self.zoom,
            integrator: self.integrator.clone(),
            seed: self.seed,
            no_cache: self.no_cache,
            no_kicad_cli: self.no_kicad_cli,
            init: self.init,
            validate: self.validate,
            spectral: if self.spectral { Some(true) } else { None },
            lens: self.lens.clone(),
            f_number: self.f_number,
            focal_length: self.focal_length,
            focus_distance: self.focus_distance,
            blades: self.blades,
            mask_color: self.mask_color.clone(),
        }
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

fn cmd_geom(pcb: &PathBuf) -> anyhow::Result<()> {
    use krbr_geom::{build_board_geometry, tiles::build_relief_tiles, BoardBuildOptions, LayerArtOptions, ReliefParams, ReliefTileCache};
    let board = load_board(pcb)?;
    let mut rp = ReliefParams::default();
    if board.stackup.copper_thickness > 0.0 {
        rp.copper = board.stackup.copper_thickness;
    }
    let opts = BoardBuildOptions { core_half: rp.core_half(board.stackup.thickness), caps: false };
    let geo = build_board_geometry(&board, &opts);
    let mut cache = ReliefTileCache::default();
    let rebuilt = build_relief_tiles(&mut cache, &board, &geo, &rp, &LayerArtOptions::default());
    let tris: usize = (0..2).map(|s| cache.tiles[s].iter().map(|m| m.triangle_count()).sum::<usize>()).sum();
    info!("geom: {} tiles rebuilt, {} relief triangles, sdf {}x{} px", rebuilt, tris, cache.sdf_grid.width, cache.sdf_grid.height);
    Ok(())
}

fn cmd_models(pcb: &PathBuf, no_cache: bool) -> anyhow::Result<()> {
    use krbr_model::{library::step_support, MeshParams, ModelLibrary};
    let board = load_board(pcb)?;
    match step_support() {
        Ok(()) => info!("STEP converter: available"),
        Err(e) => krbr_core::warn!("STEP converter unavailable ({e}); STEP models will be placeholders"),
    }
    let resolver = ModelResolver::new(&board.project_dir, &BTreeMap::new(), &[]);
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
    info!("loading {}", pcb.display());
    let board = load_board(pcb)?;
    info!(
        "board v{}: {} footprints, {} pads, {} tracks, {} vias, {} zone fills, {} graphics",
        board.version,
        board.footprints.len(),
        board.pads.len(),
        board.tracks.len(),
        board.vias.len(),
        board.zones.len(),
        board.graphics.len()
    );
    if board.unfilled_zones > 0 {
        krbr_core::warn!("{} zone(s) have no fill; run zone fill in KiCad and save", board.unfilled_zones);
    }
    info!(
        "stackup: thickness {:.3} mm, finish '{}', mask '{}'",
        board.stackup.thickness, board.stackup.copper_finish, board.stackup.mask_color_front
    );
    let stroke = board.stroke_texts.iter().filter(|t| t.layer == "F.SilkS" || t.layer == "B.SilkS").count();
    info!("stroke-font texts on silkscreen (need kicad-cli): {}", stroke);
    if !models {
        return Ok(());
    }
    // TODO: board.toml の path_vars / model_search_dirs（設定ファイルの移植後）
    let resolver = ModelResolver::new(&board.project_dir, &BTreeMap::new(), &[]);
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
        eprintln!("
Enter キーで閉じます");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    std::process::exit(code)
}

fn main() {
    // サブコマンドを省略して `krbr board.kicad_pcb ...` と書いたら view とみなす
    // （exe のアイコンに .kicad_pcb をドラッグ＆ドロップしたときもこの形になる）
    let mut args: Vec<String> = std::env::args().collect();
    let explorer = launched_from_explorer();
    if let Some(i) = args.iter().skip(1).position(|a| !a.starts_with('-')) {
        let i = i + 1;
        if args[i].to_lowercase().ends_with(".kicad_pcb") {
            // エクスプローラーからの起動では作業フォルダが決まらないので、保存先（既定 out\）を基板のフォルダの下にする
            if explorer {
                if let Some(dir) = std::path::absolute(&args[i]).ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
                    let _ = std::env::set_current_dir(dir);
                }
            }
            args.insert(i, "view".into());
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
    let result = match &cli.command {
        Command::Info { pcb, models } => cmd_info(pcb, *models),
        Command::Render(args) => krbr_app::cmd_render(&args.to_options("out/board.png")),
        Command::View(args) => {
            let vo = krbr_app::ViewOptions {
                common: args.render.to_options("out/board.png"),
                test_frames: args.test_frames,
                screenshot: args.screenshot.clone(),
                hidden: args.hidden,
                test_orbit_deg: args.test_orbit,
                watch: !args.no_watch,
                test_reload: args.test_reload,
                test_pick: (args.test_pick.len() == 2).then(|| (args.test_pick[0], args.test_pick[1])),
                test_assign: args.test_assign.clone(),
            };
            krbr_app::run_viewer(vo).map(|rc| {
                if rc != 0 {
                    exit_with(rc, explorer);
                }
            })
        }
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
        },
        Command::Mask { cmd: MaskCmd::List } => krbr_app::material_cmd::cmd_mask_list(),
        Command::Geom { pcb } => cmd_geom(pcb),
        Command::Diff { a, b } => cmd_diff(a, b),
        Command::Models { pcb, no_cache } => cmd_models(pcb, *no_cache),
    };
    if let Err(e) = result {
        krbr_core::error!("{e}");
        exit_with(1, explorer);
    }
}
