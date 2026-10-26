//! 設定パネルのページ（web/panel.html）のスクリプト：画面を出さない Microsoft Edge（--headless）で開き、［−］［＋］・↑↓キー・
//! 押し続け・グループの開閉を操作して、Rust へ送る値を確かめる（ウィンドウは開かない）。
//! Edge が要るので、明示したときだけ走らせる：
//!   cargo test --release -p krbr-app --test panel_page -- --ignored --nocapture

use std::path::PathBuf;

fn edge() -> Option<PathBuf> {
    ["C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe", "C:/Program Files/Microsoft/Edge/Application/msedge.exe"].iter().map(PathBuf::from).find(|p| p.is_file())
}

/// パネルの JSON（Rust の panel_json と同じ形の、小さなもの）
const PANEL: &str = r#"{"title":"Settings","closeTip":"","resizeTip":"","stepTip":"step","sections":[
 {"id":"finish","title":"Image finishing","hint":"Instant","open":true,"items":[
  {"t":"slider","id":"exposure.ev","label":"Exposure","value":0.0,"min":-6,"max":6,"log":false,"digits":2,"suffix":" EV","step":0.3333333333333333,"big":1.0},
  {"t":"slider","id":"glow.strength","label":"Glow","value":2.4,"min":0,"max":4,"log":false,"digits":2,"suffix":"","step":0.01,"big":0.1},
  {"t":"group","id":"finish.glow","title":"Glow details","open":false,"items":[
   {"t":"slider","id":"glow.spread","label":"Spread","value":0.7,"min":0,"max":1,"log":false,"digits":2,"suffix":"","step":0.01,"big":0.1}]}]},
 {"id":"lighting","title":"Lighting","hint":null,"open":true,"items":[
  {"t":"slider","id":"env.intensity","label":"Intensity","value":1.05,"min":0.05,"max":8,"log":true,"digits":2,"suffix":"","step":0.01,"big":0.1},
  {"t":"slider","id":"env.rotation","label":"Rotation","value":179,"min":-180,"max":180,"log":false,"digits":0,"suffix":"°","step":1,"big":10}]},
 {"id":"render","title":"Rendering","hint":null,"open":true,"items":[
  {"t":"slider","id":"display.max_spp","label":"Max spp","value":11,"min":4,"max":16,"log":false,"digits":0,"suffix":"","step":1,"big":10,"int":true,"format":"pow2"}]},
 {"id":"help","plain":true,"items":[{"t":"note","text":"help","tone":"muted"}]}]}"#;

/// ページの中で操作して、結果を document.title に書く
const SCRIPT: &str = r#"
window.onerror = (m, s, l) => { document.title = "ERR " + m + " line " + l; };
krbr.panel(PANEL_JSON);
const res = [];
const sliderRow = (label) => [...document.querySelectorAll(".slider")].find((r) => r.querySelector("label").textContent === label);
const press = (b, shift) => { b.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, shiftKey: !!shift, pointerId: 1 })); b.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, pointerId: 1 })); };
const last = () => JSON.parse(msgs[msgs.length - 1]);
const [evMinus, evPlus] = sliderRow("Exposure").querySelectorAll(".step");
press(evPlus); press(evPlus); res.push("ev=" + last().value.toFixed(4));
press(evMinus, true); res.push("ev=" + last().value.toFixed(4));
const num = sliderRow("Intensity").querySelector(".num");
num.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true })); res.push("int=" + last().value);
num.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", shiftKey: true, bubbles: true })); res.push("int=" + last().value);
const rot = sliderRow("Rotation"); press(rot.querySelectorAll(".step")[1]); press(rot.querySelectorAll(".step")[1]); res.push("rot=" + last().value);
const spp = sliderRow("Max spp"); press(spp.querySelectorAll(".step")[1]); res.push("spp=" + last().value + "/" + spp.querySelector(".num").value);
const sub = document.querySelector(".subgroup"); res.push("sub=" + sub.classList.contains("open"));
sub.querySelector(".fold-head").click(); res.push("sub=" + sub.classList.contains("open") + "/" + last().id);
const glow = sliderRow("Glow").querySelectorAll(".step")[1]; const n0 = msgs.length;
glow.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, pointerId: 2 }));
setTimeout(() => {
  glow.dispatchEvent(new PointerEvent("pointerup", { bubbles: true, pointerId: 2 }));
  const n = msgs.length - n0;
  setTimeout(() => {
    res.push("held=" + (n > 5 && msgs.length - n0 === n ? "ok" : "bad " + n + "/" + (msgs.length - n0)) + "/" + last().value.toFixed(2));
    document.title = res.join(" ");
  }, 300);
}, 1200);
"#;

#[test]
#[ignore = "needs Microsoft Edge"]
fn panel_steppers_and_folds_work_in_the_page() {
    let Some(edge) = edge() else { panic!("Microsoft Edge not found") };
    let web = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
    let style: String = ["style.css", "panel.css", "editor.css", "about.css"].iter().map(|f| std::fs::read_to_string(web.join(f)).unwrap() + "\n").collect();
    let page = std::fs::read_to_string(web.join("panel.html")).unwrap().replace("/*STYLE*/", &style).replacen("<head>", "<head><script>window.msgs=[];window.ipc={postMessage(m){window.msgs.push(m)}};</script>", 1);
    let page = page.replacen("</body>", &format!("<script>{}</script></body>", SCRIPT.replace("PANEL_JSON", PANEL)), 1);
    let dir = std::env::temp_dir().join(format!("krbr_panel_page_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("panel.html");
    std::fs::write(&file, page).unwrap();
    let out = std::process::Command::new(edge)
        .args(["--headless=new", "--disable-gpu", "--no-first-run", "--virtual-time-budget=5000", "--enable-logging=stderr", "--v=0", "--dump-dom"])
        .arg(format!("--user-data-dir={}", dir.join("profile").display()))
        .arg(format!("file:///{}", file.to_string_lossy().replace('\\', "/")))
        .output()
        .unwrap();
    let dom = String::from_utf8_lossy(&out.stdout);
    let title = dom.split("<title>").nth(1).and_then(|t| t.split("</title>").next()).unwrap_or("").to_string();
    let _ = std::fs::remove_dir_all(&dir);
    println!("{title}");
    if title.is_empty() {
        // スクリプトが動かなかった（構文の誤りなど）：ブラウザのコンソールの誤りを出す
        println!("{}", String::from_utf8_lossy(&out.stderr).lines().filter(|l| l.contains("CONSOLE")).collect::<Vec<_>>().join("\n"));
    }
    // 露出は 1/3 段（0 → 1/3 → 2/3）、Shift で 1 段の目盛りへ（2/3 → 0）。↑↓ は 0.01、Shift で 0.1 の目盛り（1.06 → 1.0）。
    // 回転は上限 180 で止まる。最大 spp は指数を 1 つ（2048 → 4096）。詳細は初め畳んであり、押すと開いて Rust に知らせる。
    // 押し続けると連続し、離すと止まる（0.01 ずつ）
    let expect = ["ev=0.6667", "ev=0.0000", "int=1.06", "int=1", "rot=180", "spp=12/4096", "sub=false", "sub=true/section:finish.glow"];
    for e in expect {
        assert!(title.split(' ').any(|t| t == e), "{e} not in: {title}");
    }
    let held = title.split(' ').find(|t| t.starts_with("held=")).unwrap_or("");
    assert!(held.starts_with("held=ok/") && held["held=ok/".len()..].parse::<f64>().is_ok_and(|g| g > 2.45 && g < 2.8), "{held} in: {title}");
}
