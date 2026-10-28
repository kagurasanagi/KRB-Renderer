//! VRML の読み込みを、実物の KiCad 標準モデルで確かめる：同じ名前の STEP（インストールした KiCad の 3D ライブラリ）と、
//! 外接箱（単位と向き）と、KiCad の色の名前（材質の既定の割り当ての鍵）がそろうこと。
//! KiCad 9 以降の 3D ライブラリには .wrl が無いので、KiCad 8 のライブラリ（kicad-packages3D の 8.0.0）から .wrl を
//! 幾つか取ってきたフォルダを KRBR_TEST_VRML_DIR で渡す。STEP 変換器 DLL も要る：
//!   set KRBR_TEST_VRML_DIR=D:\tmp\krbr_vrml_samples
//!   cargo test --release -p krbr-model --test vrml_vs_step -- --ignored --nocapture

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use krbr_model::{MeshParams, ModelLibrary, ModelMesh};

/// インストールした KiCad の 3D ライブラリから、名前の同じ STEP を探す
fn installed_step(lib: &str, stem: &str) -> Option<PathBuf> {
    let pf = std::env::var("ProgramFiles").ok()?;
    let mut versions: Vec<PathBuf> = std::fs::read_dir(Path::new(&pf).join("KiCad")).ok()?.flatten().map(|e| e.path()).collect();
    versions.sort();
    versions.into_iter().rev().map(|v| v.join("share/kicad/3dmodels").join(lib).join(format!("{stem}.step"))).find(|p| p.is_file())
}

fn bounds(m: &ModelMesh) -> (glam::Vec3, glam::Vec3) {
    let b = m.mesh.bounds();
    (b.lo, b.hi)
}

fn color_names(m: &ModelMesh) -> BTreeSet<&'static str> {
    m.parts.iter().filter_map(|p| p.kicad_color_name()).collect()
}

#[test]
#[ignore = "needs KiCad 8 .wrl files (KRBR_TEST_VRML_DIR), an installed KiCad with STEP models and the STEP converter DLL"]
fn kicad_vrml_models_match_their_step_models() {
    let dir = PathBuf::from(std::env::var("KRBR_TEST_VRML_DIR").expect("set KRBR_TEST_VRML_DIR to a folder of KiCad 8 .wrl files"));
    let cache = std::env::temp_dir().join(format!("krbr_rust_vrml_vs_step_{}", std::process::id()));
    let lib = ModelLibrary::new(cache.clone(), MeshParams::default(), false);
    let mut compared = 0;
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let wrl = e.path();
        if wrl.extension().and_then(|x| x.to_str()) != Some("wrl") {
            continue;
        }
        let stem = wrl.file_stem().unwrap().to_string_lossy().into_owned();
        // ライブラリの名前はファイル名からは分からないので、よく使うライブラリを順に探す
        let libs = ["Resistor_SMD", "Capacitor_SMD", "Capacitor_THT", "LED_SMD", "Package_SO", "Package_QFP", "Connector_PinHeader_2.54mm"];
        let Some(step) = libs.iter().find_map(|l| installed_step(&format!("{l}.3dshapes"), &stem)) else {
            println!("{stem}: no STEP model installed; skipped");
            continue;
        };
        let v = lib.get(&wrl);
        let s = lib.get(&step);
        let (Some(v), Some(s)) = (v.mesh, s.mesh) else { panic!("{stem}: VRML {} / STEP {}", v.error, s.error) };
        let ((vlo, vhi), (slo, shi)) = (bounds(&v), bounds(&s));
        let d = (vlo - slo).abs().max((vhi - shi).abs()).max_element();
        println!(
            "{stem}: VRML {} tris, {} parts {:?} / STEP {} tris, {} parts {:?}; bounds differ by {d:.3} mm (VRML {vlo:.2}..{vhi:.2})",
            v.mesh.triangle_count(),
            v.parts.len(),
            color_names(&v),
            s.mesh.triangle_count(),
            s.parts.len(),
            color_names(&s),
        );
        // 同じ形を別々に分割したもの：外接箱は 0.05 mm 以内でそろう
        assert!(d < 0.05, "{stem}: bounds differ by {d} mm");
        // KiCad の色の名前（材質の既定の割り当ての鍵）は STEP にあるものがみな VRML にもある
        assert!(color_names(&s).is_subset(&color_names(&v)), "{stem}: STEP colours {:?}, VRML {:?}", color_names(&s), color_names(&v));
        // 法線は単位の長さ
        assert!(v.mesh.normals.iter().all(|n| (n.length() - 1.0).abs() < 1e-3), "{stem}: normals");
        compared += 1;
    }
    let _ = std::fs::remove_dir_all(&cache);
    assert!(compared > 0, "no .wrl in {} had a STEP model to compare with", dir.display());
}
