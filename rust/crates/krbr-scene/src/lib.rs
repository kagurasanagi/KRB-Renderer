//! 基板のシーンの組み立て：材質の割り当て（材質ライブラリ・対応表・規則）、プロジェクト設定、
//! 基板と部品からレンダラに渡すシーンを作る。C++ 版の `scene/` と `app/config` に相当する。
//! 材質・金属の測定値・分光の土台・カメラ・レンズは KiCad に依存しないので krbr-pbr にあり、ここから同じ名前で使える

pub use krbr_pbr::{camera, conductor, lens, material, spectrum};

pub mod board_file;
pub mod config;
pub mod config_templates;
pub mod mask_presets;
pub mod material_io;
pub mod material_library;
pub mod model_map;
pub mod scene;
pub mod solder;
pub mod solder_sim;

pub use camera::{frame_camera, orbit_camera, Camera, CameraParams};
pub use config::{load_project_config, IntegratorKind, ProjectConfig};
pub use lens::{LensModel, LensParams, LensSystem};
pub use mask_presets::{find_mask_preset, mask_presets, MaskPreset};
pub use material::{linear_to_srgb, srgb_to_linear, BumpPattern, Category, Material, MaterialKind};
pub use material_library::MaterialLibrary;
pub use model_map::{GlobalModelMap, MaterialQuery, MaterialResolver, MaterialStage, ProjectOverrides, ResolvedMaterial};
pub use scene::{build_scene, footprint_class, raycast_pick, raycast_pick_visible, update_mask_material, visibility, update_substrate_material, Finish, Instance, InstanceOwner, Pick, Scene, SceneContext, SceneMesh, SceneOptions};
