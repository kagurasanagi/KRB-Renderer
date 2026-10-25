//! 動画の時間方向のノイズ除去に渡すフロー（画素ごとの動き）と奥行き。物は動かずカメラだけが動く（動画の書き出しは
//! カメラの動き）ので、画素に写っている点（カメラの光線が最初に当たった点の平均。Progressive::readback_position）を
//! 前のコマのカメラで写し直せば、前のコマでどの画素の位置にあったかが分かる。何にも当たらない画素（環境光）は、
//! 無限遠の向きとして写し直す（カメラの回転の分だけ動く）。
//! 写し方はピンホール（シェーダの generateCameraRay と同じ式）。薄レンズはピントの面で同じ、実レンズはフィルムの大きさを
//! 画角に合わせてあるので近い（ボケた画素は、混ざった点の平均になる）

use glam::Vec3;
use krbr_scene::Camera;

/// 画素の中心の正規化座標（-1..1、y は下向き）を、カメラの光線の向きにする（shaders/camera.glsl のピンホールと同じ）
fn ray_dir(cam: &Camera, ndc_x: f32, ndc_y: f32) -> Vec3 {
    (cam.forward + cam.right * (ndc_x * cam.tan_half_fov_y * cam.aspect) - cam.up * (ndc_y * cam.tan_half_fov_y)).normalize()
}

/// カメラから見た向き d（カメラの位置からの差、または無限遠の向き）を、画素の座標（左上が 0、画素の中心は +0.5）にする。
/// カメラの後ろなら None
fn project(cam: &Camera, d: Vec3, w: u32, h: u32) -> Option<(f32, f32)> {
    let z = d.dot(cam.forward);
    if z <= 1e-6 {
        return None;
    }
    let nx = d.dot(cam.right) / (z * cam.tan_half_fov_y * cam.aspect);
    let ny = -d.dot(cam.up) / (z * cam.tan_half_fov_y);
    Some(((nx + 1.0) * 0.5 * w as f32, (ny + 1.0) * 0.5 * h as f32))
}

/// 後ろ向きのフロー：今のコマの画素 (x, y) に写っている物が、前のコマで写っていた位置と、今の画素の中心との差（画素単位。
/// 前の位置 = 今の中心 + フロー）。position は readback_position の値（w は当たったサンプルの割合。半分未満なら環境光とみなす）。
/// 前のコマでカメラの後ろに回った点は 0（動きが分からない）
pub fn backward_flow(position: &[[f32; 4]], w: u32, h: u32, cur: &Camera, prev: &Camera) -> Vec<[f32; 2]> {
    assert_eq!(position.len(), (w * h) as usize);
    let mut out = vec![[0.0f32; 2]; position.len()];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
            let p = position[i];
            let d = if p[3] >= 0.5 {
                Vec3::new(p[0], p[1], p[2]) - prev.position
            } else {
                // 環境光：無限遠の向き（今のカメラでの画素の向き）
                ray_dir(cur, cx / w as f32 * 2.0 - 1.0, cy / h as f32 * 2.0 - 1.0)
            };
            if let Some((px, py)) = project(prev, d, w, h) {
                out[i] = [px - cx, py - cy];
            }
        }
    }
    out
}

/// 奥行き：今のカメラの向きに沿った、写っている点までの距離（mm）。環境光の画素は f32::INFINITY
pub fn depth(position: &[[f32; 4]], cur: &Camera) -> Vec<f32> {
    position.iter().map(|p| if p[3] >= 0.5 { (Vec3::new(p[0], p[1], p[2]) - cur.position).dot(cur.forward) } else { f32::INFINITY }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use krbr_scene::orbit_camera;

    #[test]
    fn a_point_moves_with_the_camera() {
        let (w, h) = (64u32, 32u32);
        let cur = orbit_camera(Vec3::ZERO, 100.0, 30.0, 10.0, 30.0, 2.0);
        let prev = orbit_camera(Vec3::ZERO, 100.0, 30.0, 12.0, 30.0, 2.0);
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
        assert!(depth(&pos, &cur)[i] > 99.0);
    }
}
