//! 材質エディタ（ビューアの中の別の窓）：分類ごとの材質の見本の一覧、選んだ部品だけのプレビュー、部位ごとの色・粗さの
//! 調整（材質名の +color= / +rough=）、自分の材質（全体設定の materials/user.toml）の作成と編集。
//! 使い方は 3DCG ソフトの材質の扱いにならう：見本を部位・プレビューへドラッグして割り当てる、パラメータは節ごとの
//! パネル（物質・ベース・反射・透過・半透明・コート・表面の模様）、項目を右クリックで元の値、Ctrl+Z / Ctrl+Shift+Z で
//! 元に戻す・やり直す。組み込みの材質は書き換えず、値を変えると自分の材質のコピーを作ってそれを編集する（KeyShot の
//! ライブラリの材質と同じ扱い）。
//! 設定ファイルへの書き込みはビューアが行う（ここは Action を返すだけ）。見本とプレビューの描画は render（GPU が待機中に呼ぶ）

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec3;
use krbr_render::Context;
use krbr_scene::material_library::{builtin_names, builtin_order, new_user_material_id, valid_material_label, Variant};
use krbr_scene::{conductor, linear_to_srgb, srgb_to_linear, Category, Material, MaterialLibrary, Scene};

use crate::fl;
use crate::i18n;
use crate::preview::{part_camera, part_scene, swatch_camera, swatch_scene, Preview, PreviewLight, SWATCH_PX};
use crate::swatch_cache;

mod web;

/// 編集が止まってから書き込むまでの時間
const EDIT_DELAY: Duration = Duration::from_millis(400);
/// 部品のプレビューの大きさ（画素）と、積む最大のサンプル数
const PART_W: u32 = 400;
const PART_H: u32 = 300;
const PART_MAX_SPP: u32 = 256;
/// 見本 1 枚に積むサンプル数（その場で描くとき。ノイズ除去してキャッシュに書く）
const SWATCH_SPP: u32 = 64;
/// 元の値から変えた項目の名前の色
const MODIFIED: egui::Color32 = egui::Color32::from_rgb(255, 190, 90);
/// 元に戻す履歴の長さ
const UNDO_LIMIT: usize = 100;

// ---------------------------------------------------------------- 表示名

/// 材質の表示名（[krbr] label があればそれ、組み込みの材質は訳、それ以外は ID）。変種の指定は含めない
pub fn material_label(lib: &MaterialLibrary, name: &str) -> String {
    let (base, _) = Variant::parse(name);
    match lib.label(base) {
        Some(l) => l.to_string(),
        None => i18n::get(&format!("matname-{base}")).unwrap_or_else(|| base.to_string()),
    }
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

/// 電子部品であまり使わない組み込みの材質（一覧では分類の中の「その他」に畳む）
const RARE: &[&str] = &[
    "metal_red_brass",
    "metal_platinum",
    "metal_palladium",
    "metal_rhodium",
    "metal_iron",
    "metal_cobalt",
    "metal_titanium",
    "metal_tungsten",
    "metal_zinc",
    "plastic_pet",
    "glass_bk7",
    "glass_fused_silica",
    "glass_sapphire",
    "pet_clear",
];

pub fn is_user_material(name: &str) -> bool {
    !builtin_names().contains(name)
}

/// 材質の説明（出典・屈折率）
fn material_details(m: &Material) -> String {
    if let Some(c) = m.conductor.and_then(conductor::get) {
        return fl!("mated-measured", source = c.source);
    }
    if m.base_metalness > 0.5 {
        return fl!("mated-metal-f0-desc");
    }
    let mut s = fl!("mated-ior-value", ior = format!("{:.3}", m.ior_at(587.6)));
    if m.sellmeier.is_some() {
        s += &fl!("mated-sellmeier");
    }
    s
}

/// 一覧の行に出す短い説明（測定値か屈折率と、粗さ）
fn material_brief(m: &Material) -> String {
    let rough = fl!("mated-brief-rough", value = format!("{:.2}", m.specular_roughness));
    if m.conductor.is_some() {
        format!("{} · {rough}", fl!("mated-brief-measured"))
    } else if m.base_metalness > 0.5 {
        format!("F0 · {rough}")
    } else {
        format!("nd {:.3} · {rough}", m.ior_at(587.6))
    }
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
    /// 部品 reference（3D モデルの番号 model）の部位に材質を書き込む。止まってから書く調整は、その間に選んだ部品が
    /// 変わっていても、調整した部品に書く
    AssignTo { reference: String, model: Option<usize>, part: String, material: String },
    /// 自分の材質を書く（name は ID。同じ ID があれば置き換え）。label は表示名
    SaveUser { name: String, material: Box<Material>, label: String },
    DeleteUser(String),
}

// ---------------------------------------------------------------- 状態

/// 部位の色・粗さの調整（止まってから書き込む）
struct Adjust {
    reference: String,
    /// 調整した部品の 3D モデルの番号（書き込む先を「このモデル」にしたときに使う）
    model: Option<usize>,
    part: String,
    material: String,
    changed_at: Instant,
    written: bool,
}

/// 自分の材質の編集中の値（止まってから書き込む）
struct UserEdit {
    /// 材質の ID
    name: String,
    /// 表示名（名前を変えたら、材質と一緒に書き込む）
    label: String,
    material: Material,
    /// 編集を始めたときの値（項目を右クリックして戻す先。組み込みの材質から作ったコピーなら、その組み込みの材質）
    origin: Material,
    changed_at: Option<Instant>,
    /// 元に戻す・やり直す履歴（一続きの編集で 1 つ）
    undo: Vec<Material>,
    redo: Vec<Material>,
    /// 今の一続きの編集を、もう元に戻す履歴に入れた
    in_burst: bool,
}

impl UserEdit {
    fn new(name: &str, label: &str, material: Material, origin: Material) -> Self {
        Self { name: name.to_string(), label: label.to_string(), material, origin, changed_at: None, undo: Vec::new(), redo: Vec::new(), in_burst: false }
    }

    /// 値が変わった（before は変わる前の値）
    fn changed(&mut self, before: Material) {
        if !self.in_burst {
            self.undo.push(before);
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
            self.redo.clear();
            self.in_burst = true;
        }
        self.changed_at = Some(Instant::now());
    }

    fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.material, prev));
        self.in_burst = false;
        self.changed_at = Some(Instant::now());
        true
    }

    fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.material, next));
        self.in_burst = false;
        self.changed_at = Some(Instant::now());
        true
    }
}

/// ドラッグ中の材質（一覧の見本を、部位の行や部品のプレビューへ落として割り当てる）
struct MaterialDrag(String);

/// 新しい自分の材質の名前（表示名）を決めているところ。rename なら、その自分の材質（ID）の名前を変える
struct Naming {
    name: String,
    material: Material,
    /// 作ったら割り当てる部位（部位の調整から作ったとき）と、残す変種（模様なし・発光）
    assign_to: Option<(String, Variant)>,
    rename: Option<String>,
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
    /// --web-ui：ページから読むプレビューの画像の URL
    url: Option<String>,
    shown_samples: u32,
    azimuth: f64,
    elevation: f64,
    zoom: f32,
    error: String,
}

#[derive(Default)]
pub struct MaterialEditor {
    pub open: bool,
    /// egui の画面：3D 表示の領域（窓を初めに置く所。ボタン列に重ねない）
    pub area: Option<egui::Rect>,
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
    /// 知らせ（組み込みの材質のコピーを作った、など）
    notice: String,
    /// 見本を描いたときの色の計算（分光か RGB か。変わったら描き直す）
    thumbs_spectral: Option<bool>,
    /// 材質を自動で選んだ部品（部品を選ぶと、その部位の材質を下に出す）
    browsed_for: Option<String>,
    /// 左を一覧（小さな見本・名前・屈折率と粗さ）で見せる
    list_view: bool,
    /// --web-ui：見本とプレビューの画像（PNG）を置く場所、見本の URL、URL に付ける通し番号
    web: Option<crate::web_ui::Images>,
    thumb_urls: HashMap<String, String>,
    img_seq: u64,
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

    /// 選んだ部品が変わった・シーンを組み直した（調整の書き込みが反映された）ときの片付け。
    /// 書き込む前に別の部品を選んだら、調整はその場で前の部品に書く（以前は書かずに捨てていた）
    fn sync_selection(&mut self, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        let reference = part.map(|p| p.reference.as_str());
        if let Some(a) = &self.adjust {
            let reflected = part.is_some_and(|p| p.rows.iter().any(|r| r.key == a.part && r.material == a.material));
            if Some(a.reference.as_str()) != reference || (a.written && reflected) {
                if !a.written {
                    actions.push(Action::AssignTo { reference: a.reference.clone(), model: a.model, part: a.part.clone(), material: a.material.clone() });
                }
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
            // 閉じた：止まるのを待っていた編集を書く（閉じたまま捨てない）
            self.flush(part, &mut actions, true);
            return actions;
        }
        self.sync_selection(part, &mut actions);
        self.shortcuts(ctx);
        let mut open = self.open;
        egui::Window::new(fl!("mated-title")).id(egui::Id::new("material_editor")).open(&mut open).default_pos(self.area.map_or(egui::pos2(40.0, 60.0), |a| a.min + egui::vec2(20.0, 20.0))).default_size(self.area.map_or(egui::vec2(900.0, 780.0), |a| egui::vec2((a.width() - 40.0).clamp(640.0, 900.0), (a.height() - 40.0).clamp(300.0, 780.0)))).min_width(640.0).resizable(true).collapsible(true).show(ctx, |ui| {
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
        self.drag_ghost(ctx, lib);
        self.flush(part, &mut actions, false);
        actions
    }

    /// Ctrl+Z / Ctrl+Shift+Z（Ctrl+Y）：自分の材質の編集を元に戻す・やり直す（文字の入力中は入力欄のもの）
    fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let Some(u) = &mut self.user_edit else { return };
        let (redo, undo) = ctx.input_mut(|i| {
            let redo = i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)) || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Y));
            (redo, !redo && i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)))
        });
        if (redo && u.redo()) || (undo && u.undo()) {
            let name = u.name.clone();
            self.thumbs.retain(|k, _| Variant::parse(k).0 != name);
        }
    }

    /// ドラッグ中の材質を、ポインタの横に見本と名前で描く
    fn drag_ghost(&self, ctx: &egui::Context, lib: &MaterialLibrary) {
        let (Some(drag), Some(pos)) = (egui::DragAndDrop::payload::<MaterialDrag>(ctx), ctx.pointer_interact_pos()) else { return };
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("mated_drag")));
        let rect = egui::Rect::from_min_size(pos + egui::vec2(14.0, 6.0), egui::vec2(40.0, 40.0));
        match self.thumbs.get(&drag.0) {
            Some(t) => {
                painter.image(t.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            None => {
                painter.rect_filled(rect, 4.0, egui::Color32::from_gray(60));
            }
        }
        let galley = painter.layout_no_wrap(material_label(lib, &drag.0), egui::FontId::proportional(13.0), egui::Color32::WHITE);
        let at = rect.right_center() + egui::vec2(6.0, -galley.size().y * 0.5);
        painter.rect_filled(egui::Rect::from_min_size(at, galley.size()).expand(3.0), 3.0, egui::Color32::from_black_alpha(200));
        painter.galley(at, galley, egui::Color32::WHITE);
    }

    /// 材質ライブラリを読み直した：値の変わった材質の見本を描き直す（見本は材質名で覚えているので、
    /// 手で直した materials/*.toml を読み直しても、古い絵のままになっていた）
    pub fn library_reloaded(&mut self, old: &MaterialLibrary, new: &MaterialLibrary) {
        let same = |k: &str| {
            let base = Variant::parse(k).0;
            old.find(base) == new.find(base) && old.label(base) == new.label(base)
        };
        self.thumbs.retain(|k, _| same(k));
        self.thumb_urls.retain(|k, _| same(k));
    }

    /// 書き込むのを待っている編集を、今すぐ書き込む（ビューアを終えるとき）
    pub fn flush_now(&mut self, part: Option<&PartInfo>) -> Vec<Action> {
        let mut actions = Vec::new();
        self.flush(part, &mut actions, true);
        actions
    }

    /// 止まった編集を書き込む（now なら待たずに）。調整は、調整した部品に書く（今選んでいる部品でなくても）
    fn flush(&mut self, _part: Option<&PartInfo>, actions: &mut Vec<Action>, now: bool) {
        if let Some(a) = &mut self.adjust {
            if !a.written && (now || a.changed_at.elapsed() >= EDIT_DELAY) {
                actions.push(Action::AssignTo { reference: a.reference.clone(), model: a.model, part: a.part.clone(), material: a.material.clone() });
                a.written = true;
            }
        }
        if self.user_edit.as_ref().is_some_and(|u| u.changed_at.is_some_and(|t| now || t.elapsed() >= EDIT_DELAY)) {
            self.flush_user(actions);
        }
    }

    /// 見本の画像（無ければ灰色の四角を描いて、描くように頼む）
    fn swatch(&mut self, ui: &mut egui::Ui, key: &str, size: f32, sense: egui::Sense) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), sense);
        self.paint_swatch(ui, key, rect);
        resp
    }

    /// 見本を rect に描く（無ければ灰色の四角を描いて、描くように頼む）
    fn paint_swatch(&mut self, ui: &egui::Ui, key: &str, rect: egui::Rect) {
        if !ui.is_rect_visible(rect) {
            return;
        }
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

    /// 左：検索・見本と一覧の切り替えと、分類ごとの材質。電子部品であまり使わない材質は、分類の中の「その他」に畳む
    /// （検索したときは全部出す）
    fn browser_ui(&mut self, ui: &mut egui::Ui, lib: &MaterialLibrary, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(fl!("mated-search"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.selectable_value(&mut self.list_view, true, fl!("mated-view-list")).on_hover_text(fl!("mated-view-list-tip"));
                ui.selectable_value(&mut self.list_view, false, fl!("mated-view-grid"));
                ui.add(egui::TextEdit::singleline(&mut self.search).desired_width(ui.available_width()));
            });
        });
        let query = self.search.trim().to_lowercase();
        // 割り当て先の部位の今の材質（一覧で印を付ける）
        let current: HashSet<String> = part.map(|p| p.rows.iter().filter(|r| self.target.as_ref().is_none_or(|t| *t == r.key)).map(|r| Variant::parse(&self.row_material(&p.reference, r)).0.to_string()).collect()).unwrap_or_default();
        egui::ScrollArea::vertical().id_salt("mated_left").auto_shrink([false, false]).show(ui, |ui| {
            for (cat, names) in grouped_names(lib) {
                let shown: Vec<&String> = names.iter().filter(|n| query.is_empty() || n.to_lowercase().contains(&query) || material_label(lib, n).to_lowercase().contains(&query)).collect();
                if shown.is_empty() {
                    continue;
                }
                let (more, main): (Vec<&String>, Vec<&String>) = shown.iter().partition(|n| query.is_empty() && RARE.contains(&n.as_str()));
                egui::CollapsingHeader::new(format!("{} ({})", category_label(cat), shown.len())).id_salt(cat.as_str()).default_open(true).show(ui, |ui| {
                    self.items(ui, lib, &main, &current, part, actions);
                    if !more.is_empty() {
                        // 今の材質・選んでいる材質が畳んだ中にあれば、開いておく
                        let open = more.iter().any(|n| current.contains(n.as_str()) || self.browsing.as_deref() == Some(n.as_str()));
                        egui::CollapsingHeader::new(egui::RichText::new(fl!("mated-more", n = more.len())).weak()).id_salt(format!("{}_more", cat.as_str())).default_open(open).show(ui, |ui| {
                            self.items(ui, lib, &more, &current, part, actions);
                        });
                    }
                });
            }
        });
    }

    /// 材質の並び（見本のタイルか、一覧の行）
    fn items(&mut self, ui: &mut egui::Ui, lib: &MaterialLibrary, names: &[&String], current: &HashSet<String>, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        if self.list_view {
            ui.spacing_mut().item_spacing.y = 2.0;
            for name in names {
                self.list_row(ui, lib, name, current.contains(name.as_str()), part, actions);
            }
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                for name in names {
                    self.tile(ui, lib, name, current.contains(name.as_str()), part, actions);
                }
            });
        }
    }

    /// 一覧の 1 つ：見本と名前。クリックで選び、ダブルクリックで割り当てる
    fn tile(&mut self, ui: &mut egui::Ui, lib: &MaterialLibrary, name: &str, current: bool, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        const W: f32 = 84.0;
        let selected = self.browsing.as_deref() == Some(name);
        let user = is_user_material(name);
        let resp = ui
            .allocate_ui_with_layout(egui::vec2(W, W + 30.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.set_width(W);
                let r = self.swatch(ui, name, W - 8.0, egui::Sense::click_and_drag());
                let mut text = egui::RichText::new(material_label(lib, name)).small();
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
        self.item_interact(resp, lib, name, part, actions);
    }

    /// 一覧の行：小さな見本・名前と、屈折率・粗さ（見た目の差が小さい材質を、値で見比べる）
    fn list_row(&mut self, ui: &mut egui::Ui, lib: &MaterialLibrary, name: &str, current: bool, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        const H: f32 = 30.0;
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), H), egui::Sense::click_and_drag());
        if ui.is_rect_visible(rect) {
            let v = ui.visuals();
            if self.browsing.as_deref() == Some(name) {
                ui.painter().rect_filled(rect, 4.0, v.selection.bg_fill);
            } else if resp.hovered() {
                ui.painter().rect_filled(rect, 4.0, v.widgets.hovered.weak_bg_fill);
            }
            let sw = egui::Rect::from_min_size(rect.min + egui::vec2(2.0, 1.0), egui::vec2(H - 2.0, H - 2.0));
            self.paint_swatch(ui, name, sw);
            let mut label = material_label(lib, name);
            if is_user_material(name) {
                label = format!("★ {label}");
            }
            let color = if current { egui::Color32::from_rgb(255, 170, 40) } else { ui.visuals().text_color() };
            let font = egui::TextStyle::Body.resolve(ui.style());
            ui.painter().text(egui::pos2(sw.max.x + 8.0, rect.center().y), egui::Align2::LEFT_CENTER, label, font, color);
            if let Some(m) = self.material_for(lib, name) {
                ui.painter().text(egui::pos2(rect.max.x - 6.0, rect.center().y), egui::Align2::RIGHT_CENTER, material_brief(&m), egui::TextStyle::Body.resolve(ui.style()), ui.visuals().weak_text_color());
            }
        }
        self.item_interact(resp, lib, name, part, actions);
    }

    /// 見本のタイル・一覧の行の操作：説明、ドラッグ＆ドロップ、クリックで選ぶ、ダブルクリックで割り当て、右クリックでメニュー
    fn item_interact(&mut self, resp: egui::Response, lib: &MaterialLibrary, name: &str, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        let user = is_user_material(name);
        let mut tip = format!("{}\n{}", material_label(lib, name), name);
        if let Some(m) = lib.find(name) {
            tip += &format!("\n{}", material_details(m));
        }
        if user {
            tip += &format!("\n{}", fl!("mated-user-material"));
        }
        tip += &format!("\n{}", fl!("mated-tile-tip"));
        let resp = resp.on_hover_text(tip);
        resp.dnd_set_drag_payload(MaterialDrag(name.to_string()));
        if resp.clicked() || resp.drag_started() {
            self.select_browsing(lib, name, actions);
        }
        let can_assign = part.is_some_and(|p| !p.rows.is_empty());
        if resp.double_clicked() && can_assign {
            self.assign(name, part, self.target.clone(), actions);
        }
        resp.context_menu(|ui| {
            self.select_browsing(lib, name, actions);
            if ui.add_enabled(can_assign, egui::Button::new(self.assign_label(part))).clicked() {
                self.assign(name, part, self.target.clone(), actions);
                ui.close();
            }
            if ui.button(fl!("mated-duplicate")).clicked() {
                self.start_duplicate(lib, name);
                ui.close();
            }
            if user && ui.button(fl!("mated-rename")).clicked() {
                self.start_rename(lib, name);
                ui.close();
            }
            if user && ui.button(fl!("mated-delete")).clicked() {
                self.confirm_delete = true;
                ui.close();
            }
        });
    }

    /// 割り当てのボタンの名前（全部位か、選んだ部位か）
    fn assign_label(&self, part: Option<&PartInfo>) -> String {
        if self.target.is_none() && part.is_some_and(|p| p.rows.len() > 1) {
            fl!("mated-assign-all")
        } else {
            fl!("mated-assign")
        }
    }

    /// 材質を複製して、名前を決める（作ると自分の材質になる）
    fn start_duplicate(&mut self, lib: &MaterialLibrary, name: &str) {
        let Some(m) = self.material_for(lib, name) else { return };
        let mut d = m.clone();
        d.category = Some(m.category());
        self.naming = Some(Naming { name: unique_label(lib, &fl!("mated-copy-of", name = material_label(lib, name))), material: d, assign_to: None, rename: None });
    }

    /// 自分の材質の名前（表示名）を変える（新しい名前を決めてもらう）。ID は変わらないので、割り当てはそのまま
    fn start_rename(&mut self, lib: &MaterialLibrary, name: &str) {
        let Some(m) = self.material_for(lib, name) else { return };
        let label = self.user_edit.as_ref().filter(|u| u.name == name).map(|u| u.label.clone()).unwrap_or_else(|| material_label(lib, name));
        self.naming = Some(Naming { name: label, material: m, assign_to: None, rename: Some(name.to_string()) });
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
        self.notice.clear();
        self.user_edit = if is_user_material(name) { lib.find(name).map(|m| UserEdit::new(name, &material_label(lib, name), m.clone(), m.clone())) } else { None };
    }

    /// 自分の材質の編集を、待たずに書き込む
    fn flush_user(&mut self, actions: &mut Vec<Action>) {
        if let Some(u) = &mut self.user_edit {
            u.in_burst = false;
            if u.changed_at.take().is_some() {
                actions.push(Action::SaveUser { name: u.name.clone(), material: Box::new(u.material.clone()), label: u.label.clone() });
            }
        }
    }

    /// 材質を、部位 target（None なら全部位）に書き込む。別の材質にするときは、色・粗さの調整は前の材質に合わせたものなので外し、
    /// 模様のあり・なしと発光は保つ（同じ材質を割り当て直したときは、調整もそのまま）
    fn assign(&mut self, name: &str, part: Option<&PartInfo>, target: Option<String>, actions: &mut Vec<Action>) {
        let Some(p) = part else { return };
        self.flush(part, actions, true);
        self.adjust = None;
        let rows: Vec<&PartRow> = p.rows.iter().filter(|r| target.as_ref().is_none_or(|t| *t == r.key)).collect();
        if target.is_none() && rows.len() > 1 {
            actions.push(Action::Assign { part: String::new(), material: name.to_string() });
            return;
        }
        for r in rows {
            let (base, v) = Variant::parse(&r.material);
            let keep = if base == name { v } else { Variant { smooth: v.smooth, emission: v.emission, ..Default::default() } };
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
        // 見本をプレビューへ落とすと、割り当て先の部位（全部位）に割り当てる
        if drop_target(ui, &resp, rect) {
            if let Some(d) = resp.dnd_release_payload::<MaterialDrag>() {
                self.assign(&d.0, part, self.target.clone(), actions);
            }
        }
        scope(ui);
        // 割り当て先の部位
        ui.label(fl!("mated-target"));
        let multi = p.rows.len() > 1;
        if multi {
            let resp = ui.radio(self.target.is_none(), fl!("part-whole"));
            if resp.clicked() {
                self.target = None;
            }
            if drop_target(ui, &resp, resp.rect) {
                if let Some(d) = resp.dnd_release_payload::<MaterialDrag>() {
                    self.assign(&d.0, part, None, actions);
                }
            }
        }
        let rows: Vec<(PartRow, String)> = p.rows.iter().map(|r| (r.clone(), self.row_material(&p.reference, r))).collect();
        for (r, material) in &rows {
            let row = ui.horizontal(|ui| {
                let on = self.target.as_ref() == Some(&r.key) || !multi;
                if ui.radio(on, "").clicked() {
                    self.target = Some(r.key.clone());
                }
                let s = self.swatch(ui, material, 22.0, egui::Sense::click());
                let shown = if r.key.is_empty() { fl!("part-whole") } else { r.key.strip_prefix("kicad:").unwrap_or(&r.key).to_string() };
                let resp = ui.selectable_label(on, format!("{shown}: {}", material_label(lib, material))).on_hover_text(material.as_str());
                if resp.clicked() || s.clicked() {
                    self.target = Some(r.key.clone());
                    self.select_browsing(lib, Variant::parse(material).0, actions);
                }
            });
            let resp = row.response.interact(egui::Sense::hover());
            if drop_target(ui, &resp, resp.rect) {
                if let Some(d) = resp.dnd_release_payload::<MaterialDrag>() {
                    self.target = Some(r.key.clone());
                    self.assign(&d.0, part, Some(r.key.clone()), actions);
                }
            }
        }
        if !multi && self.target.is_none() {
            self.target = Some(p.rows[0].key.clone());
        }
        // 部品を選んだら、割り当て先の部位（全部位なら最初の部位）の材質を下に出す（3DCG ソフトのアクティブな材質と同じ）
        if self.browsed_for.as_deref() != Some(p.reference.as_str()) {
            self.browsed_for = Some(p.reference.clone());
            let row = rows.iter().find(|(r, _)| self.target.as_ref() == Some(&r.key)).unwrap_or(&rows[0]);
            if self.naming.is_none() {
                self.select_browsing(lib, Variant::parse(&row.1).0, actions);
            }
        }
        // 1 つの部位の調整（色・粗さ）
        let Some(t) = self.target.clone() else { return };
        let Some((row, material)) = rows.iter().find(|(r, _)| r.key == t).cloned() else { return };
        ui.add_space(4.0);
        ui.strong(fl!("mated-adjust"));
        ui.weak(fl!("mated-adjust-note"));
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
                // 初めの色：材質の今の色
                v.color = on.then(|| srgb8(web::adjust_start_color(&base_m)));
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
            self.adjust_to(part, &p.reference, &row.key, v.name(base), actions);
        }
        // 調整したものを自分の材質にする
        if (v.color.is_some() || v.roughness.is_some()) && ui.button(fl!("mated-save-adjusted")).on_hover_text(fl!("mated-save-adjusted-tip")).clicked() {
            self.start_save_adjusted(lib, &p.reference, &row.key, base, v, &base_m);
        }
    }

    /// 部位の調整（色・粗さを変えた材質名）を、止まってから書き込むように覚える
    fn adjust_to(&mut self, part: Option<&PartInfo>, reference: &str, key: &str, name: String, actions: &mut Vec<Action>) {
        match &mut self.adjust {
            Some(a) if a.reference == reference && a.part == key => {
                a.model = part.filter(|p| p.reference == reference).and_then(|p| p.model).or(a.model);
                a.material = name;
                a.changed_at = Instant::now();
                a.written = false;
            }
            _ => {
                self.flush(part, actions, true);
                let model = part.filter(|p| p.reference == reference).and_then(|p| p.model);
                self.adjust = Some(Adjust { reference: reference.to_string(), model, part: key.to_string(), material: name, changed_at: Instant::now(), written: false });
            }
        }
    }

    /// 一覧で選んだ材質が、割り当て先の部位の材質で、その部位に色・粗さの調整があれば（部位のキー, 材質, 変種）。
    /// パラメータの欄の値はその部位では使われないので、そのことを知らせる
    fn target_override(&self, part: Option<&PartInfo>) -> Option<(String, String, Variant)> {
        let (p, t, browsing) = (part?, self.target.as_ref()?, self.browsing.as_ref()?);
        let row = p.rows.iter().find(|r| &r.key == t)?;
        let material = self.row_material(&p.reference, row);
        let (base, v) = Variant::parse(&material);
        (base == browsing && (v.color.is_some() || v.roughness.is_some())).then(|| (row.key.clone(), base.to_string(), v))
    }

    /// パラメータの欄の上に出す、部位の調整の知らせ（調整が無ければ None）
    fn override_text(&self, _lib: &MaterialLibrary, part: Option<&PartInfo>) -> Option<String> {
        let (_, _, v) = self.target_override(part)?;
        let mut what = Vec::new();
        if let Some([r, g, b]) = v.color {
            what.push(format!("{} #{r:02x}{g:02x}{b:02x}", fl!("mated-color")));
        }
        if let Some(r) = v.roughness {
            what.push(format!("{} {r:.2}", fl!("mated-roughness")));
        }
        Some(fl!("mated-override", what = what.join("・")))
    }

    /// 部位の材質を書き込み、組み直しが終わるまで表示にその名前を使う
    fn write_row(&mut self, reference: &str, key: &str, material: String, actions: &mut Vec<Action>) {
        actions.push(Action::Assign { part: key.to_string(), material: material.clone() });
        self.adjust = Some(Adjust { reference: reference.to_string(), model: None, part: key.to_string(), material, changed_at: Instant::now(), written: true });
    }

    /// 部位の色・粗さの調整を材質の値にして、部位の調整を外す（組み込みの材質ならコピーを作って、それにする）
    fn adopt_override(&mut self, lib: &MaterialLibrary, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        let Some((key, base, v)) = self.target_override(part) else { return };
        let Some(p) = part else { return };
        self.flush(part, actions, true);
        let adj = Variant { color: v.color, roughness: v.roughness, ..Default::default() };
        self.edit_material(lib, &base, part, actions, |m, _| adj.apply(m));
        // 自分の材質の値を先に書く（割り当てより前に）
        self.flush_user(actions);
        let material = self.browsing.clone().unwrap_or(base);
        let keep = Variant { smooth: v.smooth, emission: v.emission, ..Default::default() };
        self.write_row(&p.reference, &key, keep.name(&material), actions);
    }

    /// 部位の色・粗さの調整を外して、材質の値に戻す
    fn clear_override(&mut self, _lib: &MaterialLibrary, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        let Some((key, base, v)) = self.target_override(part) else { return };
        let Some(p) = part else { return };
        self.flush(part, actions, true);
        let keep = Variant { color: None, roughness: None, ..v };
        self.write_row(&p.reference, &key, keep.name(&base), actions);
    }

    /// 調整した部位の材質を、自分の材質にする（名前を決めてもらう）
    fn start_save_adjusted(&mut self, lib: &MaterialLibrary, reference: &str, key: &str, base: &str, v: Variant, base_m: &Material) {
        if let Some(mut m) = self.material_for(lib, &Variant { emission: None, smooth: false, ..v }.name(base)) {
            m.category = Some(base_m.category());
            let keep = Variant { smooth: v.smooth, emission: v.emission, ..Default::default() };
            self.naming = Some(Naming { name: unique_label(lib, &format!("{} {}", material_label(lib, base), reference)), material: m, assign_to: Some((key.to_string(), keep)), rename: None });
            self.browsing = None;
        }
    }

    /// 右下：一覧で選んだ材質の説明・割り当て・複製と、パラメータの編集
    fn details_ui(&mut self, ui: &mut egui::Ui, lib: &MaterialLibrary, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        if self.naming.is_some() {
            self.naming_ui(ui, lib, part, actions);
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
        let user = is_user_material(&name);
        ui.horizontal(|ui| {
            self.swatch(ui, &name, 96.0, egui::Sense::hover());
            ui.vertical(|ui| {
                ui.heading(material_label(lib, &name));
                ui.weak(format!("{name}  ·  {}", category_label(m.category())));
                ui.label(material_details(&m));
                if user {
                    ui.weak(fl!("mated-user-material"));
                }
            });
        });
        ui.horizontal(|ui| {
            let can_assign = part.is_some_and(|p| !p.rows.is_empty());
            if ui.add_enabled(can_assign, egui::Button::new(self.assign_label(part))).on_hover_text(fl!("mated-assign-tip")).clicked() {
                self.assign(&name, part, self.target.clone(), actions);
            }
            if ui.button(fl!("mated-duplicate")).on_hover_text(fl!("mated-duplicate-tip")).clicked() {
                self.start_duplicate(lib, &name);
            }
            if user && ui.button(fl!("mated-rename")).on_hover_text(fl!("mated-rename-tip")).clicked() {
                self.start_rename(lib, &name);
            }
            if user && !self.confirm_delete && ui.button(fl!("mated-delete")).clicked() {
                self.confirm_delete = true;
            }
        });
        if user && self.confirm_delete {
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::from_rgb(255, 204, 77), fl!("mated-delete-confirm"));
                if ui.button(fl!("mated-delete")).clicked() {
                    self.delete_user(&name, actions);
                }
                if ui.button(fl!("mated-cancel")).clicked() {
                    self.confirm_delete = false;
                }
            });
            if self.browsing.is_none() {
                return;
            }
        }
        if !self.notice.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(120, 200, 255), &self.notice);
        }
        if !self.error.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(255, 102, 102), &self.error);
        }
        ui.separator();
        if let Some(text) = self.override_text(lib, part) {
            ui.colored_label(egui::Color32::from_rgb(255, 204, 77), text);
            ui.horizontal(|ui| {
                if ui.button(fl!("mated-override-adopt")).on_hover_text(fl!("mated-override-adopt-tip")).clicked() {
                    self.adopt_override(lib, part, actions);
                }
                if ui.button(fl!("mated-override-clear")).on_hover_text(fl!("mated-override-clear-tip")).clicked() {
                    self.clear_override(lib, part, actions);
                }
            });
            if self.browsing.as_deref() != Some(name.as_str()) {
                return;
            }
        }
        if user {
            self.user_editor_ui(ui, lib, &name);
        } else {
            self.builtin_editor_ui(ui, lib, &name, &m, part, actions);
        }
    }

    /// 自分の材質を消す
    fn delete_user(&mut self, name: &str, actions: &mut Vec<Action>) {
        actions.push(Action::DeleteUser(name.to_string()));
        self.user_edit = None;
        self.browsing = None;
        self.confirm_delete = false;
        self.thumbs.retain(|k, _| Variant::parse(k).0 != name);
    }

    /// 新しい自分の材質の名前を決める
    fn naming_ui(&mut self, ui: &mut egui::Ui, lib: &MaterialLibrary, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        let Some(n) = &mut self.naming else { return };
        let renaming = n.rename.is_some();
        ui.strong(if renaming { fl!("mated-rename-title") } else { fl!("mated-new-title") });
        ui.horizontal(|ui| {
            ui.label(fl!("mated-new-name"));
            ui.text_edit_singleline(&mut n.name);
        });
        let valid = valid_material_label(n.name.trim());
        if !valid {
            ui.colored_label(egui::Color32::from_rgb(255, 102, 102), fl!("mated-name-invalid"));
        }
        let mut done = None;
        ui.horizontal(|ui| {
            if ui.add_enabled(valid, egui::Button::new(if renaming { fl!("mated-rename-ok") } else { fl!("mated-create") })).clicked() {
                done = Some(true);
            }
            if ui.button(fl!("mated-cancel")).clicked() {
                done = Some(false);
            }
        });
        match done {
            Some(true) => self.finish_naming(lib, part, actions),
            Some(false) => self.naming = None,
            None => {}
        }
    }

    /// 決めた名前（表示名）で自分の材質を作る（ID は新しく振る。部位の調整から作ったときは、その部位に割り当てる）。
    /// 名前を変えているときは、その材質の表示名を変える
    fn finish_naming(&mut self, lib: &MaterialLibrary, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        let Some(n) = self.naming.take() else { return };
        let label = n.name.trim().to_string();
        if let Some(id) = n.rename {
            if self.user_edit.as_ref().is_none_or(|u| u.name != id) {
                self.flush_user(actions);
                self.user_edit = Some(UserEdit::new(&id, &label, n.material.clone(), n.material));
            }
            if let Some(u) = &mut self.user_edit {
                u.label = label;
                u.changed_at = Some(Instant::now());
            }
            self.flush_user(actions);
            self.browsing = Some(id);
            return;
        }
        let name = new_user_material_id(lib);
        let mut m = n.material;
        m.name = name.clone();
        self.flush_user(actions);
        actions.push(Action::SaveUser { name: name.clone(), material: Box::new(m.clone()), label: label.clone() });
        if let (Some((part_key, keep)), Some(p)) = (n.assign_to, part) {
            actions.push(Action::Assign { part: part_key.clone(), material: keep.name(&name) });
            self.adjust = Some(Adjust { reference: p.reference.clone(), model: p.model, part: part_key, material: keep.name(&name), changed_at: Instant::now(), written: true });
        }
        self.browsing = Some(name.clone());
        self.notice.clear();
        self.user_edit = Some(UserEdit::new(&name, &label, m.clone(), m));
    }

    /// 組み込みの材質のパラメータ。組み込みの材質は書き換えず、値を変えると自分の材質のコピーを作ってそれを編集する。
    /// 割り当て先の部位（全部位）のうちこの材質のものは、コピーに付け替える
    fn builtin_editor_ui(&mut self, ui: &mut egui::Ui, lib: &MaterialLibrary, name: &str, m: &Material, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        ui.strong(fl!("mated-edit-title-builtin"));
        ui.weak(fl!("mated-builtin-autocopy"));
        let mut edited = m.clone();
        param_editor(ui, lib, &mut edited, m);
        if edited != *m {
            self.copy_builtin(lib, name, m, edited, part, actions);
        }
    }

    /// 組み込みの材質 name（値 m）の値を変えた（edited）：自分の材質のコピーを作ってそれを編集する
    fn copy_builtin(&mut self, lib: &MaterialLibrary, name: &str, m: &Material, mut edited: Material, part: Option<&PartInfo>, actions: &mut Vec<Action>) {
        let copy = new_user_material_id(lib);
        let label = unique_label(lib, &fl!("mated-copy-of", name = material_label(lib, name)));
        edited.name = copy.clone();
        edited.category = Some(edited.category());
        self.flush_user(actions);
        actions.push(Action::SaveUser { name: copy.clone(), material: Box::new(edited.clone()), label: label.clone() });
        if let Some(p) = part {
            self.flush(part, actions, true);
            self.adjust = None;
            for r in p.rows.iter().filter(|r| self.target.as_ref().is_none_or(|t| *t == r.key)) {
                let (base, v) = Variant::parse(&r.material);
                if base == name {
                    // 色の調整は保つ（粗さの調整は外す：編集した粗さが隠れないように）
                    let keep = Variant { roughness: None, ..v };
                    actions.push(Action::Assign { part: r.key.clone(), material: keep.name(&copy) });
                }
            }
        }
        // 元に戻すと組み込みの材質の値。動かしている途中の値は、止まってから書き込む
        let mut u = UserEdit::new(&copy, &label, edited, m.clone());
        u.changed(m.clone());
        self.user_edit = Some(u);
        self.browsing = Some(copy.clone());
        self.notice = fl!("mated-copied", from = material_label(lib, name), name = label);
    }

    /// 自分の材質のパラメータと、元に戻す・やり直す
    fn user_editor_ui(&mut self, ui: &mut egui::Ui, lib: &MaterialLibrary, name: &str) {
        let Some(u) = self.user_edit.as_mut().filter(|u| u.name == name) else { return };
        let (mut undo, mut redo) = (false, false);
        ui.horizontal(|ui| {
            ui.strong(fl!("mated-edit-title"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                redo = ui.add_enabled(!u.redo.is_empty(), egui::Button::new(fl!("mated-redo")).small()).on_hover_text("Ctrl+Shift+Z / Ctrl+Y").clicked();
                undo = ui.add_enabled(!u.undo.is_empty(), egui::Button::new(fl!("mated-undo")).small()).on_hover_text("Ctrl+Z").clicked();
            });
        });
        let before = u.material.clone();
        let changed = if undo {
            u.undo()
        } else if redo {
            u.redo()
        } else {
            param_editor(ui, lib, &mut u.material, &u.origin);
            let changed = u.material != before;
            if changed {
                u.changed(before);
            }
            changed
        };
        if changed {
            let name = u.name.clone();
            self.thumbs.retain(|k, _| Variant::parse(k).0 != name);
        }
    }

    /// 材質 name の値を変える（f は値と、元に戻す先を受け取る）。自分の材質ならその編集、組み込みの材質ならコピーを作ってそれを編集する
    fn edit_material(&mut self, lib: &MaterialLibrary, name: &str, part: Option<&PartInfo>, actions: &mut Vec<Action>, f: impl FnOnce(&mut Material, &Material)) {
        if is_user_material(name) {
            let Some(u) = self.user_edit.as_mut().filter(|u| u.name == name) else { return };
            let before = u.material.clone();
            let origin = u.origin.clone();
            f(&mut u.material, &origin);
            if u.material != before {
                u.changed(before);
                let name = u.name.clone();
                self.thumbs.retain(|k, _| Variant::parse(k).0 != name);
            }
        } else if let Some(m) = self.material_for(lib, name) {
            let mut edited = m.clone();
            f(&mut edited, &m);
            if edited != m {
                self.copy_builtin(lib, name, &m, edited, part, actions);
            }
        }
    }

    /// 自分の材質の編集を元に戻す（redo ならやり直す）
    fn undo_redo(&mut self, redo: bool) {
        let Some(u) = &mut self.user_edit else { return };
        if (redo && u.redo()) || (!redo && u.undo()) {
            let name = u.name.clone();
            self.thumbs.retain(|k, _| Variant::parse(k).0 != name);
        }
    }

    // ------------------------------------------------------------ GPU（GPU が待機中に呼ぶ）

    /// GPU の資源（見本と部品のプレビュー）を捨てる。コンテキストを捨てる前に呼ぶこと
    pub fn release_gpu(&mut self) {
        self.swatch = None;
        if let Some(p) = &mut self.part {
            p.preview = None;
        }
    }

    /// 見本とプレビューを描く。epoch はシーンを組み直すたびに変わる数（変わったら部品のプレビューを作り直す）
    pub fn render(&mut self, ctx: &'static Context, main: &Scene, part: Option<&PartInfo>, lib: &MaterialLibrary, spectral: bool, epoch: u64) {
        if !self.open {
            // 閉じたら GPU の資源を返す（見本の画像は残す）
            self.release_gpu();
            return;
        }
        let Some(egui_ctx) = self.egui.clone() else { return };
        let light = self.light.get_or_insert_with(PreviewLight::new).clone();
        if self.thumbs_spectral != Some(spectral) {
            self.thumbs.clear();
            self.thumbs_spectral = Some(spectral);
        }
        // 見本：焼き付けたもの・キャッシュにあるものはすぐ、無ければその場で描く（1 フレームに 12 ms まで）
        let t0 = Instant::now();
        for key in std::mem::take(&mut self.wanted) {
            if t0.elapsed() > Duration::from_millis(12) {
                break;
            }
            if self.thumbs.contains_key(&key) {
                continue;
            }
            let Some(m) = self.material_for(lib, &key) else { continue };
            let hash = swatch_cache::swatch_key(&m, spectral);
            // 動かしている途中の自分の材質は、キャッシュに書かない（止まってから描いたものを、次に開いたときに書く）
            let editing = self.user_edit.as_ref().is_some_and(|u| u.changed_at.is_some() && u.name == Variant::parse(&key).0);
            let r = (|| -> Result<egui::ColorImage, String> {
                if let Some(img) = swatch_cache::load(&hash) {
                    return Ok(img);
                }
                if self.swatch.is_none() {
                    self.swatch = Some(Preview::new(ctx, swatch_scene(&m), &light, SWATCH_PX, SWATCH_PX, swatch_camera(), spectral)?);
                }
                let img = swatch_cache::render(ctx, self.swatch.as_mut().unwrap(), &m, spectral, SWATCH_SPP, SWATCH_SPP)?;
                if !editing {
                    swatch_cache::store(&hash, &img);
                }
                Ok(img)
            })();
            match r {
                Ok(img) => {
                    if let Some(url) = web::publish(&self.web, &mut self.img_seq, &web::swatch_path(&key), &img) {
                        self.thumb_urls.insert(key.clone(), url);
                    }
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
            self.part = Some(PartView { reference: p.reference.clone(), model: p.model, epoch: u64::MAX, preview: None, map: Vec::new(), base: Vec::new(), tex: None, url: None, shown_samples: 0, azimuth: 45.0, elevation: 30.0, zoom: 1.0, error: String::new() });
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
                mats[local] = m.clone();
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
                    let img = pv.image_denoised(ctx, 0.0)?;
                    if let Some(url) = web::publish(&self.web, &mut self.img_seq, "part", &img) {
                        v.url = Some(url);
                    }
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

// ---------------------------------------------------------------- パラメータのパネル

/// 材質の種類（パネルに出す項目が変わる）
#[derive(PartialEq, Clone, Copy)]
enum Kind {
    /// 拡散する地を持つ誘電体（樹脂・ゴム・セラミック・塗装）
    Dielectric,
    Metal,
    /// 光が屈折して通る誘電体（ガラス・透明な樹脂・宝石）
    Clear,
}

impl Kind {
    fn of(m: &Material) -> Self {
        if m.base_metalness > 0.5 {
            Kind::Metal
        } else if m.transmission_weight > 0.0 {
            Kind::Clear
        } else {
            Kind::Dielectric
        }
    }
}

/// 種類を変える：ほかの種類の項目を物理的に矛盾しない値にし、分類が前の種類のものなら合わせる
fn set_kind(m: &mut Material, kind: Kind) {
    m.subsurface_weight = 0.0;
    match kind {
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
            if m.conductor.is_none() {
                if let Some(c) = conductor::find("Ni") {
                    m.set_conductor(c);
                }
            }
        }
        Kind::Clear => {
            m.base_metalness = 0.0;
            m.conductor = None;
            m.transmission_weight = 1.0;
            m.base_color = Vec3::ONE;
            m.specular_color = Vec3::ONE;
            m.coat_weight = 0.0;
            m.bump_pattern = krbr_scene::BumpPattern::None;
            m.bump_strength = 0.0;
            m.roughness_variation = 0.0;
        }
    }
    let cat = m.category();
    m.category = Some(match kind {
        Kind::Metal => Category::Metal,
        Kind::Clear if cat == Category::Gem => Category::Gem,
        Kind::Clear => Category::Glass,
        Kind::Dielectric if matches!(cat, Category::Metal | Category::Glass | Category::Gem) => Category::Plastic,
        Kind::Dielectric => cat,
    });
}

/// 誘電体の光学定数（屈折率と分散）
#[derive(Debug, Clone, PartialEq)]
struct Optics {
    ior: f32,
    sellmeier: Option<krbr_scene::material::Sellmeier>,
    dispersion_scale: f32,
    abbe: f32,
}

impl Optics {
    fn of(m: &Material) -> Self {
        Self { ior: m.specular_ior, sellmeier: m.sellmeier, dispersion_scale: m.transmission_dispersion_scale, abbe: m.transmission_dispersion_abbe_number }
    }

    fn apply(&self, m: &mut Material) {
        m.specular_ior = self.ior;
        m.sellmeier = self.sellmeier;
        m.transmission_dispersion_scale = self.dispersion_scale;
        m.transmission_dispersion_abbe_number = self.abbe;
    }

    fn ior_at(&self, lambda_nm: f32) -> f32 {
        let mut m = Material::default();
        self.apply(&mut m);
        m.ior_at(lambda_nm)
    }

    /// 同じ光学定数か（分散が無ければアッベ数は見ない）
    fn same(&self, o: &Optics) -> bool {
        self.ior == o.ior && self.sellmeier == o.sellmeier && self.dispersion_scale == o.dispersion_scale && (self.dispersion_scale <= 0.0 || self.abbe == o.abbe)
    }
}

/// 物質の一覧（誘電体）：組み込みの材質から光学定数だけを借りる（色・粗さはそのまま）。表示名は subst-<材質名>
const SUBSTANCES: &[&str] = &[
    "plastic",
    "plastic_abs",
    "plastic_pc",
    "plastic_pbt",
    "plastic_pa66",
    "plastic_pom",
    "plastic_pp",
    "plastic_pvc",
    "pet_clear",
    "plastic_ptfe",
    "acrylic",
    "epoxy_black",
    "rubber_silicone",
    "rubber_black",
    "ceramic",
    "glass_sapphire",
    "glass_bk7",
    "glass_fused_silica",
    "glass_soda_lime",
    "gem_diamond",
    "gem_emerald",
    "gem_cubic_zirconia",
];

fn substance_label(name: &str) -> String {
    // 物質は組み込みの材質だけ（表示名は訳）
    i18n::get(&format!("subst-{name}")).or_else(|| i18n::get(&format!("matname-{name}"))).unwrap_or_else(|| name.to_string())
}

/// 折りたためる節（Blender の Principled BSDF のパネルのように）。中は 2 列の表
fn section(ui: &mut egui::Ui, id: &str, title: String, open: bool, add: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(egui::RichText::new(title).strong()).id_salt(id).default_open(open).show(ui, |ui| {
        egui::Grid::new(format!("{id}_grid")).num_columns(2).spacing([10.0, 6.0]).min_col_width(96.0).show(ui, add);
    });
}

/// パラメータの 1 行（節の表の中）：名前と値。元の値から変えた項目は名前に印を付け、名前を右クリックすると元の値に戻せる
fn row<T: PartialEq + Clone>(ui: &mut egui::Ui, label: &str, tip: &str, value: &mut T, origin: &T, widget: impl FnOnce(&mut egui::Ui, &mut T)) {
    let modified = value != origin;
    let text = if modified { egui::RichText::new(format!("{label} •")).color(MODIFIED) } else { egui::RichText::new(label) };
    let mut resp = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
    if !tip.is_empty() {
        resp = resp.on_hover_text(tip);
    }
    if modified {
        resp = resp.on_hover_text(fl!("mated-modified-tip"));
    }
    resp.context_menu(|ui| {
        if ui.add_enabled(modified, egui::Button::new(fl!("mated-reset"))).clicked() {
            *value = origin.clone();
            ui.close();
        }
    });
    ui.horizontal(|ui| widget(ui, value));
    ui.end_row();
}

/// 値のスライダー。表示しただけでは値を変えない（刻みへの丸めと範囲への収めは、動かしたときだけ）：
/// 組み込みの材質は値が変わるとコピーを作るので、表示で値が動くとコピーが勝手にできてしまう
fn slider(ui: &mut egui::Ui, v: &mut f32, range: std::ops::RangeInclusive<f32>) {
    ui.add(egui::Slider::new(v, range).clamping(egui::SliderClamping::Edits).max_decimals(3));
}

fn log_slider(ui: &mut egui::Ui, v: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str) {
    ui.add(egui::Slider::new(v, range).logarithmic(true).clamping(egui::SliderClamping::Edits).suffix(suffix).max_decimals(3));
}

/// 数値の入力欄（スライダーと同じく、表示しただけでは範囲に収めない）
fn number(v: &mut f32, range: std::ops::RangeInclusive<f32>, speed: f64) -> egui::DragValue<'_> {
    egui::DragValue::new(v).range(range).clamp_existing_to_range(false).speed(speed).max_decimals(3)
}

/// 材質のパラメータのパネル。m を編集し、o は元の値（右クリックで戻す先）。種類（不透明・金属・透明）ごとに、
/// レンダラが扱い、物理的に意味のある項目だけを出す
fn param_editor(ui: &mut egui::Ui, lib: &MaterialLibrary, m: &mut Material, o: &Material) {
    let kind = Kind::of(m);
    let mut new_kind = kind;
    ui.horizontal(|ui| {
        ui.label(fl!("mated-kind"));
        ui.radio_value(&mut new_kind, Kind::Dielectric, fl!("mated-kind-dielectric")).on_hover_text(fl!("mated-kind-dielectric-tip"));
        ui.radio_value(&mut new_kind, Kind::Metal, fl!("mated-kind-metal")).on_hover_text(fl!("mated-kind-metal-tip"));
        ui.radio_value(&mut new_kind, Kind::Clear, fl!("mated-kind-clear")).on_hover_text(fl!("mated-kind-clear-tip"));
    });
    if new_kind != kind {
        set_kind(m, new_kind);
    }
    let kind = new_kind;

    // ---- 物質と表面の色・粗さ
    section(ui, "mated_sec_base", fl!("mated-sec-base"), true, |ui| {
        if kind == Kind::Metal {
            let mut v = (m.conductor, m.base_color, m.specular_color);
            row(ui, &fl!("mated-substance"), &fl!("mated-metal-custom-tip"), &mut v, &(o.conductor, o.base_color, o.specular_color), |ui, v| {
                let current = v.0.and_then(conductor::get);
                let text = current.map(|c| conductor_label(c.id)).unwrap_or_else(|| fl!("mated-metal-custom"));
                egui::ComboBox::from_id_salt("mated_conductor").selected_text(text).width(220.0).show_ui(ui, |ui| {
                    for c in conductor::all() {
                        if ui.selectable_label(current.is_some_and(|x| x.index == c.index), conductor_label(c.id)).on_hover_text(c.source).clicked() {
                            let mut t = Material::default();
                            t.set_conductor(c);
                            *v = (t.conductor, t.base_color, t.specular_color);
                        }
                    }
                    if ui.selectable_label(current.is_none(), fl!("mated-metal-custom")).on_hover_text(fl!("mated-metal-custom-tip")).clicked() {
                        v.0 = None;
                    }
                });
            });
            (m.conductor, m.base_color, m.specular_color) = v;
            if m.conductor.is_none() {
                row(ui, &fl!("mated-f0"), &fl!("mated-f0-tip"), &mut m.base_color, &o.base_color, color_edit);
                row(ui, &fl!("mated-f82"), &fl!("mated-f82-tip"), &mut m.specular_color, &o.specular_color, color_edit);
            }
        } else {
            let mut v = Optics::of(m);
            row(ui, &fl!("mated-substance"), &fl!("mated-substance-tip"), &mut v, &Optics::of(o), |ui, v| {
                let subs: Vec<(&str, Optics)> = SUBSTANCES.iter().filter_map(|n| lib.find(n).map(|s| (*n, Optics::of(s)))).collect();
                let current = subs.iter().find(|(_, s)| s.same(v)).map(|(n, _)| *n);
                let text = current.map(substance_label).unwrap_or_else(|| fl!("mated-substance-custom", ior = format!("{:.3}", v.ior_at(587.6))));
                egui::ComboBox::from_id_salt("mated_substance").selected_text(text).width(220.0).height(400.0).show_ui(ui, |ui| {
                    for (n, s) in &subs {
                        let tip = format!("nd {:.3}{}", s.ior_at(587.6), if s.sellmeier.is_some() { fl!("mated-sellmeier") } else { String::new() });
                        if ui.selectable_label(current == Some(*n), substance_label(n)).on_hover_text(tip).clicked() {
                            *v = s.clone();
                        }
                    }
                });
            });
            row(ui, &fl!("mated-ior"), &fl!("mated-ior-tip"), &mut v, &Optics::of(o), |ui, v| {
                let mut nd = v.ior_at(587.6);
                if ui.add(number(&mut nd, 1.0..=3.0, 0.002)).changed() {
                    if v.sellmeier.is_some() {
                        // 分散は、分散式から求めたアッベ数で残す
                        let (nf, nc, d) = (v.ior_at(486.1), v.ior_at(656.3), v.ior_at(587.6));
                        v.sellmeier = None;
                        if nf > nc {
                            v.abbe = (d - 1.0) / (nf - nc);
                            v.dispersion_scale = 1.0;
                        }
                    }
                    v.ior = nd;
                }
                if v.sellmeier.is_some() {
                    ui.weak(fl!("mated-sellmeier"));
                }
            });
            if kind == Kind::Clear && v.sellmeier.is_none() {
                row(ui, &fl!("mated-abbe"), &fl!("mated-abbe-tip"), &mut v, &Optics::of(o), |ui, v| {
                    let mut on = v.dispersion_scale > 0.0;
                    if ui.checkbox(&mut on, "").changed() {
                        v.dispersion_scale = if on { 1.0 } else { 0.0 };
                    }
                    if on {
                        ui.add(number(&mut v.abbe, 10.0..=100.0, 0.2));
                    }
                });
            }
            v.apply(m);
            if kind == Kind::Dielectric {
                row(ui, &fl!("mated-base-color"), "", &mut m.base_color, &o.base_color, |ui, c| {
                    color_edit(ui, c);
                    albedo_warning(ui, *c);
                });
            } else {
                row(ui, &fl!("mated-trans-color"), "", &mut m.transmission_color, &o.transmission_color, color_edit);
                row(ui, &fl!("mated-trans-depth"), &fl!("mated-trans-depth-tip"), &mut m.transmission_depth, &o.transmission_depth, |ui, d| {
                    ui.add(number(d, 0.0..=1000.0, 0.05).suffix(" mm"));
                });
            }
        }
        row(ui, &fl!("mated-roughness"), &fl!("mated-roughness-param-tip"), &mut m.specular_roughness, &o.specular_roughness, |ui, r| slider(ui, r, 0.0..=1.0));
    });

    // ---- 半透明（拡散の透過）
    if kind == Kind::Dielectric {
        section(ui, "mated_sec_sss", fl!("mated-sec-subsurface"), m.subsurface_weight > 0.0, |ui| {
            row(ui, &fl!("mated-sss-weight"), &fl!("mated-sss-weight-tip"), &mut m.subsurface_weight, &o.subsurface_weight, |ui, w| slider(ui, w, 0.0..=1.0));
            if m.subsurface_weight > 0.0 {
                row(ui, &fl!("mated-sss-color"), &fl!("mated-sss-color-tip"), &mut m.subsurface_color, &o.subsurface_color, color_edit);
                row(ui, &fl!("mated-sss-radius"), &fl!("mated-sss-radius-tip"), &mut m.subsurface_radius, &o.subsurface_radius, |ui, r| {
                    log_slider(ui, r, 0.01..=100.0, " mm");
                });
            }
        });
    }

    // ---- コート（透明な材質には付けない）
    if kind != Kind::Clear {
        section(ui, "mated_sec_coat", fl!("mated-coat"), m.coat_weight > 0.0, |ui| {
            row(ui, &fl!("mated-coat-weight"), &fl!("mated-coat-tip"), &mut m.coat_weight, &o.coat_weight, |ui, w| slider(ui, w, 0.0..=1.0));
            if m.coat_weight > 0.0 {
                row(ui, &fl!("mated-roughness"), "", &mut m.coat_roughness, &o.coat_roughness, |ui, r| slider(ui, r, 0.0..=1.0));
                row(ui, &fl!("mated-coat-ior"), &fl!("mated-coat-ior-tip"), &mut m.coat_ior, &o.coat_ior, |ui, n| {
                    ui.add(number(n, 1.3..=2.0, 0.002));
                });
                row(ui, &fl!("mated-coat-color"), &fl!("mated-coat-color-tip"), &mut m.coat_color, &o.coat_color, color_edit);
            }
        });
    }

    // ---- 表面の模様
    if kind != Kind::Clear {
        use krbr_scene::BumpPattern;
        section(ui, "mated_sec_pattern", fl!("mated-pattern"), m.bump_pattern != BumpPattern::None, |ui| {
            let mut v = (m.bump_pattern, m.bump_strength, m.bump_scale, m.roughness_variation);
            row(ui, &fl!("mated-pattern-kind"), &fl!("mated-pattern-tip"), &mut v, &(o.bump_pattern, o.bump_strength, o.bump_scale, o.roughness_variation), |ui, v| {
                let label = |p: BumpPattern| i18n::get(&format!("mated-pattern-{}", p.as_str())).unwrap_or_else(|| p.as_str().to_string());
                egui::ComboBox::from_id_salt("mated_pattern").selected_text(label(v.0)).show_ui(ui, |ui| {
                    for p in BumpPattern::ALL {
                        if ui.selectable_label(v.0 == p, label(p)).clicked() && v.0 != p {
                            *v = match p {
                                BumpPattern::None => (p, 0.0, v.2, 0.0),
                                // 初めの値：樹脂の粒状感、ガラス繊維の織り目
                                BumpPattern::Noise => (p, 0.012, 0.2, 0.02),
                                BumpPattern::Weave => (p, 0.02, 0.5, 0.05),
                            };
                        }
                    }
                });
            });
            (m.bump_pattern, m.bump_strength, m.bump_scale, m.roughness_variation) = v;
            if m.bump_pattern != BumpPattern::None {
                row(ui, &fl!("mated-pattern-strength"), "", &mut m.bump_strength, &o.bump_strength, |ui, s| slider(ui, s, 0.0..=0.05));
                row(ui, &fl!("mated-pattern-scale"), &fl!("mated-pattern-scale-tip"), &mut m.bump_scale, &o.bump_scale, |ui, s| {
                    log_slider(ui, s, 0.02..=2.0, " mm");
                });
                row(ui, &fl!("mated-pattern-rough"), &fl!("mated-pattern-rough-tip"), &mut m.roughness_variation, &o.roughness_variation, |ui, r| slider(ui, r, 0.0..=0.3));
            }
        });
    }

    // ---- 一覧での分類
    egui::Grid::new("mated_general").num_columns(2).spacing([10.0, 6.0]).min_col_width(96.0).show(ui, |ui| {
        let mut cat = m.category();
        row(ui, &fl!("mated-category"), "", &mut cat, &o.category(), |ui, cat| {
            egui::ComboBox::from_id_salt("mated_category").selected_text(category_label(*cat)).show_ui(ui, |ui| {
                for c in Category::ALL {
                    ui.selectable_value(cat, c, category_label(c));
                }
            });
        });
        if cat != m.category() {
            m.category = Some(cat);
        }
    });
}

// ---------------------------------------------------------------- 小物

/// 見本を落とせる場所：材質をドラッグして上に来たら枠を描く。上に来ているか
fn drop_target(ui: &egui::Ui, resp: &egui::Response, rect: egui::Rect) -> bool {
    if resp.dnd_hover_payload::<MaterialDrag>().is_none() {
        return false;
    }
    ui.painter().rect_stroke(rect.expand(2.0), 4.0, egui::Stroke::new(2.0, ui.visuals().selection.stroke.color), egui::StrokeKind::Outside);
    true
}

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

/// 一覧に無い名前（"名前"、"名前 2"、…）
/// 新しい材質の名前（表示名）の候補：ほかの材質と見分けられるよう、同じ表示名があれば番号を付ける
/// （ID で区別するので同じ名前でも作れるが、一覧で紛らわしい）
fn unique_label(lib: &MaterialLibrary, want: &str) -> String {
    let base: String = want.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().to_string();
    let base = if base.is_empty() { "material".to_string() } else { base };
    let taken: HashSet<String> = lib.names().iter().map(|n| material_label(lib, n)).collect();
    if !taken.contains(&base) {
        return base;
    }
    (2..).map(|i| format!("{base} {i}")).find(|n| !taken.contains(n)).unwrap()
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
        for n in RARE {
            assert!(builtin_names().contains(*n), "{n}");
        }
        // 一覧の順：金属が先頭で、全部の材質がどこかに入る
        let g = grouped_names(&lib);
        assert_eq!(g[0].0, Category::Metal);
        assert_eq!(g[0].1[0], "metal_gold");
        assert_eq!(g.iter().map(|(_, v)| v.len()).sum::<usize>(), lib.names().len());
    }

    /// パネルを描いただけで値が変わらないこと（組み込みの材質は値が変わるとコピーを作るので、勝手にコピーができてしまう）
    #[test]
    fn showing_the_panel_does_not_change_the_material() {
        let lib = MaterialLibrary::builtin();
        let ctx = egui::Context::default();
        for name in builtin_names() {
            let o = lib.find(name).unwrap().clone();
            let mut m = o.clone();
            for _ in 0..3 {
                // 描いた結果（フォントの画像など）は使わないので捨てる（捨てる前に clear しないと epaint が警告で止まる）
                ctx.run_ui(egui::RawInput::default(), |ui| param_editor(ui, &lib, &mut m, &o)).textures_delta.clear();
            }
            assert_eq!(m, o, "{name}");
        }
    }

    #[test]
    fn kind_changes_keep_the_material_physical() {
        let lib = MaterialLibrary::builtin();
        let mut m = lib.find("plastic_abs").unwrap().clone();
        set_kind(&mut m, Kind::Metal);
        assert!(m.conductor.is_some() && m.base_metalness == 1.0 && m.category() == Category::Metal);
        set_kind(&mut m, Kind::Clear);
        assert!(m.conductor.is_none() && m.base_metalness == 0.0 && m.transmission_weight == 1.0 && m.category() == Category::Glass);
        assert_eq!(m.coat_weight, 0.0);
        set_kind(&mut m, Kind::Dielectric);
        assert!(m.transmission_weight == 0.0 && m.base_color.max_element() <= 0.9 && m.category() == Category::Plastic);
        // 物質を選ぶと光学定数だけが変わる
        let pc = Optics::of(lib.find("plastic_pc").unwrap());
        let before = m.clone();
        pc.apply(&mut m);
        assert!(Optics::of(&m).same(&pc) && m.base_color == before.base_color && m.specular_roughness == before.specular_roughness);
        for n in SUBSTANCES {
            assert!(lib.find(n).is_some_and(|s| Kind::of(s) != Kind::Metal), "{n}");
            assert!(i18n::english_has(&format!("subst-{n}")), "subst-{n}");
        }
    }

    #[test]
    fn undo_and_redo() {
        let lib = MaterialLibrary::builtin();
        let a = lib.find("plastic_abs").unwrap().clone();
        let mut u = UserEdit::new("x", "x", a.clone(), a.clone());
        // 一続きの編集は 1 つの履歴
        for r in [0.1, 0.2, 0.3] {
            let before = u.material.clone();
            u.material.specular_roughness = r;
            u.changed(before);
        }
        u.in_burst = false; // 書き込んで一続きの編集が終わった
        let before = u.material.clone();
        u.material.specular_ior = 1.6;
        u.changed(before);
        assert_eq!(u.undo.len(), 2);
        assert!(u.undo());
        assert_eq!((u.material.specular_roughness, u.material.specular_ior), (0.3, a.specular_ior));
        assert!(u.undo());
        assert_eq!(u.material, a);
        assert!(!u.undo());
        assert!(u.redo() && u.redo() && !u.redo());
        assert_eq!(u.material.specular_ior, 1.6);
    }

    #[test]
    fn part_adjustment_over_the_material() {
        let mut lib = MaterialLibrary::builtin();
        lib.insert("user_1", lib.find("plastic").unwrap().clone(), "test");
        let part = |material: &str| PartInfo { reference: "J2".into(), model: None, instances: Vec::new(), rows: vec![PartRow { key: "#a0a0a0".into(), material: material.into(), scene_material: 0 }] };
        let assigned = |actions: &[Action]| actions.iter().rev().find_map(|a| if let Action::Assign { material, .. } = a { Some(material.clone()) } else { None });
        let editor = |browsing: &str| MaterialEditor { browsing: Some(browsing.into()), target: Some("#a0a0a0".into()), ..Default::default() };

        // 調整が無ければ知らせない。一覧で別の材質を選んでいても知らせない
        assert!(editor("user_1").target_override(Some(&part("user_1+smooth"))).is_none());
        assert!(editor("plastic").target_override(Some(&part("user_1+color=#ff0000"))).is_none());

        // 外す：模様なしは残して、色・粗さだけ外す
        let p = part("user_1+smooth+color=#ff0000+rough=0.2");
        let mut e = editor("user_1");
        assert!(e.override_text(&lib, Some(&p)).is_some_and(|t| t.contains("#ff0000") && t.contains("0.20")));
        let mut actions = Vec::new();
        e.clear_override(&lib, Some(&p), &mut actions);
        assert_eq!(assigned(&actions).as_deref(), Some("user_1+smooth"));

        // 取り込む（自分の材質）：材質の値にして、割り当ては調整なしに
        let mut e = editor("user_1");
        e.select_browsing(&lib, "plastic", &mut Vec::new());
        e.select_browsing(&lib, "user_1", &mut Vec::new());
        let mut actions = Vec::new();
        e.adopt_override(&lib, Some(&p), &mut actions);
        let saved = actions.iter().find_map(|a| if let Action::SaveUser { name, material, .. } = a { Some((name.clone(), material.clone())) } else { None }).unwrap();
        assert_eq!((saved.0.as_str(), saved.1.base_color, saved.1.specular_roughness), ("user_1", Vec3::new(1.0, 0.0, 0.0), 0.2));
        assert_eq!(assigned(&actions).as_deref(), Some("user_1+smooth"));

        // 取り込む（組み込みの材質）：コピーを作ってそれにする
        let p = part("plastic_pbt+color=#ff0000");
        let mut e = editor("plastic_pbt");
        let mut actions = Vec::new();
        e.adopt_override(&lib, Some(&p), &mut actions);
        let copy = assigned(&actions).unwrap();
        assert!(copy.starts_with("user_") && Variant::parse(&copy).1 == Variant::default(), "{copy}");
        assert!(actions.iter().any(|a| matches!(a, Action::SaveUser { name, material, .. } if *name == copy && material.base_color == Vec3::new(1.0, 0.0, 0.0))));
    }

    #[test]
    fn unique_labels() {
        let mut lib = MaterialLibrary::builtin();
        assert_eq!(unique_label(&lib, "my+red.1"), "my+red.1");
        let taken = material_label(&lib, "plastic");
        assert_eq!(unique_label(&lib, &taken), format!("{taken} 2"));
        lib.insert("user_1", Material::default(), "test");
        lib.set_label("user_1", "mine");
        assert_eq!(unique_label(&lib, "mine"), "mine 2");
        assert_eq!(material_label(&lib, "user_1+smooth"), "mine");
        assert!(valid_material_label(&unique_label(&lib, " a\nb ")));
    }
}
