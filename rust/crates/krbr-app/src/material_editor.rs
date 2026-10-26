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

    /// 自分の材質を消す
    fn delete_user(&mut self, name: &str, actions: &mut Vec<Action>) {
        actions.push(Action::DeleteUser(name.to_string()));
        self.user_edit = None;
        self.browsing = None;
        self.confirm_delete = false;
        self.thumbs.retain(|k, _| Variant::parse(k).0 != name);
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

    /// 止まってから書く部位の調整は、書く前に別の部品を選んでも、エディタを閉じても、調整した部品に書く（以前は捨てていた）
    #[test]
    fn pending_adjustments_are_written_to_their_part() {
        let lib = MaterialLibrary::builtin();
        let part = |r: &str| PartInfo { reference: r.into(), model: Some(0), instances: Vec::new(), rows: vec![PartRow { key: "Body 1".into(), material: "plastic".into(), scene_material: 0 }] };
        let assigned_to = |actions: &[Action], r: &str| {
            actions.iter().find_map(|a| match a {
                Action::AssignTo { reference, model, part, material } if reference == r => Some((*model, part.clone(), material.clone())),
                _ => None,
            })
        };
        let mut e = MaterialEditor { open: true, ..Default::default() };
        let mut actions = Vec::new();
        e.adjust_to(Some(&part("U1")), "U1", "Body 1", "plastic+color=#ff0000".into(), &mut actions);
        assert!(actions.is_empty(), "written only after the edit stops");
        // 書く前に U2 を選んだ：U1 の調整をその場で書く
        let actions = e.web_update(&lib, Some(&part("U2")));
        assert_eq!(assigned_to(&actions, "U1"), Some((Some(0), "Body 1".to_string(), "plastic+color=#ff0000".to_string())));
        // 書く前に閉じた：閉じたときに書く
        e.adjust_to(Some(&part("U2")), "U2", "Body 1", "plastic+rough=0.5".into(), &mut Vec::new());
        e.open = false;
        let actions = e.web_update(&lib, Some(&part("U2")));
        assert_eq!(assigned_to(&actions, "U2").map(|a| a.2), Some("plastic+rough=0.5".to_string()));
    }
}
