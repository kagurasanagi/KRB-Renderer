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
pub mod swatch_cache;
pub mod toolbar;
pub mod viewer;
pub mod watch;
pub mod video;
pub mod web_ui;

pub use prepare::{cmd_render, prepare_scene, RenderOptions};
pub use viewer::{run_viewer, ViewOptions};
pub use watch::{cmd_watch, run_watch, WatchEvent, WatchOptions, WatchUpdate};

/// テスト：全体設定のフォルダ（KRBR_CONFIG_DIR）を差し替えるテストは 1 つずつ走らせる
#[cfg(test)]
pub(crate) fn config_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
