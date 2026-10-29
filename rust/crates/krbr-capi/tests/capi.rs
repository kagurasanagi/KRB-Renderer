//! C API を、DLL を読み込むアプリと同じ順で呼ぶ：材質・メッシュ・配置を作り、描いて、読み戻す。
//! GPU（Vulkan の ray tracing）が要るので #[ignore]。明示したときだけ走らせる：
//!   cargo test --release -p krbr-capi -- --ignored --nocapture

use std::ffi::{CStr, CString};

use krbr_renderer::*;

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn last_error() -> String {
    unsafe { CStr::from_ptr(krbr_last_error()) }.to_string_lossy().into_owned()
}

/// 失敗の理由が krbr_last_error に入る（GPU は要らない）
#[test]
fn errors_are_reported() {
    unsafe {
        let m = krbr_material_create(c("m").as_ptr());
        assert!(!m.is_null());
        assert_eq!(krbr_material_set_float(m, c("specular_roughness").as_ptr(), 0.2), 0);
        assert_eq!(krbr_material_set_float(m, c("no_such_param").as_ptr(), 1.0), -1);
        assert!(last_error().contains("no_such_param"), "{}", last_error());
        assert_eq!(krbr_material_set_string(m, c("conductor").as_ptr(), c("Au").as_ptr()), 0);
        assert_eq!(krbr_material_set_float(std::ptr::null_mut(), c("base_weight").as_ptr(), 1.0), -1);
        assert!(last_error().contains("NULL"));

        let s = krbr_scene_create();
        assert_eq!(krbr_scene_add_material(s, m), 0);
        let pos = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let bad = [0u32, 1, 3];
        assert_eq!(krbr_scene_add_mesh(s, pos.as_ptr(), std::ptr::null(), 3, bad.as_ptr(), 1, std::ptr::null()), -1);
        assert!(last_error().contains("out of range"));
        let idx = [0u32, 1, 2];
        assert_eq!(krbr_scene_add_mesh(s, pos.as_ptr(), std::ptr::null(), 3, idx.as_ptr(), 1, std::ptr::null()), 0);
        assert_eq!(krbr_scene_add_instance(s, 1, std::ptr::null(), 0, 0xFF), -1);
        assert_eq!(krbr_scene_add_instance(s, 0, std::ptr::null(), 0, 0xFF), 0);
        krbr_scene_destroy(s);
        krbr_material_destroy(m);
    }
}

/// 一様な環境光の中の、拡散反射の床と光る箱。積み増すと増え、ノイズ除去もでき、材質だけを送り直せる
#[test]
#[ignore = "needs a Vulkan GPU with ray tracing"]
fn renders_through_the_c_api() {
    unsafe {
        krbr_set_log_level(2);
        let r = krbr_renderer_create(0);
        assert!(!r.is_null(), "{}", last_error());
        println!("device: {}", CStr::from_ptr(krbr_renderer_device_name(r)).to_string_lossy());

        let floor = krbr_material_create(c("floor").as_ptr());
        krbr_material_set_color(floor, c("base_color").as_ptr(), 0.6, 0.6, 0.6);
        krbr_material_set_float(floor, c("specular_weight").as_ptr(), 0.0);
        let lamp = krbr_material_create(c("lamp").as_ptr());
        krbr_material_set_color(lamp, c("base_color").as_ptr(), 0.0, 0.0, 0.0);
        krbr_material_set_float(lamp, c("emission_luminance").as_ptr(), 2000.0);

        let s = krbr_scene_create();
        assert_eq!(krbr_scene_add_material(s, floor), 0);
        assert_eq!(krbr_scene_add_material(s, lamp), 1);
        // 床（材質 0）と箱（材質 1）を 1 つのメッシュに
        let mut pos: Vec<f32> = vec![-100.0, 0.0, -100.0, 100.0, 0.0, -100.0, 100.0, 0.0, 100.0, -100.0, 0.0, 100.0];
        let mut idx: Vec<u32> = vec![0, 2, 1, 0, 3, 2];
        let mut mats: Vec<u32> = vec![0, 0];
        let (lo, hi) = ([-10.0f32, 0.0, -10.0], [10.0f32, 20.0, 10.0]);
        let base = (pos.len() / 3) as u32;
        for k in 0..8 {
            pos.extend_from_slice(&[if k & 1 == 0 { lo[0] } else { hi[0] }, if k & 2 == 0 { lo[1] } else { hi[1] }, if k & 4 == 0 { lo[2] } else { hi[2] }]);
        }
        // 外から見て反時計回り（発光は表の面だけ）
        for q in [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]] {
            idx.extend_from_slice(&[base + q[0], base + q[1], base + q[2], base + q[0], base + q[2], base + q[3]]);
            mats.extend_from_slice(&[1, 1]);
        }
        let mesh = krbr_scene_add_mesh(s, pos.as_ptr(), std::ptr::null(), (pos.len() / 3) as u32, idx.as_ptr(), (idx.len() / 3) as u32, mats.as_ptr());
        assert_eq!(mesh, 0, "{}", last_error());
        assert_eq!(krbr_scene_add_instance(s, 0, std::ptr::null(), 0, 0xFF), 0);

        assert_eq!(krbr_renderer_set_scene(r, s), 0, "{}", last_error());
        assert_eq!(krbr_renderer_set_environment_color(r, 0.05, 0.05, 0.05), 0, "{}", last_error());
        let (w, h) = (160u32, 120u32);
        assert_eq!(krbr_renderer_set_resolution(r, w, h), 0);
        assert_eq!(krbr_renderer_set_camera(r, [0.0f32, 80.0, 150.0].as_ptr(), [0.0f32, 5.0, 0.0].as_ptr(), [0.0f32, 1.0, 0.0].as_ptr(), 40.0), 0, "{}", last_error());
        assert_eq!(krbr_renderer_set_float(r, c("max_depth").as_ptr(), 6.0), 0);
        assert_eq!(krbr_renderer_set_float(r, c("max_depth").as_ptr(), 1.5), -1);
        assert_eq!(krbr_renderer_set_string(r, c("integrator").as_ptr(), c("pt").as_ptr()), 0);
        assert_eq!(krbr_renderer_set_string(r, c("integrator").as_ptr(), c("nope").as_ptr()), -1);

        assert_eq!(krbr_renderer_render(r, 16), 16, "{}", last_error());
        assert_eq!(krbr_renderer_render(r, 16), 32);
        assert_eq!(krbr_renderer_samples(r), 32);
        let n = (w * h * 4) as usize;
        let mut img = vec![0.0f32; n];
        assert_eq!(krbr_renderer_read(r, 0, img.as_mut_ptr(), n), 0, "{}", last_error());
        assert_eq!(krbr_renderer_read(r, 0, img.as_mut_ptr(), n - 1), -1);
        // 光る箱は画面の中央、床は下の方。どちらも真っ黒ではない
        let px = |img: &[f32], x: u32, y: u32| img[((y * w + x) * 4) as usize];
        let center = px(&img, w / 2, h / 2);
        let floor_px = px(&img, w / 2, h - 5);
        println!("center {center}, floor {floor_px}");
        assert!(center > 1.0, "the emitter should be bright: {center}");
        assert!(floor_px > 0.0 && floor_px < center, "floor {floor_px}, emitter {center}");

        // 材質だけを送り直す：床を黒にすると床は暗くなり、累積は 0 から
        krbr_material_set_color(floor, c("base_color").as_ptr(), 0.0, 0.0, 0.0);
        assert_eq!(krbr_scene_set_material(s, 0, floor), 0);
        assert_eq!(krbr_renderer_update_materials(r, s), 0, "{}", last_error());
        assert_eq!(krbr_renderer_samples(r), 0);
        assert_eq!(krbr_renderer_render(r, 16), 16);
        let mut dark = vec![0.0f32; n];
        krbr_renderer_read(r, 0, dark.as_mut_ptr(), n);
        assert!(px(&dark, w / 2, h - 5) < 0.5 * floor_px, "black floor {} vs {floor_px}", px(&dark, w / 2, h - 5));

        // 発光を変えると、材質だけの送り直しはできない（set_scene で送り直す）
        krbr_material_set_float(lamp, c("emission_luminance").as_ptr(), 10.0);
        krbr_scene_set_material(s, 1, lamp);
        assert_eq!(krbr_renderer_update_materials(r, s), -1);
        assert_eq!(krbr_renderer_set_scene(r, s), 0, "{}", last_error());
        assert_eq!(krbr_renderer_render(r, 4), 4);

        if krbr_denoiser_available() != 0 {
            let mut den = vec![0.0f32; n];
            assert_eq!(krbr_renderer_denoise(r, 0, 1.0, den.as_mut_ptr(), n), 0, "{}", last_error());
            assert!(den.iter().all(|v| v.is_finite()));
        }

        krbr_scene_destroy(s);
        krbr_material_destroy(floor);
        krbr_material_destroy(lamp);
        krbr_renderer_destroy(r);
    }
}
