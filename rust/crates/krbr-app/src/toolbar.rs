//! メニューの下のボタン列：大きめのアイコンと、その下の短い名前のボタン。アイコンは線と図形で描く（画像やフォントを足さない）

use egui::{pos2, vec2, Color32, Pos2, Rect, Shape, Stroke};

/// ボタンの大きさ（egui のポイント）。名前は「スルーホール」「Through-hole」が 1 行に収まる幅
const BUTTON_W: f32 = 80.0;
const BUTTON_H: f32 = 56.0;
/// アイコンを描く正方形の一辺
const ICON: f32 = 28.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Pause,
    Resume,
    Done,
    TopView,
    BottomView,
    Fit,
    Smd,
    ThroughHole,
    Denoise,
}

/// ボタンを 1 つ置く。on なら押してある状態（選択の色の背景）、crossed なら斜線を重ねる（隠している）、
/// enabled でなければ押せない（薄く描く）
pub fn button(ui: &mut egui::Ui, icon: Icon, label: &str, on: bool, crossed: bool, enabled: bool) -> egui::Response {
    let sense = if enabled { egui::Sense::click() } else { egui::Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(BUTTON_W, BUTTON_H), sense);
    if ui.is_rect_visible(rect) {
        let v = ui.visuals();
        let p = ui.painter();
        if on {
            p.rect_filled(rect, 5.0, v.selection.bg_fill);
        } else if enabled && resp.hovered() {
            p.rect_filled(rect, 5.0, v.widgets.hovered.weak_bg_fill);
        }
        let fg = if !enabled {
            v.weak_text_color()
        } else if on {
            v.selection.stroke.color
        } else {
            v.widgets.inactive.fg_stroke.color
        };
        let icon_rect = Rect::from_center_size(pos2(rect.center().x, rect.top() + 6.0 + ICON / 2.0), vec2(ICON, ICON));
        draw(p, icon, icon_rect, fg);
        if crossed {
            p.line_segment([icon_rect.left_bottom() + vec2(1.0, -1.0), icon_rect.right_top() + vec2(-1.0, 1.0)], Stroke::new(2.5, Color32::from_rgb(230, 90, 80)));
        }
        p.text(pos2(rect.center().x, rect.bottom() - 5.0), egui::Align2::CENTER_BOTTOM, label, egui::FontId::proportional(12.0), fg);
    }
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// 正方形 r の中にアイコンを描く（座標は r の左上を 0、一辺を 1 とした割合で決める）
fn draw(p: &egui::Painter, icon: Icon, r: Rect, c: Color32) {
    let at = |x: f32, y: f32| -> Pos2 { pos2(r.left() + x * r.width(), r.top() + y * r.height()) };
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_two_pos(at(x0, y0), at(x1, y1));
    let dim = c.gamma_multiply(0.45);
    match icon {
        Icon::Pause => {
            p.rect_filled(rect(0.22, 0.15, 0.42, 0.85), 2.0, c);
            p.rect_filled(rect(0.58, 0.15, 0.78, 0.85), 2.0, c);
        }
        Icon::Resume => {
            p.add(Shape::convex_polygon(vec![at(0.25, 0.12), at(0.85, 0.5), at(0.25, 0.88)], c, Stroke::NONE));
        }
        Icon::Done => {
            p.add(Shape::line(vec![at(0.15, 0.52), at(0.4, 0.78), at(0.88, 0.22)], Stroke::new(3.0, c)));
        }
        // 基板（横向きの板）と部品、見る向きの矢印
        Icon::TopView | Icon::BottomView => {
            let top = icon == Icon::TopView;
            // 板は矢印と反対の側に置く。部品はいつも板の上面
            let (board_y, arrow) = if top { (0.78, (0.12, 0.55)) } else { (0.3, (0.92, 0.5)) };
            p.rect_filled(rect(0.05, board_y, 0.95, board_y + 0.1), 1.0, dim);
            p.rect_filled(rect(0.2, board_y - 0.12, 0.38, board_y), 1.0, c);
            p.rect_filled(rect(0.55, board_y - 0.2, 0.8, board_y), 1.0, c);
            let (y0, y1) = arrow;
            let x = 0.5;
            p.line_segment([at(x, y0), at(x, y1 - if top { 0.08 } else { -0.08 })], Stroke::new(2.4, c));
            let dir = if top { 1.0 } else { -1.0 };
            p.add(Shape::convex_polygon(vec![at(x - 0.14, y1 - 0.12 * dir), at(x + 0.14, y1 - 0.12 * dir), at(x, y1 + 0.06 * dir)], c, Stroke::NONE));
        }
        // 全体を表示：四隅の枠の中に基板
        Icon::Fit => {
            let k = 0.22;
            for (x, y, dx, dy) in [(0.08, 0.12, 1.0, 1.0), (0.92, 0.12, -1.0, 1.0), (0.08, 0.88, 1.0, -1.0), (0.92, 0.88, -1.0, -1.0)] {
                p.add(Shape::line(vec![at(x + k * dx, y), at(x, y), at(x, y + k * dy)], Stroke::new(2.2, c)));
            }
            p.rect_filled(rect(0.26, 0.34, 0.74, 0.66), 2.0, dim);
            p.rect_filled(rect(0.34, 0.42, 0.5, 0.56), 1.0, c);
        }
        // 表面実装：板の上に載ったチップ（両端が電極）
        Icon::Smd => {
            p.rect_filled(rect(0.05, 0.72, 0.95, 0.84), 1.0, dim);
            p.rect_filled(rect(0.22, 0.48, 0.78, 0.72), 1.0, c);
            p.rect_filled(rect(0.14, 0.48, 0.3, 0.72), 1.0, dim);
            p.rect_filled(rect(0.7, 0.48, 0.86, 0.72), 1.0, dim);
        }
        // スルーホール：本体から足が板を貫く
        Icon::ThroughHole => {
            p.rect_filled(rect(0.05, 0.58, 0.95, 0.7), 1.0, dim);
            p.rect_filled(rect(0.22, 0.12, 0.78, 0.44), 3.0, c);
            for x in [0.34, 0.66] {
                p.line_segment([at(x, 0.44), at(x, 0.92)], Stroke::new(2.2, c));
            }
        }
        // ノイズ除去：粒のある左から、なめらかな右へ（きらめき）
        Icon::Denoise => {
            for (x, y) in [(0.12, 0.2), (0.3, 0.12), (0.18, 0.42), (0.36, 0.34), (0.1, 0.66), (0.28, 0.58), (0.2, 0.86), (0.4, 0.78)] {
                p.circle_filled(at(x, y), 1.4, dim);
            }
            // 4 つの尖りを持つ星は凸でないので、縦と横の細い菱形を重ねて描く
            for (cx, cy, s) in [(0.68, 0.45, 0.3), (0.9, 0.12, 0.12)] {
                p.add(Shape::convex_polygon(vec![at(cx, cy - s), at(cx + s * 0.28, cy), at(cx, cy + s), at(cx - s * 0.28, cy)], c, Stroke::NONE));
                p.add(Shape::convex_polygon(vec![at(cx - s, cy), at(cx, cy - s * 0.28), at(cx + s, cy), at(cx, cy + s * 0.28)], c, Stroke::NONE));
            }
        }
    }
}
