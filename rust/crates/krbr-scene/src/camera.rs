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
        world_up = DVec3::NEG_Z; // 真上から見る場合
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
}
