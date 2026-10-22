//! コマンドの実装：シーンの準備（`prepare`）、オフライン描画（`render`）、リアルタイム表示（`viewer`）、保存の監視（`watch`）

pub mod bench;
pub mod i18n;
pub mod kicad_cli;
pub mod kicad_plugin;
pub mod material_cmd;
pub mod material_editor;
pub mod preview;
pub mod prepare;
pub mod remember;
pub mod selection;
pub mod swapchain;
pub mod viewer;
pub mod watch;

pub use prepare::{cmd_render, prepare_scene, RenderOptions};
pub use viewer::{run_viewer, ViewOptions};
pub use watch::{cmd_watch, run_watch, WatchEvent, WatchOptions, WatchUpdate};
