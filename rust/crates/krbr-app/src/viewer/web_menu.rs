//! --web-ui のメニュー（ファイル・表示・設定・ヘルプ）：ボタン列の上端の見出しを押すと、その下にプルダウン（web/menu.html）を出す。
//! 項目は egui のメニュー（App::menu_bar）と同じ。項目は id・名前・ショートカット・チェック・サブメニューで表し、
//! 選んだ項目は "menu:<id>" のクリックで返ってくる

use serde_json::{json, Value};

use super::*;

/// メニューの見出し（id, 名前）
pub(super) fn menu_heads() -> Vec<Value> {
    [("file", fl!("menu-file")), ("view", fl!("menu-view")), ("settings", fl!("menu-settings")), ("help", fl!("menu-help"))].into_iter().map(|(id, label)| json!({ "id": id, "label": label })).collect()
}

fn item(id: &str, label: String) -> Value {
    json!({ "id": id, "label": label })
}

fn key(mut v: Value, shortcut: &str) -> Value {
    v["shortcut"] = Value::String(shortcut.into());
    v
}

fn checked(mut v: Value, on: bool) -> Value {
    v["checked"] = Value::Bool(on);
    v
}

fn enabled(mut v: Value, on: bool) -> Value {
    v["enabled"] = Value::Bool(on);
    v
}

fn tip(mut v: Value, tip: String) -> Value {
    v["tip"] = Value::String(tip);
    v
}

fn sub(label: String, items: Vec<Value>) -> Value {
    json!({ "label": label, "sub": items })
}

fn sep() -> Value {
    json!({ "sep": true })
}

fn note(text: String) -> Value {
    json!({ "note": text })
}

impl App {
    /// 「KRB Renderer について」を開く（開き直すたびに、最初のタブにして新しい版を確かめ直す）
    pub(super) fn open_about(&mut self) {
        self.about_open = true;
        self.about_json = None;
        if let Some(w) = &mut self.web {
            w.call(crate::web_ui::Pane::About, "reopen");
        }
    }

    /// 「最近使った基板」の項目（名前・フォルダ。無くなったファイルは押せない）
    fn recent_menu(&self) -> Vec<Value> {
        if self.recent_boards.is_empty() {
            return vec![enabled(item("recent.none", fl!("menu-recent-empty")), false)];
        }
        let mut v: Vec<Value> = self
            .recent_boards
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let name = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let exists = p.is_file();
                let it = tip(item(&format!("recent.{i}"), name), if exists { p.display().to_string() } else { fl!("menu-recent-missing", path = p.display().to_string()) });
                enabled(it, exists)
            })
            .collect();
        v.push(sep());
        v.push(item("recent.clear", fl!("menu-recent-clear")));
        v
    }

    /// 開いたメニューの項目（web/menu.html の krbr.menu に渡す JSON）
    pub(super) fn menu_json(&self, menu: &str) -> String {
        let st = &self.st;
        let board = self.has_board();
        let items = match menu {
            "file" => {
                let now = board_file::location();
                vec![
                    tip(key(item("open", fl!("menu-open-board")), "O"), fl!("menu-open-board-tip")),
                    sub(fl!("menu-recent"), self.recent_menu()),
                    sep(),
                    tip(key(item("save", fl!("menu-save-image")), "S"), fl!("menu-save-image-tip")),
                    tip(item("save_png", fl!("menu-save-png")), fl!("menu-save-png-tip")),
                    tip(item("save_exr", fl!("menu-save-exr")), fl!("menu-save-exr-tip")),
                    sep(),
                    sub(
                        fl!("menu-settings-location"),
                        vec![
                            tip(checked(item("loc.next", fl!("loc-next-to-board")), now == board_file::Location::NextToBoard), fl!("loc-next-to-board-tip")),
                            tip(checked(item("loc.config", fl!("loc-config-dir")), now == board_file::Location::ConfigDir), fl!("loc-config-dir-tip")),
                            note(fl!("loc-hint")),
                        ],
                    ),
                    tip(item("reveal", fl!("menu-reveal-settings")), fl!("menu-reveal-settings-tip")),
                    sep(),
                    key(item("quit", fl!("menu-quit")), "Esc"),
                ]
            }
            "view" => {
                let mut size = vec![checked(item("size.fit", fl!("size-fit-window")), st.size_mode.is_none())];
                size.extend(SIZE_PRESETS.iter().map(|&(w, h)| checked(item(&format!("size.{w}x{h}"), size_label(w, h)), st.size_mode == Some((w, h)))));
                let ratio = RATIOS.iter().enumerate().map(|(i, &r)| checked(item(&format!("ratio.{i}"), ratio_label(r)), st.render_ratio == r)).collect();
                vec![
                    key(checked(item("panel", fl!("menu-settings-panel")), st.show_panel), "F1 / Tab"),
                    key(checked(item("editor", fl!("menu-material-editor")), self.editor.open), "M"),
                    key(checked(item("pause", fl!("menu-pause")), st.paused), "P"),
                    checked(item("denoise", fl!("denoise")), st.denoise),
                    sep(),
                    enabled(item("top", fl!("menu-top-view")), board),
                    enabled(item("bottom", fl!("menu-bottom-view")), board),
                    enabled(key(item("fit", fl!("menu-fit-view")), "F"), board),
                    enabled(checked(item("smd", fl!("menu-show-smd")), st.show_smd), board),
                    enabled(checked(item("tht", fl!("menu-show-tht")), st.show_tht), board),
                    sep(),
                    sub(fl!("menu-render-size"), size),
                    sub(fl!("menu-render-ratio"), ratio),
                    sep(),
                    key(checked(item("mode.image", fl!("mode-image")), st.mode == DisplayMode::Beauty), "1"),
                    key(checked(item("mode.albedo", fl!("mode-albedo")), st.mode == DisplayMode::Albedo), "2"),
                    key(checked(item("mode.normal", fl!("mode-normal")), st.mode == DisplayMode::Normal), "3"),
                    sep(),
                    key(item("reset_view", fl!("menu-reset-view")), "R"),
                    item("auto_exposure", fl!("menu-auto-exposure")),
                    sep(),
                    tip(item("reset_all", fl!("menu-reset-all")), fl!("menu-reset-all-tip")),
                ]
            }
            "settings" => {
                let mut langs = vec![tip(checked(item("lang.auto", fl!("language-auto")), self.language_choice.is_none()), fl!("language-auto-tip")), sep()];
                langs.extend(i18n::LANGUAGES.iter().map(|(tag, name)| checked(item(&format!("lang.{tag}"), name.to_string()), self.language_choice.as_deref() == Some(*tag))));
                vec![sub(fl!("menu-language"), langs), sep(), tip(item("kicad_plugin", fl!("menu-kicad-plugin")), fl!("menu-kicad-plugin-tip"))]
            }
            "help" => {
                let mut v: Vec<Value> = [fl!("help-rotate"), fl!("help-select"), fl!("help-pan"), fl!("help-zoom"), fl!("help-focus")].into_iter().map(note).collect();
                v.push(sep());
                v.extend(KEY_BINDINGS.iter().map(|b| json!({ "note": i18n::get(b.help).unwrap_or_else(|| b.log.to_string()), "key": b.label })));
                v.push(sep());
                v.push(item("about", fl!("menu-about")));
                v
            }
            _ => Vec::new(),
        };
        json!({ "key": menu, "items": items }).to_string()
    }

    /// メニューで選んだ項目を反映する（egui のメニューと同じ要求にする）
    pub(super) fn apply_menu_click(&mut self, id: &str) {
        let st = &mut self.st;
        match id {
            "open" => st.req.open_board = true,
            "recent.clear" => {
                remember::clear_recent_boards();
                self.recent_boards.clear();
            }
            "save" => st.req.save = true,
            "save_png" => st.req.save_as = Some(SaveFormat::Png),
            "save_exr" => st.req.save_as = Some(SaveFormat::Exr),
            "loc.next" => st.req.settings_location = Some(board_file::Location::NextToBoard),
            "loc.config" => st.req.settings_location = Some(board_file::Location::ConfigDir),
            "reveal" => st.req.reveal_settings = true,
            "quit" => st.req.quit = true,
            "panel" => st.show_panel = !st.show_panel,
            "editor" => self.editor.open = !self.editor.open,
            "pause" => st.paused = !st.paused,
            "denoise" => st.denoise = !st.denoise,
            "top" => st.req.view_preset = Some(TOP_VIEW),
            "bottom" => st.req.view_preset = Some(BOTTOM_VIEW),
            "fit" => st.req.fit_view = true,
            "smd" => {
                st.show_smd = !st.show_smd;
                st.changed.visibility = true;
            }
            "tht" => {
                st.show_tht = !st.show_tht;
                st.changed.visibility = true;
            }
            "size.fit" => st.size_mode = None,
            "mode.image" => st.mode = DisplayMode::Beauty,
            "mode.albedo" => st.mode = DisplayMode::Albedo,
            "mode.normal" => st.mode = DisplayMode::Normal,
            "reset_view" => st.req.reset_view = true,
            "auto_exposure" => st.req.auto_exposure = true,
            "reset_all" => st.req.reset_all = true,
            "lang.auto" => st.req.language = Some(None),
            "kicad_plugin" => st.req.kicad_plugin = true,
            "about" => self.open_about(),
            _ => {
                if let Some(p) = id.strip_prefix("recent.").and_then(|i| i.parse::<usize>().ok()).and_then(|i| self.recent_boards.get(i)) {
                    st.req.open_path = Some(p.clone());
                } else if let Some((w, h)) = id.strip_prefix("size.").and_then(|s| s.split_once('x')).and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))) {
                    st.size_mode = Some((w, h));
                    st.req.fit_window = true;
                } else if let Some(&r) = id.strip_prefix("ratio.").and_then(|i| i.parse::<usize>().ok()).and_then(|i| RATIOS.get(i)) {
                    st.render_ratio = r;
                    st.req.fit_window = st.size_mode.is_some();
                } else if let Some((tag, _)) = id.strip_prefix("lang.").and_then(|t| i18n::LANGUAGES.iter().find(|(tag, _)| *tag == t)) {
                    st.req.language = Some(Some(tag.to_string()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn menu_ids_round_trip() {
        // サイズの id は "size.<幅>x<高さ>"
        let id = format!("size.{}x{}", 1920, 1080);
        let parsed = id.strip_prefix("size.").and_then(|s| s.split_once('x')).and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)));
        assert_eq!(parsed, Some((1920, 1080)));
    }
}
