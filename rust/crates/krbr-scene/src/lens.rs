//! カメラのレンズ：pinhole / thin（薄レンズ）/ realistic（実際のレンズ設計データで光線を追跡）

use std::f64::consts::PI;
use std::path::Path;

use glam::DVec3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u32)]
pub enum LensModel {
    #[default]
    Pinhole = 0,
    Thin = 1,
    Realistic = 2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LensParams {
    pub model: LensModel,
    pub focal_length_mm: f32,
    pub f_number: f32,
    /// 0 なら注視点までの距離
    pub focus_distance_mm: f32,
    /// 絞りの羽根の枚数（0 か 3 未満で円）
    pub blades: i32,
    pub blade_rotation_deg: f32,
    /// 0：多角形、1：円
    pub blade_roundness: f32,
    /// cos⁴θ
    pub natural_vignetting: bool,
    /// 口径食の強さ
    pub optical_vignetting: f32,
    /// 軸上色収差：レンズのアッベ数（0 で無効。分光モードのみ）
    pub abbe_number: f32,
    /// 倍率色収差：F 線と C 線の像の大きさの差（割合）
    pub lateral_ca: f32,
    /// realistic：レンズデータ（空なら内蔵の double Gauss 50mm f/2）
    pub lens_file: String,
}

impl Default for LensParams {
    fn default() -> Self {
        Self {
            model: LensModel::Pinhole,
            focal_length_mm: 50.0,
            f_number: 2.8,
            focus_distance_mm: 0.0,
            blades: 0,
            blade_rotation_deg: 0.0,
            blade_roundness: 0.0,
            natural_vignetting: false,
            optical_vignetting: 0.0,
            abbe_number: 0.0,
            lateral_ca: 0.0,
            lens_file: String::new(),
        }
    }
}

/// 実レンズの面（物体側から順に）。pbrt と同じ書式：曲率半径, 次の面までの距離, 屈折率（次の面までの媒質）, 有効径
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LensSurface {
    /// 0 なら絞り（平面）
    pub radius: f32,
    pub thickness: f32,
    pub ior: f32,
    /// 直径（mm）
    pub aperture: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LensSystem {
    pub surfaces: Vec<LensSurface>,
    pub name: String,
}

impl LensSystem {
    /// D-GAUSS F/2 22deg HFOV（US patent 2,673,491 Tronnier、50 mm に縮小。pbrt の dgauss.50mm.dat）
    pub fn double_gauss50() -> Self {
        let s = |radius, thickness, ior, aperture| LensSurface { radius, thickness, ior, aperture };
        Self {
            name: "double Gauss 50mm f/2".into(),
            surfaces: vec![
                s(29.475, 3.76, 1.67, 25.2),
                s(84.83, 0.12, 1.0, 25.2),
                s(19.275, 4.025, 1.67, 23.0),
                s(40.77, 3.275, 1.699, 23.0),
                s(12.75, 5.705, 1.0, 18.0),
                s(0.0, 4.5, 1.0, 17.1),
                s(-14.495, 1.18, 1.603, 17.0),
                s(40.77, 6.065, 1.658, 20.0),
                s(-20.385, 0.19, 1.0, 20.0),
                s(437.065, 3.22, 1.717, 20.0),
                s(-39.73, 5.0, 1.0, 20.0),
            ],
        }
    }

    pub fn load(p: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(p).map_err(|_| format!("cannot open {}", p.display()))?;
        let mut out = Self { surfaces: Vec::new(), name: p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default() };
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("");
            let v: Vec<f32> = line.split_whitespace().filter_map(|t| t.parse().ok()).collect();
            if v.len() < 4 {
                continue;
            }
            out.surfaces.push(LensSurface { radius: v[0], thickness: v[1], ior: if v[2] == 0.0 { 1.0 } else { v[2] }, aperture: v[3] });
        }
        if out.surfaces.len() < 2 {
            return Err(format!("no lens surfaces in {}", p.display()));
        }
        Ok(out)
    }

    /// 物体側の頂点を z = 0、フィルム側を負とした各面の頂点の z
    pub fn vertex_z(&self) -> Vec<f32> {
        let mut acc = 0.0f32;
        self.surfaces
            .iter()
            .map(|s| {
                let z = acc;
                acc -= s.thickness;
                z
            })
            .collect()
    }

    /// 光線の追跡（レンズの座標：x 右、y 上、z が物体側）。物体側から入れる場合は from_scene = true。通らなければ false
    pub fn trace(&self, o: &mut DVec3, d: &mut DVec3, from_scene: bool) -> bool {
        let zs = self.vertex_z();
        let n = self.surfaces.len();
        for k in 0..n {
            let i = if from_scene { k } else { n - 1 - k };
            let s = &self.surfaces[i];
            let zv = zs[i] as f64;
            let ap2 = 0.25 * (s.aperture as f64) * (s.aperture as f64);
            if s.radius == 0.0 {
                if d.z.abs() < 1e-12 {
                    return false;
                }
                let p = *o + *d * ((zv - o.z) / d.z);
                if p.x * p.x + p.y * p.y > ap2 {
                    return false;
                }
                *o = p;
                continue;
            }
            let c = DVec3::new(0.0, 0.0, zv - s.radius as f64);
            let oc = *o - c;
            let b = oc.dot(*d);
            let cc = oc.dot(oc) - (s.radius as f64) * (s.radius as f64);
            let disc = b * b - cc;
            if disc < 0.0 {
                return false;
            }
            let sq = disc.sqrt();
            let mut t = -1.0f64;
            for tc in [-b - sq, -b + sq] {
                if tc <= 1e-9 {
                    continue;
                }
                if t < 0.0 || ((*o + *d * tc).z - zv).abs() < ((*o + *d * t).z - zv).abs() {
                    t = tc;
                }
            }
            if t < 0.0 {
                return false;
            }
            let p = *o + *d * t;
            if p.x * p.x + p.y * p.y > ap2 {
                return false;
            }
            let mut nrm = (p - c).normalize();
            if nrm.dot(*d) > 0.0 {
                nrm = -nrm;
            }
            let n_film = s.ior as f64;
            let n_scene = if i > 0 { self.surfaces[i - 1].ior as f64 } else { 1.0 };
            let eta = if from_scene { n_scene / n_film } else { n_film / n_scene };
            let cosi = -nrm.dot(*d);
            let k2 = 1.0 - eta * eta * (1.0 - cosi * cosi);
            if k2 < 0.0 {
                return false; // 全反射
            }
            *d = (eta * *d + (eta * cosi - k2.sqrt()) * nrm).normalize();
            *o = p;
        }
        true
    }

    /// 物体側の距離 focus_mm（前の頂点から）にピントを合わせるときの、最後の面からフィルムまでの距離
    pub fn film_distance_for(&self, focus_mm: f32) -> f32 {
        let h = 0.05 * 0.5 * self.surfaces[0].aperture as f64;
        let (mut o, mut d) = if focus_mm <= 0.0 || focus_mm > 1e6 {
            (DVec3::new(h, 0.0, 1.0), DVec3::NEG_Z)
        } else {
            let o = DVec3::new(0.0, 0.0, focus_mm as f64);
            (o, (DVec3::new(h, 0.0, 0.0) - o).normalize())
        };
        if !self.trace(&mut o, &mut d, true) || d.x.abs() < 1e-12 {
            return self.surfaces.last().unwrap().thickness;
        }
        let t = -o.x / d.x;
        let z_image = o.z + d.z * t;
        let z_last = *self.vertex_z().last().unwrap() as f64;
        (z_last - z_image).max(0.1) as f32
    }

    pub fn effective_focal_length(&self) -> f32 {
        let h = 0.05 * 0.5 * self.surfaces[0].aperture as f64;
        let (mut o, mut d) = (DVec3::new(h, 0.0, 1.0), DVec3::NEG_Z);
        if !self.trace(&mut o, &mut d, true) || d.x.abs() < 1e-12 {
            return 50.0;
        }
        let z_principal = o.z + d.z * ((h - o.x) / d.x);
        let z_focus = o.z + d.z * (-o.x / d.x);
        (z_principal - z_focus).abs() as f32
    }

    /// 設計上の F 値（焦点距離 / 入射瞳の直径）。入射瞳は、物体側から軸に平行に入れた光線が絞りで通る高さから求める
    pub fn nominal_f_number(&self) -> f32 {
        let Some(stop) = self.surfaces.iter().rposition(|s| s.radius == 0.0) else { return 0.0 };
        let h = 0.05 * 0.5 * self.surfaces[0].aperture as f64;
        let (mut o, mut d) = (DVec3::new(h, 0.0, 1.0), DVec3::NEG_Z);
        let zs = self.vertex_z();
        // 絞りの手前まで追跡する（絞りを含めない部分系）
        let front = LensSystem { surfaces: self.surfaces[..stop].to_vec(), name: String::new() };
        if !front.surfaces.is_empty() && !front.trace(&mut o, &mut d, true) {
            return 0.0;
        }
        if d.z.abs() < 1e-12 {
            return 0.0;
        }
        let hs = (o + d * ((zs[stop] as f64 - o.z) / d.z)).x.abs();
        if hs < 1e-9 {
            return 0.0;
        }
        let pupil_radius = 0.5 * self.surfaces[stop].aperture as f64 * h / hs;
        (self.effective_focal_length() as f64 / (2.0 * pupil_radius)) as f32
    }

    /// 絞りを f_number まで絞ったレンズ（設計値より開くことはできない）。絞りの面（radius 0）の有効径だけを縮める
    pub fn stopped_down(&self, f_number: f32) -> LensSystem {
        let mut s = self.clone();
        let nominal = self.nominal_f_number();
        if nominal <= 0.0 || f_number <= nominal {
            return s;
        }
        let scale = nominal / f_number;
        for surf in &mut s.surfaces {
            if surf.radius == 0.0 {
                surf.aperture *= scale;
            }
        }
        // 後ろの開口（フィルムから光線を出す円）は縮めない。縮めると画面の端の光線が絞りを通れなくなり、
        // 周辺が黒く欠ける（射出瞳は後ろの面には無い）。絞ると通らない光線が増えてノイズは増えるが、収束はする
        s
    }

    /// フィルムの中央での明るさ（cos⁴θ · A / z² の、光が通る割合を含めた平均）
    pub fn center_weight(&self, film_distance: f32) -> f32 {
        let zs = self.vertex_z();
        let z_rear = *zs.last().unwrap() as f64;
        let z_film = z_rear - film_distance as f64;
        let r = 0.5 * self.surfaces.last().unwrap().aperture as f64;
        let n = 64;
        let mut sum = 0.0;
        for j in 0..n {
            for i in 0..n {
                let (u, v) = ((i as f64 + 0.5) / n as f64, (j as f64 + 0.5) / n as f64);
                let (rr, ph) = (r * u.sqrt(), 2.0 * PI * v);
                let pr = DVec3::new(rr * ph.cos(), rr * ph.sin(), z_rear);
                let mut o = DVec3::new(0.0, 0.0, z_film);
                let mut d = (pr - o).normalize();
                let c = d.z;
                if !self.trace(&mut o, &mut d, false) {
                    continue;
                }
                sum += c * c * c * c;
            }
        }
        let area = PI * r * r;
        let z = z_rear - z_film;
        (sum / (n * n) as f64 * area / (z * z)) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn double_gauss_focal_length() {
        let s = LensSystem::double_gauss50();
        let f = s.effective_focal_length();
        assert!((f - 50.0).abs() < 2.0, "{f}");
        let fd = s.film_distance_for(0.0);
        assert!(fd > 20.0 && fd < 60.0, "{fd}");
        assert!(s.center_weight(fd) > 0.0);
        // double Gauss 50mm f/2：設計上の F 値は 2 前後
        let n = s.nominal_f_number();
        assert!(n > 1.6 && n < 2.4, "{n}");
        // f/8 に絞ると絞りの径は 1/4
        let stopped = s.stopped_down(8.0);
        let stop = s.surfaces.iter().position(|x| x.radius == 0.0).unwrap();
        assert!((stopped.surfaces[stop].aperture / s.surfaces[stop].aperture - n / 8.0).abs() < 1e-5);
        assert_eq!(s.stopped_down(1.0).surfaces[stop].aperture, s.surfaces[stop].aperture);
    }
}
