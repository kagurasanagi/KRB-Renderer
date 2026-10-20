//! 保存の監視と差分更新（C++ 版 tests/test_watch.cpp と同じ確かめ方）。基板のコピーを書き換え、保存から画像の更新までの時間を測る。
//! Vulkan の GPU と KiCad の 3D モデルが要るので、明示したときだけ走らせる：
//!   cargo test --release -p krbr-app --test watch -- --ignored --nocapture

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use krbr_app::{run_watch, RenderOptions, WatchEvent, WatchOptions, WatchUpdate};


/// 保存をまねる（KiCad と同じく、ファイル全体を書き直す）。start 以降で最初の from を to に替える
fn save_with_replace(pcb: &Path, from: &str, to: &str, start: usize) {
    let mut text = std::fs::read_to_string(pcb).unwrap();
    let pos = start + text[start..].find(from).unwrap_or_else(|| panic!("'{from}' not found"));
    text.replace_range(pos..pos + from.len(), to);
    std::fs::write(pcb, text).unwrap();
}

/// 書き換える線フォントの文字：シルクにある、render_cache の無い基板の文字（gr_text）の最初のもの。(元の "(gr_text \"…\"", 書き換えたもの)
fn find_stroke_silk_text(pcb: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(pcb).unwrap();
    let root = krbr_kicad::sexpr::parse(&text).ok()?;
    let t = root.items.iter().find(|n| n.is_list && n.tag() == "gr_text" && !n.has("render_cache") && n.child_str("layer").is_some_and(|l| l.ends_with(".SilkS")) && !n.str(1).is_empty())?;
    let from = format!("(gr_text {:?}", t.str(1));
    // 同じ文字の gr_text が複数あるときは、最初のものだけを替える（save_with_replace）
    text.contains(&from).then(|| (from.clone(), format!("(gr_text {:?}", format!("{}~", t.str(1)))))
}

/// 動かす部品：KiCad 標準の 0603 コンデンサを使う最初の部品。(部品の S 式の位置, "(at x y" の元, x を 1.5 mm ずらしたもの)
fn find_target(pcb: &Path) -> (usize, String, String) {
    let text = std::fs::read_to_string(pcb).unwrap();
    let fp = text.find("(footprint \"Capacitor_SMD:C_0603_1608Metric\"").expect("no C_0603_1608Metric footprint on the test board");
    let at = fp + text[fp..].find("(at ").unwrap();
    let mut it = text[at + 4..].split([' ', ')']);
    let (x, y): (f64, f64) = (it.next().unwrap().parse().unwrap(), it.next().unwrap().parse().unwrap());
    let from = format!("(at {} {}", fmt(x), fmt(y));
    assert!(text[at..].starts_with(&from), "unexpected (at ...) format near {}", &text[at..at + 30]);
    (fp, from, format!("(at {} {}", fmt(x + 1.5), fmt(y)))
}

fn fmt(v: f64) -> String {
    let s = format!("{v:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// 基板に出てこない（＝空のキャッシュに無い）KiCad 標準のコンデンサのモデル
fn unused_capacitor_model(pcb: &Path) -> &'static str {
    let text = std::fs::read_to_string(pcb).unwrap();
    ["C_1206_3216Metric", "C_1210_3225Metric", "C_2220_5750Metric", "C_0805_2012Metric"].into_iter().find(|n| !text.contains(n)).expect("every candidate capacitor model is already used")
}

/// 保存から画像の更新までの目標時間。1 秒は ray query を持つ GPU での値（C++ 版と同じ）。
/// 持たない GPU（ray tracing pipeline で描く）では仕組みの検証だけにして、目標を緩める
fn budget_ms() -> f64 {
    let fast = krbr_render::Context::enumerate().unwrap_or_default().iter().any(|d| d.ray_query && d.acceleration_structure);
    if fast {
        1000.0
    } else {
        30000.0
    }
}

#[test]
#[ignore = "needs a Vulkan GPU with ray tracing and the KiCad 3D models"]
fn save_updates_the_image_and_rebuilds_only_nearby_tiles() {
    let budget = budget_ms();
    let ws = std::env::temp_dir().join(format!("krbr_rust_watch_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(ws.join("proj")).unwrap();
    // 3D モデルのキャッシュと全体設定も空のものにする（裏での分割を確かめるため。環境マップの記憶も書き込まない）
    std::env::set_var("KRBR_CACHE_DIR", ws.join("cache"));
    std::env::set_var("KRBR_CONFIG_DIR", ws.join("config"));
    let pcb = ws.join("proj").join("board.kicad_pcb");
    std::fs::copy(common::test_board(), &pcb).unwrap();
    let out = ws.join("out").join("watch.png");
    let o = WatchOptions {
        common: RenderOptions { pcb: pcb.clone(), hdr: "builtin:studio".into(), out: out.clone(), spp: Some(32), width: Some(1920), height: Some(1080), zoom: 1.0, ..Default::default() },
        max_updates: 0,
    };
    let (fp, at_from, at_to) = find_target(&pcb);
    let model_to = unused_capacitor_model(&pcb);
    // 線フォントの文字（kicad-cli で描く）の書き換えは、kicad-cli があり、基板に線フォントのシルクの文字があるときだけ確かめる
    let stroke_text = find_stroke_silk_text(&pcb);
    let with_cli = krbr_app::kicad_cli::find_kicad_cli("").is_some() && stroke_text.is_some();
    if stroke_text.is_none() {
        println!("no stroke-font silk text (gr_text) on the board: skipping the kicad-cli steps");
    }
    let total = if with_cli { 6 } else { 4 };

    // 1. 部品を動かす  2. 中身を変えずに保存する  3. 3D モデルを未分割のものに替える（箱で仮表示）  4. その分割の完了
    // 5. 線フォントの文字を書き換える  6. kicad-cli での書き出し直しの完了
    let mut updates: Vec<WatchUpdate> = Vec::new();
    let mut deadline = Instant::now();
    let mut timed_out = false;
    let save = |step: usize| {
        std::thread::sleep(Duration::from_millis(50)); // 更新時刻の分解能を超えるよう、少し待つ
        match step {
            0 => save_with_replace(&pcb, &at_from, &at_to, fp),
            1 => {
                let text = std::fs::read_to_string(&pcb).unwrap();
                std::fs::write(&pcb, text + "\n").unwrap();
            }
            2 => save_with_replace(&pcb, "Capacitor_SMD.3dshapes/C_0603_1608Metric.step", &format!("Capacitor_SMD.3dshapes/{model_to}.step"), fp),
            _ => {
                let (from, to) = stroke_text.as_ref().unwrap();
                save_with_replace(&pcb, from, to, 0)
            }
        }
    };
    run_watch(&o, Duration::from_millis(200), |e| match e {
        WatchEvent::Ready(p) => {
            assert!(p.exists());
            save(0);
            deadline = Instant::now() + Duration::from_secs(10);
            true
        }
        WatchEvent::Updated(u) => {
            updates.push(u.clone());
            let n = updates.len();
            if n <= 2 || (n == 4 && with_cli) {
                save(n);
            }
            // 分割と kicad-cli の完了は最大 30 秒待つ
            deadline = Instant::now() + Duration::from_secs(if n == 3 || n == 5 { 30 } else { 10 });
            n < total
        }
        WatchEvent::Idle => {
            timed_out = Instant::now() > deadline;
            !timed_out
        }
    })
    .unwrap();
    for (i, u) in updates.iter().enumerate() {
        println!(
            "update {}: saved {}, {:.0} ms after save; reload {:.0} ms (parse {:.0}, {} tiles), gpu {:.0} ms, render {:.0} ms (denoise {:.0}), write {:.0} ms; {} changed, {} added, {} removed, {} pending",
            i + 1, u.saved, u.since_save_ms, u.reload_ms, u.stats.parse_ms, u.stats.tiles_rebuilt, u.gpu_ms, u.render_ms, u.denoise_ms, u.write_ms, u.stats.diff.changed, u.stats.diff.added, u.stats.diff.removed, u.pending_models
        );
    }
    assert!(!timed_out, "timed out after {} update(s)", updates.len());
    assert_eq!(updates.len(), total);

    // 1. 部品を動かした：その周辺のタイルだけを作り直し、1 秒以内に画像を更新する
    let u = &updates[0];
    assert!(u.saved);
    assert_eq!(u.stats.diff.changed, 1);
    assert_eq!(u.stats.diff.added, 0);
    assert!(!u.stats.diff.full);
    assert!(u.stats.tiles_rebuilt > 0 && u.stats.tiles_rebuilt <= 16, "{} tiles", u.stats.tiles_rebuilt);
    assert!(u.since_save_ms < budget, "{:.0} ms after save (budget {budget:.0})", u.since_save_ms);
    // ノイズ除去が使えるなら、それ込みで間に合っていること（watch は既定でノイズ除去する）
    if krbr_render::denoiser_available() {
        assert!(u.denoise_ms > 0.0);
    }
    // 速さ優先で書いた PNG が読めること
    let img = image::open(&out).unwrap();
    assert_eq!((img.width(), img.height()), (1920, 1080));

    // 2. 中身を変えずに保存した：何も作り直さない
    let u = &updates[1];
    assert_eq!(u.stats.diff.changed + u.stats.diff.added + u.stats.diff.removed, 0);
    assert_eq!(u.stats.tiles_rebuilt, 0);

    // 3. 未分割のモデル：分割を待たずに箱で更新し、4. 分割が終わったら組み直す
    let u = &updates[2];
    assert!(u.saved);
    assert_eq!(u.pending_models, 1);
    assert!(u.since_save_ms < budget, "{:.0} ms after save (budget {budget:.0})", u.since_save_ms);
    let u = &updates[3];
    assert!(!u.saved);
    assert_eq!(u.pending_models, 0);

    // 5. 線フォントの文字を書き換えた：まず読み直し（文字はまだ前のもの）、6. 書き出し直したシルクで全部のタイルを作り直す
    if with_cli {
        let u = &updates[4];
        assert!(u.saved && u.stats.diff.stroke_silk_changed);
        let u = &updates[5];
        assert!(!u.saved && u.silk);
        assert!(u.stats.tiles_rebuilt == 0); // 統計は保存のときだけ（作り直しは invalidate_all で全部）
    }
    let _ = std::fs::remove_dir_all(&ws);
}
