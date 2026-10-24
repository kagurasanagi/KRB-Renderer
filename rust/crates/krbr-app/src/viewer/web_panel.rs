//! --web-ui の設定パネル：パネルの部品の並び（web/panel.html の krbr.panel に渡す JSON）を作り、パネルからの操作を反映する。
//! 中身は egui のパネル（App::panel・selection_panel）と同じ。部品は種類（t）・id・名前・今の値で表し、
//! ページはそれを汎用に描いて、値の変更（set）・ボタン（click）を id で返す

use serde_json::{json, Value};

use super::*;

fn slider(id: &str, label: String, value: f64, min: f64, max: f64, log: bool, digits: u32, suffix: &str) -> Value {
    json!({ "t": "slider", "id": id, "label": label, "value": value, "min": min, "max": max, "log": log, "digits": digits, "suffix": suffix })
}

fn tip(mut v: Value, tip: String) -> Value {
    v["tip"] = Value::String(tip);
    v
}

fn check(id: &str, label: String, value: bool) -> Value {
    json!({ "t": "check", "id": id, "label": label, "value": value })
}

fn radio(id: &str, value: &str, options: Vec<(&str, String)>) -> Value {
    json!({ "t": "radio", "id": id, "value": value, "options": options.into_iter().map(|(v, l)| json!({ "v": v, "label": l })).collect::<Vec<_>>() })
}

/// 選択肢。groups：(見出し（空なら無し）, [(値, 名前, 説明)])
fn select(id: &str, label: Option<String>, value: &str, groups: Vec<(String, Vec<(String, String, String)>)>) -> Value {
    let groups: Vec<Value> = groups
        .into_iter()
        .map(|(g, opts)| {
            let options: Vec<Value> = opts.into_iter().map(|(v, l, t)| json!({ "v": v, "label": l, "tip": t })).collect();
            if g.is_empty() {
                json!({ "options": options })
            } else {
                json!({ "label": g, "options": options })
            }
        })
        .collect();
    json!({ "t": "select", "id": id, "label": label, "value": value, "groups": groups })
}

/// ボタンの並び：(id, 名前, 説明, 押せるか)
fn buttons(items: Vec<(&str, String, String, bool)>) -> Value {
    json!({ "t": "buttons", "items": items.into_iter().map(|(id, l, t, e)| json!({ "id": id, "label": l, "tip": t, "enabled": e })).collect::<Vec<_>>() })
}

fn note(text: String, tone: &str) -> Value {
    json!({ "t": "note", "text": text, "tone": tone })
}

fn kv(label: String, value: String) -> Value {
    json!({ "t": "kv", "label": label, "value": value })
}

fn row(items: Vec<Value>) -> Value {
    json!({ "t": "row", "items": items })
}

fn section(title: String, items: Vec<Value>) -> Value {
    json!({ "title": title, "items": items })
}

fn lens_model_name(m: LensModel) -> &'static str {
    match m {
        LensModel::Pinhole => "pinhole",
        LensModel::Thin => "thin",
        LensModel::Realistic => "realistic",
    }
}

fn scope_name(s: AssignScope) -> &'static str {
    match s {
        AssignScope::Part => "part",
        AssignScope::ModelProject => "model",
        AssignScope::ModelGlobal => "global",
    }
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.strip_prefix('#')?;
    if s.len() != 6 {
        return None;
    }
    let b = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some([b(0)?, b(2)?, b(4)?])
}

/// 部位のキー（部位の行の part_keys の最初の 1 つ。空なら部品全体）
fn part_key(r: &MaterialAssignment) -> String {
    r.part_keys.split(' ').next().unwrap_or("").to_string()
}

impl App {
    /// 選んだ部品のこのインスタンスの部位の行（egui の selection_panel と同じ）
    fn selection_rows(&self, sel: &Selected) -> Vec<MaterialAssignment> {
        sel.find_instance(&self.prep.scene, &self.prep.board)
            .and_then(|i| match self.prep.scene.instances[i].owner {
                InstanceOwner::Model { assignments, .. } => {
                    let a = &self.prep.scene.assignments;
                    let start = assignments as usize;
                    let spec = &a.get(start)?.model_spec;
                    Some(a[start..].iter().take_while(|r| r.reference == sel.reference && &r.model_spec == spec).cloned().collect())
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    /// 部位の今の材質名（発光の色・明るさを編集中なら、その値）
    fn part_material(&self, sel: &Selected, r: &MaterialAssignment) -> String {
        match self.emit_edit.as_ref().filter(|e| e.reference == sel.reference && e.part == part_key(r)) {
            Some(e) => e.material.clone(),
            None => r.material.clone(),
        }
    }

    fn material_groups(&self) -> Vec<Value> {
        material_editor::grouped_names(&self.library)
            .into_iter()
            .map(|(cat, names)| json!({ "label": material_editor::category_label(cat), "options": names.iter().map(|n| json!({ "v": n, "label": material_editor::material_label(n) })).collect::<Vec<_>>() }))
            .collect()
    }

    fn selection_section(&self) -> Value {
        let mut items = Vec::new();
        let Some(sel) = self.selected.clone() else {
            items.push(note(fl!("sel-none-hint"), "muted"));
            return section(fl!("sel-title"), items);
        };
        items.push(row(vec![
            json!({ "t": "note", "text": sel.reference, "tone": "accent", "strong": true }),
            buttons(vec![("sel.deselect", fl!("sel-deselect"), String::new(), true)]),
            tip(check("sel.outline", fl!("sel-outline"), self.show_outline), fl!("sel-outline-tip")),
        ]));
        items.push(buttons(vec![("sel.editor", fl!("sel-open-editor"), fl!("sel-open-editor-tip"), true)]));
        let fp = self.prep.board.footprints.iter().find(|f| f.reference == sel.reference);
        let lib_id = fp.map(|f| f.lib_id.clone()).unwrap_or_default();
        items.push(tip(note(lib_id.clone(), "muted"), lib_id));
        if self.prep.cfg.lens.model == LensModel::Realistic {
            items.push(note(fl!("sel-no-outline"), "muted"));
        }
        let Some(mi) = sel.model else {
            let missing: Vec<_> = self.prep.scene.missing.iter().filter(|m| m.reference == sel.reference).collect();
            if missing.is_empty() {
                items.push(note(fl!("sel-meshing"), "warn"));
            } else {
                items.push(note(fl!("sel-missing"), "warn"));
                for m in missing {
                    items.push(note(fl!("sel-missing-model", spec = m.spec.as_str(), resolved = m.resolved.as_str(), reason = m.reason.as_str()), "muted"));
                }
                items.push(note(fl!("sel-missing-hint"), "muted"));
            }
            return section(fl!("sel-title"), items);
        };
        let model_file = fp.and_then(|f| f.models.get(mi)).map(|m| Path::new(&m.path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()).unwrap_or_default();
        items.push(tip(note(model_file.clone(), "muted"), model_file));
        let scope = self.assign_scope;
        items.push(json!({ "t": "radio", "id": "sel.scope", "value": scope_name(scope), "options": [
            { "v": "part", "label": fl!("scope-part"), "tip": fl!("scope-part-tip") },
            { "v": "model", "label": fl!("scope-model"), "tip": fl!("scope-model-tip") },
            { "v": "global", "label": fl!("scope-all-boards"), "tip": fl!("scope-all-boards-tip") },
        ] }));
        let rows = self.selection_rows(&sel);
        let decided_by = |stage: MaterialStage| rows.iter().any(|r| r.stage() == Some(stage));
        if scope != AssignScope::Part && decided_by(MaterialStage::ProjectRef) {
            items.push(note(fl!("sel-shadowed-ref"), "warn"));
        } else if scope == AssignScope::ModelGlobal && decided_by(MaterialStage::ProjectModel) {
            items.push(note(fl!("sel-shadowed-project"), "warn"));
        }
        let part_rows: Vec<Value> = rows
            .iter()
            .map(|r| {
                let key = part_key(r);
                let material = self.part_material(&sel, r);
                let (base, variant) = Variant::parse(&material);
                let shown = if key.is_empty() { fl!("part-whole") } else { key.strip_prefix("kicad:").unwrap_or(&key).to_string() };
                let mut v = json!({
                    "key": key, "shown": shown, "tip": fl!("part-source-tip", keys = r.part_keys.as_str(), source = r.source.as_str()),
                    "base": base, "pattern": !variant.smooth, "emit": variant.emission.is_some(),
                });
                if let Some(e) = variant.emission {
                    v["emitColor"] = Value::String(hex(e.srgb));
                    v["emitLum"] = json!(e.luminance);
                }
                v
            })
            .collect();
        let all_smooth = !rows.is_empty() && rows.iter().all(|r| split_variant(&r.material).1);
        items.push(json!({
            "t": "parts", "rows": part_rows, "allPattern": !all_smooth, "groups": self.material_groups(),
            "labels": {
                "pattern": fl!("part-pattern"), "patternTip": fl!("part-pattern-tip"), "emit": fl!("part-emit"), "emitTip": fl!("part-emit-tip"),
                "emitColorTip": fl!("emit-color-tip"), "emitLumTip": fl!("emit-luminance-tip"),
                "all": fl!("parts-all"), "chooseAll": fl!("parts-choose-all"), "patternAllTip": fl!("parts-pattern-all-tip"),
            },
        }));
        let mut sources: Vec<&str> = rows.iter().map(|r| r.source.as_str()).collect();
        sources.dedup();
        if let [one] = sources.as_slice() {
            items.push(note(fl!("sel-source", source = one.to_string()), "muted"));
        } else if !sources.is_empty() {
            items.push(note(fl!("sel-source-varies"), "muted"));
        }
        items.push(buttons(vec![("sel.clear_override", fl!("sel-clear-override"), fl!("sel-clear-override-tip"), true)]));
        if !self.material_error.is_empty() {
            items.push(note(self.material_error.clone(), "error"));
        }
        section(fl!("sel-title"), items)
    }

    /// 設定パネルの JSON（egui の App::panel と同じ項目）
    pub(super) fn panel_json(&self) -> String {
        let st = &self.st;
        let cfg = &self.prep.cfg;
        let cam = self.view_camera();
        let mut sections = vec![self.selection_section()];

        // 環境マップ
        let mut env = Vec::new();
        let groups = self.env_list.iter().map(|g| (String::new(), g.iter().map(|p| (p.clone(), env_label(p), env_tip(p))).collect())).collect();
        let current = self.env_list.iter().flatten().find(|p| remember::same_hdr(p, &self.env_path)).cloned().unwrap_or_else(|| self.env_path.clone());
        env.push(row(vec![select("env.hdr", None, &current, groups), buttons(vec![("env.open", fl!("env-open"), fl!("env-open-tip"), self.open_dialog.is_none())])]));
        env.push(note(fl!("env-remember-hint"), "muted"));
        if let Some(job) = &self.env_job {
            env.push(note(fl!("env-loading", path = job.path.as_str()), "warn"));
        }
        if !self.env_error.is_empty() {
            env.push(note(self.env_error.clone(), "error"));
        }
        env.push(slider("env.intensity", fl!("env-intensity"), st.env_intensity as f64, 0.05, 8.0, true, 2, ""));
        env.push(slider("env.rotation", fl!("env-rotation"), st.env_rotation_deg as f64, -180.0, 180.0, false, 0, "°"));
        sections.push(section(fl!("panel-environment"), env));

        // 露出・グロー・ノイズ除去
        let mut ex = vec![
            slider("exposure.ev", "EV".into(), st.exposure as f64, -6.0, 6.0, false, 2, ""),
            buttons(vec![("exposure.auto", fl!("exposure-auto"), String::new(), true), ("exposure.zero", "0".into(), String::new(), true)]),
            tip(slider("glow.strength", fl!("glow"), st.glow as f64, 0.0, 4.0, false, 2, ""), fl!("glow-tip")),
        ];
        if st.glow > 0.0 {
            ex.push(tip(slider("glow.threshold", fl!("glow-threshold"), st.glow_threshold as f64, 0.5, 20.0, true, 1, ""), fl!("glow-threshold-tip")));
            ex.push(tip(slider("glow.spread", fl!("glow-spread"), st.glow_spread as f64, 0.0, 1.0, false, 2, ""), fl!("glow-spread-tip")));
        }
        let denoiser = self.gpu.as_ref().and_then(|g| g.denoiser.as_ref());
        let on_cpu = denoiser.is_some_and(|d| d.device_name() == "CPU");
        let mut dn = vec![check("denoise.on", fl!("denoise"), st.denoise)];
        if let Some(d) = denoiser {
            dn.push(note(format!("{}, {:.0} ms", d.device_name(), self.last_denoise_ms), "muted"));
            if on_cpu {
                dn.push(buttons(vec![("denoise.now", fl!("denoise-now"), String::new(), true)]));
            }
        } else if self.denoiser_tried && st.denoise {
            dn.push(note(fl!("denoise-unavailable"), "muted"));
        }
        ex.push(row(dn));
        if st.denoise {
            ex.push(tip(slider("denoise.strength", fl!("denoise-strength"), st.denoise_strength as f64, 0.0, 1.0, false, 2, ""), fl!("denoise-strength-tip")));
            if on_cpu {
                ex.push(note(fl!("denoise-cpu-hint"), "muted"));
            }
            if !self.denoise_error.is_empty() {
                ex.push(note(self.denoise_error.clone(), "error"));
            }
        }
        sections.push(section(fl!("panel-exposure"), ex));

        // 積分器
        let kind = cfg.integrator;
        sections.push(section(
            fl!("panel-integrator"),
            vec![
                radio("integrator", kind.as_str(), IntegratorKind::ALL.iter().map(|k| (k.as_str(), k.as_str().to_uppercase())).collect()),
                note(
                    match kind {
                        IntegratorKind::Pt => fl!("integrator-pt-hint"),
                        IntegratorKind::Bdpt => fl!("integrator-bdpt-hint"),
                        IntegratorKind::Vcm => fl!("integrator-vcm-hint"),
                    },
                    "muted",
                ),
            ],
        ));

        // レンズ
        let l = &cfg.lens;
        let mut lens = vec![radio("lens.model", lens_model_name(l.model), vec![("pinhole", fl!("lens-pinhole")), ("thin", fl!("lens-thin")), ("realistic", fl!("lens-realistic"))])];
        if l.model != LensModel::Pinhole {
            lens.push(slider("lens.f", fl!("lens-f-number"), l.f_number as f64, 0.7, 22.0, true, 1, ""));
            let focus = st.focus_point.map(|p| focus_distance_to(p, &cam)).unwrap_or(st.distance);
            lens.push(slider("lens.focus", fl!("lens-focus"), focus as f64, 5.0, 5000.0, true, 0, ""));
            lens.push(tip(buttons(vec![("lens.focus_auto", fl!("lens-focus-auto"), fl!("lens-focus-auto-tip"), st.focus_point.is_some())]), fl!("lens-focus-auto-tip")));
            lens.push(note(if st.focus_point.is_some() { fl!("lens-focus-point-hint") } else { fl!("lens-focus-hint") }, "muted"));
        }
        if l.model == LensModel::Thin {
            lens.push(slider("lens.focal", fl!("lens-focal-length"), l.focal_length_mm as f64, 10.0, 300.0, true, 0, ""));
            let mut blades = slider("lens.blades", fl!("lens-blades"), l.blades as f64, 0.0, 12.0, false, 0, "");
            blades["int"] = json!(true);
            lens.push(blades);
            if l.blades >= 3 {
                lens.push(slider("lens.blade_rot", fl!("lens-blade-rotation"), l.blade_rotation_deg as f64, 0.0, 360.0, false, 0, "°"));
                lens.push(slider("lens.blade_round", fl!("lens-blade-roundness"), l.blade_roundness as f64, 0.0, 1.0, false, 2, ""));
            }
            lens.push(check("lens.natural_vig", fl!("lens-natural-vignetting"), l.natural_vignetting));
            lens.push(slider("lens.optical_vig", fl!("lens-optical-vignetting"), l.optical_vignetting as f64, 0.0, 1.0, false, 2, ""));
            lens.push(slider("lens.abbe", fl!("lens-axial-ca"), l.abbe_number as f64, 0.0, 80.0, false, 0, ""));
            lens.push(slider("lens.lateral", fl!("lens-lateral-ca"), l.lateral_ca as f64, 0.0, 0.02, false, 3, ""));
            if (l.abbe_number > 0.0 || l.lateral_ca > 0.0) && !st.spectral {
                lens.push(note(fl!("lens-ca-hint"), "warn"));
            }
        }
        sections.push(section(fl!("panel-lens"), lens));

        // 色の計算
        sections.push(section(fl!("panel-color"), vec![radio("color.spectral", if st.spectral { "spectral" } else { "rgb" }, vec![("rgb", "RGB".into()), ("spectral", fl!("color-spectral"))])]));

        // 表面
        let presets = mask_presets();
        let mut mask_opts = vec![(String::new(), fl!("mask-as-configured"), String::new())];
        mask_opts.extend(presets.iter().map(|p| (p.name.to_string(), p.name.to_string(), i18n::get(&format!("mask-note-{}", p.name)).unwrap_or_else(|| p.note.to_string()))));
        let mask_value = st.mask_choice.map(|i| presets[i].name.to_string()).unwrap_or_default();
        let mut sub_groups = vec![(String::new(), vec![(String::new(), fl!("substrate-fr4"), String::new())])];
        for (cat, names) in material_editor::grouped_names(&self.library) {
            if matches!(cat, krbr_scene::Category::Glass | krbr_scene::Category::Gem | krbr_scene::Category::Ceramic) {
                sub_groups.push((material_editor::category_label(cat), names.iter().map(|n| (n.clone(), material_editor::material_label(n), n.clone())).collect()));
            }
        }
        let mut surface = vec![
            slider("surface.bump", fl!("bump-intensity"), st.bump_intensity as f64, 0.0, 2.0, false, 2, ""),
            select("surface.mask", None, &mask_value, vec![(String::new(), mask_opts)]),
            note(fl!("mask-hint"), "muted"),
            select("surface.substrate", Some(fl!("substrate")), &cfg.scene.substrate, sub_groups),
        ];
        let clear = self.library.find(&cfg.scene.substrate).is_some_and(|m| m.transmission_weight > 0.0);
        if clear && cfg.scene.mask_preset != "clear" {
            surface.push(note(fl!("substrate-clear-hint"), "muted"));
        }
        sections.push(section(fl!("panel-surface"), surface));

        // 表示
        let mut sizes = vec![("fit".to_string(), fl!("size-fit-window"), String::new())];
        sizes.extend(SIZE_PRESETS.iter().map(|&(w, h)| (format!("{w}x{h}"), size_label(w, h), String::new())));
        let size_value = st.size_mode.map(|(w, h)| format!("{w}x{h}")).unwrap_or_else(|| "fit".into());
        let mut size_groups = vec![(String::new(), sizes)];
        if let Some((w, h)) = st.size_mode.filter(|s| !SIZE_PRESETS.contains(s)) {
            // コマンドラインで決めた、一覧に無い大きさ
            size_groups[0].1.push((format!("{w}x{h}"), size_label(w, h), String::new()));
        }
        let ratios: Vec<(String, String, String)> = RATIOS.iter().map(|&r| (format!("{r}"), ratio_label(r), String::new())).collect();
        let mut ratio_groups = vec![(String::new(), ratios)];
        if !RATIOS.contains(&st.render_ratio) {
            ratio_groups[0].1.push((format!("{}", st.render_ratio), ratio_label(st.render_ratio), String::new()));
        }
        let mut e = 4u32;
        while (1u32 << e) < st.max_spp && e < 16 {
            e += 1;
        }
        let mut max_spp = slider("display.max_spp", fl!("max-spp"), e as f64, 4.0, 16.0, false, 0, "");
        max_spp["int"] = json!(true);
        max_spp["format"] = json!("pow2");
        sections.push(section(
            fl!("panel-display"),
            vec![
                select("display.size", None, &size_value, size_groups),
                note(fl!("size-hint"), "muted"),
                select("display.ratio", None, &format!("{}", st.render_ratio), ratio_groups),
                note(fl!("ratio-hint"), "muted"),
                radio("display.mode", display_mode_name(st.mode), vec![("image", fl!("mode-image")), ("albedo", fl!("mode-albedo")), ("normal", fl!("mode-normal"))]),
                max_spp,
            ],
        ));

        // カメラ
        let mut camera = vec![
            slider("camera.fov", fl!("camera-fov"), st.fov, 5.0, 90.0, false, 0, "°"),
            note(fl!("camera-angles", elevation = format!("{:.0}", st.elevation), azimuth = format!("{:.0}", st.azimuth.rem_euclid(360.0))), "muted"),
        ];
        if self.has_board() {
            let (cx, cy) = (self.prep.scene.board.center.x, self.prep.scene.board.center.y);
            let t = st.target;
            camera.push(kv(fl!("camera-web-target"), format!("x {}  y {}", format_length(t.x as f64 + cx), format_length(t.z as f64 + cy))));
            camera.push(kv(fl!("camera-web-distance"), format_length(st.distance as f64)));
            let p = cam.position;
            camera.push(kv(fl!("camera-web-position"), format!("x {}  y {}", format_length(p.x as f64 + cx), format_length(p.z as f64 + cy))));
            camera.push(kv(fl!("camera-web-height"), format_length(p.y as f64 - 0.5 * self.prep.scene.board.thickness)));
        }
        camera.push(buttons(vec![
            ("camera.fit", fl!("camera-fit"), fl!("toolbar-fit-tip"), self.has_board()),
            ("camera.reset", fl!("camera-reset"), String::new(), true),
            ("camera.save", fl!("camera-save"), String::new(), true),
        ]));
        camera.push(note(fl!("panel-help-mouse"), "muted"));
        camera.push(note(fl!("panel-help-toggle"), "muted"));
        sections.push(section(fl!("panel-camera"), camera));

        json!({ "title": fl!("panel-settings"), "closeTip": fl!("panel-close-tip"), "resizeTip": fl!("panel-resize-tip"), "sections": sections }).to_string()
    }

    /// パネルのボタン。パネルのものなら true
    pub(super) fn apply_panel_click(&mut self, id: &str) -> bool {
        let st = &mut self.st;
        match id {
            "panel.close" => st.show_panel = false,
            "env.open" => self.start_open_dialog(),
            "exposure.auto" => st.req.auto_exposure = true,
            "exposure.zero" => st.exposure = 0.0,
            "denoise.now" => st.req.denoise_now = true,
            "lens.focus_auto" => {
                st.focus_point = None;
                st.changed.frame = true;
            }
            "camera.fit" => st.req.fit_view = true,
            "camera.reset" => st.req.reset_view = true,
            "camera.save" => st.req.save = true,
            "sel.deselect" => {
                self.flush_emit_edit(true);
                self.emit_edit = None;
                self.selected = None;
                self.outline = None;
            }
            "sel.editor" => self.editor.open = true,
            "sel.clear_override" => self.apply_part_actions(vec![(String::new(), String::new(), true)]),
            _ => return false,
        }
        true
    }

    /// パネルの値の変更
    pub(super) fn apply_panel_set(&mut self, id: &str, v: &Value) {
        let f = v.as_f64().unwrap_or(0.0);
        let b = v.as_bool().unwrap_or(false);
        let s = v.as_str().unwrap_or("").to_string();
        let cam = self.view_camera();
        let st = &mut self.st;
        let l = &mut self.prep.cfg.lens;
        match id {
            "env.hdr" if !s.is_empty() => self.start_env_load(s, true),
            "env.intensity" => {
                st.env_intensity = f as f32;
                st.changed.frame = true;
            }
            "env.rotation" => {
                st.env_rotation_deg = f as f32;
                st.changed.frame = true;
            }
            "exposure.ev" => st.exposure = f as f32,
            "glow.strength" => st.glow = f as f32,
            "glow.threshold" => st.glow_threshold = f as f32,
            "glow.spread" => st.glow_spread = f as f32,
            "denoise.on" => st.denoise = b,
            "denoise.strength" => st.denoise_strength = f as f32,
            "integrator" => {
                if let Some(k) = IntegratorKind::ALL.iter().copied().find(|k| k.as_str() == s) {
                    if k != self.prep.cfg.integrator {
                        self.prep.cfg.integrator = k;
                        st.changed.integrator = true;
                    }
                }
            }
            "lens.model" => {
                l.model = match s.as_str() {
                    "thin" => LensModel::Thin,
                    "realistic" => LensModel::Realistic,
                    _ => LensModel::Pinhole,
                };
                st.changed.frame = true;
            }
            "lens.f" | "lens.focal" | "lens.blades" | "lens.blade_rot" | "lens.blade_round" | "lens.natural_vig" | "lens.optical_vig" | "lens.abbe" | "lens.lateral" => {
                match id {
                    "lens.f" => l.f_number = f as f32,
                    "lens.focal" => l.focal_length_mm = f as f32,
                    "lens.blades" => l.blades = f.round() as i32,
                    "lens.blade_rot" => l.blade_rotation_deg = f as f32,
                    "lens.blade_round" => l.blade_roundness = f as f32,
                    "lens.natural_vig" => l.natural_vignetting = b,
                    "lens.optical_vig" => l.optical_vignetting = f as f32,
                    "lens.abbe" => l.abbe_number = f as f32,
                    _ => l.lateral_ca = f as f32,
                }
                st.changed.frame = true;
            }
            "lens.focus" => {
                st.focus_point = Some(cam.position + cam.forward * f as f32);
                st.changed.frame = true;
            }
            "color.spectral" => {
                let spectral = s == "spectral";
                if spectral != st.spectral {
                    st.spectral = spectral;
                    st.changed.frame = true;
                }
            }
            "surface.bump" => {
                st.bump_intensity = f as f32;
                st.changed.materials = true;
            }
            "surface.mask" => {
                let choice = mask_presets().iter().position(|p| p.name == s);
                if choice != st.mask_choice {
                    st.mask_choice = choice;
                    st.changed.mask = true;
                }
            }
            "surface.substrate" => {
                if s != self.prep.cfg.scene.substrate {
                    self.prep.cfg.scene.substrate = s;
                    update_substrate_material(&mut self.prep.scene, &self.prep.cfg.scene, &self.library);
                    st.changed.materials = true;
                    self.scene_epoch += 1;
                }
            }
            "display.size" => {
                let mode = s.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                if mode != st.size_mode {
                    st.size_mode = mode;
                    st.req.fit_window = mode.is_some();
                }
            }
            "display.ratio" => {
                if let Ok(r) = s.parse::<f32>() {
                    if r != st.render_ratio {
                        st.render_ratio = r;
                        st.req.fit_window = st.size_mode.is_some();
                    }
                }
            }
            "display.mode" => st.mode = display_mode_of(&s),
            "display.max_spp" => st.max_spp = 1 << (f.round() as u32).clamp(4, 16),
            "camera.fov" => {
                st.fov = f;
                st.changed.frame = true;
            }
            "sel.outline" => self.show_outline = b,
            "sel.scope" => {
                self.assign_scope = match s.as_str() {
                    "model" => AssignScope::ModelProject,
                    "global" => AssignScope::ModelGlobal,
                    _ => AssignScope::Part,
                }
            }
            "part.material" | "part.pattern" | "part.emit" | "part.emit_color" | "part.emit_lum" | "parts.all" | "parts.pattern_all" => self.apply_part_set(id, v),
            _ => krbr_core::debug!("web UI: unknown panel control {id}"),
        }
    }

    /// 部位の材質の表の操作（egui の selection_panel と同じ書き込み方）
    fn apply_part_set(&mut self, id: &str, v: &Value) {
        let Some(sel) = self.selected.clone() else { return };
        let rows = self.selection_rows(&sel);
        let part = v.get("part").and_then(|p| p.as_str()).unwrap_or("").to_string();
        let row = rows.iter().find(|r| part_key(r) == part);
        let mut actions: Vec<(String, String, bool)> = Vec::new();
        match (id, row) {
            ("part.material", Some(r)) => {
                let Some(m) = v.get("material").and_then(|m| m.as_str()) else { return };
                let material = self.part_material(&sel, r);
                let (_, variant) = Variant::parse(&material);
                // 材質を選び直しても、模様のあり・なしと発光は保つ（色・粗さの調整は前の材質に合わせたものなので外す）
                actions.push((part, with_emission(&with_smooth(m, variant.smooth), variant.emission), false));
            }
            ("part.pattern", Some(r)) => {
                let on = v.get("on").and_then(|b| b.as_bool()).unwrap_or(true);
                actions.push((part, with_smooth(&self.part_material(&sel, r), !on), false));
            }
            ("part.emit", Some(r)) => {
                let on = v.get("on").and_then(|b| b.as_bool()).unwrap_or(false);
                let e = on.then(|| Emission { srgb: emission_color_of_part(&r.part_keys), luminance: DEFAULT_EMISSION_LUMINANCE });
                actions.push((part, with_emission(&self.part_material(&sel, r), e), false));
            }
            ("part.emit_color" | "part.emit_lum", Some(r)) => {
                // 色・明るさは止まってから書き込む（egui と同じく EmitEdit で）
                let material = self.part_material(&sel, r);
                let (_, variant) = Variant::parse(&material);
                let Some(mut e) = variant.emission else { return };
                if id == "part.emit_color" {
                    match v.get("color").and_then(|c| c.as_str()).and_then(parse_hex) {
                        Some(c) => e.srgb = c,
                        None => return,
                    }
                } else {
                    e.luminance = v.get("value").and_then(|x| x.as_f64()).unwrap_or(e.luminance as f64).clamp(0.0, 1.0e6).round() as f32;
                }
                if self.emit_edit.as_ref().is_some_and(|ed| ed.part != part || ed.reference != sel.reference) {
                    self.flush_emit_edit(true);
                }
                self.emit_edit = Some(EmitEdit { reference: sel.reference.clone(), part, material: with_emission(&material, Some(e)), changed_at: Instant::now(), written: false });
                return;
            }
            ("parts.all", _) => {
                let Some(m) = v.as_str() else { return };
                let all_smooth = !rows.is_empty() && rows.iter().all(|r| split_variant(&r.material).1);
                actions.push((String::new(), with_smooth(m, all_smooth), false));
            }
            ("parts.pattern_all", _) => {
                let on = v.as_bool().unwrap_or(true);
                for r in &rows {
                    actions.push((part_key(r), with_smooth(&r.material, !on), false));
                }
            }
            _ => return,
        }
        self.apply_part_actions(actions);
    }

    /// 部位の材質を書き込む（egui の selection_panel の終わりと同じ）
    fn apply_part_actions(&mut self, actions: Vec<(String, String, bool)>) {
        if actions.is_empty() {
            return;
        }
        if self.emit_edit.as_ref().is_some_and(|e| !actions.iter().any(|(p, _, _)| *p == e.part)) {
            self.flush_emit_edit(true);
        }
        self.emit_edit = None;
        for (part, m, clear) in actions {
            self.assign_material(&part, &m, clear);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_and_controls() {
        assert_eq!(hex([255, 128, 0]), "#ff8000");
        assert_eq!(parse_hex("#ff8000"), Some([255, 128, 0]));
        assert_eq!(parse_hex("ff8000"), None);
        let s = slider("a", "A".into(), 1.0, 0.0, 2.0, false, 2, "°");
        assert_eq!((s["t"].as_str(), s["suffix"].as_str()), (Some("slider"), Some("°")));
        let sel = select("b", None, "x", vec![(String::new(), vec![("x".into(), "X".into(), String::new())])]);
        assert!(sel["groups"][0].get("label").is_none());
    }
}
