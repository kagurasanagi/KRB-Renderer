//! 凹凸のタイルのディスクのキャッシュ（`<キャッシュ>\relief\<基板のパスのハッシュ>.krbt`）。
//! 変えていない基板を開き直すときに、タイルを作り直さずに読む（大きな基板で数秒かかる処理）。
//! タイルごとに入力のキー（tiles::tile_keys）を持ち、キーが同じタイルだけを使うので、基板を少し直した後でも、
//! 変わった範囲のタイルだけを作り直せばよい。
//!   "KRBT" + u32 版 + u32 タイル数 + u64 タイルごとの始まりの位置[2 x タイル数]（並列に読むため）+ 面ごと・タイルごとに
//!   { u64 キー, u32 頂点数, u32 三角形数,
//!     高さの表 u32 数 + f32[数], 頂点ごとに { f32 x, f32 z, u8 高さの番号 },
//!     法線：頂点ごとに u8 種類（0 = (0,1,0), 1 = (0,-1,0), 2 = (x,0,z) で f32 x, z が続く, 3 = f32 x, y, z が続く）,
//!     添字 u32[3t], 材質 u8[t],
//!     u32 x0, y0, w, h, 銅の距離場, レジスト開口の距離場（どちらも u32 長さ + PackBits） }
//! 凹凸の形は高さが層の数ほどしかなく、面の大半は真上・真下か垂直なので、そこを詰める（値はどれもビットまでそのまま戻る）。
//! 距離場は縁から 0.3 mm より先がすべて 0 か 255 なので、同じ値の並びを詰める。
//! 頂点の高さが 256 種を超える・材質が 256 以上のタイルがあれば書かない（encode が Err。その基板はキャッシュしない）

use std::io::{self, Read, Write};
use std::path::Path;

use glam::Vec3;
use krbr_core::Mesh;

use crate::tiles::SdfPatch;

/// 形式を変えたら上げる
const VERSION: u32 = 3;
const MAGIC: &[u8; 4] = b"KRBT";

/// キャッシュから読んだタイル
pub struct CachedTile {
    pub key: u64,
    pub mesh: Mesh,
    pub sdf: SdfPatch,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// PackBits：制御のバイト c が 128 以上なら次の 1 バイトを (c - 128) + 3 回、128 未満なら続く c + 1 バイトをそのまま
fn pack_bits(v: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&(v.len() as u32).to_le_bytes());
    let flush = |out: &mut Vec<u8>, mut a: usize, b: usize| {
        while a < b {
            let n = (b - a).min(128);
            out.push((n - 1) as u8);
            out.extend_from_slice(&v[a..a + n]);
            a += n;
        }
    };
    let (mut i, mut lit) = (0, 0);
    while i < v.len() {
        let mut j = i + 1;
        while j < v.len() && v[j] == v[i] && j - i < 130 {
            j += 1;
        }
        if j - i >= 3 {
            flush(out, lit, i);
            out.push((128 + (j - i - 3)) as u8);
            out.push(v[i]);
            lit = j;
        }
        i = j;
    }
    flush(out, lit, v.len());
}

fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn put_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn encode_mesh(m: &Mesh, out: &mut Vec<u8>) -> io::Result<()> {
    put_u32(out, m.positions.len() as u32);
    put_u32(out, m.triangle_count() as u32);
    let mut heights: Vec<u32> = m.positions.iter().map(|v| v.y.to_bits()).collect();
    heights.sort_unstable();
    heights.dedup();
    if heights.len() > 256 {
        return Err(bad("more than 256 heights in a tile"));
    }
    put_u32(out, heights.len() as u32);
    for &h in &heights {
        put_u32(out, h);
    }
    for v in &m.positions {
        put_f32(out, v.x);
        put_f32(out, v.z);
        out.push(heights.binary_search(&v.y.to_bits()).unwrap() as u8);
    }
    let (one, minus_one) = (1.0f32.to_bits(), (-1.0f32).to_bits());
    for n in &m.normals {
        let flat = n.x.to_bits() == 0 && n.z.to_bits() == 0;
        if flat && n.y.to_bits() == one {
            out.push(0);
        } else if flat && n.y.to_bits() == minus_one {
            out.push(1);
        } else if n.y.to_bits() == 0 {
            out.push(2);
            put_f32(out, n.x);
            put_f32(out, n.z);
        } else {
            out.push(3);
            for c in n.to_array() {
                put_f32(out, c);
            }
        }
    }
    for &i in &m.indices {
        put_u32(out, i);
    }
    for &t in &m.tri_material {
        out.push(u8::try_from(t).map_err(|_| bad("material number over 255"))?);
    }
    Ok(())
}

/// タイルを書き出す形にする（ファイルへ書くのは別のスレッドでよいように、バイト列にまとめるだけ）
pub fn encode(tiles: &[Vec<(u64, &Mesh, &SdfPatch)>; 2]) -> io::Result<Vec<u8>> {
    let count = tiles[0].len();
    let mut body = Vec::new();
    let mut offsets: Vec<u64> = Vec::with_capacity(2 * count);
    let head = 12 + 16 * count as u64;
    for side in tiles {
        for (key, m, p) in side {
            offsets.push(head + body.len() as u64);
            body.extend_from_slice(&key.to_le_bytes());
            encode_mesh(m, &mut body)?;
            for v in [p.x0, p.y0, p.w, p.h] {
                put_u32(&mut body, v as u32);
            }
            pack_bits(&p.copper, &mut body);
            pack_bits(&p.mask_open, &mut body);
        }
    }
    let mut out = Vec::with_capacity(head as usize + body.len());
    out.extend_from_slice(MAGIC);
    put_u32(&mut out, VERSION);
    put_u32(&mut out, count as u32);
    for o in offsets {
        out.extend_from_slice(&o.to_le_bytes());
    }
    out.extend_from_slice(&body);
    Ok(out)
}

static WRITERS: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>> = std::sync::Mutex::new(Vec::new());

/// 裏で書き出す（write_in_background で始めたものは、終了の前に wait_for_writes で待つ）
pub fn write_in_background(f: impl FnOnce() + Send + 'static) {
    let h = std::thread::spawn(f);
    let mut w = WRITERS.lock().unwrap();
    w.retain(|h| !h.is_finished());
    w.push(h);
}

/// 裏の書き出しが終わるまで待つ（プロセスを終える前に。途中で終わっても一時ファイルが残るだけだが、次の起動が速くならない）
pub fn wait_for_writes() {
    let handles: Vec<_> = std::mem::take(&mut *WRITERS.lock().unwrap());
    for h in handles {
        let _ = h.join();
    }
}

/// 一時ファイルに書いてから置き換える（書いている途中で読まれても壊れたファイルを見せない）
pub fn write_file(file: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("krbt.tmp");
    std::fs::File::create(&tmp)?.write_all(bytes)?;
    std::fs::rename(&tmp, file)
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> io::Result<&[u8]> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| bad("truncated .krbt file"))?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn byte(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u32s(&mut self, n: usize) -> io::Result<Vec<u32>> {
        let b = self.take(n.checked_mul(4).ok_or_else(|| bad("bad count"))?)?;
        Ok(b.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect())
    }
    fn unpack_bits(&mut self, len: usize) -> io::Result<Vec<u8>> {
        if self.u32()? as usize != len {
            return Err(bad("distance field of a different size"));
        }
        let mut v = Vec::with_capacity(len);
        while v.len() < len {
            let c = self.byte()?;
            if c >= 128 {
                let b = self.byte()?;
                v.resize(v.len() + (c as usize - 128) + 3, b);
            } else {
                v.extend_from_slice(self.take(c as usize + 1)?);
            }
        }
        if v.len() != len {
            return Err(bad("distance field longer than the tile"));
        }
        Ok(v)
    }
    fn mesh(&mut self) -> io::Result<Mesh> {
        let (nv, nt) = (self.u32()? as usize, self.u32()? as usize);
        // 壊れたファイルで巨大な確保をしない（頂点 1 つで 10 バイト以上、三角形 1 つで 13 バイト）
        if nv.saturating_mul(10).saturating_add(nt.saturating_mul(13)) > self.b.len() - self.at {
            return Err(bad("truncated .krbt file"));
        }
        let nh = self.u32()? as usize;
        if nh > 256 {
            return Err(bad("too many heights"));
        }
        let heights: Vec<f32> = self.u32s(nh)?.into_iter().map(f32::from_bits).collect();
        let mut m = Mesh { positions: Vec::with_capacity(nv), normals: Vec::with_capacity(nv), ..Default::default() };
        for _ in 0..nv {
            let (x, z) = (self.f32()?, self.f32()?);
            let y = *heights.get(self.byte()? as usize).ok_or_else(|| bad("height out of range"))?;
            m.positions.push(Vec3::new(x, y, z));
        }
        for _ in 0..nv {
            let n = match self.byte()? {
                0 => Vec3::new(0.0, 1.0, 0.0),
                1 => Vec3::new(0.0, -1.0, 0.0),
                2 => {
                    let x = self.f32()?;
                    Vec3::new(x, 0.0, self.f32()?)
                }
                3 => Vec3::new(self.f32()?, self.f32()?, self.f32()?),
                _ => return Err(bad("bad normal")),
            };
            m.normals.push(n);
        }
        m.indices = self.u32s(nt * 3)?;
        if m.indices.iter().any(|&i| i as usize >= nv) {
            return Err(bad("vertex index out of range"));
        }
        m.tri_material = self.take(nt)?.iter().map(|&t| t as u32).collect();
        Ok(m)
    }
}

/// 読む。タイルの数が count と違う、版が違う、壊れている、のどれでも Err（作り直せばよい）
pub fn read_file(file: &Path, count: usize) -> io::Result<[Vec<CachedTile>; 2]> {
    let mut bytes = Vec::new();
    std::fs::File::open(file)?.read_to_end(&mut bytes)?;
    let mut r = Reader { b: &bytes, at: 0 };
    if r.take(4)? != MAGIC || r.u32()? != VERSION {
        return Err(bad("not a .krbt file of this version"));
    }
    if r.u32()? as usize != count {
        return Err(bad("the number of tiles differs"));
    }
    let offsets: Vec<usize> = (0..2 * count).map(|_| r.u64().map(|o| o as usize)).collect::<io::Result<_>>()?;
    let tile = |at: usize| -> io::Result<CachedTile> {
        let mut r = Reader { b: &bytes, at };
        let key = r.u64()?;
        let mesh = r.mesh()?;
        let (x0, y0, w, h) = (r.u32()? as usize, r.u32()? as usize, r.u32()? as usize, r.u32()? as usize);
        let n = w.checked_mul(h).ok_or_else(|| bad("bad size"))?;
        let copper = r.unpack_bits(n)?;
        let mask_open = r.unpack_bits(n)?;
        Ok(CachedTile { key, mesh, sdf: SdfPatch { x0, y0, w, h, copper, mask_open } })
    };
    // タイルごとに並列に解く
    let threads = krbr_core::platform::worker_threads().min(offsets.len()).max(1);
    let per = offsets.len().div_ceil(threads).max(1);
    let decoded: Vec<io::Result<Vec<CachedTile>>> = std::thread::scope(|sc| {
        let hs: Vec<_> = offsets.chunks(per).map(|c| sc.spawn(move || c.iter().map(|&at| tile(at)).collect::<io::Result<Vec<_>>>())).collect();
        hs.into_iter().map(|h| h.join().unwrap_or_else(|_| Err(bad("decoder thread panicked")))).collect()
    });
    let mut all = Vec::with_capacity(2 * count);
    for d in decoded {
        all.extend(d?);
    }
    let back = all.split_off(count);
    Ok([all, back])
}

/// フォルダの .krbt を新しいものから keep 個だけ残す（基板ごとに数十 MB になるので）
pub fn prune(dir: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "krbt"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    files.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_bits_round_trip() {
        let mut v = vec![0u8; 300];
        v.extend((0..200).map(|i| (i % 7) as u8));
        v.extend([9, 9, 1, 1, 1, 2]);
        v.extend(vec![255u8; 194]);
        for data in [v.clone(), Vec::new(), vec![5], vec![1, 2], vec![3; 3], vec![3; 131], (0..=255).collect()] {
            let mut out = Vec::new();
            pack_bits(&data, &mut out);
            let mut r = Reader { b: &out, at: 0 };
            assert_eq!(r.unpack_bits(data.len()).unwrap(), data);
            assert_eq!(r.at, out.len());
        }
    }

    #[test]
    fn round_trip() {
        let mut m = Mesh::default();
        m.positions = vec![Vec3::new(1.0, 2.0, 3.0), Vec3::new(-0.5, 2.0, 7.0), Vec3::new(0.25, -0.0, 1.0)];
        m.normals = vec![Vec3::Y, Vec3::new(0.6, 0.0, -0.8), Vec3::new(0.1, -0.2, 0.3)];
        m.indices = vec![0, 1, 2];
        m.tri_material = vec![4];
        let mut copper = vec![0u8; 300];
        copper.extend((0..200).map(|i| (i % 7) as u8));
        copper.extend(vec![255u8; 400]);
        let p = SdfPatch { x0: 3, y0: 4, w: 30, h: 30, copper, mask_open: vec![255; 900] };
        let empty = (Mesh::default(), SdfPatch::default());
        let bytes = encode(&[vec![(7, &m, &p), (8, &empty.0, &empty.1)], vec![(9, &empty.0, &empty.1), (10, &m, &p)]]).unwrap();
        let dir = std::env::temp_dir().join(format!("krbr_tile_file_{}", std::process::id()));
        let f = dir.join("a.krbt");
        write_file(&f, &bytes).unwrap();
        let t = read_file(&f, 2).unwrap();
        assert!(read_file(&f, 3).is_err());
        std::fs::write(&f, &bytes[..bytes.len() - 3]).unwrap();
        assert!(read_file(&f, 2).is_err());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!((t[0][0].key, t[0][1].key, t[1][0].key, t[1][1].key), (7, 8, 9, 10));
        let (a, b) = (&t[1][1].mesh, &t[1][1].sdf);
        let bits = |v: &[Vec3]| v.iter().map(|q| q.to_array().map(f32::to_bits)).collect::<Vec<_>>();
        assert_eq!(bits(&a.positions), bits(&m.positions));
        assert_eq!(bits(&a.normals), bits(&m.normals));
        assert_eq!((a.indices.clone(), a.tri_material.clone()), (m.indices.clone(), m.tri_material.clone()));
        assert_eq!((b.x0, b.y0, b.w, b.h, b.copper.clone(), b.mask_open.clone()), (3, 4, 30, 30, p.copper.clone(), p.mask_open.clone()));
        // 材質の番号が 255 を超えると書かない
        m.tri_material = vec![300];
        assert!(encode(&[vec![(1, &m, &p)], vec![(2, &m, &p)]]).is_err());
    }
}
