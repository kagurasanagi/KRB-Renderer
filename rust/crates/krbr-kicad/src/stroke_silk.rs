//! kicad-cli に渡す基板のコピー：シルク層には、krbr が自分では描けないもの（KiCad の線フォントの文字と、文字の枠）だけを残す。
//! TrueType の文字（render_cache があるもの）・線・図形は krbr が描くので、kicad-cli には描かせない（同じものが
//! わずかに違う形で重なって太ったり二重になったりしないように）。C++ 版はシルク層全体を描かせて重ねている

use crate::sexpr::{parse, write, Node};

/// 文字の層がシルクなら、その面の部品図の層（書き出さない層）
fn fab_of_silk(layer: &str) -> Option<&'static str> {
    match layer {
        "F.SilkS" => Some("F.Fab"),
        "B.SilkS" => Some("B.Fab"),
        _ => None,
    }
}

fn layer_of(n: &Node) -> &str {
    n.child_str("layer").unwrap_or("")
}

/// 文字（property / fp_text / gr_text）が TrueType か（KiCad は TrueType の文字の形を render_cache に入れて保存する）
fn is_truetype(n: &Node) -> bool {
    n.has("render_cache")
}

/// シルクにある TrueType の文字を、同じ面の部品図の層へ移す（消すと KiCad が既定の文字を作り直すことがあるので残す）
fn move_off_silk(n: &mut Node) {
    if let Some(l) = n.items.iter_mut().find(|c| c.is_list && c.tag() == "layer") {
        if let Some(fab) = l.items.get(1).and_then(|a| fab_of_silk(&a.text)) {
            l.items[1].text = fab.to_string();
            // knockout（文字を抜いた塗り）はシルク以外では意味がないので外す
            l.items.truncate(2);
        }
    }
}

fn filter_footprint(fp: &Node) -> Node {
    let mut out = Node { is_list: true, ..Default::default() };
    for it in &fp.items {
        if !it.is_list {
            out.items.push(it.clone());
            continue;
        }
        match it.tag() {
            "property" | "fp_text" => {
                let mut t = it.clone();
                if fab_of_silk(layer_of(it)).is_some() && is_truetype(it) {
                    move_off_silk(&mut t);
                }
                out.items.push(t);
            }
            // 文字の枠は krbr が描かないので、シルクのものは kicad-cli に描かせる
            "fp_text_box" => {
                if fab_of_silk(layer_of(it)).is_some() {
                    out.items.push(it.clone());
                }
            }
            // 図形・パッド・ゾーン・3D モデルなどは要らない
            "pad" | "zone" | "model" | "group" | "dimension" | "image" | "barcode" => {}
            t if t.starts_with("fp_") => {}
            _ => out.items.push(it.clone()),
        }
    }
    out
}

/// 基板の S 式から、kicad-cli でシルクを書き出すためのコピーを作る
pub fn stroke_silk_only_board(text: &str) -> Result<String, String> {
    let root = parse(text).map_err(|e| e.to_string())?;
    if root.tag() != "kicad_pcb" {
        return Err("not a kicad_pcb file".into());
    }
    let mut out = Node { is_list: true, ..Default::default() };
    for it in &root.items {
        if !it.is_list {
            out.items.push(it.clone());
            continue;
        }
        match it.tag() {
            // 基板の設定（層の名前・ページ・文字の変数）
            "version" | "generator" | "generator_version" | "general" | "paper" | "title_block" | "layers" | "setup" | "property" | "embedded_fonts" | "embedded_files" => {
                out.items.push(it.clone())
            }
            "footprint" | "module" => out.items.push(filter_footprint(it)),
            "gr_text" => {
                if fab_of_silk(layer_of(it)).is_some() && !is_truetype(it) {
                    out.items.push(it.clone());
                }
            }
            "gr_text_box" => {
                if fab_of_silk(layer_of(it)).is_some() {
                    out.items.push(it.clone());
                }
            }
            _ => {}
        }
    }
    Ok(write(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOARD: &str = r#"(kicad_pcb (version 20260206) (generator "pcbnew")
  (layers (0 "F.Cu" signal) (37 "F.SilkS" user "F.Silkscreen"))
  (net 1 "GND")
  (footprint "Capacitor_SMD:C_0603_1608Metric" (layer "F.Cu") (at 10 20 90)
    (property "Reference" "C1" (at 0 -1.4 90) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
    (property "Value" "100n" (at 0 1.4 90) (layer "F.Fab") (effects (font (size 1 1))))
    (fp_text user "TT" (at 0 0) (layer "F.SilkS" knockout) (effects (font (face "Arial") (size 1 1))) (render_cache "TT" 0 (polygon (pts (xy 0 0) (xy 1 0) (xy 1 1)))))
    (fp_line (start -1 -1) (end 1 -1) (stroke (width 0.12) (type solid)) (layer "F.SilkS"))
    (pad "1" smd roundrect (at -0.8 0 90) (size 0.9 0.95) (layers "F.Cu" "F.Mask" "F.Paste"))
    (model "${KICAD10_3DMODEL_DIR}/Capacitor_SMD.3dshapes/C_0603_1608Metric.step"))
  (gr_text "STROKE" (at 5 5) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
  (gr_text "TRUETYPE" (at 5 8) (layer "F.SilkS") (effects (font (face "Arial") (size 1 1))) (render_cache "TRUETYPE" 0 (polygon (pts (xy 0 0) (xy 1 0) (xy 1 1)))))
  (gr_line (start 0 0) (end 10 0) (stroke (width 0.1) (type solid)) (layer "F.SilkS"))
  (segment (start 0 0) (end 1 1) (width 0.2) (layer "F.Cu") (net 1))
)"#;

    #[test]
    fn keeps_only_stroke_font_silk_texts() {
        let out = stroke_silk_only_board(BOARD).unwrap();
        let n = parse(&out).unwrap();
        // 線フォントの文字（部品番号・基板の文字）は残る
        let fp = n.child("footprint").unwrap();
        let reference = fp.children("property").find(|p| p.str(1) == "Reference").unwrap();
        assert_eq!(reference.child_str("layer"), Some("F.SilkS"));
        assert_eq!(n.children("gr_text").map(|t| t.str(1)).collect::<Vec<_>>(), vec!["STROKE"]);
        // TrueType の部品の文字は部品図の層へ移り（knockout も外れる）、基板の TrueType の文字は消える
        let tt = fp.child("fp_text").unwrap();
        assert_eq!(tt.child("layer").unwrap().items.len(), 2);
        assert_eq!(tt.child_str("layer"), Some("F.Fab"));
        // 図形・パッド・3D モデル・配線は消える
        assert!(!fp.has("fp_line") && !fp.has("pad") && !fp.has("model"));
        assert!(!n.has("gr_line") && !n.has("segment") && !n.has("net"));
        // 設定と部品の位置は残る
        assert!(n.has("layers") && n.has("version"));
        assert_eq!(fp.child("at").unwrap().num(3), 90.0);
        assert_eq!(fp.str(1), "Capacitor_SMD:C_0603_1608Metric");
    }
}
