//! ビューアのメニュー・ボタン列・ステータスバー・設定パネル・警告の帯を WebView（WebView2）で描く（`krbr view --web-ui`）。
//! 3D 表示は今までどおり Vulkan で描き、その上下・右の四角い領域に WebView を子ウィンドウとして並べる（ボタン列・ステータスバー・
//! 設定パネルは 3D 表示と重ならないので透過は要らない。警告の帯は 3D 表示の上端に重ねる、不透明な帯）。
//! 画面は web/ の HTML・CSS を exe に埋め込んだもの。操作は JavaScript から window.ipc.postMessage で受け取り、
//! 状態は evaluate_script で `krbr.<名前>(...)` を呼んで渡す（変わったときだけ）。
//! Tauri 全体は使わず、その中の WebView の部品（wry）だけを使う

use std::sync::mpsc;

use winit::event_loop::EventLoopProxy;
use winit::window::Window;
use wry::{dpi, Rect, WebContext, WebView, WebViewBuilder};

const STYLE: &str = include_str!("../web/style.css");
const PANEL_STYLE: &str = include_str!("../web/panel.css");
const TOOLBAR_HTML: &str = include_str!("../web/toolbar.html");
const STATUS_HTML: &str = include_str!("../web/status.html");
const PANEL_HTML: &str = include_str!("../web/panel.html");
const BANNER_HTML: &str = include_str!("../web/banner.html");
const MENU_HTML: &str = include_str!("../web/menu.html");

/// WebView からの知らせでイベントループを起こす（待機中のビューアにも、押したボタンをすぐ反映する）
#[derive(Debug, Clone, Copy)]
pub struct Wake;

/// WebView からの操作
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// ボタン（id はページのもの）
    Click(String),
    /// 値を変えた（スライダー・チェック・選択肢など）
    Set(String, serde_json::Value),
    /// キー（KeyboardEvent.code）。WebView にフォーカスがあっても、ビューアのキー操作が効くように
    Key(String),
    /// 設定パネルの幅を変えた（CSS の px = egui のポイント）
    Resize(f32),
    /// メニューの見出しを押した（開いているときは、見出しに載せただけでも）。x・y はボタン列の中の、プルダウンを出す位置（CSS の px）
    Menu { id: String, x: f32, y: f32 },
    /// プルダウンの中身の大きさ（CSS の px。サブメニューを開くと変わる）
    MenuSize(f32, f32),
    /// ページを読み終えた（状態を送り直す）
    Ready,
}

fn parse(body: &str) -> Option<Msg> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let f = |k: &str| v.get(k).and_then(|x| x.as_f64()).filter(|x| x.is_finite()).map(|x| x as f32);
    match s("type")?.as_str() {
        "click" => s("id").map(Msg::Click),
        "set" => Some(Msg::Set(s("id")?, v.get("value")?.clone())),
        "key" => s("code").map(Msg::Key),
        "resize" => v.get("width").and_then(|w| w.as_f64()).filter(|w| w.is_finite()).map(|w| Msg::Resize(w as f32)),
        "menu" => Some(Msg::Menu { id: s("id")?, x: f("x")?, y: f("y")? }),
        "size" => Some(Msg::MenuSize(f("width")?, f("height")?)),
        "ready" => Some(Msg::Ready),
        _ => None,
    }
}

/// 物理ピクセルの矩形（x, y, 幅, 高さ）
pub type PxRect = (i32, i32, u32, u32);

fn rect(r: PxRect) -> Rect {
    Rect { position: dpi::PhysicalPosition::new(r.0, r.1).into(), size: dpi::PhysicalSize::new(r.2.max(1), r.3.max(1)).into() }
}

/// 並べる WebView
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Toolbar,
    Status,
    Panel,
    Banner,
    /// メニューのプルダウン（3D 表示などの上に重ねる）
    Menu,
}

impl Pane {
    pub const ALL: [Pane; 5] = [Pane::Toolbar, Pane::Status, Pane::Panel, Pane::Banner, Pane::Menu];

    /// ページの window.krbr の関数の名前
    fn func(self) -> &'static str {
        match self {
            Pane::Toolbar => "toolbar",
            Pane::Status => "status",
            Pane::Panel => "panel",
            Pane::Banner => "banner",
            Pane::Menu => "menu",
        }
    }

    fn html(self) -> &'static str {
        match self {
            Pane::Toolbar => TOOLBAR_HTML,
            Pane::Status => STATUS_HTML,
            Pane::Panel => PANEL_HTML,
            Pane::Banner => BANNER_HTML,
            Pane::Menu => MENU_HTML,
        }
    }
}

struct View {
    view: WebView,
    /// 置き場所。None なら隠している
    bounds: Option<PxRect>,
    /// 最後に送った状態（同じなら送らない）
    sent: String,
}

pub struct WebUi {
    views: Vec<View>,
    rx: mpsc::Receiver<Msg>,
    /// WebView で共有する WebView2 の環境（裏のプロセスを 1 組にする）。データは全体用キャッシュの webview2\
    /// （何も指定しないと exe の横に krbr.exe.WebView2 を作る）。WebView より後に捨てる（フィールドの順）
    _context: WebContext,
}

impl WebUi {
    pub fn new(window: &Window, proxy: EventLoopProxy<Wake>) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let mut context = WebContext::new(Some(krbr_core::platform::cache_dir().join("webview2")));
        let style = format!("{STYLE}\n{PANEL_STYLE}");
        let mut views = Vec::new();
        for pane in Pane::ALL {
            let (tx, proxy) = (tx.clone(), proxy.clone());
            let view = WebViewBuilder::new_with_web_context(&mut context)
                .with_html(pane.html().replace("/*STYLE*/", &style))
                // 読み込みの間も、まわりと同じ暗い色（白く光らない）
                .with_background_color((17, 19, 22, 255))
                .with_bounds(rect((0, 0, 1, 1)))
                .with_visible(false)
                .with_ipc_handler(move |req| {
                    if let Some(m) = parse(req.body()) {
                        let _ = tx.send(m);
                        let _ = proxy.send_event(Wake);
                    }
                })
                .build_as_child(window)
                .map_err(|e| format!("WebView: {e}"))?;
            views.push(View { view, bounds: None, sent: String::new() });
        }
        Ok(Self { views, rx, _context: context })
    }

    fn view(&mut self, pane: Pane) -> &mut View {
        &mut self.views[Pane::ALL.iter().position(|p| *p == pane).unwrap()]
    }

    /// 置き場所（ウィンドウの中の物理ピクセル）。None なら隠す。変わったときだけ動かす
    pub fn layout(&mut self, pane: Pane, r: Option<PxRect>) {
        let v = self.view(pane);
        if v.bounds == r {
            return;
        }
        match r {
            Some(r) => {
                if let Err(e) = v.view.set_bounds(rect(r)) {
                    krbr_core::warn!("WebView bounds: {e}");
                }
                if v.bounds.is_none() {
                    let _ = v.view.set_visible(true);
                }
            }
            None => {
                let _ = v.view.set_visible(false);
            }
        }
        v.bounds = r;
    }

    /// キーボードの入力をこのページへ（プルダウンを開いたとき。外をクリックしたら閉じるのは、ページがフォーカスを失った知らせで）
    pub fn focus(&mut self, pane: Pane) {
        let _ = self.view(pane).view.focus();
    }

    /// キーボードの入力をビューアのウィンドウへ戻す
    pub fn focus_parent(&mut self, pane: Pane) {
        let _ = self.view(pane).view.focus_parent();
    }

    /// 届いた操作。ページを読み終えた知らせがあれば、次の送信で状態を送り直す
    pub fn messages(&mut self) -> Vec<Msg> {
        let msgs: Vec<Msg> = self.rx.try_iter().collect();
        if msgs.contains(&Msg::Ready) {
            for v in &mut self.views {
                v.sent.clear();
            }
        }
        msgs
    }

    /// 状態を送る（JSON。前に送ったものと同じなら送らない）
    pub fn send(&mut self, pane: Pane, json: &str) {
        let v = self.view(pane);
        if v.sent != json && v.view.evaluate_script(&format!("window.krbr && window.krbr.{}({json})", pane.func())).is_ok() {
            v.sent = json.to_string();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_from_the_page() {
        assert_eq!(parse(r#"{"type":"click","id":"fit"}"#), Some(Msg::Click("fit".into())));
        assert_eq!(parse(r#"{"type":"set","id":"env.intensity","value":1.5}"#), Some(Msg::Set("env.intensity".into(), serde_json::json!(1.5))));
        assert_eq!(parse(r#"{"type":"key","code":"KeyP"}"#), Some(Msg::Key("KeyP".into())));
        assert_eq!(parse(r#"{"type":"resize","width":360}"#), Some(Msg::Resize(360.0)));
        assert_eq!(parse(r#"{"type":"menu","id":"file","x":4,"y":28}"#), Some(Msg::Menu { id: "file".into(), x: 4.0, y: 28.0 }));
        assert_eq!(parse(r#"{"type":"size","width":240,"height":300.5}"#), Some(Msg::MenuSize(240.0, 300.5)));
        assert_eq!(parse(r#"{"type":"ready"}"#), Some(Msg::Ready));
        assert_eq!(parse(r#"{"type":"other"}"#), None);
        assert_eq!(parse("not json"), None);
        // ページに埋め込む CSS の目印がある
        for p in Pane::ALL {
            assert!(p.html().contains("/*STYLE*/"), "{p:?}");
        }
    }
}
