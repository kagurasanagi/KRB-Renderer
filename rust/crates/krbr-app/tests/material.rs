//! 材質の割り当ての CLI（C++ 版 tests/test_projects.cpp と同じ確かめ方）。基板を 2 つのプロジェクトにコピーし、
//! 全体設定・部品番号・部位ごとの指定がどう効くかを見る。KiCad の 3D モデルが要るので、明示したときだけ走らせる：
//!   cargo test --release -p krbr-app --test material -- --ignored --nocapture

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use krbr_app::material_cmd::{compute_assignments, set_material, MaterialSetOptions};
use krbr_scene::scene::MaterialAssignment;

fn repo_board() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../kicad/mini_dsp_radio.kicad_pcb")
}

/// 試す部品：KiCad 標準の色（kicad:metal_grey_pins）を持つ IC（U…）のうち、部位が 2 つ以上あるもの。(部品番号, モデルのパス)
fn pick_subject(rows: &[MaterialAssignment]) -> Option<(String, String)> {
    let mut by_ref: BTreeMap<&str, Vec<&MaterialAssignment>> = BTreeMap::new();
    for r in rows.iter().filter(|r| r.reference.starts_with('U')) {
        by_ref.entry(&r.reference).or_default().push(r);
    }
    by_ref.into_iter().find(|(_, l)| l.len() >= 2 && l.iter().filter(|r| r.part_keys.contains("kicad:metal_grey_pins")).count() == 1).map(|(r, l)| (r.to_string(), l[0].model_spec.clone()))
}

/// 部品の（部位のキー → 材質）と、決まった段階
fn parts_of(rows: &[MaterialAssignment], reference: &str) -> (BTreeMap<String, String>, String) {
    let mut out = BTreeMap::new();
    let mut source = String::new();
    for r in rows.iter().filter(|r| r.reference == reference) {
        out.insert(r.part_keys.clone(), r.material.clone());
        source = r.source.clone();
    }
    (out, source)
}

#[test]
#[ignore = "needs the KiCad 3D models (and the STEP converter DLL or the mesh cache)"]
fn global_model_material_is_shared_across_projects_and_reference_override_wins() {
    let ws = std::env::temp_dir().join(format!("krbr_rust_material_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    let config = ws.join("config");
    std::fs::create_dir_all(config.join("materials")).unwrap();
    std::env::set_var("KRBR_CONFIG_DIR", &config);
    let (pcb_a, pcb_b) = (ws.join("projA").join("board.kicad_pcb"), ws.join("projB").join("board.kicad_pcb"));
    for p in [&pcb_a, &pcb_b] {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::copy(repo_board(), p).unwrap();
    }
    std::fs::write(config.join("materials").join("test.toml"), "[material.test_red]\nbase_color = [0.8, 0.02, 0.02]\n[material.test_blue]\nbase_color = [0.02, 0.02, 0.8]\n").unwrap();

    // 変更前：組み込みの規則で決まっている
    let before = compute_assignments(&pcb_b).unwrap();
    let (reference, model) = pick_subject(&before).expect("no IC with kicad:metal_grey_pins and another part");
    println!("subject: {reference} ({model})");

    // 全体設定で対象のモデルを test_red に（プロジェクト A から設定）
    let global = MaterialSetOptions { pcb: pcb_a.clone(), model: model.clone(), material: "test_red".into(), global: true, ..Default::default() };
    assert_eq!(set_material(&global).unwrap(), config.join("model_map.toml"));
    for pcb in [&pcb_a, &pcb_b] {
        let (parts, source) = parts_of(&compute_assignments(pcb).unwrap(), &reference);
        assert!(!parts.is_empty());
        assert!(parts.values().all(|m| m == "test_red"), "{parts:?}");
        assert_eq!(source, "model (global)");
    }

    // プロジェクト A だけ、部品番号で上書き
    let by_ref = MaterialSetOptions { pcb: pcb_a.clone(), reference: reference.clone(), material: "test_blue".into(), ..Default::default() };
    assert_eq!(set_material(&by_ref).unwrap(), ws.join("projA").join("board.krbr").join("overrides.toml"));
    let (a, source) = parts_of(&compute_assignments(&pcb_a).unwrap(), &reference);
    assert!(a.values().all(|m| m == "test_blue"), "{a:?}");
    assert_eq!(source, format!("ref {reference} (project)"));
    let (b, _) = parts_of(&compute_assignments(&pcb_b).unwrap(), &reference);
    assert!(b.values().all(|m| m == "test_red"), "{b:?}");

    // プロジェクト B では端子だけを部品番号で上書き（他の部位は全体設定のまま）
    let pins = MaterialSetOptions { pcb: pcb_b.clone(), reference: reference.clone(), part: "kicad:metal_grey_pins".into(), material: "metal_gold".into(), ..Default::default() };
    set_material(&pins).unwrap();
    let (b, _) = parts_of(&compute_assignments(&pcb_b).unwrap(), &reference);
    let gold = b.iter().filter(|(k, m)| k.contains("kicad:metal_grey_pins") && *m == "metal_gold").count();
    let red = b.iter().filter(|(k, m)| !k.contains("kicad:metal_grey_pins") && *m == "test_red").count();
    assert_eq!(gold, 1, "{b:?}");
    assert_eq!(gold + red, b.len(), "{b:?}");

    // 存在しない材質名、--ref と --model の両方・どちらも無し、--global に --ref はエラー
    assert!(set_material(&MaterialSetOptions { material: "no_such_material".into(), ..by_ref.clone() }).unwrap_err().contains("not in the material library"));
    assert!(set_material(&MaterialSetOptions { model: model.clone(), ..by_ref.clone() }).unwrap_err().contains("exactly one"));
    assert!(set_material(&MaterialSetOptions { reference: String::new(), ..by_ref.clone() }).unwrap_err().contains("exactly one"));
    assert!(set_material(&MaterialSetOptions { global: true, ..by_ref.clone() }).unwrap_err().contains("--global"));
    assert!(set_material(&MaterialSetOptions { model: "no_such_model.step".into(), reference: String::new(), ..by_ref.clone() }).unwrap_err().contains("no 3D model"));

    // 読めない overrides.toml は上書きしない（C++ 版は 1 件だけの内容で上書きしてしまう）
    let broken = ws.join("projA").join("board.krbr").join("overrides.toml");
    std::fs::write(&broken, "[ref.U1\nmaterial = ").unwrap();
    let err = set_material(&by_ref).unwrap_err();
    assert!(err.contains("not overwriting"), "{err}");
    assert_eq!(std::fs::read_to_string(&broken).unwrap(), "[ref.U1\nmaterial = ");
    let _ = std::fs::remove_dir_all(&ws);
}
