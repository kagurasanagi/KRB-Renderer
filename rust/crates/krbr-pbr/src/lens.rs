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
        let text = std::fs::read_to_string(p).map_err(|e| format!("cannot open {}: {e}", p.display()))?;
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
        self.trace_with(&self.vertex_z(), o, d, from_scene)
    }

    /// trace と同じ（面の頂点の z を先に求めておく。たくさん追跡するとき）
    pub fn trace_with(&self, zs: &[f32], o: &mut DVec3, d: &mut DVec3, from_scene: bool) -> bool {
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

    /// フィルムの高さ y（mm、フィルムの中心から）の点から出て、絞りの中心を通る光線（主光線）が、物体側で軸となす角の正接。
    /// 主光線の向きだけを見るので、開口（絞りを含む）は無視する。絞りの面が無い・追跡できないなら None
    pub fn chief_ray_tan(&self, film_distance: f32, y: f64) -> Option<f64> {
        let (_, d) = self.chief_ray(film_distance, y)?;
        (d.z.abs() >= 1e-12).then(|| (d.x / d.z).abs())
    }

    /// フィルムの高さ y（mm、+x の上）の点から出る主光線の、前の面を出たところの点と向き（レンズの座標）。
    /// 開口は無視する。絞りの面が無い・追跡できないなら None
    pub fn chief_ray(&self, film_distance: f32, y: f64) -> Option<(DVec3, DVec3)> {
        let stop = self.surfaces.iter().rposition(|s| s.radius == 0.0)?;
        let mut open = self.clone();
        for s in &mut open.surfaces {
            s.aperture *= 1000.0;
        }
        let zs = open.vertex_z();
        let (z_rear, z_stop) = (*zs.last().unwrap() as f64, zs[stop] as f64);
        let z_film = z_rear - film_distance as f64;
        // フィルムから絞りまでの部分（座標は絞りの面を 0 にした部分系のもの）
        let rear_part = LensSystem { surfaces: open.surfaces[stop..].to_vec(), name: String::new() };
        let start = |a: f64| {
            let o = DVec3::new(y, 0.0, z_film);
            (o, (DVec3::new(a, 0.0, z_rear) - o).normalize())
        };
        let x_at_stop = |a: f64| -> Option<f64> {
            let (o, d) = start(a);
            let (mut o, mut d) = (o - DVec3::new(0.0, 0.0, z_stop), d);
            rear_part.trace(&mut o, &mut d, false).then_some(o.x)
        };
        // 後ろの面のどこへ向ければ絞りの中心を通るか（割線法）
        let (mut a0, mut a1) = (0.0f64, -0.5 * y);
        let (mut f0, mut f1) = (x_at_stop(a0)?, x_at_stop(a1)?);
        for _ in 0..40 {
            if f1.abs() < 1e-9 || (f1 - f0).abs() < 1e-15 {
                break;
            }
            let a2 = a1 - f1 * (a1 - a0) / (f1 - f0);
            (a0, f0) = (a1, f1);
            a1 = a2;
            f1 = x_at_stop(a1)?;
        }
        if f1.abs() > 1e-6 {
            return None;
        }
        let (mut o, mut d) = start(a1);
        open.trace(&mut o, &mut d, false).then_some((o, d))
    }

    /// 主光線が物体側で軸となす角の正接が tan_half になるフィルムの高さ（mm）。ピントを変えても（フィルムの位置が変わっても）
    /// 画角が変わらないように、フィルムの大きさをこれで決める。端の主光線が追跡できない（画角が広い・ピントが近くてフィルムが
    /// 大きい）ときは、中心の近くの主光線の傾きから比例で求める（近軸。歪曲は無視する）。それも求まらなければ None
    pub fn film_height_for_angle(&self, film_distance: f32, tan_half: f64) -> Option<f32> {
        if tan_half <= 0.0 {
            return None;
        }
        let near = 0.5f64;
        let paraxial = self.chief_ray_tan(film_distance, near).filter(|t| *t > 0.0).map(|t| (tan_half * near / t) as f32);
        self.film_height_exact(film_distance, tan_half).or(paraxial)
    }

    fn film_height_exact(&self, film_distance: f32, tan_half: f64) -> Option<f32> {
        let g = |y: f64| self.chief_ray_tan(film_distance, y).map(|t| t - tan_half);
        let (mut y0, mut y1) = (film_distance as f64 * tan_half, film_distance as f64 * tan_half * 1.05);
        let (mut g0, mut g1) = (g(y0)?, g(y1)?);
        for _ in 0..40 {
            if g1.abs() < 1e-10 || (g1 - g0).abs() < 1e-15 {
                break;
            }
            let y2 = y1 - g1 * (y1 - y0) / (g1 - g0);
            (y0, g0) = (y1, g1);
            y1 = y2;
            if y1 <= 0.0 {
                return None;
            }
            g1 = g(y1)?;
        }
        (g1.abs() < 1e-6).then_some(y1 as f32)
    }

    /// 射出瞳の範囲：フィルムの点 (r, 0)（光軸から r mm）から後ろの面へ向けた光線のうち、レンズを通るものが後ろの面の
    /// 平面で占める範囲を、少し広げて囲む矩形（x0, y0, x1, y1。mm）。通る光線が無ければ None。
    /// 範囲は y = 0 について対称で、y = 0 の上で x の幅がいちばん広い（軸対称な光学系で、点が x 軸の上にあるので）。
    /// 絞ると範囲は後ろの面のごく一部（F 22 で 1 % ほど）になり、後ろの面全体から選ぶと光線の大半が無駄になる
    pub fn exit_pupil_box(&self, film_distance: f32, r: f64) -> Option<[f64; 4]> {
        let zs = self.vertex_z();
        let z_rear = *zs.last().unwrap() as f64;
        let z_film = z_rear - film_distance as f64;
        let rear = 0.5 * self.surfaces.last().unwrap().aperture as f64;
        let passes = |x: f64, y: f64| {
            let mut o = DVec3::new(r, 0.0, z_film);
            let mut d = (DVec3::new(x, y, z_rear) - o).normalize();
            self.trace_with(&zs, &mut o, &mut d, false)
        };
        // 通る範囲は円い開口どうしの重なり（凸）なので、y = 0 の上でも、ある x の縦の線の上でも区間になる。
        // 粗く探して区間の中の点を見つけ、端は二分法で詰める（細かく全部調べるより 10 倍ほど速い。ピントを変えるたびに作る）
        let bisect = |mut inside: f64, mut outside: f64, f: &dyn Fn(f64) -> bool| {
            for _ in 0..24 {
                let m = 0.5 * (inside + outside);
                if f(m) {
                    inside = m;
                } else {
                    outside = m;
                }
            }
            outside
        };
        let seed = |n: usize| (0..n).map(|i| -rear + (i as f64 + 0.5) * 2.0 * rear / n as f64).filter(|&x| passes(x, 0.0)).collect::<Vec<f64>>();
        let mut hits = seed(128);
        if hits.is_empty() {
            hits = seed(2048);
        }
        let (&first, &last) = (hits.first()?, hits.last()?);
        let x0 = bisect(first, -rear, &|x| passes(x, 0.0));
        let x1 = bisect(last, rear, &|x| passes(x, 0.0));
        // y の範囲：区間の中のいくつかの x で、y = 0（中）から上の端（外）へ二分法（対称なので下は同じ）
        let mut y1 = 0.0f64;
        for k in 0..=8 {
            let x = x0 + (x1 - x0) * k as f64 / 8.0;
            if passes(x, 0.0) {
                y1 = y1.max(bisect(0.0, rear, &|y| passes(x, y)));
            }
        }
        // 格子で見落としがあっても入るように、幅の 8 % 広げる
        let (mx, my) = (0.08 * (x1 - x0) + 1e-3, 0.08 * (2.0 * y1) + 1e-3);
        Some([x0 - mx, -y1 - my, x1 + mx, y1 + my])
    }

    /// 射出瞳の範囲の表：フィルムの光軸からの距離 0〜r_max を bins 個に分け、それぞれの区間（両端と真ん中）の範囲を
    /// 合わせた矩形（x0, y0, x1, y1）。光線が通らない区間は 0（そこは描かない）
    pub fn exit_pupil_table(&self, film_distance: f32, r_max: f32, bins: usize) -> Vec<[f32; 4]> {
        let n = 2 * bins + 1;
        let boxes: Vec<Option<[f64; 4]>> = (0..n).map(|i| self.exit_pupil_box(film_distance, r_max as f64 * i as f64 / (n - 1) as f64)).collect();
        (0..bins)
            .map(|b| {
                let mut u: Option<[f64; 4]> = None;
                for bx in boxes[2 * b..=2 * b + 2].iter().flatten() {
                    u = Some(match u {
                        None => *bx,
                        Some(a) => [a[0].min(bx[0]), a[1].min(bx[1]), a[2].max(bx[2]), a[3].max(bx[3])],
                    });
                }
                u.map(|a| a.map(|v| v as f32)).unwrap_or([0.0; 4])
            })
            .collect()
    }

    /// フィルムの中央での明るさ（cos⁴θ · A / z² の、光が通る割合を含めた平均）
    pub fn center_weight(&self, film_distance: f32) -> f32 {
        let zs = self.vertex_z();
        let z_rear = *zs.last().unwrap() as f64;
        let z_film = z_rear - film_distance as f64;
        let rear = 0.5 * self.surfaces.last().unwrap().aperture as f64;
        // 中央から後ろの面へ向けた光線が通る範囲（軸対称なので円になる）の半径を先に探し、その円の中で積分する。
        // 後ろの面全体を粗い格子で見ると、絞ったときに通る範囲が格子の間に落ちて 0 になり、明るさの補正（この値の逆数）が
        // 無限大に近くなって画面が真っ白になる（F 22 で、ピントを 300 mm より遠くにしたとき）
        const STEPS: usize = 4096;
        let passes = |x: f64| {
            let mut o = DVec3::new(0.0, 0.0, z_film);
            let mut d = (DVec3::new(x, 0.0, z_rear) - o).normalize();
            self.trace(&mut o, &mut d, false)
        };
        let Some(k) = (0..STEPS).rev().find(|&k| passes(rear * (k as f64 + 0.5) / STEPS as f64)) else { return 0.0 };
        let r = (rear * (k as f64 + 1.0) / STEPS as f64).min(rear);
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
mod pupil_tests {
    use super::*;

    /// 射出瞳の矩形の外へ向けた光線は通らず、矩形は絞るほど小さい
    #[test]
    fn exit_pupil_box_contains_every_passing_ray() {
        for (f_number, focus) in [(22.0, 300.0), (8.0, 1000.0), (2.0, 150.0)] {
        let sys = LensSystem::double_gauss50().stopped_down(f_number);
        let film = sys.film_distance_for(focus);
        let zs = sys.vertex_z();
        let z_rear = *zs.last().unwrap() as f64;
        let rear = 0.5 * sys.surfaces.last().unwrap().aperture as f64;
        for r in [0.0, 3.0, 6.0, 9.0, 12.0, 15.0, 18.0, 21.0] {
            let Some(b) = sys.exit_pupil_box(film, r) else { continue };
            // 後ろの面全体を細かく調べて、通る光線がすべて矩形の中にあること
            let n = 400;
            let mut inside = 0;
            for j in 0..n {
                for i in 0..n {
                    let (x, y) = (-rear + 2.0 * rear * (i as f64 + 0.5) / n as f64, -rear + 2.0 * rear * (j as f64 + 0.5) / n as f64);
                    let mut o = DVec3::new(r, 0.0, z_rear - film as f64);
                    let mut d = (DVec3::new(x, y, z_rear) - o).normalize();
                    if sys.trace(&mut o, &mut d, false) {
                        assert!(x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3], "r {r}: ({x}, {y}) is outside {b:?}");
                        inside += 1;
                    }
                }
            }
            let area = (b[2] - b[0]) * (b[3] - b[1]);
            if f_number > 20.0 {
                assert!(area < 0.1 * PI * rear * rear, "r {r}: the box {b:?} is not much smaller than the rear element");
            }
            let _ = inside;
        }
        }
        let sys = LensSystem::double_gauss50().stopped_down(22.0);
        let film = sys.film_distance_for(300.0);
        let wide = LensSystem::double_gauss50().stopped_down(2.0);
        let (a, b) = (wide.exit_pupil_box(wide.film_distance_for(300.0), 0.0).unwrap(), sys.exit_pupil_box(film, 0.0).unwrap());
        assert!((b[2] - b[0]) < 0.5 * (a[2] - a[0]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 後ろの面全体を細かい格子で積分した中央の明るさ（center_weight と比べる用）
    fn brute_center_weight(s: &LensSystem, film_distance: f32, n: usize) -> f64 {
        let z_rear = *s.vertex_z().last().unwrap() as f64;
        let z_film = z_rear - film_distance as f64;
        let r = 0.5 * s.surfaces.last().unwrap().aperture as f64;
        let mut sum = 0.0;
        for j in 0..n {
            for i in 0..n {
                let (x, y) = ((2.0 * (i as f64 + 0.5) / n as f64 - 1.0) * r, (2.0 * (j as f64 + 0.5) / n as f64 - 1.0) * r);
                if x * x + y * y > r * r {
                    continue;
                }
                let mut o = DVec3::new(0.0, 0.0, z_film);
                let mut d = (DVec3::new(x, y, z_rear) - o).normalize();
                let c = d.z;
                if s.trace(&mut o, &mut d, false) {
                    sum += c.powi(4);
                }
            }
        }
        sum * (2.0 * r / n as f64).powi(2) / (z_rear - z_film).powi(2)
    }

    /// ピントをどこに合わせても、フィルムの端の主光線が設定の画角の向きになるフィルムの高さが求まる（ピントで画角が変わらない）
    #[test]
    fn film_height_keeps_the_angle_of_view_when_refocusing() {
        let s = LensSystem::double_gauss50().stopped_down(8.0);
        let tan_half = 0.25f64;
        let mut heights = Vec::new();
        for focus in [70.0f32, 150.0, 300.0, 1000.0, 0.0] {
            let film = s.film_distance_for(focus);
            let h = s.film_height_for_angle(film, tan_half).unwrap_or_else(|| panic!("focus {focus}"));
            // 端の主光線が追跡できれば設定の画角ちょうど。できなければ近軸で求めた値（歪曲の分だけ違ってよい）
            if let Some(t) = s.chief_ray_tan(film, h as f64) {
                assert!((t - tan_half).abs() < 0.01, "focus {focus}: tan {t}");
            }
            heights.push((focus, film, h));
        }
        // 近くに合わせるほどフィルムは遠く、同じ画角にするフィルムは大きい
        assert!(heights.windows(2).all(|w| w[0].1 > w[1].1 && w[0].2 > w[1].2), "{heights:?}");
    }

    /// 絞っても、ピントをどこに合わせても、中央の明るさは 0 にならない（0 だと明るさの補正が無限大になって画面が真っ白になった：
    /// F 22 でピントを 300 mm より遠くにしたとき）。開放と絞ったときで、細かい格子で全面を積分した値と合う
    #[test]
    fn center_weight_is_positive_and_accurate_when_stopped_down() {
        let open = LensSystem::double_gauss50();
        for f_number in [2.0f32, 8.0, 22.0] {
            let s = open.stopped_down(f_number);
            for focus in [80.0f32, 300.0, 1000.0, 0.0] {
                let film = s.film_distance_for(focus);
                let w = s.center_weight(film) as f64;
                assert!(w > 0.0, "f/{f_number}, focus {focus}: {w}");
                if f_number < 10.0 {
                    let b = brute_center_weight(&s, film, 1024);
                    assert!((w / b - 1.0).abs() < 0.03, "f/{f_number}, focus {focus}: {w} vs {b}");
                }
            }
        }
    }

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
