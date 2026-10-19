//! krbr（Rust 版）のコマンドライン。移植の進み具合に合わせてサブコマンドを増やす。
//! フェーズ1：`info`（基板の解析と 3D モデルのパス解決。GPU は使わない）

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
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
    /// build the board geometry and the relief tiles (no GPU) and print statistics
    Geom {
        /// .kicad_pcb file
        pcb: PathBuf,
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

fn main() {
    let cli = Cli::parse();
    if cli.verbose {
        log::set_level(log::Level::Debug);
    }
    let result = match &cli.command {
        Command::Info { pcb, models } => cmd_info(pcb, *models),
        Command::Geom { pcb } => cmd_geom(pcb),
        Command::Models { pcb, no_cache } => cmd_models(pcb, *no_cache),
    };
    if let Err(e) = result {
        krbr_core::error!("{e}");
        std::process::exit(1);
    }
}
