//! 3D モデルのメッシュ：`.krbm` ファイル（C++ 版・変換器 DLL と同じ形式）、部位（名前と色）、
//! ファイル内容のハッシュをキーにしたキャッシュ、STEP 変換器 DLL（step\krbr_step.dll）の呼び出し。
//! sRGB とリニアの変換（color）も、krbr-scene と共有するためここに置く。

pub mod color;
pub mod library;
pub mod mesh_file;
pub mod part;

pub use library::{LoadResult, MeshParams, ModelLibrary};
pub use mesh_file::{read_mesh_file, write_mesh_file, MESH_FILE_VERSION};
pub use part::ModelPart;

pub use krbr_core::Mesh;

/// 3D モデルを分割したメッシュ。座標は mm（モデル自身の座標系、KiCad 3D と同じ Z 上向き）
#[derive(Debug, Clone, Default)]
pub struct ModelMesh {
    pub mesh: Mesh,
    pub parts: Vec<ModelPart>,
}
