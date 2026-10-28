//! シーンの組み立て：材質（OpenPBR Surface）、材質の割り当て（材質ライブラリ・対応表・規則）、
//! 分光の土台（等色関数・色空間・RGB → スペクトルの表）、カメラ・レンズ、プロジェクト設定、
//! 基板と部品からレンダラに渡すシーンを作る。C++ 版の `scene/` と `app/config` に相当する。

pub mod board_file;
pub mod camera;
pub mod conductor;
mod conductor_data;
pub mod config;
pub mod config_templates;
pub mod lens;
pub mod mask_presets;
pub mod material;
pub mod material_io;
pub mod material_library;
pub mod model_map;
pub mod scene;
pub mod solder;
pub mod spectrum;

pub use camera::{frame_camera, orbit_camera, Camera, CameraParams};
pub use config::{load_project_config, IntegratorKind, ProjectConfig};
pub use lens::{LensModel, LensParams, LensSystem};
pub use mask_presets::{find_mask_preset, mask_presets, MaskPreset};
pub use material::{linear_to_srgb, srgb_to_linear, BumpPattern, Category, Material, MaterialKind};
pub use material_library::MaterialLibrary;
pub use model_map::{GlobalModelMap, MaterialQuery, MaterialResolver, MaterialStage, ProjectOverrides, ResolvedMaterial};
pub use scene::{build_scene, footprint_class, raycast_pick, raycast_pick_visible, update_mask_material, visibility, update_substrate_material, Finish, Instance, InstanceOwner, Pick, Scene, SceneContext, SceneMesh, SceneOptions};
