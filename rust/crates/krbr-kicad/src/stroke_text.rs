//! kicad-cli に渡す基板のコピー：krbr が描く層（シルク・銅箔・ソルダーマスク）には、krbr が自分では描けないもの
//! （KiCad の線フォントの文字、文字の枠、寸法線・表・ターゲット）だけを残す。
//! TrueType の文字（render_cache があるもの）・線・図形・パッドは krbr が描くので、kicad-cli には描かせない（同じものが
//! わずかに違う形で重なって太ったり二重になったりしないように）

use crate::board::{is_plotted_text_layer, Side};
use crate::sexpr::{parse, write, Node};

/// kicad-cli に書き出させる層と、コピーの中で付ける表示名。`--mode-multi` は「基板名-<表示名>.svg」に書き出し、
/// 表示名は基板（と KiCad の言語）ごとに違い得るので、決まった名前に書き換えてファイルを見つけられるようにする
/// （表示名の '.' は '_' になるので、初めから '_' にしておく）
pub const PLOT_LAYERS: [(&str, &str); 6] = [
    ("F.SilkS", "KRBR_F_SilkS"),
    ("B.SilkS", "KRBR_B_SilkS"),
    ("F.Cu", "KRBR_F_Cu"),
    ("B.Cu", "KRBR_B_Cu"),
    ("F.Mask", "KRBR_F_Mask"),
    ("B.Mask", "KRBR_B_Mask"),
];

/// 文字の層が kicad-cli に書き出させる層なら、その面の部品図の層（書き出さない層）
fn fab_of(layer: &str) -> Option<&'static str> {
    if !is_plotted_text_layer(layer) {
        return None;
    }
    Some(if layer.starts_with("F.") { Side::Front.fab_layer() } else { Side::Back.fab_layer() })
}

fn layer_of(n: &Node) -> &str {
    n.child_str("layer").unwrap_or("")
}

/// 文字（property / fp_text / gr_text）が TrueType か（KiCad は TrueType の文字の形を render_cache に入れて保存する）
fn is_truetype(n: &Node) -> bool {
    n.has("render_cache")
}

/// krbr が自分では形を作れない文字の項目か（層は問わない）：線フォントの文字と、文字の枠（枠の線は krbr が描かず、
/// 中の文字も枠に合わせた折り返しが要る）。シルク・銅箔・マスクの層のこれらだけを kicad-cli に描かせる。
/// stroke_text_only_board が残す項目と、パーサが StrokeText に入れる項目（Board::stroke_text_count で数え、
/// 差分の検出で BoardItem::stroke_text を立てる）の両方をこの判定で決める
pub(crate) fn needs_kicad_cli(n: &Node) -> bool {
    match n.tag() {
        "property" | "fp_text" | "gr_text" => !is_truetype(n),
        "fp_text_box" | "gr_text_box" => true,
        // 寸法線・表・ターゲット（krbr は形を作らない。以前はどの経路でも描かれず、消えていた）
        "dimension" | "table" | "target" => true,
        _ => false,
    }
}

/// kicad-cli に描かせる項目か
fn plotted_by_kicad_cli(n: &Node) -> bool {
    is_plotted_text_layer(layer_of(n)) && needs_kicad_cli(n)
}

/// 書き出す層にある TrueType の文字を、同じ面の部品図の層へ移す（消すと KiCad が既定の文字を作り直すことがあるので残す）
fn move_to_fab(n: &mut Node) {
    if let Some(l) = n.items.iter_mut().find(|c| c.is_list && c.tag() == "layer") {
        if let Some(fab) = l.items.get(1).and_then(|a| fab_of(&a.text)) {
            l.items[1].text = fab.to_string();
            // knockout（文字を抜いた塗り）は部品図の層では意味がないので外す
            l.items.truncate(2);
        }
    }
}

/// 層の一覧の、書き出す層の表示名を PLOT_LAYERS の名前にする。(番号 "名前" 種類 ["表示名"])
fn rename_plot_layers(layers: &mut Node) {
    for l in layers.items.iter_mut().filter(|l| l.is_list) {
        let Some((_, shown)) = PLOT_LAYERS.iter().find(|(name, _)| l.str(1) == *name) else { continue };
        let atom = Node { quoted: true, text: shown.to_string(), ..Default::default() };
        if l.items.len() >= 4 {
            l.items[3] = atom;
        } else if l.items.len() == 3 {
            l.items.push(atom);
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
                if !plotted_by_kicad_cli(it) {
                    move_to_fab(&mut t);
                }
                out.items.push(t);
            }
            "fp_text_box" | "dimension" | "table" | "target" => {
                if plotted_by_kicad_cli(it) {
                    out.items.push(it.clone());
                }
            }
            // 図形・パッド・ゾーン・3D モデルなどは要らない
            "pad" | "zone" | "model" | "group" | "image" | "barcode" => {}
            t if t.starts_with("fp_") => {}
            _ => out.items.push(it.clone()),
        }
    }
    out
}

/// 基板の S 式から、kicad-cli で線フォントの文字を書き出すためのコピーを作る
pub fn stroke_text_only_board(text: &str) -> Result<String, String> {
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
            "version" | "generator" | "generator_version" | "general" | "paper" | "title_block" | "setup" | "property" | "embedded_fonts" | "embedded_files" => {
                out.items.push(it.clone())
            }
            "layers" => {
                let mut l = it.clone();
                rename_plot_layers(&mut l);
                out.items.push(l);
            }
            "footprint" | "module" => out.items.push(filter_footprint(it)),
            "gr_text" | "gr_text_box" | "dimension" | "table" | "target" => {
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
        let out = stroke_text_only_board(BOARD).unwrap();
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

    /// kicad-cli に描かせる項目の数が、stroke_text_only_board が残すものとパーサが数えるもので一致する
    #[test]
    fn text_boxes_are_plotted_and_counted_alike() {
        let out = parse(&stroke_text_only_board(BOARD_WITH_TEXT_BOXES).unwrap()).unwrap();
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
        assert_eq!(b.stroke_text_count(), in_fp + on_board);
        // 文字の枠だけの項目でも、変えれば書き出し直す
        assert!(b.items.iter().filter(|it| it.key.starts_with("tb")).all(|it| it.stroke_text));
        // 枠の範囲は枠そのもの（＋線と文字のはみ出し）。部品内の枠は部品の位置と向きで基板座標に直る
        let tb1 = b.items.iter().find(|it| it.key == "tb1").unwrap();
        assert!(tb1.bbox.lo.x <= 30.0 && tb1.bbox.hi.x >= 40.0 && tb1.bbox.lo.x > 27.0 && tb1.bbox.hi.y < 38.0);
        let boxed = b.stroke_texts.iter().find(|t| t.text == "boxed").unwrap();
        assert!((boxed.pos - crate::board::Vec2::new(10.0, 20.0)).length() < 1e-9);
        // 部品の文字の位置も基板座標：(0, -2) を 90° 回すと (-2, 0)
        let u1 = b.stroke_texts.iter().find(|t| t.text == "U1").unwrap();
        assert!((u1.pos - crate::board::Vec2::new(8.0, 20.0)).length() < 1e-9, "{}", u1.pos);
    }

    /// 寸法線・表・ターゲット：シルク・銅箔・マスクの層のものは kicad-cli に描かせ、パーサも数える（以前はどこにも描かれなかった）
    const BOARD_WITH_DIMENSIONS: &str = r#"(kicad_pcb (version 20260206) (generator "pcbnew")
  (layers (0 "F.Cu" signal) (37 "F.SilkS" user "F.Silkscreen") (49 "F.Fab" user))
  (footprint "Lib:Part" (layer "F.Cu") (uuid "fp1") (at 10 20 90)
    (dimension (type aligned) (layer "F.SilkS") (uuid "fd1") (pts (xy -1 0) (xy 1 0)) (height 1)
      (gr_text "2 mm" (at 0 -1.5 0) (layer "F.SilkS") (effects (font (size 0.5 0.5) (thickness 0.1)))))
    (dimension (type aligned) (layer "F.Fab") (uuid "fd2") (pts (xy -1 0) (xy 1 0)) (height 1)))
  (dimension (type aligned) (layer "F.SilkS") (uuid "d1") (pts (xy 110 150) (xy 150 150)) (height -5)
    (gr_text "40 mm" (at 130 143.85 0) (layer "F.SilkS") (effects (font (size 1 1) (thickness 0.15)))))
  (table (column_count 1) (layer "F.SilkS") (uuid "tb1") (cells (table_cell "A" (start 10 10) (end 20 15) (layer "F.SilkS") (effects (font (size 1 1))))))
  (target plus (at 5 5) (size 5) (width 0.1) (layer "Edge.Cuts") (uuid "tg1"))
)"#;

    #[test]
    fn dimensions_and_tables_are_plotted_and_counted() {
        let out = parse(&stroke_text_only_board(BOARD_WITH_DIMENSIONS).unwrap()).unwrap();
        let fp = out.child("footprint").unwrap();
        assert_eq!(fp.children("dimension").count(), 1); // 部品図の層のものは渡さない
        assert_eq!(out.children("dimension").count(), 1);
        assert_eq!(out.children("table").count(), 1);
        assert!(!out.has("target")); // 外形の層は描かせない

        let dir = std::env::temp_dir().join(format!("krbr_rust_dimension_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("dims.kicad_pcb");
        std::fs::write(&file, BOARD_WITH_DIMENSIONS).unwrap();
        let b = crate::parser::load_board(&file);
        let _ = std::fs::remove_dir_all(&dir);
        let b = b.unwrap();
        assert_eq!(b.stroke_text_count(), 3);
        let d1 = b.items.iter().find(|it| it.key == "d1").unwrap();
        assert!(d1.stroke_text && d1.bbox.lo.x <= 110.0 && d1.bbox.hi.x >= 150.0 && d1.bbox.lo.y <= 143.85);
    }

    /// 銅箔・マスク・内層・部品図の層の文字を含む基板
    const BOARD_WITH_LAYERS: &str = r#"(kicad_pcb (version 20260206) (generator "pcbnew")
  (layers (0 "F.Cu" signal) (4 "In1.Cu" signal) (2 "B.Cu" signal "Bottom") (1 "F.Mask" user) (3 "B.Mask" user) (5 "F.SilkS" user "F.Silkscreen") (7 "B.SilkS" user "B.Silkscreen") (25 "Edge.Cuts" user))
  (footprint "Lib:Part" (layer "B.Cu") (uuid "fp1") (at 10 20)
    (property "Reference" "U1" (at 0 -2) (layer "B.SilkS") (effects (font (size 1 1) (thickness 0.15)) (justify mirror)))
    (property "Value" "X" (at 0 2) (layer "B.Fab") (effects (font (size 1 1))))
    (fp_text user "TTCU" (at 0 0) (layer "B.Cu") (effects (font (face "Arial") (size 1 1)) (justify mirror)) (render_cache "TTCU" 0 (polygon (pts (xy 0 0) (xy 1 0) (xy 1 1)))))
    (fp_text user "STROKECU" (at 0 1) (layer "B.Cu") (effects (font (size 1 1) (thickness 0.15)) (justify mirror))))
  (gr_text "CU" (at 5 5) (layer "F.Cu") (uuid "t1") (effects (font (size 1 1) (thickness 0.15))))
  (gr_text "MASK" (at 5 8) (layer "F.Mask") (uuid "t2") (effects (font (size 1 1) (thickness 0.15))))
  (gr_text "INNER" (at 5 11) (layer "In1.Cu") (uuid "t3") (effects (font (size 1 1) (thickness 0.15))))
  (gr_text "TTCU" (at 5 14) (layer "F.Cu") (uuid "t4") (effects (font (face "Arial") (size 1 1))) (render_cache "TTCU" 0 (polygon (pts (xy 5 14) (xy 6 14) (xy 6 15)))))
  (gr_line (start 0 0) (end 10 0) (stroke (width 0.1) (type solid)) (layer "F.Cu"))
)"#;

    #[test]
    fn keeps_stroke_texts_on_copper_and_mask_and_fixes_layer_names() {
        let out = parse(&stroke_text_only_board(BOARD_WITH_LAYERS).unwrap()).unwrap();
        // 銅箔・マスクの線フォントの文字は残り、内層の文字と TrueType の文字は消える
        assert_eq!(out.children("gr_text").map(|t| t.str(1)).collect::<Vec<_>>(), vec!["CU", "MASK"]);
        assert!(!out.has("gr_line"));
        // 部品の銅箔の線フォントの文字は残り、TrueType の文字は同じ面の部品図の層へ移る
        let fp = out.child("footprint").unwrap();
        let texts: Vec<(&str, &str)> = fp.children("fp_text").map(|t| (t.str(2), layer_of(t))).collect();
        assert_eq!(texts, vec![("TTCU", "B.Fab"), ("STROKECU", "B.Cu")]);
        // 書き出す層の表示名は決まった名前になり、ほかの層はそのまま
        let layers = out.child("layers").unwrap();
        let shown = |name: &str| layers.items.iter().find(|l| l.is_list && l.str(1) == name).map(|l| l.str(3).to_string()).unwrap();
        for (name, fixed) in PLOT_LAYERS {
            assert_eq!(shown(name), fixed, "{name}");
        }
        assert_eq!(shown("In1.Cu"), "");
        assert_eq!(layers.items.iter().find(|l| l.is_list && l.str(1) == "Edge.Cuts").unwrap().len(), 3);

        // パーサが数える文字：CU、MASK、部品の U1 と STROKECU（内層と TrueType は数えない）
        let dir = std::env::temp_dir().join(format!("krbr_rust_layer_text_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("layers.kicad_pcb");
        std::fs::write(&file, BOARD_WITH_LAYERS).unwrap();
        let b = crate::parser::load_board(&file);
        let _ = std::fs::remove_dir_all(&dir);
        let b = b.unwrap();
        assert_eq!(b.stroke_text_count(), 4);
        let flagged = |key: &str| b.items.iter().find(|it| it.key == key).unwrap().stroke_text;
        assert!(flagged("t1") && flagged("t2") && flagged("fp1"));
        assert!(!flagged("t3") && !flagged("t4"));
    }
}
