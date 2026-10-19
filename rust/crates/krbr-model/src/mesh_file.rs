//! `.krbm`：3D モデルの分割結果のファイル形式（C++ の `model/mesh_file.hpp` と同じ）
//!   "KRBM" + u32[4]{version, 頂点数, 三角形数, 部位数}
//!   + 頂点 f32x3[n] + 法線 f32x3[n] + 添字 u32[3t] + 三角形ごとの部位番号 u32[t]
//!   + 部位ごとに { u32 名前の長さ, 名前(UTF-8), f32[5]{r, g, b, a, 色あり} }

use std::io::{self, Read, Write};
use std::path::Path;

use glam::Vec3;

use crate::{ModelMesh, ModelPart};

pub const MESH_FILE_VERSION: u32 = 3;
const MAGIC: &[u8; 4] = b"KRBM";

fn read_exact_vec(r: &mut impl Read, bytes: usize) -> io::Result<Vec<u8>> {
    let mut v = vec![0u8; bytes];
    r.read_exact(&mut v)?;
    Ok(v)
}

fn f32s(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn u32s(bytes: &[u8]) -> Vec<u32> {
    bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect()
}

fn vec3s(bytes: &[u8]) -> Vec<Vec3> {
    f32s(bytes).chunks_exact(3).map(|c| Vec3::new(c[0], c[1], c[2])).collect()
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

pub fn read_mesh_file(p: &Path) -> io::Result<ModelMesh> {
    let mut f = io::BufReader::new(std::fs::File::open(p)?);
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(bad("not a .krbm file"));
    }
    let hdr = u32s(&read_exact_vec(&mut f, 16)?);
    if hdr[0] != MESH_FILE_VERSION {
        return Err(bad("unsupported .krbm version"));
    }
    let (nv, nt, nc) = (hdr[1] as usize, hdr[2] as usize, hdr[3] as usize);
    let mut m = ModelMesh::default();
    m.mesh.positions = vec3s(&read_exact_vec(&mut f, nv * 12)?);
    m.mesh.normals = vec3s(&read_exact_vec(&mut f, nv * 12)?);
    m.mesh.indices = u32s(&read_exact_vec(&mut f, nt * 12)?);
    m.mesh.tri_material = u32s(&read_exact_vec(&mut f, nt * 4)?);
    for _ in 0..nc {
        let len = u32s(&read_exact_vec(&mut f, 4)?)[0] as usize;
        if len > 4096 {
            return Err(bad("part name too long"));
        }
        let name = String::from_utf8_lossy(&read_exact_vec(&mut f, len)?).into_owned();
        let c = f32s(&read_exact_vec(&mut f, 20)?);
        m.parts.push(ModelPart { name, srgb: Vec3::new(c[0], c[1], c[2]), alpha: c[3], has_color: c[4] != 0.0 });
    }
    Ok(m)
}

pub fn write_mesh_file(p: &Path, m: &ModelMesh) -> io::Result<()> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("krbm.tmp");
    {
        let mut f = io::BufWriter::new(std::fs::File::create(&tmp)?);
        f.write_all(MAGIC)?;
        for v in [MESH_FILE_VERSION, m.mesh.positions.len() as u32, m.mesh.triangle_count() as u32, m.parts.len() as u32] {
            f.write_all(&v.to_le_bytes())?;
        }
        for p in &m.mesh.positions {
            for c in p.to_array() {
                f.write_all(&c.to_le_bytes())?;
            }
        }
        for n in &m.mesh.normals {
            for c in n.to_array() {
                f.write_all(&c.to_le_bytes())?;
            }
        }
        for i in &m.mesh.indices {
            f.write_all(&i.to_le_bytes())?;
        }
        for t in &m.mesh.tri_material {
            f.write_all(&t.to_le_bytes())?;
        }
        for part in &m.parts {
            f.write_all(&(part.name.len() as u32).to_le_bytes())?;
            f.write_all(part.name.as_bytes())?;
            for c in [part.srgb.x, part.srgb.y, part.srgb.z, part.alpha, if part.has_color { 1.0 } else { 0.0 }] {
                f.write_all(&c.to_le_bytes())?;
            }
        }
        f.flush()?;
    }
    std::fs::rename(&tmp, p) // 書き込み途中のファイルを残さない
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut m = ModelMesh::default();
        let a = m.mesh.add_vertex(Vec3::ZERO, Vec3::Z);
        let b = m.mesh.add_vertex(Vec3::X, Vec3::Z);
        let c = m.mesh.add_vertex(Vec3::Y, Vec3::Z);
        m.mesh.add_triangle(a, b, c, 1);
        m.parts.push(ModelPart { name: String::new(), srgb: Vec3::splat(0.5), alpha: 1.0, has_color: false });
        m.parts.push(ModelPart { name: "端子".into(), srgb: Vec3::new(0.824, 0.820, 0.781), alpha: 1.0, has_color: true });
        let dir = std::env::temp_dir().join("krbr_rust_meshfile_test");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.krbm");
        write_mesh_file(&p, &m).unwrap();
        let r = read_mesh_file(&p).unwrap();
        assert_eq!(r.mesh.positions, m.mesh.positions);
        assert_eq!(r.mesh.indices, m.mesh.indices);
        assert_eq!(r.mesh.tri_material, vec![1]);
        assert_eq!(r.parts.len(), 2);
        assert_eq!(r.parts[1].name, "端子");
        assert!(r.parts[1].has_color && !r.parts[0].has_color);
        assert_eq!(r.parts[1].kicad_color_name(), Some("metal_grey_pins"));
    }
}
