//! sRGB（表示値）とリニアの変換。krbr-model（STEP の面の色）と krbr-scene（材質・設定の色）の両方がこれを使う。
//! 範囲外の値（STEP や設定に書かれた 1 を超える値・負の値）は 0..1 に収めてから変換する

use glam::Vec3;

/// sRGB（表示値）からリニアへ
pub fn srgb_to_linear_f(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn srgb_to_linear(c: Vec3) -> Vec3 {
    Vec3::new(srgb_to_linear_f(c.x), srgb_to_linear_f(c.y), srgb_to_linear_f(c.z))
}

/// リニアから sRGB（表示値）へ
pub fn linear_to_srgb_f(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

pub fn linear_to_srgb(c: Vec3) -> Vec3 {
    Vec3::new(linear_to_srgb_f(c.x), linear_to_srgb_f(c.y), linear_to_srgb_f(c.z))
}

/// 16 進 6 桁 "rrggbb"（# は付けない）を sRGB の 8 bit 値にする。ASCII の 16 進数字 6 つでなければ None
pub fn parse_hex_rgb(hex: &str) -> Option<[u8; 3]> {
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    // ASCII だけなので、バイト位置で切り出してよい
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([byte(0)?, byte(2)?, byte(4)?])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_round_trip_and_clamp() {
        for v in [0.0f32, 0.1, 0.5, 0.9, 1.0] {
            assert!((linear_to_srgb_f(srgb_to_linear_f(v)) - v).abs() < 1e-5);
        }
        assert!((srgb_to_linear(Vec3::splat(0.5)).x - 0.2140).abs() < 1e-3);
        // 範囲外は 0..1 に収める
        assert_eq!(srgb_to_linear(Vec3::new(-0.5, 1.5, 0.0)), Vec3::new(0.0, 1.0, 0.0));
    }

    #[test]
    fn hex() {
        assert_eq!(parse_hex_rgb("ff8000"), Some([255, 128, 0]));
        assert_eq!(parse_hex_rgb("FF8000"), Some([255, 128, 0]));
        for bad in ["ff80", "ff8000aa", "gg8000", "あいう", "ff80あ"] {
            assert_eq!(parse_hex_rgb(bad), None, "{bad}");
        }
    }
}
