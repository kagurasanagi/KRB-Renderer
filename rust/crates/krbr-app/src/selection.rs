//! ビューアでクリックして選んだ部品：シーンを組み直しても同じ部品を指すための覚え方と、輪郭線（シルエット）の計算

use std::collections::HashMap;

use glam::{Mat3, Vec3};
use krbr_kicad::Board;
use krbr_scene::{Camera, InstanceOwner, Scene};

/// 選んだ部品（部品番号と、その何番目の 3D モデルか。シーンを組み直すとインスタンスの番号が変わるので、こちらで覚える）
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selected {
    pub reference: String,
    /// 代わりの箱なら None
    pub model: Option<usize>,
}

impl Selected {
    pub fn from_instance(scene: &Scene, board: &Board, instance: usize) -> Option<Selected> {
        match scene.instances.get(instance)?.owner {
            InstanceOwner::Board | InstanceOwner::Solder { .. } => None,
            InstanceOwner::Model { footprint, model, .. } => Some(Selected { reference: board.footprints.get(footprint as usize)?.reference.clone(), model: Some(model as usize) }),
            InstanceOwner::Placeholder { footprint } => Some(Selected { reference: board.footprints.get(footprint as usize)?.reference.clone(), model: None }),
        }
    }

    /// 今のシーンでのインスタンス。同じモデルが無ければ（モデルが見つからなくなったなど）、同じ部品の別のもの
    pub fn find_instance(&self, scene: &Scene, board: &Board) -> Option<usize> {
        let fp_ref = |fp: u32| board.footprints.get(fp as usize).map(|f| f.reference.as_str());
        let mut fallback = None;
        for (i, inst) in scene.instances.iter().enumerate() {
            let (fp, model) = match inst.owner {
                InstanceOwner::Board | InstanceOwner::Solder { .. } => continue,
                InstanceOwner::Model { footprint, model, .. } => (footprint, Some(model as usize)),
                InstanceOwner::Placeholder { footprint } => (footprint, None),
            };
            if fp_ref(fp) != Some(self.reference.as_str()) {
                continue;
            }
            if model == self.model {
                return Some(i);
            }
            fallback.get_or_insert(i);
        }
        fallback
    }
}

/// 輪郭線の候補の辺（ワールド座標）。両側の面の法線（片側しか無ければ縁）
struct Edge {
    a: Vec3,
    b: Vec3,
    n0: Vec3,
    n1: Option<Vec3>,
}

/// 選んだインスタンスの辺（選んだとき・シーンを組み直したときに作る）
pub struct Outline {
    edges: Vec<Edge>,
}

/// ワールド座標の点を、カメラの画面（w x h 画素）の画素の位置にする。カメラのすぐ前・後ろの点は None。
/// ピンホール・薄レンズの投影（実レンズは投影が違うので使わない）
pub fn project(cam: &Camera, p: Vec3, w: f32, h: f32) -> Option<[f32; 2]> {
    let v = p - cam.position;
    let z = v.dot(cam.forward);
    if z < 0.1 {
        return None;
    }
    let nx = v.dot(cam.right) / (z * cam.tan_half_fov_y * cam.aspect);
    let ny = -v.dot(cam.up) / (z * cam.tan_half_fov_y);
    Some([(nx + 1.0) * 0.5 * w, (ny + 1.0) * 0.5 * h])
}

/// project の逆：カメラの画面（w x h 画素）の画素の位置 (x, y) を通る視線の方向（正規化）
pub fn unproject(cam: &Camera, x: f32, y: f32, w: f32, h: f32) -> Vec3 {
    let (nx, ny) = (2.0 * x / w - 1.0, 2.0 * y / h - 1.0);
    (cam.forward + nx * cam.tan_half_fov_y * cam.aspect * cam.right - ny * cam.tan_half_fov_y * cam.up).normalize()
}

impl Outline {
    pub fn new(scene: &Scene, instance: usize) -> Outline {
        let inst = &scene.instances[instance];
        let mesh = &scene.meshes[inst.mesh as usize].mesh;
        let normal_m = Mat3::from_mat4(inst.xform).inverse().transpose();
        let world: Vec<Vec3> = mesh.positions.iter().map(|p| inst.xform.transform_point3(*p)).collect();
        // 面ごとに頂点が分かれているメッシュでも辺がつながるよう、位置で頂点をまとめる（0.1 µm）
        let mut ids: HashMap<[i64; 3], u32> = HashMap::new();
        let key = |p: Vec3| [(p.x as f64 * 1e4).round() as i64, (p.y as f64 * 1e4).round() as i64, (p.z as f64 * 1e4).round() as i64];
        let vid: Vec<u32> = world.iter().map(|p| { let n = ids.len() as u32; *ids.entry(key(*p)).or_insert(n) }).collect();
        let mut edges: HashMap<(u32, u32), Edge> = HashMap::new();
        for t in mesh.indices.chunks_exact(3) {
            let (a, b, c) = (world[t[0] as usize], world[t[1] as usize], world[t[2] as usize]);
            let n = (normal_m * (mesh.positions[t[1] as usize] - mesh.positions[t[0] as usize]).cross(mesh.positions[t[2] as usize] - mesh.positions[t[0] as usize])).normalize_or_zero();
            if n == Vec3::ZERO || (b - a).cross(c - a).length_squared() == 0.0 {
                continue;
            }
            for (i, j) in [(0, 1), (1, 2), (2, 0)] {
                let (vi, vj) = (vid[t[i] as usize], vid[t[j] as usize]);
                if vi == vj {
                    continue;
                }
                let k = (vi.min(vj), vi.max(vj));
                edges.entry(k).and_modify(|e| { e.n1.get_or_insert(n); }).or_insert(Edge { a: world[t[i] as usize], b: world[t[j] as usize], n0: n, n1: None });
            }
        }
        Outline { edges: edges.into_values().collect() }
    }

    /// カメラから見た輪郭（表と裏の境の辺と、縁の辺）を、画面の画素の線分にする。
    /// w x h はカメラの画面の大きさ（画素）。画面の外・カメラの後ろにかかる辺は除く
    pub fn segments(&self, cam: &Camera, w: f32, h: f32) -> Vec<([f32; 2], [f32; 2])> {
        let project = |p: Vec3| project(cam, p, w, h);
        let mut out = Vec::new();
        for e in &self.edges {
            let front = |n: Vec3| n.dot(cam.position - e.a) > 0.0;
            let silhouette = match e.n1 {
                None => true,
                Some(n1) => front(e.n0) != front(n1),
            };
            if !silhouette {
                continue;
            }
            if let (Some(a), Some(b)) = (project(e.a), project(e.b)) {
                out.push((a, b));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krbr_scene::orbit_camera;

    #[test]
    fn cube_outline_from_above_is_its_top_square() {
        // 2 mm 角の立方体を真上から見ると、輪郭は上面の 4 辺（側面との境）
        let mut s = Scene::default();
        let mut mesh = krbr_core::Mesh::default();
        let h = 1.0f32;
        let corners = |x: f32, y: f32, z: f32| Vec3::new(x * h, y * h, z * h);
        // 6 面を面ごとに頂点を分けて作る
        let faces = [
            [corners(-1., 1., -1.), corners(-1., 1., 1.), corners(1., 1., 1.), corners(1., 1., -1.)],
            [corners(-1., -1., -1.), corners(1., -1., -1.), corners(1., -1., 1.), corners(-1., -1., 1.)],
            [corners(1., -1., -1.), corners(1., 1., -1.), corners(1., 1., 1.), corners(1., -1., 1.)],
            [corners(-1., -1., -1.), corners(-1., -1., 1.), corners(-1., 1., 1.), corners(-1., 1., -1.)],
            [corners(-1., -1., 1.), corners(1., -1., 1.), corners(1., 1., 1.), corners(-1., 1., 1.)],
            [corners(-1., -1., -1.), corners(-1., 1., -1.), corners(1., 1., -1.), corners(1., -1., -1.)],
        ];
        for f in faces {
            let base = mesh.positions.len() as u32;
            mesh.positions.extend(f);
            mesh.normals.extend([Vec3::ZERO; 4]);
            mesh.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
            mesh.tri_material.extend([0, 0]);
        }
        s.meshes.push(krbr_scene::SceneMesh { name: "cube".into(), mesh });
        s.instances.push(krbr_scene::Instance { mesh: 0, xform: glam::Mat4::IDENTITY, material_base: 0, owner: InstanceOwner::Placeholder { footprint: 0 } });
        let o = Outline::new(&s, 0);
        assert_eq!(o.edges.len(), 12 + 6); // 立方体の 12 辺と、各面の対角線
        let cam = orbit_camera(Vec3::ZERO, 20.0, 89.0, 0.0, 30.0, 1.0);
        let segs = o.segments(&cam, 100.0, 100.0);
        assert_eq!(segs.len(), 4, "{segs:?}");
        for (a, b) in segs {
            for p in [a, b] {
                assert!(p[0] > 40.0 && p[0] < 60.0 && p[1] > 40.0 && p[1] < 60.0, "{p:?}");
            }
        }
    }

    #[test]
    fn unproject_inverts_project() {
        let cam = orbit_camera(Vec3::new(1.0, 2.0, 3.0), 50.0, 35.0, 45.0, 30.0, 1.6);
        for p in [Vec3::new(1.0, 2.0, 3.0), Vec3::new(5.0, -3.0, 8.0), Vec3::new(-6.0, 1.0, 0.0)] {
            let [x, y] = project(&cam, p, 320.0, 200.0).unwrap();
            let dir = unproject(&cam, x, y, 320.0, 200.0);
            let to = (p - cam.position).normalize();
            assert!(dir.dot(to) > 0.99999, "{dir:?} vs {to:?}");
        }
    }
}
