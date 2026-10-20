//! コマンドの実装：シーンの準備（`prepare`）、オフライン描画（`render`）、リアルタイム表示（`viewer`）

pub mod prepare;
pub mod remember;
pub mod swapchain;
pub mod viewer;

pub use prepare::{cmd_render, prepare_scene, RenderOptions};
pub use viewer::{run_viewer, ViewOptions};
