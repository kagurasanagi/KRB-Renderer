//! 環境マップの重点的サンプリング用の分布（輝度 × sinθ に比例）

use crate::image_io::ImageF;

#[derive(Debug, Clone, Default)]
pub struct EnvDistribution {
    pub width: u32,
    pub height: u32,
    /// height + 1
    pub marginal_cdf: Vec<f32>,
    /// height * (width + 1)
    pub conditional_cdf: Vec<f32>,
    /// width * height（(u,v) 上の密度。∫∫ pdf du dv = 1）
    pub pdf: Vec<f32>,
    pub integral: f64,
}

/// 分布は最大 max_width まで縮小して作る（放射輝度の参照は元の解像度のまま）
pub fn build_env_distribution(img: &ImageF, max_width: u32) -> EnvDistribution {
    let f = ((img.width + max_width - 1) / max_width).max(1);
    let w = (img.width / f).max(1);
    let h = (img.height / f).max(1);
    let mut func = vec![0.0f64; (w * h) as usize];
    for j in 0..h {
        let sin_theta = ((j as f64 + 0.5) / h as f64 * std::f64::consts::PI).sin();
        for i in 0..w {
            let mut sum = 0.0;
            for y in 0..f {
                for x in 0..f {
                    let sx = (i * f + x).min(img.width - 1) as usize;
                    let sy = (j * f + y).min(img.height - 1) as usize;
                    let p = &img.rgba[(sy * img.width as usize + sx) * 4..];
                    sum += 0.2126 * p[0].max(0.0) as f64 + 0.7152 * p[1].max(0.0) as f64 + 0.0722 * p[2].max(0.0) as f64;
                }
            }
            func[(j * w + i) as usize] = sum / (f * f) as f64 * sin_theta;
        }
    }
    let mut d = EnvDistribution { width: w, height: h, ..Default::default() };
    d.conditional_cdf = vec![0.0; (h * (w + 1)) as usize];
    let mut row_integral = vec![0.0f64; h as usize];
    for j in 0..h as usize {
        let cdf = &mut d.conditional_cdf[j * (w as usize + 1)..(j + 1) * (w as usize + 1)];
        let mut acc = 0.0;
        let mut tmp = vec![0.0f64; w as usize + 1];
        for i in 0..w as usize {
            acc += func[j * w as usize + i] / w as f64;
            tmp[i + 1] = acc;
        }
        row_integral[j] = acc;
        for i in 0..=w as usize {
            cdf[i] = if acc > 0.0 { (tmp[i] / acc) as f32 } else { i as f32 / w as f32 };
        }
        cdf[w as usize] = 1.0;
    }
    d.marginal_cdf = vec![0.0; h as usize + 1];
    let mut acc = 0.0;
    let mut tmp = vec![0.0f64; h as usize + 1];
    for j in 0..h as usize {
        acc += row_integral[j] / h as f64;
        tmp[j + 1] = acc;
    }
    d.integral = acc;
    for j in 0..=h as usize {
        d.marginal_cdf[j] = if acc > 0.0 { (tmp[j] / acc) as f32 } else { j as f32 / h as f32 };
    }
    d.marginal_cdf[h as usize] = 1.0;
    d.pdf = func.iter().map(|&v| if acc > 0.0 { (v / acc) as f32 } else { 1.0 }).collect();
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_map_has_sin_weighted_pdf() {
        let img = ImageF { width: 8, height: 4, rgba: vec![1.0; 8 * 4 * 4] };
        let d = build_env_distribution(&img, 2048);
        assert_eq!((d.width, d.height), (8, 4));
        assert!((d.marginal_cdf[4] - 1.0).abs() < 1e-6);
        // 中央の行（赤道）の方が極より密度が高い
        assert!(d.pdf[(1 * 8) as usize] > d.pdf[0]);
        let mean: f32 = d.pdf.iter().sum::<f32>() / d.pdf.len() as f32;
        assert!((mean - 1.0).abs() < 1e-4);
    }
}
