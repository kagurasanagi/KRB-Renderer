//! レジスト（ソルダーマスク）の色のプリセット：KiCad の stackup の色名と、基板メーカーが選ばせる色

use glam::Vec3;

#[derive(Debug, Clone)]
pub struct MaskPreset {
    /// 例 "jlcpcb-purple"（照合は大小・区切りを無視）
    pub name: &'static str,
    /// 銅の上で見えるレジストの色（sRGB）
    pub srgb: Vec3,
    /// コートの粗さ（光沢 0.1〜、つや消し 0.5〜）
    pub roughness: f32,
    /// 顔料自身が光を散らす割合（0 = 透明なコート、1 = 不透明）
    pub scatter: f32,
    /// このレジストに合わせたシルクの色（黄・白のレジストでは黒いシルク）
    pub silk_srgb: Vec3,
    /// 説明（英語。ビューアでは翻訳 mask-note-<name> があればそちら）
    pub note: &'static str,
}

const W: Vec3 = Vec3::new(0.93, 0.93, 0.90);
const K: Vec3 = Vec3::new(0.08, 0.08, 0.08);

// 色は「銅の上で見えるレジストの色」（sRGB）。メーカーの色は写真からの目分量で、実測ではない
const PRESETS: &[MaskPreset] = &[
    MaskPreset { name: "green", srgb: Vec3::new(0.05, 0.42, 0.16), roughness: 0.18, scatter: 0.3, silk_srgb: W, note: "KiCad / the common green" },
    MaskPreset { name: "red", srgb: Vec3::new(0.62, 0.07, 0.06), roughness: 0.18, scatter: 0.4, silk_srgb: W, note: "KiCad" },
    MaskPreset { name: "blue", srgb: Vec3::new(0.05, 0.18, 0.52), roughness: 0.18, scatter: 0.4, silk_srgb: W, note: "KiCad" },
    MaskPreset { name: "yellow", srgb: Vec3::new(0.80, 0.68, 0.10), roughness: 0.18, scatter: 0.5, silk_srgb: K, note: "KiCad" },
    MaskPreset { name: "purple", srgb: Vec3::new(0.30, 0.10, 0.42), roughness: 0.18, scatter: 0.4, silk_srgb: W, note: "KiCad" },
    MaskPreset { name: "black", srgb: Vec3::new(0.04, 0.04, 0.04), roughness: 0.18, scatter: 0.7, silk_srgb: W, note: "KiCad" },
    MaskPreset { name: "white", srgb: Vec3::new(0.90, 0.90, 0.88), roughness: 0.22, scatter: 0.9, silk_srgb: K, note: "KiCad" },
    MaskPreset { name: "matte-black", srgb: Vec3::new(0.05, 0.05, 0.05), roughness: 0.55, scatter: 0.8, silk_srgb: W, note: "KiCad" },
    MaskPreset { name: "jlcpcb-green", srgb: Vec3::new(0.08, 0.40, 0.19), roughness: 0.18, scatter: 0.3, silk_srgb: W, note: "JLCPCB Green: a slightly lighter green, glossy" },
    MaskPreset { name: "jlcpcb-purple", srgb: Vec3::new(0.33, 0.12, 0.46), roughness: 0.18, scatter: 0.4, silk_srgb: W, note: "JLCPCB Purple: a bluish purple" },
    MaskPreset { name: "jlcpcb-red", srgb: Vec3::new(0.60, 0.08, 0.07), roughness: 0.18, scatter: 0.4, silk_srgb: W, note: "JLCPCB Red" },
    MaskPreset { name: "jlcpcb-yellow", srgb: Vec3::new(0.85, 0.70, 0.10), roughness: 0.18, scatter: 0.5, silk_srgb: K, note: "JLCPCB Yellow" },
    MaskPreset { name: "jlcpcb-blue", srgb: Vec3::new(0.05, 0.20, 0.50), roughness: 0.18, scatter: 0.4, silk_srgb: W, note: "JLCPCB Blue" },
    MaskPreset { name: "jlcpcb-white", srgb: Vec3::new(0.92, 0.92, 0.88), roughness: 0.25, scatter: 0.9, silk_srgb: K, note: "JLCPCB White: nearly opaque" },
    MaskPreset { name: "jlcpcb-black", srgb: Vec3::new(0.06, 0.06, 0.06), roughness: 0.15, scatter: 0.7, silk_srgb: W, note: "JLCPCB Black: glossy black" },
    MaskPreset { name: "jlcpcb-matte-black", srgb: Vec3::new(0.07, 0.07, 0.07), roughness: 0.55, scatter: 0.8, silk_srgb: W, note: "JLCPCB Matte Black" },
    MaskPreset { name: "jlcpcb-matte-green", srgb: Vec3::new(0.10, 0.38, 0.20), roughness: 0.50, scatter: 0.5, silk_srgb: W, note: "JLCPCB Matte Green" },
];

pub fn mask_presets() -> &'static [MaskPreset] {
    PRESETS
}

/// 大小・区切り（空白、ハイフン、下線、コロン）を無視して比べる
fn normalize(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, ' ' | '-' | '_' | ':')).flat_map(|c| c.to_lowercase()).collect()
}

/// 名前で探す。"JLCPCB Purple" / "jlcpcb:purple" / "jlcpcb_purple" も同じ扱い
pub fn find_mask_preset(name: &str) -> Option<&'static MaskPreset> {
    let key = normalize(name);
    if key.is_empty() {
        return None;
    }
    PRESETS.iter().find(|p| normalize(p.name) == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_ignores_case_and_separators() {
        assert_eq!(find_mask_preset("JLCPCB Purple").unwrap().name, "jlcpcb-purple");
        assert_eq!(find_mask_preset("jlcpcb:white").unwrap().silk_srgb, K);
        assert!(find_mask_preset("nope").is_none());
        assert_eq!(find_mask_preset("Green").unwrap().name, "green");
    }
}
