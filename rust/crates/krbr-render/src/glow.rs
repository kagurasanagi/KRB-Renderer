//! グロー（光のにじみ。ブルーム）：明るい画素の光を周りへ広げて足す。点灯した LED などが「光って見える」ようにする。
//! 露出をかけた後の明るさがしきい値（白の付近）を超えた分だけを、解像度を半分ずつにした段（ピラミッド）でぼかして重ねる
//! （Jimenez 2014 "Next Generation Post Processing in Call of Duty" の方式。Blender の Glare（Fog Glow）や Unreal の Bloom と同じ考え方）。
//! 表示（shaders/glow.comp と tonemap.comp）と、画像の保存（image_io の tonemap_rgba8）で同じ式を使うこと

/// しきい値の前後をなめらかにつなぐ幅（しきい値に対する割合）
pub const KNEE: f32 = 0.5;
/// 段の数の上限
pub const MAX_LEVELS: usize = 8;

/// 段の大きさ（1 段目が半分の解像度）。一番小さい段の短い辺が 4 画素未満にならない数だけ作る（ぼかしの広がりが画像の大きさに比例する）
pub fn level_sizes(width: u32, height: u32) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let (mut w, mut h) = (width, height);
    while out.len() < MAX_LEVELS {
        let (nw, nh) = (w.div_ceil(2), h.div_ceil(2));
        if nw.min(nh) < 4 {
            break;
        }
        out.push((nw, nh));
        (w, h) = (nw, nh);
    }
    out
}

/// グローの設定
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlowParams {
    /// 強さ（にじんだ光の量。しきい値を超えた分に対する割合）。0 でなし
    pub strength: f32,
    /// 露出をかけた後のしきい値（RGB の最も明るい成分。1 が画面の白）。これを超えた分だけがにじむ。
    /// 輝度で比べると、色の濃い光（赤い LED など）はトーンマッピングで白く飛ぶまでにじまないので、成分の最大で比べる（Unity の Bloom と同じ）
    pub threshold: f32,
    /// 広がり（0..1）。段を 1 つ大きくするごとの重み。小さいと光の近くに締まり、1 で画像の大きさ近くまで広がる
    pub spread: f32,
}

impl Default for GlowParams {
    fn default() -> Self {
        Self { strength: 1.0, threshold: 4.0, spread: 0.7 }
    }
}

impl GlowParams {
    pub const OFF: Self = Self { strength: 0.0, threshold: 4.0, spread: 0.7 };

    pub fn is_on(&self) -> bool {
        self.strength > 0.0
    }

    /// 段ごとの重み（spread の累乗）の合計。最後に割って、にじんだ光の総量をしきい値を超えた分 × 強さにそろえる
    pub fn weight_sum(&self, levels: usize) -> f32 {
        (0..levels).map(|i| self.spread.powi(i as i32)).sum::<f32>().max(1e-6)
    }
}

/// しきい値を超えた分（なめらかなひざ付き）。色合いは保つ
pub fn prefilter(c: [f32; 3], threshold: f32) -> [f32; 3] {
    let knee = KNEE * threshold;
    let l = c[0].max(c[1]).max(c[2]);
    let soft = (l - threshold + knee).clamp(0.0, 2.0 * knee);
    let soft = soft * soft / (4.0 * knee).max(1e-6);
    let k = soft.max(l - threshold) / l.max(1e-4);
    [c[0] * k, c[1] * k, c[2] * k]
}

struct Plane {
    w: u32,
    h: u32,
    px: Vec<[f32; 3]>,
}

impl Plane {
    fn at(&self, x: i64, y: i64) -> [f32; 3] {
        let x = x.clamp(0, self.w as i64 - 1) as usize;
        let y = y.clamp(0, self.h as i64 - 1) as usize;
        self.px[y * self.w as usize + x]
    }
    /// 双線形（座標は画素の中心が整数）
    fn bilinear(&self, fx: f32, fy: f32) -> [f32; 3] {
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (a, b, c, d) = (self.at(x0, y0), self.at(x0 + 1, y0), self.at(x0, y0 + 1), self.at(x0 + 1, y0 + 1));
        std::array::from_fn(|i| (a[i] * (1.0 - tx) + b[i] * tx) * (1.0 - ty) + (c[i] * (1.0 - tx) + d[i] * tx) * ty)
    }
}

/// 1 段下へ：4×4 の (1,3,3,1)/8 の重みで縮める
fn down(src: &Plane, w: u32, h: u32, f: impl Fn([f32; 3]) -> [f32; 3]) -> Plane {
    const W: [f32; 4] = [0.125, 0.375, 0.375, 0.125];
    let mut px = vec![[0.0; 3]; (w * h) as usize];
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            let mut s = [0.0f32; 3];
            for j in 0..4 {
                for i in 0..4 {
                    let c = f(src.at(2 * x - 1 + i, 2 * y - 1 + j));
                    let wt = W[i as usize] * W[j as usize];
                    for k in 0..3 {
                        s[k] += c[k] * wt;
                    }
                }
            }
            px[(y * w as i64 + x) as usize] = s;
        }
    }
    Plane { w, h, px }
}

/// 1 段上の座標（画素の中心）
fn up_coord(x: u32) -> f32 {
    (x as f32 + 0.5) * 0.5 - 0.5
}

/// グローを足す。rgb は露出をかけたリニアの RGB（width × height）。強さが 0 以下なら何もしない
pub fn apply(rgb: &mut [[f32; 3]], width: u32, height: u32, p: &GlowParams) {
    let sizes = level_sizes(width, height);
    if !p.is_on() || sizes.is_empty() {
        return;
    }
    let src = Plane { w: width, h: height, px: rgb.to_vec() };
    let mut levels: Vec<Plane> = Vec::with_capacity(sizes.len());
    for (i, &(w, h)) in sizes.iter().enumerate() {
        let l = if i == 0 { down(&src, w, h, |c| prefilter(c, p.threshold)) } else { down(&levels[i - 1], w, h, |c| c) };
        levels.push(l);
    }
    // 小さい段から順に、広がりの重みをかけて上の段へ広げながら足す
    for i in (0..levels.len() - 1).rev() {
        let (lo, hi) = levels.split_at_mut(i + 1);
        let (dst, small) = (&mut lo[i], &hi[0]);
        for y in 0..dst.h {
            for x in 0..dst.w {
                let u = small.bilinear(up_coord(x), up_coord(y));
                let d = &mut dst.px[(y * dst.w + x) as usize];
                for k in 0..3 {
                    d[k] += u[k] * p.spread;
                }
            }
        }
    }
    let k = p.strength / p.weight_sum(sizes.len());
    let top = &levels[0];
    for y in 0..height {
        for x in 0..width {
            let g = top.bilinear(up_coord(x), up_coord(y));
            let p = &mut rgb[(y * width + x) as usize];
            for c in 0..3 {
                p[c] += g[c] * k;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_follow_the_image_size() {
        assert_eq!(level_sizes(1920, 1080).len(), 8);
        assert_eq!(level_sizes(1920, 1080)[0], (960, 540));
        assert_eq!(level_sizes(64, 64), vec![(32, 32), (16, 16), (8, 8), (4, 4)]);
        assert!(level_sizes(6, 6).is_empty());
    }

    #[test]
    fn only_light_above_the_threshold_spreads_and_keeps_its_color() {
        let (w, h) = (64u32, 64u32);
        let mut img = vec![[0.5f32, 0.5, 0.5]; (w * h) as usize];
        let p = GlowParams { strength: 1.0, threshold: 1.0, spread: 1.0 };
        apply(&mut img, w, h, &p);
        assert!(img.iter().all(|p| *p == [0.5, 0.5, 0.5]), "below the threshold nothing changes");
        // 赤い点：周りが赤くにじむ（緑・青は増えない）
        let mut img = vec![[0.0f32; 3]; (w * h) as usize];
        img[(32 * w + 32) as usize] = [400.0, 0.0, 0.0];
        apply(&mut img, w, h, &p);
        let near = img[(32 * w + 36) as usize];
        let far = img[(32 * w + 50) as usize];
        assert!(near[0] > far[0] && far[0] > 0.0, "{near:?} {far:?}");
        assert!(near[1] == 0.0 && near[2] == 0.0);
        // にじみの総量は、しきい値を超えた分 × 強さ（端で切れる分を除いて保たれる）
        let added: f32 = img.iter().map(|p| p[0]).sum::<f32>() - 400.0;
        let excess = prefilter([400.0, 0.0, 0.0], 1.0)[0];
        assert!((added / excess - 1.0).abs() < 0.05, "{added} vs {excess}");
    }

    #[test]
    fn threshold_and_spread_are_adjustable() {
        let (w, h) = (128u32, 128u32);
        let run = |level: f32, p: GlowParams| {
            let mut img = vec![[0.0f32; 3]; (w * h) as usize];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                img[((64 + dy) * w + 64 + dx) as usize] = [level; 3];
            }
            apply(&mut img, w, h, &p);
            img
        };
        // しきい値より暗い光はにじまない
        let p = GlowParams { strength: 1.0, threshold: 4.0, spread: 0.7 };
        assert!(run(1.5, p).iter().filter(|c| c[0] > 0.0).count() == 4);
        assert!(run(50.0, p).iter().filter(|c| c[0] > 0.0).count() > 4);
        // 広がりを小さくすると、光の近くに集まる（総量は同じ）
        let near_far = |spread: f32| {
            let img = run(50.0, GlowParams { strength: 1.0, threshold: 1.0, spread });
            let (near, far) = (img[(64 * w + 68) as usize][0], img[(64 * w + 100) as usize][0]);
            let total: f32 = img.iter().map(|c| c[0]).sum::<f32>() - 4.0 * 50.0;
            (near / far, total)
        };
        let (tight, t1) = near_far(0.3);
        let (wide, t2) = near_far(1.0);
        assert!(tight > 4.0 * wide, "{tight} vs {wide}");
        assert!((t1 / t2 - 1.0).abs() < 0.05, "{t1} vs {t2}");
    }
}
