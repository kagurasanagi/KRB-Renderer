//! --web-ui の材質エディタ：状態を web/editor.html の krbr.editor に渡す JSON にし、ページからの操作（id と値）を反映する。
//! 中身は egui の材質エディタ（ui）と同じ。見本と部品のプレビューの画像は PNG にして WebView の画像の置き場に置き、
//! ページは URL（描き直すたびに変わる）で読む

use image::ImageEncoder;
use serde_json::{json, Value};

use super::*;

/// 画像を置き場に置き、ページから読む URL を返す（描き直すたびに変わる）。置き場が無ければ（egui のとき）何もしない
pub(super) fn publish(store: &Option<crate::web_ui::Images>, seq: &mut u64, path: &str, img: &egui::ColorImage) -> Option<String> {
    let store = store.as_ref()?;
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(&mut png, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Sub)
        .write_image(&rgba, img.size[0] as u32, img.size[1] as u32, image::ExtendedColorType::Rgba8)
        .ok()?;
    store.lock().ok()?.insert(path.to_string(), png);
    *seq += 1;
    Some(format!("{}{path}?v={seq}", crate::web_ui::IMAGE_URL))
}

/// 見本の画像の置き場の名前（材質名には + = # などが入るので 16 進に）
pub(super) fn swatch_path(key: &str) -> String {
    let hex: String = key.bytes().map(|b| format!("{b:02x}")).collect();
    format!("swatch/{hex}")
}

fn color_hex(linear: Vec3) -> String {
    let c = srgb8(linear);
    srgb_hex(c)
}

fn srgb_hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn parse_srgb(v: &Value) -> Option<[u8; 3]> {
    let s = v.as_str()?.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let b = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some([b(0)?, b(2)?, b(4)?])
}

fn parse_linear(v: &Value) -> Option<Vec3> {
    let c = parse_srgb(v)?;
    Some(srgb_to_linear(Vec3::new(c[0] as f32, c[1] as f32, c[2] as f32) / 255.0))
}

fn num(v: &Value) -> Option<f32> {
    v.as_f64().filter(|x| x.is_finite()).map(|x| x as f32)
}

// ---------------------------------------------------------------- パラメータの部品

/// パラメータの 1 行：名前と値の部品。元の値から変えた行は名前に印（右クリックで元の値）
fn prow(id: &str, label: String, tip: String, modified: bool, controls: Vec<Value>) -> Value {
    json!({ "id": id, "label": label, "tip": tip, "modified": modified, "controls": controls })
}

fn c_select(id: &str, value: &str, options: Vec<(String, String, String)>) -> Value {
    json!({ "t": "select", "id": id, "value": value, "options": options.into_iter().map(|(v, l, t)| json!({ "v": v, "label": l, "tip": t })).collect::<Vec<_>>() })
}

fn c_number(id: &str, value: f32, min: f32, max: f32, step: f32, digits: u32, suffix: &str) -> Value {
    json!({ "t": "number", "id": id, "value": value, "min": min, "max": max, "step": step, "digits": digits, "suffix": suffix })
}

fn c_slider(id: &str, value: f32, min: f32, max: f32, log: bool, suffix: &str) -> Value {
    json!({ "t": "slider", "id": id, "value": value, "min": min, "max": max, "log": log, "digits": 3, "suffix": suffix })
}

fn c_color(id: &str, linear: Vec3) -> Value {
    json!({ "t": "color", "id": id, "value": color_hex(linear) })
}

fn c_check(id: &str, value: bool, label: String, tip: String) -> Value {
    json!({ "t": "check", "id": id, "value": value, "label": label, "tip": tip })
}

fn c_note(text: String, tone: &str, tip: String) -> Value {
    json!({ "t": "note", "text": text, "tone": tone, "tip": tip })
}

fn section(id: &str, title: String, open: bool, rows: Vec<Value>) -> Value {
    json!({ "id": id, "title": title, "open": open, "rows": rows })
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Dielectric => "dielectric",
        Kind::Metal => "metal",
        Kind::Clear => "clear",
    }
}

fn pattern_label(p: krbr_scene::BumpPattern) -> String {
    i18n::get(&format!("mated-pattern-{}", p.as_str())).unwrap_or_else(|| p.as_str().to_string())
}

/// 誘電体の物質の一覧（光学定数を借りる組み込みの材質）
fn substances(lib: &MaterialLibrary) -> Vec<(&'static str, Optics)> {
    SUBSTANCES.iter().filter_map(|n| lib.find(n).map(|s| (*n, Optics::of(s)))).collect()
}

/// パラメータのパネル（egui の param_editor と同じ項目）。m は今の値、o は元の値
fn param_json(lib: &MaterialLibrary, m: &Material, o: &Material) -> Value {
    use krbr_scene::BumpPattern;
    let kind = Kind::of(m);
    let mut base = Vec::new();
    if kind == Kind::Metal {
        let current = m.conductor.and_then(conductor::get);
        let mut opts: Vec<(String, String, String)> = conductor::all().map(|c| (c.id.to_string(), conductor_label(c.id), c.source.to_string())).collect();
        opts.push(("custom".into(), fl!("mated-metal-custom"), fl!("mated-metal-custom-tip")));
        let modified = (m.conductor, m.base_color, m.specular_color) != (o.conductor, o.base_color, o.specular_color);
        base.push(prow("metal", fl!("mated-substance"), fl!("mated-metal-custom-tip"), modified, vec![c_select("conductor", current.map(|c| c.id).unwrap_or("custom"), opts)]));
        if m.conductor.is_none() {
            base.push(prow("f0", fl!("mated-f0"), fl!("mated-f0-tip"), m.base_color != o.base_color, vec![c_color("f0", m.base_color)]));
            base.push(prow("f82", fl!("mated-f82"), fl!("mated-f82-tip"), m.specular_color != o.specular_color, vec![c_color("f82", m.specular_color)]));
        }
    } else {
        let v = Optics::of(m);
        let modified = v != Optics::of(o);
        let subs = substances(lib);
        let current = subs.iter().find(|(_, s)| s.same(&v)).map(|(n, _)| *n);
        let mut opts: Vec<(String, String, String)> =
            subs.iter().map(|(n, s)| (n.to_string(), substance_label(n), format!("nd {:.3}{}", s.ior_at(587.6), if s.sellmeier.is_some() { fl!("mated-sellmeier") } else { String::new() }))).collect();
        if current.is_none() {
            opts.push(("custom".into(), fl!("mated-substance-custom", ior = format!("{:.3}", v.ior_at(587.6))), String::new()));
        }
        base.push(prow("substance", fl!("mated-substance"), fl!("mated-substance-tip"), modified, vec![c_select("substance", current.unwrap_or("custom"), opts)]));
        let mut ior = vec![c_number("ior", v.ior_at(587.6), 1.0, 3.0, 0.002, 3, "")];
        if v.sellmeier.is_some() {
            ior.push(c_note(fl!("mated-sellmeier"), "muted", String::new()));
        }
        base.push(prow("ior", fl!("mated-ior"), fl!("mated-ior-tip"), modified, ior));
        if kind == Kind::Clear && v.sellmeier.is_none() {
            let on = v.dispersion_scale > 0.0;
            let mut c = vec![c_check("abbe.on", on, String::new(), String::new())];
            if on {
                c.push(c_number("abbe", v.abbe, 10.0, 100.0, 0.2, 1, ""));
            }
            base.push(prow("abbe", fl!("mated-abbe"), fl!("mated-abbe-tip"), modified, c));
        }
        if kind == Kind::Dielectric {
            let mut c = vec![c_color("base.color", m.base_color)];
            let e = m.base_color.max_element();
            if !(0.015..=0.9).contains(&e) {
                c.push(c_note("⚠".into(), "warn", fl!("mated-albedo-warning")));
            }
            base.push(prow("base", fl!("mated-base-color"), String::new(), m.base_color != o.base_color, c));
        } else {
            base.push(prow("trans.color", fl!("mated-trans-color"), String::new(), m.transmission_color != o.transmission_color, vec![c_color("trans.color", m.transmission_color)]));
            base.push(prow("trans.depth", fl!("mated-trans-depth"), fl!("mated-trans-depth-tip"), m.transmission_depth != o.transmission_depth, vec![c_number("trans.depth", m.transmission_depth, 0.0, 1000.0, 0.05, 2, " mm")]));
        }
    }
    base.push(prow("rough", fl!("mated-roughness"), fl!("mated-roughness-param-tip"), m.specular_roughness != o.specular_roughness, vec![c_slider("rough", m.specular_roughness, 0.0, 1.0, false, "")]));
    let mut sections = vec![section("base", fl!("mated-sec-base"), true, base)];

    if kind == Kind::Dielectric {
        let mut rows = vec![prow("sss.weight", fl!("mated-sss-weight"), fl!("mated-sss-weight-tip"), m.subsurface_weight != o.subsurface_weight, vec![c_slider("sss.weight", m.subsurface_weight, 0.0, 1.0, false, "")])];
        if m.subsurface_weight > 0.0 {
            rows.push(prow("sss.color", fl!("mated-sss-color"), fl!("mated-sss-color-tip"), m.subsurface_color != o.subsurface_color, vec![c_color("sss.color", m.subsurface_color)]));
            rows.push(prow("sss.radius", fl!("mated-sss-radius"), fl!("mated-sss-radius-tip"), m.subsurface_radius != o.subsurface_radius, vec![c_slider("sss.radius", m.subsurface_radius, 0.01, 100.0, true, " mm")]));
        }
        sections.push(section("sss", fl!("mated-sec-subsurface"), m.subsurface_weight > 0.0, rows));
    }
    if kind != Kind::Clear {
        let mut rows = vec![prow("coat.weight", fl!("mated-coat-weight"), fl!("mated-coat-tip"), m.coat_weight != o.coat_weight, vec![c_slider("coat.weight", m.coat_weight, 0.0, 1.0, false, "")])];
        if m.coat_weight > 0.0 {
            rows.push(prow("coat.rough", fl!("mated-roughness"), String::new(), m.coat_roughness != o.coat_roughness, vec![c_slider("coat.rough", m.coat_roughness, 0.0, 1.0, false, "")]));
            rows.push(prow("coat.ior", fl!("mated-coat-ior"), fl!("mated-coat-ior-tip"), m.coat_ior != o.coat_ior, vec![c_number("coat.ior", m.coat_ior, 1.3, 2.0, 0.002, 3, "")]));
            rows.push(prow("coat.color", fl!("mated-coat-color"), fl!("mated-coat-color-tip"), m.coat_color != o.coat_color, vec![c_color("coat.color", m.coat_color)]));
        }
        sections.push(section("coat", fl!("mated-coat"), m.coat_weight > 0.0, rows));

        let opts = BumpPattern::ALL.iter().map(|p| (p.as_str().to_string(), pattern_label(*p), String::new())).collect();
        let modified = (m.bump_pattern, m.bump_strength, m.bump_scale, m.roughness_variation) != (o.bump_pattern, o.bump_strength, o.bump_scale, o.roughness_variation);
        let mut rows = vec![prow("pattern", fl!("mated-pattern-kind"), fl!("mated-pattern-tip"), modified, vec![c_select("pattern", m.bump_pattern.as_str(), opts)])];
        if m.bump_pattern != BumpPattern::None {
            rows.push(prow("pattern.strength", fl!("mated-pattern-strength"), String::new(), m.bump_strength != o.bump_strength, vec![c_slider("pattern.strength", m.bump_strength, 0.0, 0.05, false, "")]));
            rows.push(prow("pattern.scale", fl!("mated-pattern-scale"), fl!("mated-pattern-scale-tip"), m.bump_scale != o.bump_scale, vec![c_slider("pattern.scale", m.bump_scale, 0.02, 2.0, true, " mm")]));
            rows.push(prow("pattern.rough", fl!("mated-pattern-rough"), fl!("mated-pattern-rough-tip"), m.roughness_variation != o.roughness_variation, vec![c_slider("pattern.rough", m.roughness_variation, 0.0, 0.3, false, "")]));
        }
        sections.push(section("pattern", fl!("mated-pattern"), m.bump_pattern != BumpPattern::None, rows));
    }
    let cats = Category::ALL.iter().map(|c| (c.as_str().to_string(), category_label(*c), String::new())).collect();
    let general = vec![prow("category", fl!("mated-category"), String::new(), m.category() != o.category(), vec![c_select("category", m.category().as_str(), cats)])];
    json!({
        "kind": {
            "label": fl!("mated-kind"), "value": kind_name(kind),
            "options": [
                { "v": "dielectric", "label": fl!("mated-kind-dielectric"), "tip": fl!("mated-kind-dielectric-tip") },
                { "v": "metal", "label": fl!("mated-kind-metal"), "tip": fl!("mated-kind-metal-tip") },
                { "v": "clear", "label": fl!("mated-kind-clear"), "tip": fl!("mated-kind-clear-tip") },
            ],
        },
        "sections": sections,
        "general": general,
        "modifiedTip": fl!("mated-modified-tip"),
        "reset": fl!("mated-reset"),
    })
}

/// パラメータの部品の値を変える（egui の param_editor と同じ決まりで）
fn apply_param(lib: &MaterialLibrary, m: &mut Material, id: &str, v: &Value) {
    use krbr_scene::BumpPattern;
    let s = v.as_str().unwrap_or("");
    let b = v.as_bool().unwrap_or(false);
    let set = |slot: &mut f32, lo: f32, hi: f32| {
        if let Some(x) = num(v) {
            *slot = x.clamp(lo, hi);
        }
    };
    match id {
        "kind" => {
            let k = match s {
                "metal" => Kind::Metal,
                "clear" => Kind::Clear,
                _ => Kind::Dielectric,
            };
            if k != Kind::of(m) {
                set_kind(m, k);
            }
        }
        "conductor" => match conductor::find(s) {
            Some(c) => {
                let mut t = Material::default();
                t.set_conductor(c);
                (m.conductor, m.base_color, m.specular_color) = (t.conductor, t.base_color, t.specular_color);
            }
            None => m.conductor = None,
        },
        "f0" => m.base_color = parse_linear(v).unwrap_or(m.base_color),
        "f82" => m.specular_color = parse_linear(v).unwrap_or(m.specular_color),
        "substance" => {
            if let Some((_, o)) = substances(lib).into_iter().find(|(n, _)| *n == s) {
                o.apply(m);
            }
        }
        "ior" => {
            let Some(nd) = num(v) else { return };
            let mut o = Optics::of(m);
            if o.sellmeier.is_some() {
                // 分散は、分散式から求めたアッベ数で残す
                let (nf, nc, d) = (o.ior_at(486.1), o.ior_at(656.3), o.ior_at(587.6));
                o.sellmeier = None;
                if nf > nc {
                    o.abbe = (d - 1.0) / (nf - nc);
                    o.dispersion_scale = 1.0;
                }
            }
            o.ior = nd.clamp(1.0, 3.0);
            o.apply(m);
        }
        "abbe.on" => m.transmission_dispersion_scale = if b { 1.0 } else { 0.0 },
        "abbe" => set(&mut m.transmission_dispersion_abbe_number, 10.0, 100.0),
        "base.color" => m.base_color = parse_linear(v).unwrap_or(m.base_color),
        "trans.color" => m.transmission_color = parse_linear(v).unwrap_or(m.transmission_color),
        "trans.depth" => set(&mut m.transmission_depth, 0.0, 1000.0),
        "rough" => set(&mut m.specular_roughness, 0.0, 1.0),
        "sss.weight" => set(&mut m.subsurface_weight, 0.0, 1.0),
        "sss.color" => m.subsurface_color = parse_linear(v).unwrap_or(m.subsurface_color),
        "sss.radius" => set(&mut m.subsurface_radius, 0.01, 100.0),
        "coat.weight" => set(&mut m.coat_weight, 0.0, 1.0),
        "coat.rough" => set(&mut m.coat_roughness, 0.0, 1.0),
        "coat.ior" => set(&mut m.coat_ior, 1.3, 2.0),
        "coat.color" => m.coat_color = parse_linear(v).unwrap_or(m.coat_color),
        "pattern" => {
            let Some(p) = BumpPattern::ALL.into_iter().find(|p| p.as_str() == s) else { return };
            if p != m.bump_pattern {
                (m.bump_pattern, m.bump_strength, m.roughness_variation) = match p {
                    BumpPattern::None => (p, 0.0, 0.0),
                    // 初めの値：樹脂の粒状感、ガラス繊維の織り目
                    BumpPattern::Noise => {
                        m.bump_scale = 0.2;
                        (p, 0.012, 0.02)
                    }
                    BumpPattern::Weave => {
                        m.bump_scale = 0.5;
                        (p, 0.02, 0.05)
                    }
                };
            }
        }
        "pattern.strength" => set(&mut m.bump_strength, 0.0, 0.05),
        "pattern.scale" => set(&mut m.bump_scale, 0.02, 2.0),
        "pattern.rough" => set(&mut m.roughness_variation, 0.0, 0.3),
        "category" => {
            if let Some(c) = Category::ALL.into_iter().find(|c| c.as_str() == s) {
                if c != m.category() {
                    m.category = Some(c);
                }
            }
        }
        _ => {}
    }
}

/// 部位の調整が優先している行（色・粗さ）に印を付ける（param_json の行の id で探す）
fn mark_overridden(params: &mut Value, m: &Material, v: Variant) {
    let mut rows: Vec<&str> = Vec::new();
    if v.color.is_some() {
        rows.extend(match Kind::of(m) {
            Kind::Dielectric => &["base"][..],
            Kind::Clear => &["trans.color"][..],
            Kind::Metal => &["metal", "f0"][..],
        });
    }
    if v.roughness.is_some() {
        rows.push("rough");
    }
    let Some(sections) = params.get_mut("sections").and_then(|s| s.as_array_mut()) else { return };
    for row in sections.iter_mut().filter_map(|s| s.get_mut("rows").and_then(|r| r.as_array_mut())).flatten() {
        if row.get("id").and_then(|i| i.as_str()).is_some_and(|id| rows.contains(&id)) {
            if let Some(c) = row.get_mut("controls").and_then(|c| c.as_array_mut()) {
                c.push(c_note(fl!("mated-overridden"), "warn", fl!("mated-overridden-tip")));
            }
        }
    }
}

/// パラメータの行を元の値に戻す（行の id は param_json のもの）
fn reset_row(m: &mut Material, o: &Material, row: &str) {
    match row {
        "metal" => (m.conductor, m.base_color, m.specular_color) = (o.conductor, o.base_color, o.specular_color),
        "f0" => m.base_color = o.base_color,
        "f82" => m.specular_color = o.specular_color,
        "substance" | "ior" | "abbe" => Optics::of(o).apply(m),
        "base" => m.base_color = o.base_color,
        "trans.color" => m.transmission_color = o.transmission_color,
        "trans.depth" => m.transmission_depth = o.transmission_depth,
        "rough" => m.specular_roughness = o.specular_roughness,
        "sss.weight" => m.subsurface_weight = o.subsurface_weight,
        "sss.color" => m.subsurface_color = o.subsurface_color,
        "sss.radius" => m.subsurface_radius = o.subsurface_radius,
        "coat.weight" => m.coat_weight = o.coat_weight,
        "coat.rough" => m.coat_roughness = o.coat_roughness,
        "coat.ior" => m.coat_ior = o.coat_ior,
        "coat.color" => m.coat_color = o.coat_color,
        "pattern" => (m.bump_pattern, m.bump_strength, m.bump_scale, m.roughness_variation) = (o.bump_pattern, o.bump_strength, o.bump_scale, o.roughness_variation),
        "pattern.strength" => m.bump_strength = o.bump_strength,
        "pattern.scale" => m.bump_scale = o.bump_scale,
        "pattern.rough" => m.roughness_variation = o.roughness_variation,
        "category" => m.category = o.category,
        _ => {}
    }
}

impl MaterialEditor {
    /// --web-ui：画像の置き場をつなぐ（見本の egui のテクスチャも作るので、egui も）
    pub fn attach_web(&mut self, ctx: &egui::Context, images: &crate::web_ui::Images) {
        self.egui.get_or_insert_with(|| ctx.clone());
        if self.web.is_none() {
            self.web = Some(images.clone());
        }
    }

    /// 毎フレーム（--web-ui）：選んだ部品に合わせた片付けと自動の選択（egui の ui と同じ）、止まった編集の書き込み
    pub fn web_update(&mut self, lib: &MaterialLibrary, part: Option<&PartInfo>) -> Vec<Action> {
        let mut actions = Vec::new();
        if !self.open {
            return actions;
        }
        self.sync_selection(part);
        if let Some(p) = part.filter(|p| !p.rows.is_empty()) {
            if p.rows.len() == 1 && self.target.is_none() {
                self.target = Some(p.rows[0].key.clone());
            }
            // 部品を選んだら、割り当て先の部位（全部位なら最初の部位）の材質を下に出す
            if self.browsed_for.as_deref() != Some(p.reference.as_str()) {
                self.browsed_for = Some(p.reference.clone());
                let row = p.rows.iter().find(|r| self.target.as_ref() == Some(&r.key)).unwrap_or(&p.rows[0]);
                let material = self.row_material(&p.reference, row);
                if self.naming.is_none() {
                    self.select_browsing(lib, Variant::parse(&material).0, &mut actions);
                }
            }
        }
        self.flush(part, &mut actions, false);
        actions
    }

    /// 見本の URL（まだ無ければ null にして、描くように頼む）
    fn thumb(&mut self, key: &str) -> Value {
        if self.thumbs.contains_key(key) {
            if let Some(u) = self.thumb_urls.get(key) {
                return Value::String(u.clone());
            }
            // egui で描いた見本（画像の置き場に無い）：描き直す
            self.thumbs.remove(key);
        }
        if !self.wanted.iter().any(|w| w == key) {
            self.wanted.push(key.to_string());
        }
        Value::Null
    }

    fn web_item(&mut self, lib: &MaterialLibrary, name: &str, current: bool) -> Value {
        let user = is_user_material(name);
        let mut tip = format!("{}\n{}", material_label(lib, name), name);
        if let Some(m) = lib.find(name) {
            tip += &format!("\n{}", material_details(m));
        }
        if user {
            tip += &format!("\n{}", fl!("mated-user-material"));
        }
        tip += &format!("\n{}", fl!("mated-tile-tip"));
        json!({
            "name": name, "label": material_label(lib, name), "user": user, "current": current,
            "selected": self.browsing.as_deref() == Some(name),
            "thumb": self.thumb(name),
            "brief": self.material_for(lib, name).map(|m| material_brief(&m)).unwrap_or_default(),
            "tip": tip,
        })
    }

    /// ページに渡す状態（JSON）。scope は書き込む先の選択肢（ビューアの設定パネルと同じ部品）
    pub fn web_json(&mut self, lib: &MaterialLibrary, part: Option<&PartInfo>, scope: Value) -> String {
        self.wanted.clear();
        let query = self.search.trim().to_lowercase();
        let current: HashSet<String> = part.map(|p| p.rows.iter().filter(|r| self.target.as_ref().is_none_or(|t| *t == r.key)).map(|r| Variant::parse(&self.row_material(&p.reference, r)).0.to_string()).collect()).unwrap_or_default();
        let mut cats = Vec::new();
        for (cat, names) in grouped_names(lib) {
            let shown: Vec<&String> = names.iter().filter(|n| query.is_empty() || n.to_lowercase().contains(&query) || material_label(lib, n).to_lowercase().contains(&query)).collect();
            if shown.is_empty() {
                continue;
            }
            let (more, main): (Vec<&String>, Vec<&String>) = shown.iter().partition(|n| query.is_empty() && RARE.contains(&n.as_str()));
            let more_open = more.iter().any(|n| current.contains(n.as_str()) || self.browsing.as_deref() == Some(n.as_str()));
            let main: Vec<Value> = main.iter().map(|n| self.web_item(lib, n, current.contains(n.as_str()))).collect();
            let more_items: Vec<Value> = more.iter().map(|n| self.web_item(lib, n, current.contains(n.as_str()))).collect();
            cats.push(json!({
                "id": cat.as_str(), "label": category_label(cat), "count": shown.len(),
                "items": main, "more": more_items, "moreLabel": fl!("mated-more", n = more.len()), "moreOpen": more_open,
            }));
        }
        let part_v = part.map(|p| self.web_part(lib, p, scope)).unwrap_or(Value::Null);
        let details = self.web_details(lib, part);
        json!({
            "title": fl!("mated-title"),
            "closeTip": fl!("panel-close-tip"),
            "search": self.search, "searchLabel": fl!("mated-search"),
            "listView": self.list_view, "viewList": fl!("mated-view-list"), "viewListTip": fl!("mated-view-list-tip"), "viewGrid": fl!("mated-view-grid"),
            "categories": cats,
            "part": part_v, "noPart": fl!("mated-no-part"),
            "details": details,
            "menu": { "assign": self.assign_label(part), "canAssign": part.is_some_and(|p| !p.rows.is_empty()), "duplicate": fl!("mated-duplicate"), "rename": fl!("mated-rename"), "delete": fl!("mated-delete") },
        })
        .to_string()
    }

    /// 右上：選んだ部品のプレビュー・割り当て先の部位・部位の調整
    fn web_part(&mut self, lib: &MaterialLibrary, p: &PartInfo, scope: Value) -> Value {
        let (preview, error) = match self.part.as_ref().filter(|v| v.reference == p.reference) {
            Some(v) => (v.url.clone(), v.error.clone()),
            None => (None, String::new()),
        };
        let multi = p.rows.len() > 1;
        let rows: Vec<(PartRow, String)> = p.rows.iter().map(|r| (r.clone(), self.row_material(&p.reference, r))).collect();
        let rows_v: Vec<Value> = rows
            .iter()
            .map(|(r, material)| {
                let shown = if r.key.is_empty() { fl!("part-whole") } else { r.key.strip_prefix("kicad:").unwrap_or(&r.key).to_string() };
                json!({
                    "key": r.key, "shown": shown, "material": material, "label": material_label(lib, material),
                    "thumb": self.thumb(material), "on": self.target.as_ref() == Some(&r.key) || !multi,
                })
            })
            .collect();
        let adjust = self.target.clone().and_then(|t| rows.iter().find(|(r, _)| r.key == t).cloned()).map(|(_, material)| {
            let (base, v) = Variant::parse(&material);
            let Some(base_m) = lib.find(base).cloned().or_else(|| self.material_for(lib, base)) else {
                return json!({ "title": fl!("mated-adjust"), "unknown": fl!("mated-unknown-material", name = material.as_str()) });
            };
            let measured = base_m.conductor.is_some();
            let start = adjust_start_color(&base_m);
            json!({
                "title": fl!("mated-adjust"),
                "note": fl!("mated-adjust-note"),
                "color": { "on": v.color.is_some(), "value": srgb_hex(v.color.unwrap_or(srgb8(start))), "label": fl!("mated-color"),
                           "tip": if measured { fl!("mated-color-measured-tip") } else { fl!("mated-color-tip") },
                           "measured": if measured && v.color.is_some() { fl!("mated-color-measured") } else { String::new() } },
                "rough": { "on": v.roughness.is_some(), "value": v.roughness.unwrap_or(base_m.specular_roughness), "label": fl!("mated-roughness"), "tip": fl!("mated-roughness-tip") },
                "save": (v.color.is_some() || v.roughness.is_some()).then(|| json!({ "label": fl!("mated-save-adjusted"), "tip": fl!("mated-save-adjusted-tip") })),
            })
        });
        json!({
            "reference": p.reference, "hint": fl!("mated-preview-hint"), "preview": preview, "error": error,
            "placeholder": p.rows.is_empty().then(|| fl!("mated-placeholder")),
            "scope": scope,
            "targetLabel": fl!("mated-target"), "multi": multi,
            "whole": { "label": fl!("part-whole"), "on": self.target.is_none() },
            "rows": rows_v,
            "adjust": adjust,
        })
    }

    /// 右下：選んだ材質の説明・割り当て・複製・削除と、パラメータ（名前を決めているときは、その欄）
    fn web_details(&mut self, lib: &MaterialLibrary, part: Option<&PartInfo>) -> Value {
        if let Some(n) = &self.naming {
            let error = (!valid_material_label(n.name.trim())).then(|| fl!("mated-name-invalid"));
            let renaming = n.rename.is_some();
            return json!({ "naming": {
                "title": if renaming { fl!("mated-rename-title") } else { fl!("mated-new-title") },
                "nameLabel": fl!("mated-new-name"), "name": n.name, "error": error,
                "create": if renaming { fl!("mated-rename-ok") } else { fl!("mated-create") }, "cancel": fl!("mated-cancel"),
            } });
        }
        let Some(name) = self.browsing.clone() else { return json!({ "hint": fl!("mated-choose-hint") }) };
        let Some(m) = self.material_for(lib, &name) else {
            self.browsing = None;
            return json!({ "hint": fl!("mated-choose-hint") });
        };
        let user = is_user_material(&name);
        let can_assign = part.is_some_and(|p| !p.rows.is_empty());
        let over = self.target_override(part).map(|(_, _, v)| v);
        let mark = |mut params: Value, shown: &Material| {
            if let Some(v) = over {
                mark_overridden(&mut params, shown, v);
            }
            params
        };
        let edit = if user {
            self.user_edit.as_ref().filter(|u| u.name == name).map(|u| {
                json!({
                    "title": fl!("mated-edit-title"),
                    "undo": { "label": fl!("mated-undo"), "enabled": !u.undo.is_empty(), "tip": "Ctrl+Z" },
                    "redo": { "label": fl!("mated-redo"), "enabled": !u.redo.is_empty(), "tip": "Ctrl+Shift+Z / Ctrl+Y" },
                    "params": mark(param_json(lib, &u.material, &u.origin), &u.material),
                })
            })
        } else {
            Some(json!({ "title": fl!("mated-edit-title-builtin"), "note": fl!("mated-builtin-autocopy"), "params": mark(param_json(lib, &m, &m), &m) }))
        };
        let override_v = self.override_text(lib, part).map(|text| {
            json!({
                "text": text,
                "adopt": { "label": fl!("mated-override-adopt"), "tip": fl!("mated-override-adopt-tip") },
                "clear": { "label": fl!("mated-override-clear"), "tip": fl!("mated-override-clear-tip") },
            })
        });
        json!({
            "name": name, "label": material_label(lib, &name), "sub": format!("{name}  ·  {}", category_label(m.category())),
            "desc": material_details(&m), "user": user, "userNote": fl!("mated-user-material"),
            "thumb": self.thumb(&name),
            "assign": { "label": self.assign_label(part), "enabled": can_assign, "tip": fl!("mated-assign-tip") },
            "duplicate": { "label": fl!("mated-duplicate"), "tip": fl!("mated-duplicate-tip") },
            "rename": user.then(|| json!({ "label": fl!("mated-rename"), "tip": fl!("mated-rename-tip") })),
            "delete": (user && !self.confirm_delete).then(|| fl!("mated-delete")),
            "confirm": (user && self.confirm_delete).then(|| json!({ "text": fl!("mated-delete-confirm"), "delete": fl!("mated-delete"), "cancel": fl!("mated-cancel") })),
            "notice": self.notice, "error": self.error,
            "override": override_v,
            "edit": edit,
        })
    }

    /// 割り当て先の部位の調整（色・粗さ）を変える
    fn web_adjust(&mut self, lib: &MaterialLibrary, part: Option<&PartInfo>, actions: &mut Vec<Action>, f: impl FnOnce(&mut Variant, &Material)) {
        let (Some(p), Some(t)) = (part, self.target.clone()) else { return };
        let Some(row) = p.rows.iter().find(|r| r.key == t) else { return };
        let material = self.row_material(&p.reference, row);
        let (base, mut v) = Variant::parse(&material);
        let Some(base_m) = lib.find(base).cloned().or_else(|| self.material_for(lib, base)) else { return };
        let before = v;
        f(&mut v, &base_m);
        if v != before {
            let base = base.to_string();
            self.adjust_to(part, &p.reference, &t, v.name(&base), actions);
        }
    }

    /// ページからの操作（id は "mated." を除いたもの）
    pub fn web_apply(&mut self, lib: &MaterialLibrary, part: Option<&PartInfo>, id: &str, v: &Value) -> Vec<Action> {
        let mut actions = Vec::new();
        let s = v.as_str().unwrap_or("").to_string();
        let can_assign = part.is_some_and(|p| !p.rows.is_empty());
        match id {
            "close" => self.open = false,
            "search" => self.search = s,
            "view" => self.list_view = s == "list",
            "select" => self.select_browsing(lib, &s, &mut actions),
            "dblclick" => {
                self.select_browsing(lib, &s, &mut actions);
                if can_assign {
                    self.assign(&s, part, self.target.clone(), &mut actions);
                }
            }
            "assign" => {
                if let Some(name) = self.browsing.clone().filter(|_| can_assign) {
                    self.assign(&name, part, self.target.clone(), &mut actions);
                }
            }
            "assign_menu" => {
                if can_assign {
                    self.select_browsing(lib, &s, &mut actions);
                    self.assign(&s, part, self.target.clone(), &mut actions);
                }
            }
            // 見本を落とした：{ name, target }（target が無ければ割り当て先の部位、"" 以外の文字なら その部位、null なら全部位）
            "drop" => {
                let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                if name.is_empty() || !can_assign {
                    return actions;
                }
                self.select_browsing(lib, &name, &mut actions);
                match v.get("target") {
                    Some(Value::String(k)) => {
                        self.target = Some(k.clone());
                        self.assign(&name, part, Some(k.clone()), &mut actions);
                    }
                    Some(Value::Null) => {
                        self.target = None;
                        self.assign(&name, part, None, &mut actions);
                    }
                    _ => self.assign(&name, part, self.target.clone(), &mut actions),
                }
            }
            "duplicate" => {
                let name = if s.is_empty() { self.browsing.clone().unwrap_or_default() } else { s };
                self.select_browsing(lib, &name, &mut actions);
                self.start_duplicate(lib, &name);
            }
            "rename" => {
                let name = if s.is_empty() { self.browsing.clone().unwrap_or_default() } else { s };
                if is_user_material(&name) {
                    self.select_browsing(lib, &name, &mut actions);
                    self.start_rename(lib, &name);
                }
            }
            "delete.ask" => {
                if !s.is_empty() {
                    self.select_browsing(lib, &s, &mut actions);
                }
                self.confirm_delete = true;
            }
            "delete.yes" => {
                if let Some(name) = self.browsing.clone() {
                    self.delete_user(&name, &mut actions);
                }
            }
            "delete.no" => self.confirm_delete = false,
            "whole" => self.target = None,
            // 部位の行を押した：割り当て先にして、その材質を下に出す
            "row" => {
                if let Some(p) = part {
                    if let Some(r) = p.rows.iter().find(|r| r.key == s) {
                        let material = self.row_material(&p.reference, r);
                        self.target = Some(s);
                        self.select_browsing(lib, Variant::parse(&material).0, &mut actions);
                    }
                }
            }
            "adj.color.on" => {
                let on = v.as_bool().unwrap_or(false);
                self.web_adjust(lib, part, &mut actions, |v, m| v.color = on.then(|| srgb8(adjust_start_color(m))));
            }
            "adj.color" => {
                if let Some(c) = parse_srgb(v) {
                    self.web_adjust(lib, part, &mut actions, |v, _| v.color = Some(c));
                }
            }
            "adj.rough.on" => {
                let on = v.as_bool().unwrap_or(false);
                self.web_adjust(lib, part, &mut actions, |v, m| v.roughness = on.then_some(m.specular_roughness));
            }
            "adj.rough" => {
                if let Some(r) = num(v) {
                    self.web_adjust(lib, part, &mut actions, |v, _| v.roughness = Some(r.clamp(0.0, 1.0)));
                }
            }
            "adj.save" => {
                if let (Some(p), Some(t)) = (part, self.target.clone()) {
                    if let Some(row) = p.rows.iter().find(|r| r.key == t) {
                        let material = self.row_material(&p.reference, row);
                        let (base, v) = Variant::parse(&material);
                        if let Some(base_m) = lib.find(base).cloned().or_else(|| self.material_for(lib, base)) {
                            self.start_save_adjusted(lib, &p.reference, &t, base, v, &base_m);
                        }
                    }
                }
            }
            "override.adopt" => self.adopt_override(lib, part, &mut actions),
            "override.clear" => self.clear_override(lib, part, &mut actions),
            "naming.name" => {
                if let Some(n) = &mut self.naming {
                    n.name = s;
                }
            }
            "naming.create" => {
                if self.naming.as_ref().is_some_and(|n| valid_material_label(n.name.trim())) {
                    self.finish_naming(lib, part, &mut actions);
                }
            }
            "naming.cancel" => self.naming = None,
            "undo" => self.undo_redo(false),
            "redo" => self.undo_redo(true),
            "reset" => {
                if let Some(name) = self.browsing.clone() {
                    self.edit_material(lib, &name, part, &mut actions, |m, o| reset_row(m, o, &s));
                }
            }
            // 部品のプレビュー：ドラッグで回し、ホイールで拡大、ダブルクリックで元の向き
            "orbit" => {
                if let Some(pv) = &mut self.part {
                    let (dx, dy) = (v.get("dx").and_then(num).unwrap_or(0.0), v.get("dy").and_then(num).unwrap_or(0.0));
                    pv.azimuth -= dx as f64 * 0.4;
                    pv.elevation = (pv.elevation + dy as f64 * 0.4).clamp(-89.0, 89.0);
                }
            }
            "zoom" => {
                if let (Some(pv), Some(d)) = (&mut self.part, num(v)) {
                    pv.zoom = (pv.zoom * 1.0015f32.powf(-d)).clamp(0.3, 8.0);
                }
            }
            "view_reset" => {
                if let Some(pv) = &mut self.part {
                    (pv.azimuth, pv.elevation, pv.zoom) = (45.0, 30.0, 1.0);
                }
            }
            _ => {
                if let Some(param) = id.strip_prefix("param.") {
                    if let Some(name) = self.browsing.clone() {
                        self.edit_material(lib, &name, part, &mut actions, |m, _| apply_param(lib, m, param, v));
                    }
                }
            }
        }
        actions
    }
}

/// 部位の色の調整の初めの色：材質の今の色（透明な材質なら透過の色）
pub(super) fn adjust_start_color(m: &Material) -> Vec3 {
    if m.transmission_weight > 0.0 && m.base_metalness < 0.5 {
        m.transmission_color
    } else {
        m.base_color
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_round_trip() {
        assert_eq!(parse_srgb(&json!("#ff8000")), Some([255, 128, 0]));
        assert_eq!(parse_srgb(&json!("ff8000")), None);
        assert_eq!(srgb_hex([1, 2, 255]), "#0102ff");
        assert_eq!(swatch_path("a+b"), "swatch/612b62");
    }

    #[test]
    fn params_apply_and_reset() {
        let lib = MaterialLibrary::builtin();
        let o = lib.find("plastic_abs").unwrap().clone();
        let mut m = o.clone();
        apply_param(&lib, &mut m, "rough", &json!(0.75));
        assert_eq!(m.specular_roughness, 0.75);
        apply_param(&lib, &mut m, "kind", &json!("metal"));
        assert!(m.base_metalness > 0.5);
        apply_param(&lib, &mut m, "conductor", &json!("Au"));
        assert!(m.conductor.is_some());
        reset_row(&mut m, &o, "rough");
        assert_eq!(m.specular_roughness, o.specular_roughness);
        // パネルの JSON が組み立てられる
        let v = param_json(&lib, &m, &o);
        assert_eq!(v["kind"]["value"], "metal");
    }
}
