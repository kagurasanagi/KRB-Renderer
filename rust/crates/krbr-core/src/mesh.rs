//! 三角形メッシュ（.krbm と同じ並び：step_dll/model_mesh.hpp の Mesh）。`tri_material` の意味は用途ごと（基板なら面の区分、モデルなら部位番号）

use glam::Vec3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds3 {
    pub lo: Vec3,
    pub hi: Vec3,
}

impl Default for Bounds3 {
    fn default() -> Self {
        Self { lo: Vec3::splat(f32::MAX), hi: Vec3::splat(-f32::MAX) }
    }
}

impl Bounds3 {
    pub fn add(&mut self, p: Vec3) {
        self.lo = self.lo.min(p);
        self.hi = self.hi.max(p);
    }
    pub fn add_bounds(&mut self, b: &Bounds3) {
        if b.valid() {
            self.add(b.lo);
            self.add(b.hi);
        }
    }
    /// 3 軸とも lo <= hi（add は 3 軸を同時に更新するので x だけでも結果は同じだが、意味をそろえて 3 軸を見る）
    pub fn valid(&self) -> bool {
        self.lo.cmple(self.hi).all()
    }
    pub fn center(&self) -> Vec3 {
        (self.lo + self.hi) * 0.5
    }
    pub fn extent(&self) -> Vec3 {
        self.hi - self.lo
    }
}

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
    pub fn bounds(&self) -> Bounds3 {
        let mut b = Bounds3::default();
        for p in &self.positions {
            b.add(*p);
        }
        b
    }
    /// 別のメッシュを後ろにつなぐ
    pub fn append(&mut self, other: &Mesh) {
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(&other.positions);
        self.normals.extend_from_slice(&other.normals);
        self.indices.extend(other.indices.iter().map(|i| i + base));
        self.tri_material.extend_from_slice(&other.tri_material);
    }
}
