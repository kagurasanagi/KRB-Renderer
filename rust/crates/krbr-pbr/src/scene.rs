//! 描画するシーンの記述：メッシュ、材質、インスタンス（メッシュの配置）。
//! GPU 側（krbr-render）は SceneSource からだけ読むので、基板のシーン（krbr-scene）も、ほかのアプリが作るシーン
//! （RenderScene）も同じように描ける。座標の単位は mm、Y が上（右手系）

use glam::{Mat4, Vec2};
use krbr_core::{Bounds3, Mesh};

use crate::material::Material;

/// インスタンスの表示マスクの既定値（すべてのビット）
pub const MASK_ALL: u8 = 0xFF;

/// メッシュの配置
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderInstance {
    /// メッシュの番号
    pub mesh: u32,
    /// オブジェクト → ワールドの変換
    pub xform: Mat4,
    /// メッシュの三角形の材質番号（Mesh::tri_material）に足す値。材質は materials[material_base + tri_material]
    pub material_base: u32,
    /// 表示マスク（8 bit）。光線の cull mask と照合し、GpuScene::set_visible_mask で選んだビットと重なるものだけを描く
    pub mask: u8,
}

/// レジストの厚みのモデル（材質の種別が Masked の面）。0 なら厚みを考えない
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MaskParams {
    /// 銅の上のレジストの厚み（mm）。散乱・吸収の基準の厚みにもなる
    pub on_copper: f32,
    /// 基材の上の厚み
    pub on_substrate: f32,
    /// 銅の縁の上の厚み
    pub at_edge: f32,
    /// 縁から中央への変化の距離
    pub falloff: f32,
    /// 銅の壁ぎわにたまる分
    pub pool: f32,
    pub pool_falloff: f32,
    /// 銅の厚み
    pub copper: f32,
}

/// レジストの厚みを決める距離場（表・裏の 2 層。RG8：R = 銅の縁、G = レジスト開口の縁からの距離）。
/// 値は 128 が縁、1 単位が range_mm / 127 mm（外側が正）
#[derive(Debug, Clone, Copy)]
pub struct MaskSdf<'a> {
    pub width: usize,
    pub height: usize,
    /// 画素 (0, 0) の隅のワールド X, Z
    pub origin: Vec2,
    /// 格子全体の大きさ（mm。ワールド X, Z 方向）
    pub size: Vec2,
    pub range_mm: f32,
    /// 表・裏の画素（width * height * 2 バイトずつ）
    pub layers: [&'a [u8]; 2],
    /// 前に送ってから作り直した（層, 画素の矩形 [x0, y0, x1, y1]）。大きさが同じなら、GPU にはこの範囲だけ送る
    pub dirty: &'a [(usize, [usize; 4])],
}

/// GPU に送るシーンの中身。krbr-render はこれだけを読む
pub trait SceneSource {
    fn mesh_count(&self) -> usize;
    fn mesh(&self, index: usize) -> &Mesh;
    fn materials(&self) -> &[Material];
    fn instance_count(&self) -> usize;
    fn instance(&self, index: usize) -> RenderInstance;
    /// レジストの厚みのモデル（基板のシーン以外は 0 のまま）
    fn mask_params(&self) -> MaskParams {
        MaskParams::default()
    }
    /// レジストの厚みの距離場（無ければ厚みは一様）
    fn mask_sdf(&self) -> Option<MaskSdf<'_>> {
        None
    }
}

/// メッシュ・材質・インスタンスを並べただけのシーン（基板以外のシーンをライブラリとして描くとき用）
#[derive(Debug, Clone, Default)]
pub struct RenderScene {
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    pub instances: Vec<RenderInstance>,
}

impl RenderScene {
    /// メッシュを足して番号を返す
    pub fn add_mesh(&mut self, mesh: Mesh) -> u32 {
        self.meshes.push(mesh);
        (self.meshes.len() - 1) as u32
    }

    /// 材質を足して番号を返す
    pub fn add_material(&mut self, material: Material) -> u32 {
        self.materials.push(material);
        (self.materials.len() - 1) as u32
    }

    /// メッシュを置く（表示マスクは MASK_ALL）。インスタンスの番号を返す
    pub fn add_instance(&mut self, mesh: u32, xform: Mat4, material_base: u32) -> u32 {
        self.instances.push(RenderInstance { mesh, xform, material_base, mask: MASK_ALL });
        (self.instances.len() - 1) as u32
    }

    /// 置いたメッシュ全体のワールド座標の範囲
    pub fn bounds(&self) -> Bounds3 {
        let mut b = Bounds3::default();
        for i in &self.instances {
            for p in &self.meshes[i.mesh as usize].positions {
                b.add(i.xform.transform_point3(*p));
            }
        }
        b
    }

    /// 番号の食い違い（無いメッシュを置いた、材質が足りない、頂点と法線の数が違う、添字が範囲外）を調べる
    pub fn validate(&self) -> Result<(), String> {
        for (k, m) in self.meshes.iter().enumerate() {
            if m.normals.len() != m.positions.len() {
                return Err(format!("mesh {k}: {} normals for {} positions", m.normals.len(), m.positions.len()));
            }
            if m.indices.len() % 3 != 0 || m.tri_material.len() != m.indices.len() / 3 {
                return Err(format!("mesh {k}: {} indices and {} triangle materials", m.indices.len(), m.tri_material.len()));
            }
            if let Some(&i) = m.indices.iter().find(|&&i| i as usize >= m.positions.len()) {
                return Err(format!("mesh {k}: index {i} out of range ({} vertices)", m.positions.len()));
            }
        }
        for (k, i) in self.instances.iter().enumerate() {
            let Some(m) = self.meshes.get(i.mesh as usize) else {
                return Err(format!("instance {k}: mesh {} does not exist ({} meshes)", i.mesh, self.meshes.len()));
            };
            let need = m.tri_material.iter().max().map_or(0, |&t| i.material_base as usize + t as usize + 1);
            if need > self.materials.len() {
                return Err(format!("instance {k}: needs {need} materials, the scene has {}", self.materials.len()));
            }
        }
        Ok(())
    }
}

impl SceneSource for RenderScene {
    fn mesh_count(&self) -> usize {
        self.meshes.len()
    }
    fn mesh(&self, index: usize) -> &Mesh {
        &self.meshes[index]
    }
    fn materials(&self) -> &[Material] {
        &self.materials
    }
    fn instance_count(&self) -> usize {
        self.instances.len()
    }
    fn instance(&self, index: usize) -> RenderInstance {
        self.instances[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn triangle(mat: u32) -> Mesh {
        let mut m = Mesh::default();
        let (a, b, c) = (m.add_vertex(Vec3::ZERO, Vec3::Y), m.add_vertex(Vec3::X, Vec3::Y), m.add_vertex(Vec3::Z, Vec3::Y));
        m.add_triangle(a, b, c, mat);
        m
    }

    #[test]
    fn validate_catches_bad_references() {
        let mut s = RenderScene::default();
        let m = s.add_mesh(triangle(1));
        s.add_material(Material::default());
        s.add_instance(m, Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0)), 0);
        // 三角形の材質 1 には材質が 2 つ要る
        assert!(s.validate().unwrap_err().contains("needs 2 materials"));
        s.add_material(Material::default());
        assert_eq!(s.validate(), Ok(()));
        assert_eq!(s.bounds().lo, Vec3::new(10.0, 0.0, 0.0));
        s.add_instance(5, Mat4::IDENTITY, 0);
        assert!(s.validate().unwrap_err().contains("mesh 5 does not exist"));
        s.instances.pop();
        s.meshes[0].indices[2] = 7;
        assert!(s.validate().unwrap_err().contains("out of range"));
    }
}
