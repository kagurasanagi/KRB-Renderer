//! 描画するシーンの記述：材質（OpenPBR Surface）、金属の測定値、分光の土台（等色関数・色空間・RGB → スペクトルの表）、
//! カメラ・レンズ、メッシュ・材質・インスタンスを並べたシーン（RenderScene と、GPU 側が読む SceneSource）。
//! GPU（krbr-render）にも KiCad（krbr-kicad 以降）にも依存しないので、基板以外のアプリからも使える

pub mod camera;
pub mod conductor;
mod conductor_data;
mod integrator_kind;
pub mod lens;
pub mod material;
pub mod scene;
pub mod spectrum;

pub use camera::{frame_camera, orbit_camera, Camera, CameraParams};
pub use integrator_kind::IntegratorKind;
pub use krbr_core::{Bounds3, Mesh};
pub use lens::{LensModel, LensParams, LensSystem};
pub use material::{linear_to_srgb, srgb_to_linear, BumpPattern, Category, Material, MaterialKind};
pub use scene::{MaskParams, MaskSdf, RenderInstance, RenderScene, SceneSource, MASK_ALL};
