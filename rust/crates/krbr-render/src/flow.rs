//! 動画の時間方向のノイズ除去に渡すフロー（画素ごとの動き）と奥行き。物は動かずカメラだけが動く（動画の書き出しは
//! カメラの動き）ので、画素に写っている点（カメラの光線が最初に当たった点の平均。Progressive::readback_position）を
//! 前のコマのカメラで写し直せば、前のコマでどの画素の位置にあったかが分かる。何にも当たらない画素（環境光）は、
//! 無限遠の向きとして写し直す（カメラの回転の分だけ動く）。
//! 写し方はピンホール（シェーダの generateCameraRay と同じ式）。薄レンズはピントの面で同じ。実レンズは画素と向きの対応が
//! ピンホールと違う（入射瞳がカメラの位置に無い・歪曲がある）ので、主光線（絞りの中心を通る光線）の表で写す
//! （ボケた画素は、混ざった点の平均になる）

use glam::{DVec3, Vec3};
use krbr_scene::lens::{LensModel, LensParams};
use krbr_scene::Camera;

/// 主光線の表の数（フィルムの中心から隅の少し外まで）
const CHIEF_RAYS: usize = 96;

/// フローを求めるためのカメラ（写し方を含む）
#[derive(Clone)]
pub struct FlowCamera {
    pub cam: Camera,
    lens: Option<LensMap>,
}

/// 実レンズの写し方：フィルムの高さ ys[k]（mm、+x の上）から出る主光線が前の面を出る点 (x, z) と向き (x, z)（レンズの
/// 座標：x 右、y 上、z が物体側。前の面の頂点が z = 0 でカメラの位置）。像は上下左右が逆になる
#[derive(Clone)]
struct LensMap {
    half_w: f64,
    half_h: f64,
    ys: Vec<f64>,
    o: Vec<[f64; 2]>,
    d: Vec<[f64; 2]>,
}

impl FlowCamera {
    /// ピンホール（薄レンズも同じ）
    pub fn pinhole(cam: Camera) -> Self {
        Self { cam, lens: None }
    }

    /// レンズの設定に合わせる（実レンズなら主光線の表を作る。追跡できない主光線は表から抜く）
    pub fn new(cam: Camera, lens: &LensParams) -> Self {
        if lens.model != LensModel::Realistic {
            return Self::pinhole(cam);
        }
        let film = crate::kernel::realistic_film(lens, &cam);
        let (half_h, half_w) = (film.half_h as f64, (film.half_h * cam.aspect) as f64);
        let r_max = 1.1 * (half_w * half_w + half_h * half_h).sqrt();
        let mut m = LensMap { half_w, half_h, ys: vec![], o: vec![], d: vec![] };
        for k in 0..CHIEF_RAYS {
            let y = r_max * k as f64 / (CHIEF_RAYS - 1) as f64;
            let ray = if k == 0 { Some((DVec3::ZERO, DVec3::Z)) } else { film.sys.chief_ray(film.film_dist, y) };
            if let Some((o, d)) = ray.filter(|(_, d)| d.z > 1e-9) {
                m.ys.push(y);
                m.o.push([o.x, o.z]);
                m.d.push([d.x, d.z]);
            }
        }
        if m.ys.len() < 3 {
            return Self::pinhole(cam);
        }
        // 画角が広いと、画面の隅の主光線は追跡できない（全反射など）。隅は、追跡できた最後の 2 本から延ばす（位置と傾き）
        let n = m.ys.len();
        let (ya, yb) = (m.ys[n - 2], m.ys[n - 1]);
        let (sa, sb) = (m.d[n - 2][0] / m.d[n - 2][1], m.d[n - 1][0] / m.d[n - 1][1]);
        let (oa, ob) = (m.o[n - 2], m.o[n - 1]);
        let step = yb - ya;
        let mut y = yb + step;
        while y <= r_max + step {
            let t = (y - yb) / step;
            m.ys.push(y);
            m.o.push([ob[0] + (ob[0] - oa[0]) * t, ob[1] + (ob[1] - oa[1]) * t]);
            let sl = sb + (sb - sa) * t;
            let l = (1.0 + sl * sl).sqrt();
            m.d.push([sl / l, 1.0 / l]);
            y += step;
        }
        Self { cam, lens: Some(m) }
    }

    /// 画素の中心の正規化座標（-1..1、y は下向き）を、カメラの光線の向きにする（shaders/camera.glsl と同じ）
    fn ray_dir(&self, ndc_x: f32, ndc_y: f32) -> Vec3 {
        let c = &self.cam;
        let Some(m) = &self.lens else {
            return (c.forward + c.right * (ndc_x * c.tan_half_fov_y * c.aspect) - c.up * (ndc_y * c.tan_half_fov_y)).normalize();
        };
        let (fx, fy) = (-ndc_x as f64 * m.half_w, ndc_y as f64 * m.half_h);
        let r = (fx * fx + fy * fy).sqrt();
        let [dx, dz] = m.interp_d(r);
        let (ux, uy) = if r > 1e-12 { (fx / r, fy / r) } else { (1.0, 0.0) };
        (c.right * (dx * ux) as f32 + c.up * (dx * uy) as f32 + c.forward * dz as f32).normalize()
    }

    /// 点（カメラの位置からの差 d。無限遠の向きなら far = true）を、画素の座標（左上が 0、画素の中心は +0.5）にする。
    /// カメラの後ろ・表の外なら None
    fn project(&self, d: Vec3, far: bool, w: u32, h: u32) -> Option<(f32, f32)> {
        let c = &self.cam;
        let z = d.dot(c.forward);
        if z <= 1e-6 {
            return None;
        }
        let (nx, ny) = match &self.lens {
            None => (d.dot(c.right) / (z * c.tan_half_fov_y * c.aspect), -d.dot(c.up) / (z * c.tan_half_fov_y)),
            Some(m) => {
                let s = if far { 1e7 } else { 1.0 };
                let q = d.as_dvec3() * s;
                let (qx, qy, qz) = (q.dot(c.right.as_dvec3()), q.dot(c.up.as_dvec3()), q.dot(c.forward.as_dvec3()));
                let rho = (qx * qx + qy * qy).sqrt();
                let (fx, fy) = if rho < 1e-12 {
                    (0.0, 0.0)
                } else {
                    let (y, sign) = m.film_height(rho, qz)?;
                    (sign * y * qx / rho, sign * y * qy / rho)
                };
                ((-fx / m.half_w) as f32, (fy / m.half_h) as f32)
            }
        };
        Some(((nx + 1.0) * 0.5 * w as f32, (ny + 1.0) * 0.5 * h as f32))
    }
}

impl LensMap {
    /// フィルムの高さ r の主光線の向き (x, z)（表から補間）
    fn interp_d(&self, r: f64) -> [f64; 2] {
        let n = self.ys.len();
        let k = self.ys.partition_point(|&y| y <= r).clamp(1, n - 1);
        let t = ((r - self.ys[k - 1]) / (self.ys[k] - self.ys[k - 1])).clamp(0.0, 1.0);
        let (a, b) = (self.d[k - 1], self.d[k]);
        [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
    }

    /// 軸からの距離 rho・奥行き z の点を通る主光線の、フィルムの高さと、像の向き（主光線が軸のどちら側へ出るか：
    /// フィルムの +x の点が物体側の -x へ写るなら -1）
    fn film_height(&self, rho: f64, z: f64) -> Option<(f64, f64)> {
        // 主光線 k が奥行き z で軸から離れている距離（符号つき）
        let x_at = |k: usize| self.o[k][0] + self.d[k][0] * (z - self.o[k][1]) / self.d[k][1];
        let n = self.ys.len();
        let sign = if x_at(n - 1) < 0.0 { -1.0 } else { 1.0 };
        let mut prev = sign * x_at(0);
        for k in 1..n {
            let cur = sign * x_at(k);
            if cur >= rho && prev <= rho && cur > prev {
                let t = (rho - prev) / (cur - prev);
                return Some((self.ys[k - 1] + (self.ys[k] - self.ys[k - 1]) * t, sign));
            }
            prev = cur;
        }
        None
    }
}

/// 後ろ向きのフロー：今のコマの画素 (x, y) に写っている物が、前のコマで写っていた位置と、今の画素の中心との差（画素単位。
/// 前の位置 = 今の中心 + フロー）。position は readback_position の値（w は当たったサンプルの割合。0 なら環境光。実レンズでは
/// レンズの中で止まった光線も当たらない分に数えるので、1 より小さくても物が写っている）。
/// 前のコマでカメラの後ろに回った点は 0（動きが分からない）
pub fn backward_flow(position: &[[f32; 4]], w: u32, h: u32, cur: &FlowCamera, prev: &FlowCamera) -> Vec<[f32; 2]> {
    assert_eq!(position.len(), (w * h) as usize);
    let mut out = vec![[0.0f32; 2]; position.len()];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
            let p = position[i];
            let (d, far) = if p[3] > 0.0 {
                (Vec3::new(p[0], p[1], p[2]) - prev.cam.position, false)
            } else {
                // 環境光：無限遠の向き（今のカメラでの画素の向き）
                (cur.ray_dir(cx / w as f32 * 2.0 - 1.0, cy / h as f32 * 2.0 - 1.0), true)
            };
            if let Some((px, py)) = prev.project(d, far, w, h) {
                out[i] = [px - cx, py - cy];
            }
        }
    }
    out
}

/// 奥行き：今のカメラの向きに沿った、写っている点までの距離（mm）。環境光の画素は f32::INFINITY
pub fn depth(position: &[[f32; 4]], cur: &Camera) -> Vec<f32> {
    position.iter().map(|p| if p[3] > 0.0 { (Vec3::new(p[0], p[1], p[2]) - cur.position).dot(cur.forward) } else { f32::INFINITY }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use krbr_scene::orbit_camera;

    #[test]
    fn a_point_moves_with_the_camera() {
        let (w, h) = (64u32, 32u32);
        let cur = FlowCamera::pinhole(orbit_camera(Vec3::ZERO, 100.0, 30.0, 10.0, 30.0, 2.0));
        let prev = FlowCamera::pinhole(orbit_camera(Vec3::ZERO, 100.0, 30.0, 12.0, 30.0, 2.0));
        // 注視点（画面の中央）にある点：前のコマでも中央の近く
        let mut pos = vec![[0.0f32; 4]; (w * h) as usize];
        let i = ((h / 2) * w + w / 2) as usize;
        pos[i] = [0.0, 0.0, 0.0, 1.0];
        let f = backward_flow(&pos, w, h, &cur, &prev);
        let (px, py) = (w as f32 / 2.0 + 0.5 + f[i][0], h as f32 / 2.0 + 0.5 + f[i][1]);
        assert!((px - w as f32 / 2.0).abs() < 0.6 && (py - h as f32 / 2.0).abs() < 0.6, "{px} {py}");
        // 同じカメラなら、環境光の画素は動かない（点を置いた画素は、点が画素の中心から半画素ずれている分だけ）
        let still = backward_flow(&pos, w, h, &cur, &cur);
        assert!(still.iter().enumerate().all(|(k, v)| k == i || (v[0].abs() < 1e-3 && v[1].abs() < 1e-3)));
        assert!((still[i][0] + 0.5).abs() < 1e-3 && (still[i][1] + 0.5).abs() < 1e-3, "{:?}", still[i]);
        // 方位を変えると、環境光の画素も横へ動く
        assert!(f[0][0].abs() > 0.5);
        assert!(depth(&pos, &cur.cam)[i] > 99.0);
    }

    /// 実レンズ：画素の主光線の上の点を同じカメラで写すと、その画素に戻る（環境光の画素も）
    #[test]
    fn realistic_lens_maps_its_own_rays_back() {
        let (w, h) = (96u32, 54u32);
        let cam = orbit_camera(Vec3::ZERO, 130.0, 30.0, 10.0, 30.0, w as f32 / h as f32);
        let lens = LensParams { model: LensModel::Realistic, f_number: 8.0, ..Default::default() };
        let fc = FlowCamera::new(cam, &lens);
        assert!(fc.lens.is_some());
        let m = fc.lens.as_ref().unwrap();
        let mut worst = 0.0f32;
        for &(x, y) in &[(48u32, 27u32), (10, 5), (90, 50), (70, 12), (3, 40)] {
            let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
            let (nx, ny) = (cx / w as f32 * 2.0 - 1.0, cy / h as f32 * 2.0 - 1.0);
            // 主光線の上、前の面から 100 mm 先の点（レンズの座標からワールドへ）
            let (fx, fy) = (-nx as f64 * m.half_w, ny as f64 * m.half_h);
            let r = (fx * fx + fy * fy).sqrt();
            let k = m.ys.partition_point(|&v| v <= r).clamp(1, m.ys.len() - 1);
            let t = (r - m.ys[k - 1]) / (m.ys[k] - m.ys[k - 1]);
            let ox = m.o[k - 1][0] + (m.o[k][0] - m.o[k - 1][0]) * t;
            let oz = m.o[k - 1][1] + (m.o[k][1] - m.o[k - 1][1]) * t;
            let [dx, dz] = fc.ray_dir_lens(r);
            let s = (100.0 - oz) / dz;
            let lat = ox + dx * s;
            let (ux, uy) = if r > 1e-12 { (fx / r, fy / r) } else { (1.0, 0.0) };
            let p = cam.position + cam.right * (lat * ux) as f32 + cam.up * (lat * uy) as f32 + cam.forward * 100.0;
            let (px, py) = fc.project(p - cam.position, false, w, h).unwrap();
            worst = worst.max((px - cx).abs().max((py - cy).abs()));
            let (ex, ey) = fc.project(fc.ray_dir(nx, ny), true, w, h).unwrap();
            worst = worst.max((ex - cx).abs().max((ey - cy).abs()));
        }
        assert!(worst < 0.05, "{worst}");
    }

    impl FlowCamera {
        fn ray_dir_lens(&self, r: f64) -> [f64; 2] {
            self.lens.as_ref().unwrap().interp_d(r)
        }
    }
}
