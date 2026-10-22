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

/// 部位の名前の長さの上限（バイト）。壊れたファイルで巨大な確保をしないための歯止め
const MAX_PART_NAME: usize = 4096;
/// ヘッダ（"KRBM" + u32[4]）の大きさ
const HEADER_BYTES: u64 = 4 + 16;
/// 1 頂点・1 三角形・1 部位（名前を除く）あたりのバイト数
const VERTEX_BYTES: u64 = 12 + 12;
const TRIANGLE_BYTES: u64 = 12 + 4;
const PART_BYTES: u64 = 4 + 20;

/// 読み込む。ヘッダの数がファイルの大きさと合わない、添字が頂点数以上、部位番号が部位数以上なら InvalidData
/// （キャッシュなら作り直しになる）。ヘッダの数で先に大きな確保をしないよう、ファイルの大きさを先に確かめる
pub fn read_mesh_file(p: &Path) -> io::Result<ModelMesh> {
    let file = std::fs::File::open(p)?;
    let file_len = file.metadata()?.len();
    let mut f = io::BufReader::new(file);
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(bad("not a .krbm file"));
    }
    let hdr = u32s(&read_exact_vec(&mut f, 16)?);
    if hdr[0] != MESH_FILE_VERSION {
        return Err(bad("unsupported .krbm version"));
    }
    let (nv, nt, nc) = (hdr[1] as u64, hdr[2] as u64, hdr[3] as u64);
    // 名前を除いた大きさ（u32 の数から計算するので u64 で溢れない）
    let min_len = HEADER_BYTES + nv * VERTEX_BYTES + nt * TRIANGLE_BYTES + nc * PART_BYTES;
    if min_len > file_len {
        return Err(bad("truncated .krbm file (the header does not match the file size)"));
    }
    let (nv, nt, nc) = (nv as usize, nt as usize, nc as usize);
    let mut m = ModelMesh::default();
    m.mesh.positions = vec3s(&read_exact_vec(&mut f, nv * 12)?);
    m.mesh.normals = vec3s(&read_exact_vec(&mut f, nv * 12)?);
    m.mesh.indices = u32s(&read_exact_vec(&mut f, nt * 12)?);
    m.mesh.tri_material = u32s(&read_exact_vec(&mut f, nt * 4)?);
    for _ in 0..nc {
        let len = u32s(&read_exact_vec(&mut f, 4)?)[0] as usize;
        if len > MAX_PART_NAME {
            return Err(bad("part name too long"));
        }
        let name = String::from_utf8_lossy(&read_exact_vec(&mut f, len)?).into_owned();
        let c = f32s(&read_exact_vec(&mut f, 20)?);
        m.parts.push(ModelPart { name, srgb: Vec3::new(c[0], c[1], c[2]), alpha: c[3], has_color: c[4] != 0.0 });
    }
    // 後段（ピック、GPU）は添字を検証せずに使うので、ここで確かめる。変換器は部位の無いメッシュでは三角形も作らない
    if m.mesh.indices.iter().any(|&i| i as usize >= nv) {
        return Err(bad("vertex index out of range"));
    }
    if m.mesh.tri_material.iter().any(|&t| t as usize >= nc.max(1)) {
        return Err(bad("part index out of range"));
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

        // 壊れたファイル：ヘッダの数がファイルより大きい（巨大な確保をせずにエラー）、添字が範囲外
        let bytes = std::fs::read(&p).unwrap();
        let mut huge = bytes.clone();
        huge[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        std::fs::write(&p, &huge).unwrap();
        assert_eq!(read_mesh_file(&p).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let mut bad_index = bytes.clone();
        let at = (HEADER_BYTES + 3 * VERTEX_BYTES) as usize; // 最初の添字
        bad_index[at..at + 4].copy_from_slice(&3u32.to_le_bytes());
        std::fs::write(&p, &bad_index).unwrap();
        assert_eq!(read_mesh_file(&p).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let mut bad_part = bytes;
        let at = (HEADER_BYTES + 3 * VERTEX_BYTES + 12) as usize; // 最初の三角形の部位番号
        bad_part[at..at + 4].copy_from_slice(&2u32.to_le_bytes());
        std::fs::write(&p, &bad_part).unwrap();
        assert_eq!(read_mesh_file(&p).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_file(&p);
    }
}
