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
    };
    if let Err(e) = result {
        krbr_core::error!("{e}");
        std::process::exit(1);
    }
}
