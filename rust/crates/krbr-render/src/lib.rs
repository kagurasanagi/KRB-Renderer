//! Vulkan による描画：加速構造、積分器のカーネル、累積、ノイズ除去、トーンマッピング、画像の入出力。
//! シーンは krbr-pbr の SceneSource から読むので、KiCad に依存しない（基板以外のシーンも描ける）。
//! ほかのアプリから使うときの窓口は session::Renderer。
//! シェーダ（<repo>/shaders の GLSL）は build.rs が glslc で SPIR-V にして、ライブラリに埋め込む。
//! GPU に送る構造体は gpu_types.rs、シェーダと共有する定数は shader_consts.rs が正本

pub mod denoiser;
pub mod env_map;
pub mod flow;
pub mod glow;
pub mod gpu_limit;
pub mod gpu_scene;
pub mod gpu_types;
pub mod image_io;
pub mod integrator;
pub mod kernel;
pub mod renderer;
pub mod session;
pub mod shader_consts;
pub mod studio_env;
pub mod tonemapper;
pub mod vk;

pub use denoiser::{denoiser_available, DenoiseQuality, Denoiser};
pub use env_map::{build_env_distribution, EnvDistribution};
pub use gpu_limit::GpuLimit;
pub use gpu_scene::GpuScene;
pub use image_io::ImageF;
pub use integrator::{AovImages, FrameParams, Progressive, RenderTargets};
/// シーンの記述（材質・カメラ・レンズ・RenderScene など）
pub use krbr_pbr as pbr;
pub use renderer::{render_offscreen, RenderOutput, RenderSettings};
pub use session::{Renderer, RendererOptions};
pub use studio_env::{describe_environment, environment_label, is_builtin, load_environment_map, EnvironmentName};
pub use vk::{Context, ContextOptions, DeviceInfo};

use std::borrow::Cow;

include!(concat!(env!("OUT_DIR"), "/embedded_shaders.rs"));

/// シェーダの SPIR-V（name はファイル名。例 "pathtrace.comp.spv"）。環境変数 KRBR_SHADER_DIR があればそのフォルダから読み
/// （シェーダだけを作り直して、ビルドし直さずに試すとき）、無ければ埋め込んだものを使う
pub fn shader_spirv(name: &str) -> Result<Cow<'static, [u8]>, String> {
    if let Some(dir) = std::env::var_os("KRBR_SHADER_DIR") {
        let p = std::path::Path::new(&dir).join(name);
        return std::fs::read(&p).map(Cow::Owned).map_err(|e| format!("cannot read shader {}: {e}", p.display()));
    }
    EMBEDDED_SHADERS.iter().find(|(n, _)| *n == name).map(|(_, b)| Cow::Borrowed(*b)).ok_or_else(|| format!("no shader named {name}"))
}
