//! 3D モデルのメッシュ：`.krbm` ファイル（C++ 版・変換器 DLL と同じ形式）、部位（名前と色）、
//! ファイル内容のハッシュをキーにしたキャッシュ、STEP 変換器 DLL（step\krbr_step.dll）の呼び出し。

pub mod library;
pub mod mesh_file;
pub mod part;

pub use library::{LoadResult, MeshParams, ModelLibrary};
pub use mesh_file::{read_mesh_file, write_mesh_file, MESH_FILE_VERSION};
pub use part::ModelPart;

use glam::Vec3;

/// 三角形メッシュ（C++ 版 `geom::Mesh` と同じ並び）。`tri_material` の意味は用途ごと（モデルなら部位番号）
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub indices: Vec<u32>,
    pub tri_material: Vec<u32>,
}

impl Mesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
    pub fn add_vertex(&mut self, p: Vec3, n: Vec3) -> u32 {
        self.positions.push(p);
        self.normals.push(n);
        (self.positions.len() - 1) as u32
    }
    pub fn add_triangle(&mut self, a: u32, b: u32, c: u32, mat: u32) {
        self.indices.extend_from_slice(&[a, b, c]);
        self.tri_material.push(mat);
    }
    /// 範囲（空なら None）
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let mut it = self.positions.iter();
        let first = *it.next()?;
        Some(it.fold((first, first), |(lo, hi), p| (lo.min(*p), hi.max(*p))))
    }
}

/// 3D モデルを分割したメッシュ。座標は mm（モデル自身の座標系、KiCad 3D と同じ Z 上向き）
#[derive(Debug, Clone, Default)]
pub struct ModelMesh {
    pub mesh: Mesh,
    pub parts: Vec<ModelPart>,
}
