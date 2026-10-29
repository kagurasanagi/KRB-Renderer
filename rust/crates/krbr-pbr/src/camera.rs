//! カメラ：指定した点がすべて画面に収まる構図（自動）と、注視点のまわりを回るカメラ（ビューアの操作用）

use glam::{DVec3, Vec3};

/// 画面端の余白（割合）の上限。両側で 90% を超えると、基板を収める範囲がほとんど無くなる
pub const MAX_MARGIN: f64 = 0.45;

#[derive(Debug, Clone, Copy)]
pub struct CameraParams {
    /// 基板面からの仰角
    pub elevation_deg: f64,
    /// 0 で基板の手前（KiCad の y が大きい側）から見る。正で +x 側へ回る
    pub azimuth_deg: f64,
    /// 縦の画角
    pub fov_deg: f64,
    /// 画面端に残す余白（割合）
    pub margin: f64,
    pub width: u32,
    pub height: u32,
}

impl Default for CameraParams {
    fn default() -> Self {
        Self { elevation_deg: 35.0, azimuth_deg: 45.0, fov_deg: 30.0, margin: 0.04, width: 1920, height: 1080 }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub position: Vec3,
    pub forward: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub tan_half_fov_y: f32,
    pub aspect: f32,
    /// 注視点
    pub target: Vec3,
    /// 注視点までの距離（mm）
    pub distance: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self { position: Vec3::ZERO, forward: Vec3::NEG_Z, right: Vec3::X, up: Vec3::Y, tan_half_fov_y: 0.27, aspect: 16.0 / 9.0, target: Vec3::ZERO, distance: 100.0 }
    }
}

fn basis(elevation_deg: f64, azimuth_deg: f64) -> (DVec3, DVec3, DVec3) {
    let (el, az) = (elevation_deg.to_radians(), azimuth_deg.to_radians());
    // 注視点からカメラへの向き
    let to_cam = DVec3::new(el.cos() * az.sin(), el.sin(), el.cos() * az.cos());
    let fwd = -to_cam.normalize();
    let mut world_up = DVec3::Y;
    if fwd.dot(world_up).abs() > 0.999 {
        // 真上・真下から見る場合：仰角を ±90° に近づけたときの上の向きの極限（方位で回る。真上・方位 0° で -Z、
        // つまり KiCad の画面と同じ向き。真下・方位 180° では左右が反転した、裏から見た KiCad の向き）
        let h = DVec3::new(az.sin(), 0.0, az.cos());
        world_up = if el > 0.0 { -h } else { h };
    }
    let right = fwd.cross(world_up).normalize();
    let up = right.cross(fwd);
    (fwd, right, up)
}

/// 指定した点がすべて画面に収まるように距離と注視点を決める
pub fn frame_camera(p: &CameraParams, points: &[Vec3]) -> Camera {
    let (fwd, right, up) = basis(p.elevation_deg, p.azimuth_deg);
    let aspect = p.width as f32 / p.height.max(1) as f32;
    let ty = (p.fov_deg.to_radians() / 2.0).tan();
    let tx = ty * aspect as f64;
    let keep = 1.0 - p.margin.clamp(0.0, MAX_MARGIN) * 2.0;

    let mut target = DVec3::ZERO;
    if !points.is_empty() {
        let (mut lo, mut hi) = (DVec3::splat(1e30), DVec3::splat(-1e30));
        for q in points {
            lo = lo.min(q.as_dvec3());
            hi = hi.max(q.as_dvec3());
        }
        target = (lo + hi) * 0.5;
    }
    let mut dist = 1.0;
    for iter in 0..5 {
        // 全点が入る最小の距離
        dist = 1e-3f64;
        for q in points {
            let d = q.as_dvec3() - target;
            let (x, y, z) = (d.dot(right), d.dot(up), d.dot(fwd));
            dist = dist.max(x.abs() / (tx * keep) - z);
            dist = dist.max(y.abs() / (ty * keep) - z);
        }
        if iter == 4 {
            break;
        }
        // 画面上の偏りを打ち消すように注視点をずらす
        let (mut umin, mut umax, mut vmin, mut vmax) = (1e30f64, -1e30f64, 1e30f64, -1e30f64);
        for q in points {
            let d = q.as_dvec3() - target;
            let depth = d.dot(fwd) + dist;
            if depth <= 1e-6 {
                continue;
            }
            let (u, v) = (d.dot(right) / (depth * tx), d.dot(up) / (depth * ty));
            umin = umin.min(u);
            umax = umax.max(u);
            vmin = vmin.min(v);
            vmax = vmax.max(v);
        }
        if umin > umax {
            break;
        }
        target += right * ((umax + umin) * 0.5 * tx * dist) + up * ((vmax + vmin) * 0.5 * ty * dist);
    }
    orbit_camera(target.as_vec3(), dist as f32, p.elevation_deg, p.azimuth_deg, p.fov_deg, aspect)
}

impl Camera {
    /// position から target を見るカメラ。up は画面の上のおおよその向き（視線と平行なら Y、それも平行なら Z を使う）。
    /// fov_y_deg は縦の画角、aspect は横 / 縦
    pub fn look_at(position: Vec3, target: Vec3, up: Vec3, fov_y_deg: f32, aspect: f32) -> Camera {
        let to = target - position;
        let distance = to.length();
        let forward = if distance > 0.0 { to / distance } else { Vec3::NEG_Z };
        let up_hint = [up, Vec3::Y, Vec3::Z].into_iter().find(|u| u.length_squared() > 0.0 && forward.cross(u.normalize()).length() > 1e-4).unwrap_or(Vec3::Y);
        let right = forward.cross(up_hint).normalize();
        let up = right.cross(forward);
        Camera { position, forward, right, up, tan_half_fov_y: (fov_y_deg.to_radians() / 2.0).tan(), aspect, target, distance: distance.max(1e-3) }
    }
}

/// 注視点のまわりを回るカメラ
pub fn orbit_camera(target: Vec3, distance: f32, elevation_deg: f64, azimuth_deg: f64, fov_deg: f64, aspect: f32) -> Camera {
    let (fwd, right, up) = basis(elevation_deg, azimuth_deg);
    Camera {
        aspect,
        tan_half_fov_y: (fov_deg.to_radians() / 2.0).tan() as f32,
        target,
        distance,
        position: (target.as_dvec3() - fwd * distance as f64).as_vec3(),
        forward: fwd.as_vec3(),
        right: right.as_vec3(),
        up: up.as_vec3(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真上（方位 0°）は KiCad の画面と同じ向き（x が右、基板の y が下 = ワールドの +Z が下）、真下（方位 180°）は左右が反転。
    /// ±90° の手前から近づけても向きが跳ばない
    #[test]
    fn straight_down_and_up_views() {
        let close = |a: DVec3, b: DVec3| (a - b).length() < 1e-3;
        let (fwd, right, up) = basis(90.0, 0.0);
        assert!(close(fwd, DVec3::NEG_Y) && close(right, DVec3::X) && close(up, DVec3::NEG_Z));
        let (fwd, right, up) = basis(-90.0, 180.0);
        assert!(close(fwd, DVec3::Y) && close(right, DVec3::NEG_X) && close(up, DVec3::NEG_Z));
        for az in [0.0, 30.0, 135.0, 180.0, -60.0] {
            for el in [90.0, -90.0] {
                let near = basis(el - 0.01f64.copysign(el), az);
                let at = basis(el, az);
                assert!(close(near.1, at.1) && close(near.2, at.2), "el {el} az {az}");
            }
        }
    }

    #[test]
    fn frames_all_points() {
        let pts = vec![Vec3::new(-60.0, 0.0, -38.0), Vec3::new(60.0, 0.0, -38.0), Vec3::new(60.0, 0.0, 38.0), Vec3::new(-60.0, 0.0, 38.0), Vec3::new(0.0, 5.0, 0.0)];
        let cam = frame_camera(&CameraParams::default(), &pts);
        assert!(cam.distance > 100.0 && cam.distance < 400.0, "{}", cam.distance);
        // 全点が画面内
        for q in &pts {
            let d = *q - cam.position;
            let z = d.dot(cam.forward);
            assert!(z > 0.0);
            assert!((d.dot(cam.right) / z).abs() <= cam.tan_half_fov_y * cam.aspect + 1e-4);
            assert!((d.dot(cam.up) / z).abs() <= cam.tan_half_fov_y + 1e-4);
        }
        assert!((cam.forward.length() - 1.0).abs() < 1e-5);
        assert!(cam.position.y > 0.0); // 仰角 35° で上から
    }

    /// look_at は同じ位置・注視点の orbit_camera と同じ向き。視線が up と平行でも向きが決まる
    #[test]
    fn look_at_matches_orbit() {
        let o = orbit_camera(Vec3::new(5.0, 1.0, -3.0), 120.0, 35.0, 45.0, 30.0, 1.5);
        let l = Camera::look_at(o.position, o.target, Vec3::Y, 30.0, 1.5);
        let close = |a: Vec3, b: Vec3| (a - b).length() < 1e-4;
        assert!(close(l.forward, o.forward) && close(l.right, o.right) && close(l.up, o.up));
        assert!((l.tan_half_fov_y - o.tan_half_fov_y).abs() < 1e-6 && (l.distance - o.distance).abs() < 1e-3);
        let down = Camera::look_at(Vec3::new(0.0, 100.0, 0.0), Vec3::ZERO, Vec3::Y, 30.0, 1.0);
        assert!(close(down.forward, Vec3::NEG_Y) && down.right.is_finite() && (down.right.length() - 1.0).abs() < 1e-5);
    }
}
