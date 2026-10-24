//! ビューアのボタン列とステータスバーを WebView（WebView2）で描く試作（`krbr view --web-ui`）。
//! 3D 表示は今までどおり Vulkan で描き、その上下の四角い領域に WebView を子ウィンドウとして並べる（重ねないので透過は要らない）。
//! 画面は web/ の HTML・CSS を exe に埋め込んだもの。操作は JavaScript から window.ipc.postMessage で受け取り、
//! 状態は evaluate_script で `krbr.toolbar(...)` / `krbr.status(...)` を呼んで渡す（変わったときだけ）。
//! Tauri 全体は使わず、その中の WebView の部品（wry）だけを使う

use std::sync::mpsc;

use winit::event_loop::EventLoopProxy;
use winit::window::Window;
use wry::{dpi, Rect, WebContext, WebView, WebViewBuilder};

const STYLE: &str = include_str!("../web/style.css");
const TOOLBAR_HTML: &str = include_str!("../web/toolbar.html");
const STATUS_HTML: &str = include_str!("../web/status.html");

/// WebView からの知らせでイベントループを起こす（待機中のビューアにも、押したボタンをすぐ反映する）
#[derive(Debug, Clone, Copy)]
pub struct Wake;

/// WebView からの操作
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// ボタン（id は toolbar.html のもの）
    Click(String),
    /// キー（KeyboardEvent.code）。WebView にフォーカスがあっても、ビューアのキー操作が効くように
    Key(String),
    /// ページを読み終えた（状態を送り直す）
    Ready,
}

fn parse(body: &str) -> Option<Msg> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    match s("type")?.as_str() {
        "click" => s("id").map(Msg::Click),
        "key" => s("code").map(Msg::Key),
        "ready" => Some(Msg::Ready),
        _ => None,
    }
}

/// 物理ピクセルの矩形（x, y, 幅, 高さ）
pub type PxRect = (i32, i32, u32, u32);

fn rect(r: PxRect) -> Rect {
    Rect { position: dpi::PhysicalPosition::new(r.0, r.1).into(), size: dpi::PhysicalSize::new(r.2.max(1), r.3.max(1)).into() }
}

struct Pane {
    view: WebView,
    bounds: Option<PxRect>,
    /// 最後に送った状態（同じなら送らない）
    sent: String,
}

pub struct WebUi {
    toolbar: Pane,
    status: Pane,
    rx: mpsc::Receiver<Msg>,
    /// 2 つの WebView で共有する WebView2 の環境（裏のプロセスを 1 組にする）。データは全体用キャッシュの webview2    /// （何も指定しないと exe の横に krbr.exe.WebView2 を作る）。WebView より後に捨てる（フィールドの順）
    _context: WebContext,
}

impl WebUi {
    pub fn new(window: &Window, proxy: EventLoopProxy<Wake>) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let mut context = WebContext::new(Some(krbr_core::platform::cache_dir().join("webview2")));
        let mut make = |html: &str| -> Result<Pane, String> {
            let (tx, proxy) = (tx.clone(), proxy.clone());
            let view = WebViewBuilder::new_with_web_context(&mut context)
                .with_html(html.replace("/*STYLE*/", STYLE))
                // 読み込みの間も、まわりと同じ暗い色（白く光らない）
                .with_background_color((17, 19, 22, 255))
                .with_bounds(rect((0, 0, 1, 1)))
                .with_ipc_handler(move |req| {
                    if let Some(m) = parse(req.body()) {
                        let _ = tx.send(m);
                        let _ = proxy.send_event(Wake);
                    }
                })
                .build_as_child(window)
                .map_err(|e| format!("WebView: {e}"))?;
            Ok(Pane { view, bounds: None, sent: String::new() })
        };
        let (toolbar, status) = (make(TOOLBAR_HTML)?, make(STATUS_HTML)?);
        Ok(Self { toolbar, status, rx, _context: context })
    }

    /// 置き場所（ウィンドウの中の物理ピクセル）。変わったときだけ動かす
    pub fn layout(&mut self, toolbar: PxRect, status: PxRect) {
        for (p, r) in [(&mut self.toolbar, toolbar), (&mut self.status, status)] {
            if p.bounds != Some(r) {
                if let Err(e) = p.view.set_bounds(rect(r)) {
                    krbr_core::warn!("WebView bounds: {e}");
                }
                p.bounds = Some(r);
            }
        }
    }

    /// 届いた操作。ページを読み終えた知らせがあれば、次の送信で状態を送り直す
    pub fn messages(&mut self) -> Vec<Msg> {
        let msgs: Vec<Msg> = self.rx.try_iter().collect();
        if msgs.contains(&Msg::Ready) {
            self.toolbar.sent.clear();
            self.status.sent.clear();
        }
        msgs
    }

    /// 状態を送る（JSON。前に送ったものと同じなら送らない）
    pub fn send(&mut self, toolbar: &str, status: &str) {
        for (p, func, json) in [(&mut self.toolbar, "toolbar", toolbar), (&mut self.status, "status", status)] {
            if p.sent != json {
                if p.view.evaluate_script(&format!("window.krbr && window.krbr.{func}({json})")).is_ok() {
                    p.sent = json.to_string();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_from_the_page() {
        assert_eq!(parse(r#"{"type":"click","id":"fit"}"#), Some(Msg::Click("fit".into())));
        assert_eq!(parse(r#"{"type":"key","code":"KeyP"}"#), Some(Msg::Key("KeyP".into())));
        assert_eq!(parse(r#"{"type":"ready"}"#), Some(Msg::Ready));
        assert_eq!(parse(r#"{"type":"other"}"#), None);
        assert_eq!(parse("not json"), None);
        // ページに埋め込む CSS の目印がある
        assert!(TOOLBAR_HTML.contains("/*STYLE*/") && STATUS_HTML.contains("/*STYLE*/"));
    }
}
