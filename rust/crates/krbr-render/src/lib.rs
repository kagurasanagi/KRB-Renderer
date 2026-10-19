//! Vulkan による描画（C++ 版の `render/` と `io/` に相当）。
//! シェーダは C++ 版と同じ SPIR-V（`shaders/*.spv`）をそのまま使う。

pub mod env_map;
pub mod gpu_scene;
pub mod gpu_types;
pub mod image_io;
pub mod integrator;
pub mod kernel;
pub mod renderer;
pub mod tonemapper;
pub mod vk;

pub use env_map::{build_env_distribution, EnvDistribution};
pub use gpu_scene::GpuScene;
pub use image_io::ImageF;
pub use integrator::{FrameParams, Progressive, RenderTargets};
pub use renderer::{render_offscreen, RenderOutput, RenderSettings};
pub use vk::{Context, ContextOptions, DeviceInfo};

use std::path::PathBuf;

/// シェーダ（SPIR-V）の置き場所：exe と同じフォルダの shaders\、環境変数 KRBR_SHADER_DIR、
/// 開発中は C++ 版のビルド出力（<repo>/build/release/shaders）
pub fn shader_dir() -> PathBuf {
    if let Ok(v) = std::env::var("KRBR_SHADER_DIR") {
        return PathBuf::from(v);
    }
    let exe = krbr_core::platform::exe_dir();
    let local = exe.join("shaders");
    if local.join("pathtrace.comp.spv").exists() {
        return local;
    }
    // rust/target/<profile>/krbr.exe → <repo>/build/release/shaders
    let mut p = exe.clone();
    for _ in 0..3 {
        p = p.parent().map(|d| d.to_path_buf()).unwrap_or(p);
    }
    let dev = p.join("build").join("release").join("shaders");
    if dev.join("pathtrace.comp.spv").exists() {
        return dev;
    }
    local
}
