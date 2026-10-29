//! モデルの部位（STEP の XDE の名前と面の色の組）。材質はこの単位で割り当てる

use glam::Vec3;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelPart {
    /// XDE の名前（部品・サブシェイプ）。無ければ空
    pub name: String,
    /// STEP に書かれた色の値（表示用の sRGB）
    pub srgb: Vec3,
    pub alpha: f32,
    pub has_color: bool,
}

/// KiCad 標準モデル（kicad-packages3D の生成スクリプト）でよく使われる色。STEP に書かれた値そのもの
const KICAD_COLORS: &[(&str, [f32; 3])] = &[
    ("black_body", [0.148, 0.145, 0.145]),
    ("metal_grey_pins", [0.824, 0.820, 0.781]),
    ("gold_pins", [0.859, 0.738, 0.496]),
    ("light_brown_body", [0.691, 0.664, 0.598]),
    ("white_body", [0.895, 0.891, 0.813]),
    ("green_body", [0.337, 0.678, 0.443]),
    ("blue_body", [0.137, 0.402, 0.727]),
    ("red_body", [0.700, 0.100, 0.050]),
    ("orange_body", [0.809, 0.426, 0.148]),
];

/// 変換は krbr_core::color に 1 つだけ置く（以前はここに 0..1 に収めない別の実装があった）
pub use krbr_core::color::srgb_to_linear;

impl ModelPart {
    pub fn linear_color(&self) -> Vec3 {
        srgb_to_linear(self.srgb)
    }

    /// "#rrggbb"
    pub fn hex(&self) -> String {
        let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
        format!("#{:02x}{:02x}{:02x}", b(self.srgb.x), b(self.srgb.y), b(self.srgb.z))
    }

    /// KiCad 標準モデルの色なら "metal_grey_pins" など
    pub fn kicad_color_name(&self) -> Option<&'static str> {
        if !self.has_color {
            return None;
        }
        KICAD_COLORS
            .iter()
            .find(|(_, c)| (self.srgb.x - c[0]).abs() < 0.004 && (self.srgb.y - c[1]).abs() < 0.004 && (self.srgb.z - c[2]).abs() < 0.004)
            .map(|(n, _)| *n)
    }

    /// 対応表の照合に使うキーの候補（優先順）：名前、"kicad:<色名>"、"#rrggbb"
    pub fn keys(&self) -> Vec<String> {
        let mut k = Vec::new();
        if !self.name.is_empty() {
            k.push(self.name.clone());
        }
        if let Some(c) = self.kicad_color_name() {
            k.push(format!("kicad:{c}"));
        }
        if self.has_color {
            k.push(self.hex());
        }
        k
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_in_priority_order() {
        let p = ModelPart { name: "Pins".into(), srgb: Vec3::new(0.824, 0.820, 0.781), alpha: 1.0, has_color: true };
        assert_eq!(p.keys(), vec!["Pins".to_string(), "kicad:metal_grey_pins".into(), "#d2d1c7".into()]);
        let q = ModelPart { name: String::new(), srgb: Vec3::ZERO, alpha: 1.0, has_color: false };
        assert!(q.keys().is_empty());
    }

    #[test]
    fn srgb_conversion() {
        assert!((srgb_to_linear(Vec3::splat(0.5)).x - 0.2140).abs() < 1e-3);
        assert_eq!(srgb_to_linear(Vec3::ZERO), Vec3::ZERO);
    }
}
