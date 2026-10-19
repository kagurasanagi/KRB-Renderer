//! 分光レンダリングの土台：CIE 1931 の等色関数（解析的な近似）、色空間の変換行列、
//! RGB からスペクトルへの変換表（Jakob & Hanika 2019）。白色点は等エネルギー白色（E）とし、
//! 表示用の sRGB（D65）へは Bradford で変換する。表のキャッシュは C++ 版と同じファイルを共有する。

use std::path::Path;
use std::sync::OnceLock;
use std::time::Instant;

use glam::{DMat3, DVec3, Mat3, Vec3, Vec4};

/// 波長の範囲（nm）
pub const LAMBDA_MIN: f32 = 360.0;
pub const LAMBDA_MAX: f32 = 830.0;

/// 区分的ガウス関数：λ < μ では σ1、それ以外では σ2
fn g(l: f32, mu: f32, s1: f32, s2: f32) -> f32 {
    let t = (l - mu) / if l < mu { s1 } else { s2 };
    (-0.5 * t * t).exp()
}

/// CIE 1931 2° 等色関数の解析的な近似（Wyman, Sloan & Shirley 2013）
pub fn cmf(l: f32) -> Vec3 {
    let x = 1.056 * g(l, 599.8, 37.9, 31.0) + 0.362 * g(l, 442.0, 16.0, 26.7) - 0.065 * g(l, 501.1, 20.4, 26.2);
    let y = 0.821 * g(l, 568.8, 46.9, 40.5) + 0.286 * g(l, 530.9, 16.3, 31.1);
    let z = 1.217 * g(l, 437.0, 11.8, 36.0) + 0.681 * g(l, 459.0, 26.0, 13.8);
    Vec3::new(x, y, z)
}

/// ȳ の積分（1 nm 刻み）。放射輝度 1 の平坦なスペクトルが Y = 1 になるよう割る数
pub fn cmf_y_integral() -> f32 {
    static V: OnceLock<f32> = OnceLock::new();
    *V.get_or_init(|| {
        let mut s = 0.0f64;
        let mut l = LAMBDA_MIN;
        while l < LAMBDA_MAX {
            s += cmf(l + 0.5).y as f64;
            l += 1.0;
        }
        s as f32
    })
}

/// スペクトル（関数）を XYZ に積分する
pub fn integrate_xyz(spectrum: impl Fn(f32) -> f32) -> Vec3 {
    let mut xyz = Vec3::ZERO;
    let mut l = LAMBDA_MIN;
    while l < LAMBDA_MAX {
        xyz += spectrum(l + 0.5) * cmf(l + 0.5);
        l += 1.0;
    }
    xyz / cmf_y_integral()
}

/// 平坦なスペクトル（E）の XYZ（Y = 1 に正規化）
pub fn white_e() -> Vec3 {
    static W: OnceLock<Vec3> = OnceLock::new();
    *W.get_or_init(|| integrate_xyz(|_| 1.0))
}

/// XYZ（白色点 E）→ リニア sRGB（D65）
pub fn xyz_to_linear_srgb() -> Mat3 {
    static M: OnceLock<Mat3> = OnceLock::new();
    *M.get_or_init(|| {
        // 行で書いた行列（glam は列優先なので転置して作る）
        let xyz_d65_to_srgb = Mat3::from_cols_array(&[3.2404542, -1.5371385, -0.4985314, -0.9692660, 1.8760108, 0.0415560, 0.0556434, -0.2040259, 1.0572252]).transpose();
        let bradford = Mat3::from_cols_array(&[0.8951, 0.2664, -0.1614, -0.7502, 1.7135, 0.0367, 0.0389, -0.0685, 1.0296]).transpose();
        let src_w = bradford * white_e();
        let dst_w = bradford * Vec3::new(0.95047, 1.0, 1.08883);
        let scale = Mat3::from_diagonal(Vec3::new(dst_w.x / src_w.x, dst_w.y / src_w.y, dst_w.z / src_w.z));
        let adapt = bradford.inverse() * scale * bradford;
        xyz_d65_to_srgb * adapt
    })
}

pub fn linear_srgb_to_xyz() -> Mat3 {
    static M: OnceLock<Mat3> = OnceLock::new();
    *M.get_or_init(|| xyz_to_linear_srgb().inverse())
}

/// シグモイド多項式 s(λ) = 1/2 + z / (2 √(1+z²))、z = c0 x² + c1 x + c2、x = (λ − min) / (max − min)
pub fn sigmoid_poly(c: Vec3, lambda_nm: f32) -> f32 {
    let x = (lambda_nm - LAMBDA_MIN) / (LAMBDA_MAX - LAMBDA_MIN);
    let z = (c.x * x + c.y) * x + c.z;
    0.5 + z / (2.0 * (1.0 + z * z).sqrt())
}

/// RGB（リニア sRGB、0..1）→ シグモイド多項式の係数の表
pub struct Rgb2SpecTable {
    /// z（最大の成分）の非一様な格子
    pub scale: [f32; Self::RES],
    /// data[(((l * RES + zi) * RES + yi) * RES + xi) * 3 + k]
    pub data: Vec<f32>,
}

impl Rgb2SpecTable {
    pub const RES: usize = 64;

    /// 表を引いて係数を返す（三重線形補間）
    pub fn lookup(&self, rgb: Vec3) -> Vec3 {
        const N: usize = Rgb2SpecTable::RES;
        let rgb = rgb.clamp(Vec3::ZERO, Vec3::ONE);
        // 灰色は解析的に（定数のスペクトル）
        if rgb.x == rgb.y && rgb.y == rgb.z {
            let v = rgb.x.clamp(1e-7, 1.0 - 1e-7);
            return Vec3::new(0.0, 0.0, (v - 0.5) / (v * (1.0 - v)).sqrt());
        }
        let c = rgb.to_array();
        let mut l = 0;
        if c[1] > c[l] {
            l = 1;
        }
        if c[2] > c[l] {
            l = 2;
        }
        let z = c[l];
        if z <= 0.0 {
            return Vec3::new(0.0, 0.0, -50.0); // 黒
        }
        let x = c[(l + 1) % 3] / z * (N - 1) as f32;
        let y = c[(l + 2) % 3] / z * (N - 1) as f32;
        let zi = (self.scale.partition_point(|&s| s <= z) as isize - 1).clamp(0, N as isize - 2) as usize;
        let zf = (z - self.scale[zi]) / (self.scale[zi + 1] - self.scale[zi]);
        let xi = (x as usize).min(N - 2);
        let yi = (y as usize).min(N - 2);
        let (xf, yf) = (x - xi as f32, y - yi as f32);
        let mut out = Vec3::ZERO;
        for dz in 0..2 {
            for dy in 0..2 {
                for dx in 0..2 {
                    let w = if dz == 1 { zf } else { 1.0 - zf } * if dy == 1 { yf } else { 1.0 - yf } * if dx == 1 { xf } else { 1.0 - xf };
                    let i = ((l * N + zi + dz) * N * N + (yi + dy) * N + xi + dx) * 3;
                    out += w * Vec3::new(self.data[i], self.data[i + 1], self.data[i + 2]);
                }
            }
        }
        out
    }

    pub fn eval(&self, rgb: Vec3, lambda_nm: f32) -> f32 {
        sigmoid_poly(self.lookup(rgb), lambda_nm)
    }

    /// 上限のない値（発光・環境光）：xyz = 係数、w = 倍率（pbrt の RGBUnboundedSpectrum）
    pub fn unbounded(&self, rgb: Vec3) -> Vec4 {
        let m = rgb.x.max(rgb.y).max(rgb.z);
        if m <= 0.0 {
            return Vec4::ZERO;
        }
        let c = self.lookup(rgb / (2.0 * m));
        Vec4::new(c.x, c.y, c.z, 2.0 * m)
    }
}

// ---------------------------------------------------------------- 係数の表（Jakob & Hanika 2019）

fn xyz_to_lab(xyz: DVec3) -> DVec3 {
    let w = white_e().as_dvec3();
    let f = |t: f64| {
        let d = 6.0 / 29.0f64;
        if t > d * d * d {
            t.cbrt()
        } else {
            t / (3.0 * d * d) + 4.0 / 29.0
        }
    };
    let (fx, fy, fz) = (f(xyz.x / w.x), f(xyz.y / w.y), f(xyz.z / w.z));
    DVec3::new(116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}

struct Fitter {
    lambda_x: Vec<f64>,
    cmf_w: Vec<DVec3>,
}

impl Fitter {
    fn new() -> Self {
        let step = 5.0f64;
        let norm = 1.0 / cmf_y_integral() as f64;
        let (mut lambda_x, mut cmf_w) = (Vec::new(), Vec::new());
        let mut l = LAMBDA_MIN as f64 + step / 2.0;
        while l < LAMBDA_MAX as f64 {
            lambda_x.push((l - LAMBDA_MIN as f64) / (LAMBDA_MAX - LAMBDA_MIN) as f64);
            cmf_w.push(cmf(l as f32).as_dvec3() * step * norm);
            l += step;
        }
        Self { lambda_x, cmf_w }
    }
    fn xyz_of(&self, c: DVec3) -> DVec3 {
        let mut xyz = DVec3::ZERO;
        for (x, w) in self.lambda_x.iter().zip(&self.cmf_w) {
            let z = (c.x * x + c.y) * x + c.z;
            let s = 0.5 + z / (2.0 * (1.0 + z * z).sqrt());
            xyz += s * *w;
        }
        xyz
    }
    fn residual(&self, c: DVec3, target_lab: DVec3) -> DVec3 {
        xyz_to_lab(self.xyz_of(c)) - target_lab
    }
    /// Gauss–Newton（数値微分のヤコビアン）。残差が減るまで歩幅を半分にする
    fn fit(&self, c: &mut DVec3, rgb: DVec3) {
        let target_lab = xyz_to_lab(DMat3::from(linear_srgb_to_xyz().as_dmat3()) * rgb);
        let mut r = self.residual(*c, target_lab);
        for _ in 0..40 {
            if r.length() < 1e-4 {
                break;
            }
            let mut cols = [DVec3::ZERO; 3];
            for k in 0..3 {
                let (mut a, mut b) = (*c, *c);
                a[k] -= 1e-4;
                b[k] += 1e-4;
                cols[k] = (self.residual(b, target_lab) - self.residual(a, target_lab)) / 2e-4;
            }
            let j = DMat3::from_cols(cols[0], cols[1], cols[2]);
            if j.determinant().abs() < 1e-15 {
                break;
            }
            let delta = j.inverse() * r;
            let r0 = r.length();
            let mut improved = false;
            let mut step = 1.0;
            while step > 1e-3 {
                let mut cn = *c - step * delta;
                let m = cn.x.abs().max(cn.y.abs()).max(cn.z.abs());
                if m > 400.0 {
                    cn *= 400.0 / m;
                }
                let rn = self.residual(cn, target_lab);
                if rn.length() < r0 {
                    *c = cn;
                    r = rn;
                    improved = true;
                    break;
                }
                step *= 0.5;
            }
            if !improved {
                break;
            }
        }
    }
}

fn smoothstep(x: f64) -> f64 {
    x * x * (3.0 - 2.0 * x)
}

pub fn build_rgb2spec_table() -> Rgb2SpecTable {
    const N: usize = Rgb2SpecTable::RES;
    let mut scale = [0.0f32; N];
    for (i, s) in scale.iter_mut().enumerate() {
        *s = smoothstep(smoothstep(i as f64 / (N - 1) as f64)) as f32;
    }
    let data = std::sync::Mutex::new(vec![0.0f32; 3 * N * N * N * 3]);
    let fitter = Fitter::new();
    let next = std::sync::atomic::AtomicUsize::new(0);
    let worker = || {
        let mut local: Vec<(usize, [f32; 3])> = Vec::new();
        loop {
            let job = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if job >= 3 * N * N {
                break;
            }
            let (l, yi, xi) = (job / (N * N), (job / N) % N, job % N);
            let mut solved = vec![[0.0f32; 3]; N];
            let solve_range = |solved: &mut Vec<[f32; 3]>, from: isize, to: isize, dir: isize, mut c: DVec3| {
                let mut zi = from;
                while zi != to {
                    let z = scale[zi as usize] as f64;
                    let mut rgb = DVec3::ZERO;
                    rgb[l] = z;
                    rgb[(l + 1) % 3] = xi as f64 / (N - 1) as f64 * z;
                    rgb[(l + 2) % 3] = yi as f64 / (N - 1) as f64 * z;
                    fitter.fit(&mut c, rgb);
                    solved[zi as usize] = [c.x as f32, c.y as f32, c.z as f32];
                    zi += dir;
                }
            };
            let start = (N / 5) as isize;
            solve_range(&mut solved, start, N as isize, 1, DVec3::ZERO);
            let cs = solved[start as usize];
            solve_range(&mut solved, start - 1, -1, -1, DVec3::new(cs[0] as f64, cs[1] as f64, cs[2] as f64));
            for (zi, c) in solved.iter().enumerate() {
                local.push((((l * N + zi) * N * N + yi * N + xi) * 3, *c));
            }
        }
        let mut d = data.lock().unwrap();
        for (i, c) in local {
            d[i..i + 3].copy_from_slice(&c);
        }
    };
    let n = (std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2) / 2).clamp(1, 12);
    std::thread::scope(|s| {
        for _ in 0..n {
            s.spawn(&worker);
        }
    });
    Rgb2SpecTable { scale, data: data.into_inner().unwrap() }
}

static TABLE: OnceLock<Rgb2SpecTable> = OnceLock::new();

/// すでに読み込んだ表（まだなら None）
pub fn loaded_rgb2spec_table() -> Option<&'static Rgb2SpecTable> {
    TABLE.get()
}

/// 表を作る（数秒）。cache_file があれば読み、無ければ作って書く
pub fn rgb2spec_table_with_cache(cache_file: &Path) -> &'static Rgb2SpecTable {
    TABLE.get_or_init(|| {
        const N: usize = Rgb2SpecTable::RES;
        let count = 3 * N * N * N * 3;
        if let Ok(bytes) = std::fs::read(cache_file) {
            if bytes.len() == (N + count) * 4 {
                let f: Vec<f32> = bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
                let mut scale = [0.0f32; N];
                scale.copy_from_slice(&f[..N]);
                krbr_core::info!("spectral: RGB -> spectrum table from cache");
                return Rgb2SpecTable { scale, data: f[N..].to_vec() };
            }
        }
        let t0 = Instant::now();
        let t = build_rgb2spec_table();
        krbr_core::info!("spectral: built RGB -> spectrum table ({}^3 x 3) in {:.1} s", N, t0.elapsed().as_secs_f64());
        if let Some(dir) = cache_file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let mut bytes = Vec::with_capacity((N + count) * 4);
        for v in t.scale.iter().chain(t.data.iter()) {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        let _ = std::fs::write(cache_file, bytes);
        t
    })
}

/// キャッシュの既定の場所（<cacheDir>/spectral/rgb2spec_v1.bin。C++ 版と共有）
pub fn rgb2spec_table() -> &'static Rgb2SpecTable {
    rgb2spec_table_with_cache(&krbr_core::platform::cache_dir().join("spectral").join("rgb2spec_v1.bin"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn white_point_and_matrices() {
        let w = white_e();
        assert!((w.y - 1.0).abs() < 1e-4);
        // E の白は sRGB の白（1,1,1）へ
        let rgb = xyz_to_linear_srgb() * w;
        assert!((rgb - Vec3::ONE).abs().max_element() < 2e-3, "{rgb}");
        let back = linear_srgb_to_xyz() * rgb;
        assert!((back - w).abs().max_element() < 1e-4);
    }

    #[test]
    fn gray_is_constant_spectrum() {
        let t = Rgb2SpecTable { scale: [0.0; 64], data: Vec::new() };
        let c = t.lookup(Vec3::splat(0.5));
        assert!((sigmoid_poly(c, 400.0) - 0.5).abs() < 1e-6 && (sigmoid_poly(c, 700.0) - 0.5).abs() < 1e-6);
    }
}
