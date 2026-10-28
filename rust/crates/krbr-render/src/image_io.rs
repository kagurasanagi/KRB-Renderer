//! 画像の入出力：.hdr / .exr（と、sRGB の PNG / JPG など）の読み込み、EXR（リニア）と PNG（ACES でトーンマッピング、sRGB）の書き出し

use std::path::Path;

use krbr_scene::material::{linear_to_srgb_f, srgb_to_linear_f};

/// RGBA float のリニア画像
#[derive(Debug, Clone, Default)]
pub struct ImageF {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<f32>,
}

impl ImageF {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, rgba: vec![0.0; (width * height * 4) as usize] }
    }

    /// アルファ（A）が 1 でない画素があるか（背景を透明にした画像。色はアルファを掛けた値）
    pub fn has_alpha(&self) -> bool {
        self.rgba.chunks_exact(4).any(|p| p[3] < 1.0)
    }

    /// src のアルファを写す（ノイズ除去した画像に、元の画像のアルファを付ける）。大きさが違えば何もしない
    pub fn copy_alpha_from(&mut self, src: &ImageF) {
        if (self.width, self.height) != (src.width, src.height) {
            return;
        }
        for (d, s) in self.rgba.chunks_exact_mut(4).zip(src.rgba.chunks_exact(4)) {
            d[3] = s[3];
        }
    }
}

pub fn load_hdr_image(p: &Path) -> Result<ImageF, String> {
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    if ext == "exr" {
        let img = exr::prelude::read_first_rgba_layer_from_file(
            p,
            |res, _| ImageF::new(res.width() as u32, res.height() as u32),
            |img, pos, (r, g, b, a): (f32, f32, f32, f32)| {
                let i = (pos.y() * img.width as usize + pos.x()) * 4;
                img.rgba[i] = r;
                img.rgba[i + 1] = g;
                img.rgba[i + 2] = b;
                img.rgba[i + 3] = a;
            },
        )
        .map_err(|e| format!("EXR: {e}"))?;
        return Ok(img.layer_data.channel_data.pixels);
    }
    let dyn_img = image::open(p).map_err(|e| format!("{e}"))?;
    // .hdr などの浮動小数の画像はリニア。PNG / JPG などの整数の画像は sRGB で符号化してあるので、リニアに戻す
    // （戻さないと、環境マップの中間調が明るすぎ、重点的サンプリングの分布も歪む）
    let float = matches!(dyn_img, image::DynamicImage::ImageRgb32F(_) | image::DynamicImage::ImageRgba32F(_));
    let rgba = dyn_img.to_rgba32f();
    let (w, h) = (rgba.width(), rgba.height());
    let mut data = rgba.into_raw();
    if !float {
        for px in data.chunks_exact_mut(4) {
            for c in &mut px[..3] {
                *c = srgb_to_linear_f(*c);
            }
        }
    }
    Ok(ImageF { width: w, height: h, rgba: data })
}

pub fn save_exr(p: &Path, img: &ImageF) -> Result<(), String> {
    use exr::prelude::*;
    let w = img.width as usize;
    let image = Image::from_channels(
        (w, img.height as usize),
        SpecificChannels::rgba(|pos: Vec2<usize>| {
            let i = (pos.y() * w + pos.x()) * 4;
            (img.rgba[i], img.rgba[i + 1], img.rgba[i + 2], img.rgba[i + 3])
        }),
    );
    image.write().to_file(p).map_err(|e| format!("EXR: {e}"))
}

/// Stephen Hill の ACES fitted（sRGB/Rec.709 リニア入力）
pub fn aces_fitted(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let i0 = 0.59719 * r + 0.35458 * g + 0.04823 * b;
    let i1 = 0.07600 * r + 0.90834 * g + 0.01566 * b;
    let i2 = 0.02840 * r + 0.13383 * g + 0.83777 * b;
    let rrt = |v: f32| {
        let a = v * (v + 0.0245786) - 0.000090537;
        let b2 = v * (0.983729 * v + 0.4329510) + 0.238081;
        a / b2
    };
    let (o0, o1, o2) = (rrt(i0), rrt(i1), rrt(i2));
    (1.60475 * o0 - 0.53108 * o1 - 0.07367 * o2, -0.10208 * o0 + 1.10813 * o1 - 0.00605 * o2, -0.00327 * o0 - 0.07276 * o1 + 1.07602 * o2)
}

/// 露出とグロー（crate::glow）をかけ、ACES でトーンマッピングして sRGB の 8bit にする（表示の shaders/tonemap.comp と同じ式）。
/// アルファのある画像（背景を透明にした画像。色はアルファを掛けた値）は、アルファで割ってからトーンマッピングし、
/// アルファを付けた PNG 用の値（ストレートアルファ）にする。グローは透明な所には広げない（物の上にだけ足す）
pub fn tonemap_rgba8(img: &ImageF, exposure_ev: f32, glow: &crate::glow::GlowParams) -> Vec<u8> {
    let scale = 2f32.powf(exposure_ev);
    let n = (img.width * img.height) as usize;
    let mut lin: Vec<[f32; 3]> = (0..n).map(|i| [img.rgba[i * 4] * scale, img.rgba[i * 4 + 1] * scale, img.rgba[i * 4 + 2] * scale].map(|v| v.max(0.0))).collect();
    let alpha: Option<Vec<f32>> = img.has_alpha().then(|| img.rgba.chunks_exact(4).map(|p| p[3].clamp(0.0, 1.0)).collect());
    match &alpha {
        None => crate::glow::apply(&mut lin, img.width, img.height, glow),
        Some(a) => {
            // グローはアルファを掛けた色から作り（表示と同じ）、アルファで割った色に足す
            let mut with_glow = lin.clone();
            crate::glow::apply(&mut with_glow, img.width, img.height, glow);
            for i in 0..n {
                for c in 0..3 {
                    let g = with_glow[i][c] - lin[i][c];
                    lin[i][c] = if a[i] > 0.0 { lin[i][c] / a[i] + g } else { 0.0 };
                }
            }
        }
    }
    let mut px = vec![0u8; n * 4];
    for i in 0..n {
        let (r, g, b) = aces_fitted(lin[i][0], lin[i][1], lin[i][2]);
        // 量子化の縞を減らす小さなディザ
        let d = ((((i as u32).wrapping_mul(2654435761)) & 255) as f32 / 255.0 - 0.5) / 255.0;
        px[i * 4] = ((linear_to_srgb_f(r) + d).clamp(0.0, 1.0) * 255.0).round() as u8;
        px[i * 4 + 1] = ((linear_to_srgb_f(g) + d).clamp(0.0, 1.0) * 255.0).round() as u8;
        px[i * 4 + 2] = ((linear_to_srgb_f(b) + d).clamp(0.0, 1.0) * 255.0).round() as u8;
        px[i * 4 + 3] = match &alpha {
            Some(a) => (a[i] * 255.0).round() as u8,
            None => 255,
        };
    }
    px
}

pub fn save_png_rgba8(p: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    // 書き出し中のファイルを読まれないよう、一時ファイルに書いてから置き換える
    let tmp = p.with_extension("png.tmp");
    image::save_buffer_with_format(&tmp, rgba, w, h, image::ColorType::Rgba8, image::ImageFormat::Png).map_err(|e| format!("{e}"))?;
    std::fs::rename(&tmp, p).map_err(|e| format!("{e}"))
}

/// 圧縮を軽くした PNG（保存のたびに書き直す watch 用。1080p で数十 ms）。一時ファイルに書いてから置き換える
pub fn save_png_rgba8_fast(p: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let tmp = p.with_extension("png.tmp");
    let file = std::fs::File::create(&tmp).map_err(|e| format!("{e}"))?;
    let enc = PngEncoder::new_with_quality(std::io::BufWriter::new(file), CompressionType::Fast, FilterType::Sub);
    enc.write_image(rgba, w, h, image::ExtendedColorType::Rgba8).map_err(|e| format!("{e}"))?;
    std::fs::rename(&tmp, p).map_err(|e| format!("{e}"))
}

pub fn save_png_tonemapped(p: &Path, img: &ImageF, exposure_ev: f32, glow: &crate::glow::GlowParams) -> Result<(), String> {
    save_png_rgba8(p, img.width, img.height, &tonemap_rgba8(img, exposure_ev, glow))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exr_round_trip() {
        let dir = std::env::temp_dir().join("krbr_rust_image_test");
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = ImageF::new(4, 3);
        for (i, v) in img.rgba.iter_mut().enumerate() {
            *v = i as f32 * 0.25;
        }
        let p = dir.join("t.exr");
        save_exr(&p, &img).unwrap();
        let back = load_hdr_image(&p).unwrap();
        assert_eq!((back.width, back.height), (4, 3));
        assert!((back.rgba[5] - 1.25).abs() < 1e-6);
        let png = dir.join("t.png");
        save_png_tonemapped(&png, &img, 0.0, &crate::glow::GlowParams::OFF).unwrap();
        assert!(png.exists());
    }

    #[test]
    fn aces_maps_white_below_one() {
        // RRT + ODT の近似（表示の shaders/tonemap.comp と同じ式）では、白 1.0 は約 0.62 になる
        let (r, g, b) = aces_fitted(1.0, 1.0, 1.0);
        assert!(r < 1.0 && r > 0.55 && (r - g).abs() < 0.02 && (g - b).abs() < 0.02, "{r} {g} {b}");
    }

    /// PNG など整数の画像は sRGB なので、リニアに戻して読む（.hdr / .exr はそのまま）
    #[test]
    fn ldr_images_are_read_as_linear() {
        let dir = std::env::temp_dir().join(format!("krbr_rust_ldr_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("grey.png");
        image::save_buffer(&p, &[128u8, 128, 128, 255], 1, 1, image::ColorType::Rgba8).unwrap();
        let img = load_hdr_image(&p).unwrap();
        assert!((img.rgba[0] - srgb_to_linear_f(128.0 / 255.0)).abs() < 1e-4 && (img.rgba[0] - 0.2158).abs() < 1e-3, "{}", img.rgba[0]);
        assert_eq!(img.rgba[3], 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
