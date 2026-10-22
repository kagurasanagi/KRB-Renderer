//! 材質エディタ（ビューアの中の別の窓）：分類ごとの材質の見本の一覧、選んだ部品だけのプレビュー、部位ごとの色・粗さの
//! 調整（材質名の +color= / +rough=）、自分の材質（全体設定の materials/user.toml）の作成と編集。
//! 設定ファイルへの書き込みはビューアが行う（ここは Action を返すだけ）。見本とプレビューの描画は render（GPU が待機中に呼ぶ）

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec3;
use krbr_render::Context;
use krbr_scene::material_library::{builtin_names, builtin_order, valid_material_name, Variant};
use krbr_scene::{conductor, linear_to_srgb, srgb_to_linear, Category, Material, MaterialLibrary, Scene};

use crate::fl;
use crate::i18n;
use crate::preview::{part_camera, part_scene, swatch_camera, swatch_materials, swatch_scene, Preview, PreviewLight, SWATCH_PX};

/// 編集が止まってから書き込むまでの時間
const EDIT_DELAY: Duration = Duration::from_millis(400);
/// 部品のプレビューの大きさ（画素）と、積む最大のサンプル数
const PART_W: u32 = 400;
const PART_H: u32 = 300;
const PART_MAX_SPP: u32 = 256;
/// 見本 1 枚に積むサンプル数
const SWATCH_SPP: u32 = 64;

// ---------------------------------------------------------------- 表示名

/// 材質の表示名（組み込みの材質は訳、自分の材質は名前のまま）。変種の指定は含めない
pub fn material_label(name: &str) -> String {
    let (base, _) = Variant::parse(name);
    i18n::get(&format!("matname-{base}")).unwrap_or_else(|| base.to_string())
}

pub fn category_label(c: Category) -> String {
    i18n::get(&format!("matcat-{}", c.as_str())).unwrap_or_else(|| c.as_str().to_string())
}

pub fn conductor_label(id: &str) -> String {
    i18n::get(&format!("conductor-{id}")).unwrap_or_else(|| id.to_string())
}

/// 分類ごとの材質の名前（分類の順、組み込みはファイルの順、自分の材質は名前の順で後ろ）
pub fn grouped_names(lib: &MaterialLibrary) -> Vec<(Category, Vec<String>)> {
    let builtin = builtin_names();
    let mut names: Vec<String> = builtin_order().iter().filter(|n| lib.find(n).is_some()).cloned().collect();
    names.extend(lib.names().into_iter().filter(|n| !builtin.contains(n)));
    Category::ALL
        .into_iter()
        .map(|c| (c, names.iter().filter(|n| lib.find(n).is_some_and(|m| m.category() == c)).cloned().collect::<Vec<_>>()))
        .filter(|(_, v)| !v.is_empty())
        .collect()
}

pub fn is_user_material(name: &str) -> bool {
    !builtin_names().contains(name)
}

/// 材質の説明（出典・屈折率）
fn material_details(m: &Material) -> String {
    if let Some(c) = m.conductor.and_then(conductor::get) {
        return fl!("mated-measured", source = c.source);
    }
    let mut s = fl!("mated-ior-value", ior = format!("{:.3}", m.ior_at(587.6)));
    if m.sellmeier.is_some() {
        s += &fl!("mated-sellmeier");
    }
    s
}

// ---------------------------------------------------------------- ビューアとのやりとり

/// 選んだ部品の部位 1 つ
#[derive(Debug, Clone)]
pub struct PartRow {
    /// 部位のキー（設定ファイルに書くもの。空なら部位の区別が無い）
    pub key: String,
    /// 割り当てられている材質名（変種の指定を含む）
    pub material: String,
    /// シーンの材質の番号
    pub scene_material: u32,
}

/// 選んだ部品（材質エディタに見せるもの）
#[derive(Debug, Clone)]
pub struct PartInfo {
    pub reference: String,
    pub model: Option<usize>,
    /// 部品のインスタンスすべて（プレビューに描く）
    pub instances: Vec<usize>,
    /// 選んだ 3D モデルの部位（代わりの箱なら空）
    pub rows: Vec<PartRow>,
}

/// ビューアに頼むこと
pub enum Action {
    /// 部位（空なら全部位）に材質を書き込む
    Assign { part: String, material: String },
    /// 自分の材質を書く（同じ名前があれば置き換え）
    SaveUser { name: String, material: Box<Material> },
    DeleteUser(String),
}

// ---------------------------------------------------------------- 状態

/// 部位の色・粗さの調整（止まってから書き込む）
struct Adjust {
    reference: String,
    part: String,
    material: String,
    changed_at: Instant,
    written: bool,
}

/// 自分の材質の編集中の値（止まってから書き込む）
struct UserEdit {
    name: String,
    material: Material,
    changed_at: Option<Instant>,
}

/// 新しい自分の材質の名前を決めているところ
struct Naming {
    name: String,
    material: Material,
    /// 作ったら割り当てる部位（部位の調整から作ったとき）と、残す変種（模様なし・発光）
    assign_to: Option<(String, Variant)>,
}

struct PartView {
    reference: String,
    model: Option<usize>,
    epoch: u64,
    preview: Option<Preview>,
    /// 元のシーンの材質の番号 → プレビューのシーンの材質の番号
    map: Vec<(u32, u32)>,
    /// 組み立てたときのプレビューのシーンの材質（調整の無いもの）
    base: Vec<Material>,
    tex: Option<egui::TextureHandle>,
    shown_samples: u32,
    azimuth: f64,
    elevation: f64,
    zoom: f32,
    error: String,
}

#[derive(Default)]
pub struct MaterialEditor {
    pub open: bool,
    egui: Option<egui::Context>,
    light: Option<Arc<PreviewLight>>,
    swatch: Option<Preview>,
    thumbs: HashMap<String, egui::TextureHandle>,
    /// このフレームで表示したかった見本（描いていないもの）
    wanted: Vec<String>,
    part: Option<PartView>,
    /// 一覧で選んだ材質
    browsing: Option<String>,
    /// 割り当て先の部位（None なら全部位）
    target: Option<String>,
    search: String,
    adjust: Option<Adjust>,
    user_edit: Option<UserEdit>,
    naming: Option<Naming>,
    confirm_delete: bool,
    error: String,
}


impl MaterialEditor {
    /// 裏で描くもの（見本・プレビューの累積）が残っているか（ビューアは休まずにフレームを回す）
    pub fn busy(&self) -> bool {
        self.open && (!self.wanted.is_empty() || self.part.as_ref().is_some_and(|p| p.preview.as_ref().is_some_and(|pr| pr.samples() < PART_MAX_SPP)) || self.adjust.as_ref().is_some_and(|a| !a.written) || self.user_edit.as_ref().is_some_and(|u| u.changed_at.is_some()))
    }

    /// 材質名の材質（編集中の自分の材質は、その値）
    fn material_for(&self, lib: &MaterialLibrary, name: &str) -> Option<Material> {
        let (base, v) = Variant::parse(name);
        match &self.user_edit {
            Some(u) if u.name == base => {
                let mut m = u.material.clone();
                v.apply(&mut m);
                Some(m)
            }
            _ => lib.find_variant(name),
        }
    }

    /// 編集中の値（書き込み前の調整）を含めた、部位の材質名
    fn row_material(&self, reference: &str, row: &PartRow) -> String {
        match &self.adjust {
            Some(a) if a.reference == reference && a.part == row.key => a.material.clone(),
            _ => row.material.clone(),
        }
    }

    /// 選んだ部品が変わった・シーンを組み直した（調整の書き込みが反映された）ときの片付け
    fn sync_selection(&mut self, part: Option<&PartInfo>) {
        let reference = part.map(|p| p.reference.as_str());
        if let Some(a) = &self.adjust {
            let reflected = part.is_some_and(|p| p.rows.iter().any(|r| r.key == a.part && r.material == a.material));
            if Some(a.reference.as_str()) != reference || (a.written && reflected) {
                self.adjust = None;
            }
        }
        if let (Some(t), Some(p)) = (&self.target, part) {
            if !p.rows.iter().any(|r| &r.key == t) {
                self.target = None;
            }
        }
        if part.is_none() {
            self.target = None;
        }
    }

    // ------------------------------------------------------------ UI

    /// 窓を描く。scope は書き込む先を選ぶ UI（ビューアの設定パネルと同じもの）
    pub fn ui(&mut self, ctx: &egui::Context, lib: &MaterialLibrary, part: Option<&PartInfo>, scope: &mut dyn FnMut(&mut egui::Ui)) -> Vec<Action> {
        self.egui.get_or_insert_with(|| ctx.clone());
        self.wanted.clear();
        let mut actions = Vec::new();
        if !self.open {
            return actions;
        }
        self.sync_selection(part);
        let mut open = self.open;
        egui::Window::new(fl!("mated-title")).id(egui::Id::new("material_editor")).open(&mut open).default_pos([40.0, 90.0]).default_size([860.0, 620.0]).min_width(640.0).resizable(true).collapsible(true).show(ctx, |ui| {
            ui.columns_const(|[left, right]| {
                self.browser_ui(left, lib, part, &mut actions);
                egui::ScrollArea::vertical().id_salt("mated_right").auto_shrink([false, false]).show(right, |ui| {
                    self.part_ui(ui, lib, part, scope, &mut actions);
                    ui.separator();
                    self.details_ui(ui, lib, part, &mut actions);
                });
            });
        });
        self.open = open;
        self.flush(part, &mut actions, false);
        actions
    }

    /// 止まった編集を書き込む（now なら待たずに）
    fn flush(&mut self, part: Option<&PartInfo>, actions: &mut Vec<Action>, now: bool) {
        if let Some(a) = &mut self.adjust {
            if !a.written && (now || a.changed_at.elapsed() >= EDIT_DELAY) && part.is_some_and(|p| p.reference == a.reference) {
                actions.push(Action::Assign { part: a.part.clone(), material: a.material.clone() });
                a.written = true;
            }
        }
        if self.user_edit.as_ref().is_some_and(|u| u.changed_at.is_some_and(|t| now || t.elapsed() >= EDIT_DELAY)) {
            self.flush_user(actions);
        }
    }

    /// 見本の画像（無ければ灰色の四角を描いて、描くように頼む）
    fn swatch(&mut self, ui: &mut egui::Ui, key: &str, size: f32) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
        if ui.is_rect_visible(rect) {
            match self.thumbs.get(key) {
                Some(t) => {
                    egui::Image::new((t.id(), rect.size())).corner_radius(4.0).paint_at(ui, rect);
                }
                None => {
                    ui.painter().rect_filled(rect, 4.0, egui::Color32::from_gray(60));
                    if !self.wanted.iter().any(|w| w == key) {
                        self.wanted.push(key.to_string());
                    }
                }
            }
        }
        resp
    }

    /// 左：検索と、分類ごとの見本の一覧
    fn browser_ui(&mut self, ui: &mut egui::Ui, lib: &MaterialLibrary, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(fl!("mated-search"));
            ui.add(egui::TextEdit::singleline(&mut self.search).desired_width(ui.available_width()));
        });
        let query = self.search.trim().to_lowercase();
        // 割り当て先の部位の今の材質（一覧で印を付ける）
        let current: HashSet<String> = part.map(|p| p.rows.iter().filter(|r| self.target.as_ref().is_none_or(|t| *t == r.key)).map(|r| Variant::parse(&self.row_material(&p.reference, r)).0.to_string()).collect()).unwrap_or_default();
        egui::ScrollArea::vertical().id_salt("mated_left").auto_shrink([false, false]).show(ui, |ui| {
            for (cat, names) in grouped_names(lib) {
                let shown: Vec<&String> = names.iter().filter(|n| query.is_empty() || n.to_lowercase().contains(&query) || material_label(n).to_lowercase().contains(&query)).collect();
                if shown.is_empty() {
                    continue;
                }
                egui::CollapsingHeader::new(format!("{} ({})", category_label(cat), shown.len())).id_salt(cat.as_str()).default_open(true).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                        for name in shown {
                            self.tile(ui, lib, name, current.contains(name.as_str()), part, actions);
                        }
                    });
                });
            }
        });
    }

    /// 一覧の 1 つ：見本と名前。クリックで選び、ダブルクリックで割り当てる
    fn tile(&mut self, ui: &mut egui::Ui, lib: &MaterialLibrary, name: &str, current: bool, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        const W: f32 = 84.0;
        let selected = self.browsing.as_deref() == Some(name);
        let user = is_user_material(name);
        let resp = ui
            .allocate_ui_with_layout(egui::vec2(W, W + 30.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.set_width(W);
                let r = self.swatch(ui, name, W - 8.0);
                let mut text = egui::RichText::new(material_label(name)).small();
                if current {
                    text = text.strong().color(egui::Color32::from_rgb(255, 170, 40));
                }
                ui.add(egui::Label::new(text).truncate());
                r
            })
            .inner;
        let rect = resp.rect.expand(3.0);
        if selected {
            ui.painter().rect_stroke(rect, 5.0, egui::Stroke::new(2.0, ui.visuals().selection.stroke.color), egui::StrokeKind::Outside);
        } else if resp.hovered() {
            ui.painter().rect_stroke(rect, 5.0, egui::Stroke::new(1.0, ui.visuals().widgets.hovered.fg_stroke.color), egui::StrokeKind::Outside);
        }
        if user {
            ui.painter().text(resp.rect.left_top() + egui::vec2(4.0, 2.0), egui::Align2::LEFT_TOP, "★", egui::FontId::proportional(12.0), egui::Color32::from_rgb(255, 214, 90));
        }
        let mut tip = format!("{}\n{}", material_label(name), name);
        if let Some(m) = lib.find(name) {
            tip += &format!("\n{}", material_details(m));
        }
        if user {
            tip += &format!("\n{}", fl!("mated-user-material"));
        }
        let resp = resp.on_hover_text(tip);
        if resp.clicked() {
            self.select_browsing(lib, name, actions);
        }
        if resp.double_clicked() && part.is_some_and(|p| !p.rows.is_empty()) {
            self.assign(name, part, actions);
        }
    }

    fn select_browsing(&mut self, lib: &MaterialLibrary, name: &str, actions: &mut Vec<Action>) {
        if self.browsing.as_deref() == Some(name) {
            return;
        }
        self.browsing = Some(name.to_string());
        self.naming = None;
        self.confirm_delete = false;
        // 自分の材質なら編集できるようにする（前の編集は先に書き込む）
        if self.user_edit.as_ref().is_some_and(|u| u.name == name) {
            return;
        }
        self.flush_user(actions);
        self.user_edit = if is_user_material(name) { lib.find(name).map(|m| UserEdit { name: name.to_string(), material: m.clone(), changed_at: None }) } else { None };
    }

    /// 自分の材質の編集を、待たずに書き込む
    fn flush_user(&mut self, actions: &mut Vec<Action>) {
        if let Some(u) = &mut self.user_edit {
            if u.changed_at.take().is_some() {
                actions.push(Action::SaveUser { name: u.name.clone(), material: Box::new(u.material.clone()) });
            }
        }
    }

    /// 一覧で選んだ材質を、割り当て先の部位（全部位）に書き込む。色・粗さの調整は前の材質に合わせたものなので外し、
    /// 模様のあり・なしと発光は保つ
    fn assign(&mut self, name: &str, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        let Some(p) = part else { return };
        self.flush(part, actions, true);
        self.adjust = None;
        let rows: Vec<&PartRow> = p.rows.iter().filter(|r| self.target.as_ref().is_none_or(|t| *t == r.key)).collect();
        if self.target.is_none() && rows.len() > 1 {
            actions.push(Action::Assign { part: String::new(), material: name.to_string() });
            return;
        }
        for r in rows {
            let (_, v) = Variant::parse(&r.material);
            let keep = Variant { smooth: v.smooth, emission: v.emission, ..Default::default() };
            actions.push(Action::Assign { part: r.key.clone(), material: keep.name(name) });
        }
    }

    /// 右上：選んだ部品のプレビューと、部位の一覧・調整
    fn part_ui(&mut self, ui: &mut egui::Ui, lib: &MaterialLibrary, part: Option<&PartInfo>, scope: &mut dyn FnMut(&mut egui::Ui), actions: &mut Vec<Action>) {
        let Some(p) = part else {
            ui.weak(fl!("mated-no-part"));
            return;
        };
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&p.reference).strong().color(egui::Color32::from_rgb(255, 170, 40)));
            ui.weak(fl!("mated-preview-hint"));
        });
        // プレビュー：ドラッグで回し、ホイールで拡大
        let w = ui.available_width().min(PART_W as f32);
        let size = egui::vec2(w, w * PART_H as f32 / PART_W as f32);
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        match self.part.as_ref().and_then(|v| v.tex.as_ref()) {
            Some(t) => {
                egui::Image::new((t.id(), size)).corner_radius(4.0).paint_at(ui, rect);
            }
            None => {
                ui.painter().rect_filled(rect, 4.0, egui::Color32::from_gray(40));
            }
        }
        if let Some(v) = &mut self.part {
            if resp.dragged() {
                let d = resp.drag_delta();
                v.azimuth -= d.x as f64 * 0.4;
                v.elevation = (v.elevation + d.y as f64 * 0.4).clamp(-89.0, 89.0);
            }
            if resp.hovered() {
                let s = ui.input(|i| i.smooth_scroll_delta.y);
                if s != 0.0 {
                    v.zoom = (v.zoom * 1.0015f32.powf(s)).clamp(0.3, 8.0);
                }
            }
            if resp.double_clicked() {
                (v.azimuth, v.elevation, v.zoom) = (45.0, 30.0, 1.0);
            }
            if !v.error.is_empty() {
                ui.colored_label(egui::Color32::from_rgb(255, 102, 102), &v.error);
            }
        }
        if p.rows.is_empty() {
            ui.weak(fl!("mated-placeholder"));
            return;
        }
        scope(ui);
        // 割り当て先の部位
        ui.label(fl!("mated-target"));
        let multi = p.rows.len() > 1;
        if multi && ui.radio(self.target.is_none(), fl!("part-whole")).clicked() {
            self.target = None;
        }
        let rows: Vec<(PartRow, String)> = p.rows.iter().map(|r| (r.clone(), self.row_material(&p.reference, r))).collect();
        for (r, material) in &rows {
            ui.horizontal(|ui| {
                let on = self.target.as_ref() == Some(&r.key) || !multi;
                if ui.radio(on, "").clicked() {
                    self.target = Some(r.key.clone());
                }
                let s = self.swatch(ui, material, 22.0);
                let shown = if r.key.is_empty() { fl!("part-whole") } else { r.key.strip_prefix("kicad:").unwrap_or(&r.key).to_string() };
                let resp = ui.selectable_label(on, format!("{shown}: {}", material_label(material))).on_hover_text(material.as_str());
                if resp.clicked() || s.clicked() {
                    self.target = Some(r.key.clone());
                    self.select_browsing(lib, Variant::parse(material).0, actions);
                }
            });
        }
        if !multi && self.target.is_none() {
            self.target = Some(p.rows[0].key.clone());
        }
        // 1 つの部位の調整（色・粗さ）
        let Some(t) = self.target.clone() else { return };
        let Some((row, material)) = rows.iter().find(|(r, _)| r.key == t).cloned() else { return };
        ui.add_space(4.0);
        ui.strong(fl!("mated-adjust"));
        let Some(base_m) = lib.find(Variant::parse(&material).0).cloned().or_else(|| self.material_for(lib, Variant::parse(&material).0)) else {
            ui.weak(fl!("mated-unknown-material", name = material.as_str()));
            return;
        };
        let (base, mut v) = Variant::parse(&material);
        let before = v;
        let measured = base_m.conductor.is_some();
        ui.horizontal(|ui| {
            let mut on = v.color.is_some();
            let tip = if measured { fl!("mated-color-measured-tip") } else { fl!("mated-color-tip") };
            if ui.checkbox(&mut on, fl!("mated-color")).on_hover_text(tip).changed() {
                // 初めの色：今の色（モデルの色を使う材質なら、灰色）
                let start = if base_m.base_color_from_model { Vec3::splat(0.5) } else if base_m.transmission_weight > 0.0 && base_m.base_metalness < 0.5 { base_m.transmission_color } else { base_m.base_color };
                v.color = on.then(|| srgb8(start));
            }
            if let Some(c) = &mut v.color {
                ui.color_edit_button_srgb(c);
                if measured {
                    ui.colored_label(egui::Color32::from_rgb(255, 204, 77), fl!("mated-color-measured"));
                }
            }
        });
        ui.horizontal(|ui| {
            let mut on = v.roughness.is_some();
            if ui.checkbox(&mut on, fl!("mated-roughness")).on_hover_text(fl!("mated-roughness-tip")).changed() {
                v.roughness = on.then_some(base_m.specular_roughness);
            }
            if let Some(r) = &mut v.roughness {
                ui.add(egui::Slider::new(r, 0.0..=1.0).step_by(0.01));
            } else {
                ui.weak(format!("{:.2}", base_m.specular_roughness));
            }
        });
        if v != before {
            let name = v.name(base);
            match &mut self.adjust {
                Some(a) if a.reference == p.reference && a.part == row.key => {
                    a.material = name;
                    a.changed_at = Instant::now();
                    a.written = false;
                }
                _ => {
                    self.flush(part, actions, true);
                    self.adjust = Some(Adjust { reference: p.reference.clone(), part: row.key.clone(), material: name, changed_at: Instant::now(), written: false });
                }
            }
        }
        // 調整したものを自分の材質にする
        if (v.color.is_some() || v.roughness.is_some()) && ui.button(fl!("mated-save-adjusted")).on_hover_text(fl!("mated-save-adjusted-tip")).clicked() {
            if let Some(mut m) = self.material_for(lib, &Variant { emission: None, smooth: false, ..v }.name(base)) {
                m.category = Some(base_m.category());
                let keep = Variant { smooth: v.smooth, emission: v.emission, ..Default::default() };
                self.naming = Some(Naming { name: unique_name(lib, &format!("{} {}", material_label(base), p.reference)), material: m, assign_to: Some((row.key.clone(), keep)) });
                self.browsing = None;
            }
        }
    }

    /// 右下：一覧で選んだ材質の説明・割り当て・複製、自分の材質の編集
    fn details_ui(&mut self, ui: &mut egui::Ui, lib: &MaterialLibrary, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        if let Some(n) = &mut self.naming {
            ui.strong(fl!("mated-new-title"));
            ui.horizontal(|ui| {
                ui.label(fl!("mated-new-name"));
                ui.text_edit_singleline(&mut n.name);
            });
            let valid = valid_material_name(n.name.trim());
            let exists = lib.find(n.name.trim()).is_some();
            if !valid {
                ui.colored_label(egui::Color32::from_rgb(255, 102, 102), fl!("mated-name-invalid"));
            } else if exists {
                ui.colored_label(egui::Color32::from_rgb(255, 102, 102), fl!("mated-name-exists"));
            }
            let mut done = None;
            ui.horizontal(|ui| {
                if ui.add_enabled(valid && !exists, egui::Button::new(fl!("mated-create"))).clicked() {
                    done = Some(true);
                }
                if ui.button(fl!("mated-cancel")).clicked() {
                    done = Some(false);
                }
            });
            match done {
                Some(true) => {
                    let n = self.naming.take().unwrap();
                    let name = n.name.trim().to_string();
                    let mut m = n.material;
                    m.name = name.clone();
                    self.flush_user(actions);
                    actions.push(Action::SaveUser { name: name.clone(), material: Box::new(m.clone()) });
                    if let (Some((part_key, keep)), Some(p)) = (n.assign_to, part) {
                        actions.push(Action::Assign { part: part_key.clone(), material: keep.name(&name) });
                        self.adjust = Some(Adjust { reference: p.reference.clone(), part: part_key, material: keep.name(&name), changed_at: Instant::now(), written: true });
                    }
                    self.browsing = Some(name.clone());
                    self.user_edit = Some(UserEdit { name, material: m, changed_at: None });
                }
                Some(false) => self.naming = None,
                None => {}
            }
            return;
        }
        let Some(name) = self.browsing.clone() else {
            ui.weak(fl!("mated-choose-hint"));
            return;
        };
        let Some(m) = self.material_for(lib, &name) else {
            self.browsing = None;
            return;
        };
        ui.horizontal(|ui| {
            self.swatch(ui, &name, 96.0);
            ui.vertical(|ui| {
                ui.heading(material_label(&name));
                ui.weak(format!("{name}  ·  {}", category_label(m.category())));
                ui.label(material_details(&m));
                if is_user_material(&name) {
                    ui.weak(fl!("mated-user-material"));
                }
            });
        });
        ui.horizontal(|ui| {
            let can_assign = part.is_some_and(|p| !p.rows.is_empty());
            let label = if self.target.is_none() && part.is_some_and(|p| p.rows.len() > 1) { fl!("mated-assign-all") } else { fl!("mated-assign") };
            if ui.add_enabled(can_assign, egui::Button::new(label)).on_hover_text(fl!("mated-assign-tip")).clicked() {
                self.assign(&name, part, actions);
            }
            if ui.button(fl!("mated-duplicate")).on_hover_text(fl!("mated-duplicate-tip")).clicked() {
                let mut d = m.clone();
                d.category = Some(m.category());
                self.naming = Some(Naming { name: unique_name(lib, &fl!("mated-copy-of", name = material_label(&name))), material: d, assign_to: None });
            }
        });
        if !is_user_material(&name) {
            ui.weak(fl!("mated-builtin-readonly"));
            return;
        }
        ui.separator();
        self.user_editor_ui(ui, &name);
        ui.horizontal(|ui| {
            if !self.confirm_delete {
                if ui.button(fl!("mated-delete")).clicked() {
                    self.confirm_delete = true;
                }
            } else {
                ui.colored_label(egui::Color32::from_rgb(255, 204, 77), fl!("mated-delete-confirm"));
                if ui.button(fl!("mated-delete")).clicked() {
                    actions.push(Action::DeleteUser(name.clone()));
                    self.user_edit = None;
                    self.browsing = None;
                    self.confirm_delete = false;
                    self.thumbs.retain(|k, _| Variant::parse(k).0 != name);
                }
                if ui.button(fl!("mated-cancel")).clicked() {
                    self.confirm_delete = false;
                }
            }
        });
        if !self.error.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(255, 102, 102), &self.error);
        }
    }

    /// 自分の材質のパラメータ。種類（誘電体・金属・透明）ごとに、物理的に意味のある項目だけを出す
    fn user_editor_ui(&mut self, ui: &mut egui::Ui, name: &str) {
        let Some(u) = self.user_edit.as_mut().filter(|u| u.name == name) else { return };
        let before = u.material.clone();
        let m = &mut u.material;
        ui.strong(fl!("mated-edit-title"));
        #[derive(PartialEq, Clone, Copy)]
        enum Kind {
            Dielectric,
            Metal,
            Clear,
        }
        let kind = if m.base_metalness > 0.5 {
            Kind::Metal
        } else if m.transmission_weight > 0.0 {
            Kind::Clear
        } else {
            Kind::Dielectric
        };
        let mut new_kind = kind;
        ui.horizontal(|ui| {
            ui.radio_value(&mut new_kind, Kind::Dielectric, fl!("mated-kind-dielectric")).on_hover_text(fl!("mated-kind-dielectric-tip"));
            ui.radio_value(&mut new_kind, Kind::Metal, fl!("mated-kind-metal")).on_hover_text(fl!("mated-kind-metal-tip"));
            ui.radio_value(&mut new_kind, Kind::Clear, fl!("mated-kind-clear")).on_hover_text(fl!("mated-kind-clear-tip"));
        });
        if new_kind != kind {
            // 種類に合わせて、ほかの種類の項目を物理的に矛盾しない値にする
            m.subsurface_weight = 0.0;
            match new_kind {
                Kind::Dielectric => {
                    m.base_metalness = 0.0;
                    m.conductor = None;
                    m.transmission_weight = 0.0;
                    m.specular_color = Vec3::ONE;
                    m.base_color = m.base_color.min(Vec3::splat(0.9));
                }
                Kind::Metal => {
                    m.base_metalness = 1.0;
                    m.transmission_weight = 0.0;
                    m.coat_weight = 0.0;
                    m.base_color_from_model = false;
                    if let Some(c) = conductor::find("Ni") {
                        m.set_conductor(c);
                    }
                }
                Kind::Clear => {
                    m.base_metalness = 0.0;
                    m.conductor = None;
                    m.transmission_weight = 1.0;
                    m.base_color = Vec3::ONE;
                    m.base_color_from_model = false;
                    m.specular_color = Vec3::ONE;
                    m.bump_pattern = krbr_scene::BumpPattern::None;
                    m.bump_strength = 0.0;
                    m.roughness_variation = 0.0;
                }
            }
        }
        egui::Grid::new("mated_params").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            match new_kind {
                Kind::Dielectric => {
                    ui.label(fl!("mated-base-color"));
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut m.base_color_from_model, fl!("mated-from-model")).on_hover_text(fl!("mated-from-model-tip"));
                        if !m.base_color_from_model {
                            color_edit(ui, &mut m.base_color);
                            albedo_warning(ui, m.base_color);
                        }
                    });
                    ui.end_row();
                    roughness_row(ui, &mut m.specular_roughness);
                    ior_row(ui, m);
                    ui.label(fl!("mated-coat"));
                    ui.horizontal(|ui| {
                        let mut coat = m.coat_weight > 0.0;
                        if ui.checkbox(&mut coat, "").on_hover_text(fl!("mated-coat-tip")).changed() {
                            m.coat_weight = if coat { 1.0 } else { 0.0 };
                        }
                        if coat {
                            ui.label(fl!("mated-roughness"));
                            ui.add(egui::Slider::new(&mut m.coat_roughness, 0.0..=1.0).step_by(0.01));
                        }
                    });
                    ui.end_row();
                }
                Kind::Metal => {
                    ui.label(fl!("mated-metal"));
                    let current = m.conductor.and_then(conductor::get);
                    let text = current.map(|c| conductor_label(c.id)).unwrap_or_else(|| fl!("mated-metal-custom"));
                    egui::ComboBox::from_id_salt("mated_conductor").selected_text(text).width(220.0).show_ui(ui, |ui| {
                        for c in conductor::all() {
                            if ui.selectable_label(current.is_some_and(|x| x.index == c.index), conductor_label(c.id)).on_hover_text(c.source).clicked() {
                                m.set_conductor(c);
                            }
                        }
                        if ui.selectable_label(current.is_none(), fl!("mated-metal-custom")).on_hover_text(fl!("mated-metal-custom-tip")).clicked() {
                            m.conductor = None;
                        }
                    });
                    ui.end_row();
                    if m.conductor.is_none() {
                        ui.label(fl!("mated-f0"));
                        ui.horizontal(|ui| {
                            color_edit(ui, &mut m.base_color);
                            ui.weak(fl!("mated-f0-tip"));
                        });
                        ui.end_row();
                        ui.label(fl!("mated-f82"));
                        ui.horizontal(|ui| {
                            color_edit(ui, &mut m.specular_color);
                            ui.weak(fl!("mated-f82-tip"));
                        });
                        ui.end_row();
                    }
                    roughness_row(ui, &mut m.specular_roughness);
                }
                Kind::Clear => {
                    ui.label(fl!("mated-trans-color"));
                    ui.horizontal(|ui| {
                        color_edit(ui, &mut m.transmission_color);
                        ui.label(fl!("mated-trans-depth"));
                        ui.add(egui::DragValue::new(&mut m.transmission_depth).range(0.0..=1000.0).speed(0.1).suffix(" mm")).on_hover_text(fl!("mated-trans-depth-tip"));
                    });
                    ui.end_row();
                    roughness_row(ui, &mut m.specular_roughness);
                    ior_row(ui, m);
                    if m.sellmeier.is_none() {
                        ui.label(fl!("mated-abbe"));
                        ui.horizontal(|ui| {
                            let mut on = m.transmission_dispersion_scale > 0.0;
                            if ui.checkbox(&mut on, "").on_hover_text(fl!("mated-abbe-tip")).changed() {
                                m.transmission_dispersion_scale = if on { 1.0 } else { 0.0 };
                            }
                            if on {
                                ui.add(egui::DragValue::new(&mut m.transmission_dispersion_abbe_number).range(10.0..=100.0).speed(0.2));
                            }
                        });
                        ui.end_row();
                    }
                }
            }
            if new_kind != Kind::Clear {
                ui.label(fl!("mated-pattern"));
                ui.horizontal(|ui| {
                    let mut on = m.bump_pattern != krbr_scene::BumpPattern::None;
                    if ui.checkbox(&mut on, "").on_hover_text(fl!("mated-pattern-tip")).changed() {
                        if on {
                            m.bump_pattern = krbr_scene::BumpPattern::Noise;
                            m.bump_strength = 0.012;
                            m.bump_scale = 0.2;
                            m.roughness_variation = 0.02;
                        } else {
                            m.bump_pattern = krbr_scene::BumpPattern::None;
                            m.roughness_variation = 0.0;
                        }
                    }
                    if on {
                        ui.add(egui::Slider::new(&mut m.bump_strength, 0.0..=0.05).text(fl!("mated-pattern-strength")));
                    }
                });
                ui.end_row();
            }
            ui.label(fl!("mated-category"));
            let mut cat = m.category();
            egui::ComboBox::from_id_salt("mated_category").selected_text(category_label(cat)).show_ui(ui, |ui| {
                for c in Category::ALL {
                    ui.selectable_value(&mut cat, c, category_label(c));
                }
            });
            if cat != m.category() {
                m.category = Some(cat);
            }
            ui.end_row();
        });
        if u.material != before {
            u.changed_at = Some(Instant::now());
            let name = u.name.clone();
            self.thumbs.retain(|k, _| Variant::parse(k).0 != name);
        }
    }

    // ------------------------------------------------------------ GPU（GPU が待機中に呼ぶ）

    /// 見本とプレビューを描く。epoch はシーンを組み直すたびに変わる数（変わったら部品のプレビューを作り直す）
    pub fn render(&mut self, ctx: &'static Context, main: &Scene, part: Option<&PartInfo>, lib: &MaterialLibrary, spectral: bool, epoch: u64) {
        if !self.open {
            // 閉じたら GPU の資源を返す（見本の画像は残す）
            self.swatch = None;
            if let Some(p) = &mut self.part {
                p.preview = None;
            }
            return;
        }
        let Some(egui_ctx) = self.egui.clone() else { return };
        let light = self.light.get_or_insert_with(PreviewLight::new).clone();
        // 見本：1 フレームに 2 枚まで
        let t0 = Instant::now();
        for key in std::mem::take(&mut self.wanted) {
            if t0.elapsed() > Duration::from_millis(12) {
                break;
            }
            if self.thumbs.contains_key(&key) {
                continue;
            }
            let Some(m) = self.material_for(lib, &key) else { continue };
            let r = (|| -> Result<egui::ColorImage, String> {
                if self.swatch.is_none() {
                    self.swatch = Some(Preview::new(ctx, swatch_scene(&m), &light, SWATCH_PX, SWATCH_PX, swatch_camera(), spectral)?);
                }
                let s = self.swatch.as_mut().unwrap();
                s.set_view(swatch_camera(), spectral);
                s.set_materials(swatch_materials(&m))?;
                s.add_samples(ctx, SWATCH_SPP)?;
                s.image(0.0)
            })();
            match r {
                Ok(img) => {
                    let tex = egui_ctx.load_texture(format!("swatch:{key}"), img, egui::TextureOptions::LINEAR);
                    self.thumbs.insert(key, tex);
                }
                Err(e) => {
                    krbr_core::warn!("material swatch {key}: {e}");
                    self.error = e;
                    self.swatch = None;
                    break;
                }
            }
        }
        self.render_part(ctx, &egui_ctx, &light, main, part, lib, spectral, epoch);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_part(&mut self, ctx: &'static Context, egui_ctx: &egui::Context, light: &PreviewLight, main: &Scene, part: Option<&PartInfo>, lib: &MaterialLibrary, spectral: bool, epoch: u64) {
        let Some(p) = part else {
            self.part = None;
            return;
        };
        let same = self.part.as_ref().is_some_and(|v| v.reference == p.reference && v.model == p.model);
        if !same {
            self.part = Some(PartView { reference: p.reference.clone(), model: p.model, epoch: u64::MAX, preview: None, map: Vec::new(), base: Vec::new(), tex: None, shown_samples: 0, azimuth: 45.0, elevation: 30.0, zoom: 1.0, error: String::new() });
        }
        // 調整中の材質（書き込み前の色・粗さ、編集中の自分の材質）をすぐ見せる
        let mut overrides: Vec<(u32, String)> = Vec::new();
        for r in &p.rows {
            let name = self.row_material(&p.reference, r);
            let edited = self.user_edit.as_ref().is_some_and(|u| u.name == Variant::parse(&name).0);
            if name != r.material || edited {
                overrides.push((r.scene_material, name));
            }
        }
        let resolved: Vec<(u32, Option<Material>)> = overrides.iter().map(|(i, n)| (*i, self.material_for(lib, n))).collect();
        let v = self.part.as_mut().unwrap();
        if v.epoch != epoch && self.adjust.as_ref().is_some_and(|a| a.written) {
            // 書き込んだ調整は、組み直したシーンに入っている（別の上書きに隠れて入らなかったときも、表示はシーンに合わせる）
            self.adjust = None;
        }
        let r = (|| -> Result<(), String> {
            if v.preview.is_none() || v.epoch != epoch {
                let Some((scene, map)) = part_scene(main, &p.instances) else { return Err("the part has no geometry".into()) };
                v.base = scene.materials.clone();
                v.map = map;
                match &v.preview {
                    // 材質だけが変わった（自分の材質を保存した、など）：積分器を作り直さず、材質だけ差し替える（下の set_materials）
                    Some(pv) if same_geometry(pv.scene(), &scene) => {}
                    _ => {
                        let cam = part_camera(&scene, v.elevation, v.azimuth, v.zoom, PART_W as f32 / PART_H as f32);
                        v.preview = None;
                        v.preview = Some(Preview::new(ctx, scene, light, PART_W, PART_H, cam, spectral)?);
                        v.shown_samples = 0;
                    }
                }
                v.epoch = epoch;
            }
            let pv = v.preview.as_mut().unwrap();
            let mut mats = v.base.clone();
            for (main_id, m) in &resolved {
                let (Some(m), Some(&(_, local))) = (m, v.map.iter().find(|(a, _)| a == main_id)) else { continue };
                let local = local as usize;
                let mut m = m.clone();
                if m.base_color_from_model {
                    m.base_color = v.base[local].base_color; // 3D モデルの面の色
                }
                mats[local] = m;
            }
            pv.set_materials(mats)?;
            let cam = part_camera(pv.scene(), v.elevation, v.azimuth, v.zoom, PART_W as f32 / PART_H as f32);
            pv.set_view(cam, spectral);
            if pv.samples() < v.shown_samples {
                v.shown_samples = 0;
            }
            if pv.samples() < PART_MAX_SPP {
                pv.add_samples(ctx, 2)?;
                // 読み戻しは、サンプル数が 2 の累乗のときと 16 ごと
                let n = pv.samples();
                if n.is_power_of_two() || n.is_multiple_of(16) || n >= PART_MAX_SPP {
                    let img = pv.image(0.0)?;
                    match &mut v.tex {
                        Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                        None => v.tex = Some(egui_ctx.load_texture("part_preview", img, egui::TextureOptions::LINEAR)),
                    }
                    v.shown_samples = n;
                }
            }
            Ok(())
        })();
        if let Err(e) = r {
            if v.error != e {
                krbr_core::warn!("part preview {}: {e}", p.reference);
            }
            v.error = e;
            v.preview = None;
            v.epoch = epoch; // 同じシーンでは作り直さない
        } else {
            v.error.clear();
        }
    }
}

// ---------------------------------------------------------------- 小物

/// 2 つのシーンの形状（メッシュの大きさと配置）が同じか
fn same_geometry(a: &Scene, b: &Scene) -> bool {
    a.meshes.len() == b.meshes.len()
        && a.instances.len() == b.instances.len()
        && a.meshes.iter().zip(&b.meshes).all(|(x, y)| x.mesh.positions.len() == y.mesh.positions.len() && x.mesh.indices.len() == y.mesh.indices.len())
        && a.instances.iter().zip(&b.instances).all(|(x, y)| x.mesh == y.mesh && x.xform == y.xform && x.material_base == y.material_base)
}

fn srgb8(linear: Vec3) -> [u8; 3] {
    let s = linear_to_srgb(linear.clamp(Vec3::ZERO, Vec3::ONE));
    [(s.x * 255.0).round() as u8, (s.y * 255.0).round() as u8, (s.z * 255.0).round() as u8]
}

/// リニアの色を、画面の色（sRGB）で選ぶ
fn color_edit(ui: &mut egui::Ui, linear: &mut Vec3) {
    let mut c = srgb8(*linear);
    if ui.color_edit_button_srgb(&mut c).changed() {
        *linear = srgb_to_linear(Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32) / 255.0);
    }
}

/// 現実の材質の拡散の反射率はおよそ 0.02〜0.9（雪・新しい白い塗料でも 0.9 前後、木炭で 0.04）
fn albedo_warning(ui: &mut egui::Ui, c: Vec3) {
    if c.max_element() > 0.9 || c.max_element() < 0.015 {
        ui.colored_label(egui::Color32::from_rgb(255, 204, 77), "⚠").on_hover_text(fl!("mated-albedo-warning"));
    }
}

fn roughness_row(ui: &mut egui::Ui, r: &mut f32) {
    ui.label(fl!("mated-roughness"));
    ui.add(egui::Slider::new(r, 0.0..=1.0).step_by(0.01)).on_hover_text(fl!("mated-roughness-param-tip"));
    ui.end_row();
}

/// 屈折率（nd）。分散式のある材質は、変えると分散式を外してアッベ数で表す
fn ior_row(ui: &mut egui::Ui, m: &mut Material) {
    ui.label(fl!("mated-ior"));
    ui.horizontal(|ui| {
        let mut nd = m.ior_at(587.6);
        if ui.add(egui::DragValue::new(&mut nd).range(1.0..=3.0).speed(0.002).max_decimals(3)).on_hover_text(fl!("mated-ior-tip")).changed() {
            if m.sellmeier.is_some() {
                // 分散は、分散式から求めたアッベ数で残す
                let (nf, nc, d) = (m.ior_at(486.1), m.ior_at(656.3), m.ior_at(587.6));
                m.sellmeier = None;
                if nf > nc {
                    m.transmission_dispersion_abbe_number = (d - 1.0) / (nf - nc);
                    m.transmission_dispersion_scale = 1.0;
                }
            }
            m.specular_ior = nd;
        }
        if m.sellmeier.is_some() {
            ui.weak(fl!("mated-sellmeier"));
        }
    });
    ui.end_row();
}

/// 一覧に無い名前（"名前"、"名前 2"、…）
fn unique_name(lib: &MaterialLibrary, want: &str) -> String {
    let base: String = want.chars().map(|c| if "+.\"\\[]#\n".contains(c) { ' ' } else { c }).collect::<String>().trim().to_string();
    let base = if base.is_empty() { "material".to_string() } else { base };
    if lib.find(&base).is_none() {
        return base;
    }
    (2..).map(|i| format!("{base} {i}")).find(|n| lib.find(n).is_none()).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_materials_have_names_and_categories_in_english() {
        let lib = MaterialLibrary::builtin();
        for name in builtin_names() {
            assert!(i18n::english_has(&format!("matname-{name}")), "matname-{name} is missing in en/krbr-app.ftl");
        }
        for c in Category::ALL {
            assert!(i18n::english_has(&format!("matcat-{}", c.as_str())), "matcat-{}", c.as_str());
        }
        for c in conductor::all() {
            assert!(i18n::english_has(&format!("conductor-{}", c.id)), "conductor-{}", c.id);
        }
        // 一覧の順：金属が先頭で、全部の材質がどこかに入る
        let g = grouped_names(&lib);
        assert_eq!(g[0].0, Category::Metal);
        assert_eq!(g[0].1[0], "metal_gold");
        assert_eq!(g.iter().map(|(_, v)| v.len()).sum::<usize>(), lib.names().len());
    }

    #[test]
    fn unique_names() {
        let lib = MaterialLibrary::builtin();
        assert_eq!(unique_name(&lib, "my+red"), "my red");
        assert_eq!(unique_name(&lib, "plastic"), "plastic 2");
        assert!(valid_material_name(&unique_name(&lib, "a.b")));
    }
}
