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

impl LensModel {
    pub const ALL: [LensModel; 3] = [LensModel::Pinhole, LensModel::Thin, LensModel::Realistic];

    /// 設定ファイル（[lens] model）・コマンドライン（--lens）での名前
    pub fn as_str(self) -> &'static str {
        match self {
            LensModel::Pinhole => "pinhole",
            LensModel::Thin => "thin",
            LensModel::Realistic => "realistic",
        }
    }
}

impl std::str::FromStr for LensModel {
    type Err = String;
    /// 名前（pinhole / thin / realistic）から。知らない名前は Err（打ち間違いを黙って pinhole にしない）
    fn from_str(s: &str) -> Result<Self, String> {
        Self::ALL.into_iter().find(|m| m.as_str() == s).ok_or_else(|| format!("unknown lens model '{s}' (pinhole / thin / realistic)"))
    }
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

/// 近軸光線の高さ（前の面の有効半径に対する割合）。近軸近似が成り立つほど小さく、追跡の丸め誤差に埋もれない程度に大きくする
const PARAXIAL_FRACTION: f64 = 0.05;
/// 焦点距離が求まらないときに返す値（mm）。load で検証するので、届くのは手で組んだ LensSystem だけ
const FALLBACK_FOCAL_LENGTH_MM: f32 = 50.0;

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

    /// pbrt 形式のレンズデータを読む。1 行に 4 つの数（# から後ろはコメント）。数にならない欄や数の個数の違う行は、
    /// 列がずれた面を作らないよう行番号つきのエラーにする。焦点距離が求まらないレンズもエラー
    pub fn load(p: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(p).map_err(|_| format!("cannot open {}", p.display()))?;
        Self::parse(&text, &p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()).map_err(|e| format!("{}: {e}", p.display()))
    }

    /// load の本体（name はファイル名）
    pub fn parse(text: &str, name: &str) -> Result<Self, String> {
        let mut out = Self { surfaces: Vec::new(), name: name.to_string() };
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("");
            if line.trim().is_empty() {
                continue;
            }
            let v: Vec<f32> = line.split_whitespace().map(|t| t.parse().map_err(|_| format!("line {}: '{t}' is not a number", n + 1))).collect::<Result<_, _>>()?;
            if v.len() != 4 {
                return Err(format!("line {}: expected 4 numbers (radius thickness ior aperture), found {}", n + 1, v.len()));
            }
            out.surfaces.push(LensSurface { radius: v[0], thickness: v[1], ior: if v[2] == 0.0 { 1.0 } else { v[2] }, aperture: v[3] });
        }
        if out.surfaces.len() < 2 {
            return Err("no lens surfaces".into());
        }
        if out.try_effective_focal_length().is_none() {
            return Err("a paraxial ray does not pass through the lens (cannot find the focal length)".into());
        }
        Ok(out)
    }

    /// 近軸光線を入れる高さ（mm）
    fn paraxial_height(&self) -> f64 {
        PARAXIAL_FRACTION * 0.5 * self.surfaces[0].aperture as f64
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
    /// 光線が通らない（近すぎてピントが合わない）ときは、最後の面の厚み（設計上のフィルムの位置）を返す
    pub fn film_distance_for(&self, focus_mm: f32) -> f32 {
        let h = self.paraxial_height();
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

    /// 有効焦点距離（mm）。求まらなければ FALLBACK_FOCAL_LENGTH_MM（load で検証するので、届くのは手で組んだレンズだけ）
    pub fn effective_focal_length(&self) -> f32 {
        self.try_effective_focal_length().unwrap_or(FALLBACK_FOCAL_LENGTH_MM)
    }

    /// 有効焦点距離（mm）。軸に平行な近軸光線が通らない・曲がらないなら None
    pub fn try_effective_focal_length(&self) -> Option<f32> {
        let h = self.paraxial_height();
        let (mut o, mut d) = (DVec3::new(h, 0.0, 1.0), DVec3::NEG_Z);
        if !self.trace(&mut o, &mut d, true) || d.x.abs() < 1e-12 {
            return None;
        }
        let z_principal = o.z + d.z * ((h - o.x) / d.x);
        let z_focus = o.z + d.z * (-o.x / d.x);
        Some((z_principal - z_focus).abs() as f32)
    }

    /// 設計上の F 値（焦点距離 / 入射瞳の直径）。入射瞳は、物体側から軸に平行に入れた光線が絞りで通る高さから求める。
    /// 絞りの面（radius 0）が無い・光線が絞りまで届かないなら 0（stopped_down はそのとき絞らない）
    pub fn nominal_f_number(&self) -> f32 {
        let Some(stop) = self.surfaces.iter().rposition(|s| s.radius == 0.0) else { return 0.0 };
        let h = self.paraxial_height();
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

    #[test]
    fn lens_file_parsing() {
        // 内蔵のレンズと同じ値を書いたファイルは同じレンズになる
        let dg = LensSystem::double_gauss50();
        let text: String = std::iter::once("# radius thickness ior aperture

".to_string())
            .chain(dg.surfaces.iter().map(|s| format!("{} {} {} {}   # surface
", s.radius, s.thickness, if s.ior == 1.0 { 0.0 } else { s.ior }, s.aperture)))
            .collect();
        assert_eq!(LensSystem::parse(&text, "x").unwrap().surfaces, dg.surfaces);
        // 数にならない欄・欄の数の違いは行番号つきのエラー（列がずれた面を作らない）
        let e = LensSystem::parse(&text.replacen("3.76", "3,76", 1), "x").unwrap_err();
        assert!(e.contains("line 3"), "{e}");
        let e = LensSystem::parse(&text.replacen(" 3.76", "", 1), "x").unwrap_err();
        assert!(e.contains("line 3") && e.contains("found 3"), "{e}");
        // 光が通らないレンズ（開口が 0）
        assert!(LensSystem::parse("10 1 1.5 0
-10 1 0 0
", "x").is_err());
    }

    #[test]
    fn lens_model_names() {
        for m in LensModel::ALL {
            assert_eq!(m.as_str().parse::<LensModel>(), Ok(m));
        }
        assert!("Thin".parse::<LensModel>().is_err());
        assert!("realistc".parse::<LensModel>().is_err());
    }
}
