//! 金属の複素屈折率 n + ik（測定値の表は conductor_data.rs。scripts/gen_conductors.py が作る）。
//! 分光モードでは GPU が表から波長ごとの厳密なフレネルを求める。RGB モードでは、この表から求めた
//! 正面の反射率 F0 と、OpenPBR の F82-tint（specular_color）で近似する

use glam::Vec3;

use crate::conductor_data::TABLES;
pub use crate::conductor_data::{LAMBDA_MIN, LAMBDA_STEP, SAMPLES};
use crate::spectrum;

/// F82-tint の基準の角度の余弦（OpenPBR：μ̄ = 1/7、約 82°）
pub const F82_COS: f32 = 1.0 / 7.0;

/// 金属の測定値の表 1 つ
#[derive(Debug, Clone, Copy)]
pub struct Conductor {
    /// 表の番号（GPU の表の並びと同じ）
    pub index: usize,
    /// 材質の TOML の [krbr] conductor に書く名前（"Au" など）
    pub id: &'static str,
    /// 出典
    pub source: &'static str,
}

/// 表の数
pub fn count() -> usize {
    TABLES.len()
}

pub fn all() -> impl Iterator<Item = Conductor> {
    TABLES.iter().enumerate().map(|(index, t)| Conductor { index, id: t.0, source: t.1 })
}

pub fn find(id: &str) -> Option<Conductor> {
    all().find(|c| c.id == id)
}

pub fn get(index: usize) -> Option<Conductor> {
    all().nth(index)
}

impl Conductor {
    /// 波長 λ（nm）での (n, k)。表の間は線形補間、範囲の外は端の値
    pub fn nk(&self, lambda_nm: f32) -> (f32, f32) {
        let t = &TABLES[self.index];
        let x = ((lambda_nm - LAMBDA_MIN) / LAMBDA_STEP).clamp(0.0, (SAMPLES - 1) as f32);
        let i = (x as usize).min(SAMPLES - 2);
        let f = x - i as f32;
        (t.2[i] + f * (t.2[i + 1] - t.2[i]), t.3[i] + f * (t.3[i + 1] - t.3[i]))
    }

    /// 反射率のスペクトルを、平坦な白い光（E）の下で見た色（リニア sRGB）にする。分光モードで描いた色と同じになる
    fn reflectance_rgb(&self, cos: f32) -> Vec3 {
        let xyz = spectrum::integrate_xyz(|l| {
            let (n, k) = self.nk(l);
            fresnel_conductor(cos, n, k)
        });
        (spectrum::xyz_to_linear_srgb() * xyz).clamp(Vec3::ZERO, Vec3::ONE)
    }

    /// RGB モードの (base_color = F0, specular_color = F82-tint)。F82-tint は「82° での反射率 ÷ F0 からの Schlick の値」
    pub fn f0_and_f82_tint(&self) -> (Vec3, Vec3) {
        let f0 = self.reflectance_rgb(1.0);
        let f82 = self.reflectance_rgb(F82_COS);
        let schlick = f0 + (Vec3::ONE - f0) * (1.0 - F82_COS).powi(5);
        let tint = (f82 / schlick.max(Vec3::splat(1e-6))).clamp(Vec3::ZERO, Vec3::ONE);
        (f0, tint)
    }

    /// GPU へ送る表（波長ごとに n, k の順）
    pub fn gpu_table() -> Vec<[f32; 2]> {
        TABLES.iter().flat_map(|t| (0..SAMPLES).map(move |i| [t.2[i], t.3[i]])).collect()
    }
}

/// 空気から金属（複素屈折率 n + ik）へ入る光の、偏光していない光の反射率（厳密なフレネルの式）
pub fn fresnel_conductor(cos_i: f32, n: f32, k: f32) -> f32 {
    #[derive(Clone, Copy)]
    struct C(f32, f32);
    impl C {
        fn mul(self, o: C) -> C {
            C(self.0 * o.0 - self.1 * o.1, self.0 * o.1 + self.1 * o.0)
        }
        fn div(self, o: C) -> C {
            let d = o.0 * o.0 + o.1 * o.1;
            C((self.0 * o.0 + self.1 * o.1) / d, (self.1 * o.0 - self.0 * o.1) / d)
        }
        fn sub(self, o: C) -> C {
            C(self.0 - o.0, self.1 - o.1)
        }
        fn add(self, o: C) -> C {
            C(self.0 + o.0, self.1 + o.1)
        }
        fn sqrt(self) -> C {
            let r = (self.0 * self.0 + self.1 * self.1).sqrt();
            let re = (0.5 * (r + self.0)).max(0.0).sqrt();
            let im = (0.5 * (r - self.0)).max(0.0).sqrt();
            C(re, if self.1 < 0.0 { -im } else { im })
        }
        fn norm2(self) -> f32 {
            self.0 * self.0 + self.1 * self.1
        }
    }
    let ci = cos_i.clamp(0.0, 1.0);
    let eta = C(n, k);
    let sin2 = 1.0 - ci * ci;
    // cosθt = √(1 − sin²θi / η²)
    let ct = C(1.0, 0.0).sub(C(sin2, 0.0).div(eta.mul(eta))).sqrt();
    let c = C(ci, 0.0);
    let r_parl = eta.mul(c).sub(ct).div(eta.mul(c).add(ct));
    let r_perp = c.sub(eta.mul(ct)).div(c.add(eta.mul(ct)));
    0.5 * (r_parl.norm2() + r_perp.norm2())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresnel_limits() {
        // 正面：((n−1)² + k²) / ((n+1)² + k²)
        let (n, k) = (0.2f32, 3.0f32);
        let f0 = ((n - 1.0).powi(2) + k * k) / ((n + 1.0).powi(2) + k * k);
        assert!((fresnel_conductor(1.0, n, k) - f0).abs() < 1e-5);
        // かすめる角度では 1 に近づく。k = 0 なら誘電体の式と同じ
        assert!(fresnel_conductor(1e-4, n, k) > 0.999);
        assert!((fresnel_conductor(1.0, 1.5, 0.0) - 0.04).abs() < 1e-5);
    }

    #[test]
    fn table_lookup_and_colors() {
        let au = find("Au").unwrap();
        assert_eq!(get(au.index).unwrap().id, "Au");
        let (n, k) = au.nk(550.0);
        assert!((n - 0.424).abs() < 0.01 && (k - 2.472).abs() < 0.01, "{n} {k}");
        // 金は赤・緑が高く青が低い。銀はどの成分も高い
        let (f0, tint) = au.f0_and_f82_tint();
        assert!(f0.x > 0.9 && f0.z < 0.45 && f0.x > f0.y && f0.y > f0.z, "{f0}");
        assert!(tint.max_element() <= 1.0 && tint.min_element() > 0.5, "{tint}");
        let (ag, _) = find("Ag").unwrap().f0_and_f82_tint();
        assert!(ag.min_element() > 0.9, "{ag}");
        // 全部の表：F0 が 0..1 に収まり、灰色に近い金属（クロム）は成分の差が小さい
        for c in all() {
            let (f0, t) = c.f0_and_f82_tint();
            assert!(f0.min_element() > 0.2 && f0.max_element() <= 1.0, "{}: {f0}", c.id);
            assert!(t.min_element() >= 0.0 && t.max_element() <= 1.0, "{}: {t}", c.id);
        }
        let (cr, _) = find("Cr").unwrap().f0_and_f82_tint();
        assert!(cr.max_element() - cr.min_element() < 0.08, "{cr}");
        assert_eq!(Conductor::gpu_table().len(), count() * SAMPLES);
    }
}
