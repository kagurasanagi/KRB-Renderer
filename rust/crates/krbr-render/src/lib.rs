//! Vulkan による描画：加速構造、積分器のカーネル、累積、ノイズ除去、トーンマッピング、画像の入出力。
//! シェーダ（<repo>/shaders の GLSL）は build.rs が glslc で SPIR-V にして、target/<profile>/shaders に置く。
//! GPU に送る構造体は gpu_types.rs、シェーダと共有する定数は shader_consts.rs が正本

pub mod denoiser;
pub mod env_map;
pub mod glow;
pub mod gpu_scene;
pub mod gpu_types;
pub mod image_io;
pub mod integrator;
pub mod kernel;
pub mod renderer;
pub mod shader_consts;
pub mod studio_env;
pub mod tonemapper;
pub mod vk;

pub use denoiser::{denoiser_available, DenoiseQuality, Denoiser};
pub use env_map::{build_env_distribution, EnvDistribution};
pub use gpu_scene::GpuScene;
pub use image_io::ImageF;
pub use integrator::{FrameParams, Progressive, RenderTargets};
pub use renderer::{render_offscreen, RenderOutput, RenderSettings};
pub use studio_env::{describe_environment, environment_label, is_builtin, load_environment_map, EnvironmentName};
pub use vk::{Context, ContextOptions, DeviceInfo};

use std::path::PathBuf;

/// シェーダ（SPIR-V）の置き場所：環境変数 KRBR_SHADER_DIR、exe と同じフォルダの shaders\（配布の一式と cargo build の出力）、
/// それが無ければ build.rs の出力先（テストの実行ファイルは target/<profile>/deps にあるため）
pub fn shader_dir() -> PathBuf {
    if let Ok(v) = std::env::var("KRBR_SHADER_DIR") {
        return PathBuf::from(v);
    }
    let local = krbr_core::platform::exe_dir().join("shaders");
    if local.join("pathtrace.comp.spv").exists() {
        return local;
    }
    let built = PathBuf::from(env!("KRBR_BUILD_SHADER_DIR"));
    if built.join("pathtrace.comp.spv").exists() {
        built
    } else {
        local
    }
}
