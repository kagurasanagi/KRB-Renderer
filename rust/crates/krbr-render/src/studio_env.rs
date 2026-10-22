//! 内蔵の環境マップ「スタジオ」：灰色の撮影ブースに大きなソフトボックスを置いたもの。
//! HDR を指定していない基板はこれで描く（ファイルは作らず、その場で正距円筒の画像を生成する）。

use std::path::Path;

use glam::Vec3;

use crate::image_io::{self, ImageF};

/// 設定・コマンドラインで内蔵のスタジオを指す名前
pub const STUDIO: &str = "builtin:studio";

pub fn is_builtin(spec: &str) -> bool {
    spec.starts_with("builtin:")
}

/// 環境マップの名前の種類。表示名（翻訳）はライブラリでは決めず、UI が内蔵の ID から引く
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentName {
    /// 内蔵の環境（"studio" など。spec の "builtin:" の後ろ）
    Builtin(String),
    /// 画像ファイル（ファイル名）
    File(String),
}

/// spec（設定・コマンドラインの値）が指す環境マップの名前
pub fn describe_environment(spec: &str) -> EnvironmentName {
    match spec.strip_prefix("builtin:") {
        Some(id) => EnvironmentName::Builtin(id.to_string()),
        None => EnvironmentName::File(Path::new(spec).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| spec.to_string())),
    }
}

/// ログ用の英語の名前（ファイルならファイル名）。画面に出す名前は describe_environment から UI 側で翻訳して作る
pub fn environment_label(spec: &str) -> String {
    match describe_environment(spec) {
        EnvironmentName::Builtin(id) if id == "studio" => "Studio (built-in)".into(),
        EnvironmentName::Builtin(id) => format!("{id} (built-in)"),
        EnvironmentName::File(f) => f,
    }
}

/// 環境マップを読み込む（内蔵の名前なら生成する）
pub fn load_environment_map(spec: &str) -> Result<ImageF, String> {
    match spec {
        STUDIO => Ok(studio_environment(2048)),
        s if is_builtin(s) => Err(format!("unknown built-in environment '{s}' (available: {STUDIO})")),
        s => image_io::load_hdr_image(Path::new(s)),
    }
}

/// 平らな四角い発光面（ソフトボックス）。向きは基板から見た方位・仰角、大きさは中心から見た半角
struct Softbox {
    azimuth_deg: f32,
    elevation_deg: f32,
    half_width_deg: f32,
    half_height_deg: f32,
    radiance: f32,
}

/// 方位・仰角の向き（カメラの方位と同じ取り方：方位 0° が +Z、90° が +X、+Y が天頂）
fn direction(azimuth_deg: f32, elevation_deg: f32) -> Vec3 {
    let (a, e) = (azimuth_deg.to_radians(), elevation_deg.to_radians());
    Vec3::new(e.cos() * a.sin(), e.sin(), e.cos() * a.cos())
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl Softbox {
    fn radiance(&self, d: Vec3) -> f32 {
        let c = direction(self.azimuth_deg, self.elevation_deg);
        let cos = d.dot(c);
        if cos <= 0.0 {
            return 0.0;
        }
        let u = if c.y.abs() > 0.99 { Vec3::X } else { Vec3::Y.cross(c).normalize() };
        let v = c.cross(u);
        // 心射図法：中心に垂直な平面上の位置。平らな四角い面の見え方になる
        let (x, y) = (d.dot(u) / cos / self.half_width_deg.to_radians().tan(), d.dot(v) / cos / self.half_height_deg.to_radians().tan());
        let edge = 0.04;
        let mask = (1.0 - smoothstep(1.0 - edge, 1.0, x.abs())) * (1.0 - smoothstep(1.0 - edge, 1.0, y.abs()));
        // 拡散布の中央がやや明るい
        mask * self.radiance * (1.0 - 0.2 * (x * x + y * y) * 0.5)
    }
}

/// 背景の部屋：床はやや暗い灰色、壁は地平線から天井へ少し暗くなる。境目はなだらかに
fn room(d: Vec3) -> f32 {
    let floor = 0.12 - 0.03 * (-d.y).max(0.0);
    let wall = 0.16 - 0.08 * d.y.max(0.0);
    let t = smoothstep(-0.04, 0.04, d.y);
    floor + (wall - floor) * t
}

/// 内蔵のスタジオを width x width/2 の正距円筒の画像として作る（向きはシェーダの dirToUv と同じ）
pub fn studio_environment(width: u32) -> ImageF {
    // 既定のカメラ（方位 45°・仰角 35°）から見て、左上に主光源、右に補助光、
    // 天井いっぱいに大きな面光源、基板の向こう側（基板の面に映る向き）に横長の光
    let boxes = [
        Softbox { azimuth_deg: 0.0, elevation_deg: 90.0, half_width_deg: 30.0, half_height_deg: 22.0, radiance: 3.5 },
        Softbox { azimuth_deg: 100.0, elevation_deg: 38.0, half_width_deg: 14.0, half_height_deg: 22.0, radiance: 10.0 },
        Softbox { azimuth_deg: -20.0, elevation_deg: 22.0, half_width_deg: 18.0, half_height_deg: 18.0, radiance: 3.0 },
        Softbox { azimuth_deg: 225.0, elevation_deg: 30.0, half_width_deg: 40.0, half_height_deg: 9.0, radiance: 1.2 },
    ];
    let height = (width / 2).max(1);
    let mut img = ImageF::new(width, height);
    for j in 0..height {
        let theta = (j as f32 + 0.5) / height as f32 * std::f32::consts::PI;
        for i in 0..width {
            let phi = ((i as f32 + 0.5) / width as f32 - 0.5) * 2.0 * std::f32::consts::PI;
            let d = Vec3::new(theta.sin() * phi.sin(), theta.cos(), -theta.sin() * phi.cos());
            let l = room(d) + boxes.iter().map(|b| b.radiance(d)).sum::<f32>();
            let k = ((j * width + i) * 4) as usize;
            img.rgba[k..k + 4].copy_from_slice(&[l, l, l, 1.0]);
        }
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn studio_has_lights_and_room() {
        let img = studio_environment(256);
        assert_eq!((img.width, img.height), (256, 128));
        let px = |d: Vec3| {
            // dirToUv と同じ式で画素を引く
            let u = 0.5 + d.x.atan2(-d.z) * 0.5 / std::f32::consts::PI;
            let v = d.y.clamp(-1.0, 1.0).acos() / std::f32::consts::PI;
            let (i, j) = (((u * 256.0) as u32).min(255), ((v * 128.0) as u32).min(127));
            img.rgba[((j * 256 + i) * 4) as usize]
        };
        assert!(px(Vec3::Y) > 3.0); // 天井の面光源
        assert!(px(direction(100.0, 38.0)) > 10.0); // 主光源
        let floor = px(Vec3::NEG_Y);
        assert!(floor > 0.05 && floor < 0.3);
    }

    #[test]
    fn labels() {
        assert_eq!(describe_environment(STUDIO), EnvironmentName::Builtin("studio".into()));
        assert_eq!(describe_environment("C:/hdr/a.exr"), EnvironmentName::File("a.exr".into()));
        assert_eq!(environment_label(STUDIO), "Studio (built-in)");
        assert_eq!(environment_label("C:/hdr/a.exr"), "a.exr");
        assert!(load_environment_map("builtin:nope").is_err());
    }
}
