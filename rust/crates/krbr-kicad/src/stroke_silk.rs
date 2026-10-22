//! kicad-cli に渡す基板のコピー：シルク層には、krbr が自分では描けないもの（KiCad の線フォントの文字と、文字の枠）だけを残す。
//! TrueType の文字（render_cache があるもの）・線・図形は krbr が描くので、kicad-cli には描かせない（同じものが
//! わずかに違う形で重なって太ったり二重になったりしないように）

use crate::board::is_silk_layer;
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

/// krbr が自分では形を作れない文字の項目か（層は問わない）：線フォントの文字と、文字の枠（枠の線は krbr が描かず、
/// 中の文字も枠に合わせた折り返しが要る）。シルク層のこれらだけを kicad-cli に描かせる。
/// stroke_silk_only_board が残す項目と、パーサが StrokeText に入れる項目（Board::stroke_silk_count で数え、
/// 差分の検出で BoardItem::stroke_silk を立てる）の両方をこの判定で決める
pub(crate) fn needs_kicad_cli(n: &Node) -> bool {
    match n.tag() {
        "property" | "fp_text" | "gr_text" => !is_truetype(n),
        "fp_text_box" | "gr_text_box" => true,
        _ => false,
    }
}

/// kicad-cli にシルクとして描かせる項目か
fn plotted_by_kicad_cli(n: &Node) -> bool {
    is_silk_layer(layer_of(n)) && needs_kicad_cli(n)
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
                if is_silk_layer(layer_of(it)) && !plotted_by_kicad_cli(it) {
                    move_off_silk(&mut t);
                }
                out.items.push(t);
            }
            "fp_text_box" => {
                if plotted_by_kicad_cli(it) {
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
            "gr_text" | "gr_text_box" => {
                if plotted_by_kicad_cli(it) {
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

    /// 文字の枠と、部品内の文字を含む基板
    const BOARD_WITH_TEXT_BOXES: &str = r#"(kicad_pcb (version 20260206) (generator "pcbnew")
  (layers (0 "F.Cu" signal) (37 "F.SilkS" user "F.Silkscreen"))
  (footprint "Lib:Part" (layer "F.Cu") (uuid "fp1") (at 10 20 90)
    (property "Reference" "U1" (at 0 -2 90) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15))))
    (property "Value" "X" (at 0 2 90) (layer "F.Fab") (effects (font (size 1 1))))
    (fp_text_box "boxed" (start -1 -1) (end 1 1) (layer "F.SilkS") (effects (font (size 0.8 0.8))) (border yes) (stroke (width 0.1) (type solid)))
    (fp_text_box "fab" (start -1 -1) (end 1 1) (layer "F.Fab") (effects (font (size 0.8 0.8)))))
  (gr_text_box "only a box" (start 30 30) (end 40 35) (layer "B.SilkS") (uuid "tb1") (effects (font (size 1 1))) (border yes) (stroke (width 0.15) (type solid)))
  (gr_text_box "tt" (start 50 50) (end 60 55) (layer "F.SilkS") (uuid "tb2") (effects (font (face "Arial") (size 1 1))) (render_cache "tt" 0 (polygon (pts (xy 50 50) (xy 51 50) (xy 51 51)))))
)"#;

    /// kicad-cli に描かせる項目の数が、stroke_silk_only_board が残すものとパーサが数えるもので一致する
    #[test]
    fn text_boxes_are_plotted_and_counted_alike() {
        let out = parse(&stroke_silk_only_board(BOARD_WITH_TEXT_BOXES).unwrap()).unwrap();
        let fp = out.child("footprint").unwrap();
        let in_fp = fp.items.iter().filter(|c| c.is_list && plotted_by_kicad_cli(c)).count();
        let on_board = out.items.iter().filter(|c| c.is_list && plotted_by_kicad_cli(c)).count();
        // 部品番号 U1、シルクの fp_text_box、基板の gr_text_box 2 つ（TrueType でも枠は krbr が描かない）
        assert_eq!((in_fp, on_board), (2, 2));
        assert_eq!(fp.children("fp_text_box").count(), 1); // 部品図の層の枠は渡さない

        let dir = std::env::temp_dir().join(format!("krbr_rust_text_box_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("boxes.kicad_pcb");
        std::fs::write(&file, BOARD_WITH_TEXT_BOXES).unwrap();
        let b = crate::parser::load_board(&file);
        let _ = std::fs::remove_dir_all(&dir);
        let b = b.unwrap();
        assert_eq!(b.stroke_silk_count(), in_fp + on_board);
        // 文字の枠だけの項目でも、変えればシルクを書き出し直す
        assert!(b.items.iter().filter(|it| it.key.starts_with("tb")).all(|it| it.stroke_silk));
        // 枠の範囲は枠そのもの（＋線と文字のはみ出し）。部品内の枠は部品の位置と向きで基板座標に直る
        let tb1 = b.items.iter().find(|it| it.key == "tb1").unwrap();
        assert!(tb1.bbox.lo.x <= 30.0 && tb1.bbox.hi.x >= 40.0 && tb1.bbox.lo.x > 27.0 && tb1.bbox.hi.y < 38.0);
        let boxed = b.stroke_texts.iter().find(|t| t.text == "boxed").unwrap();
        assert!((boxed.pos - crate::board::Vec2::new(10.0, 20.0)).length() < 1e-9);
        // 部品の文字の位置も基板座標：(0, -2) を 90° 回すと (-2, 0)
        let u1 = b.stroke_texts.iter().find(|t| t.text == "U1").unwrap();
        assert!((u1.pos - crate::board::Vec2::new(8.0, 20.0)).length() < 1e-9, "{}", u1.pos);
    }
}
