//! ビューアのメニュー・ボタン列・ステータスバー・設定パネル・警告の帯を WebView（WebView2）で描く（`krbr view --web-ui`）。
//! 3D 表示は今までどおり Vulkan で描き、その上下・右の四角い領域に WebView を子ウィンドウとして並べる（ボタン列・ステータスバー・
//! 設定パネルは 3D 表示と重ならないので透過は要らない。警告の帯は 3D 表示の上端に重ねる、不透明な帯）。
//! 画面は web/ の HTML・CSS を exe に埋め込んだもの。操作は JavaScript から window.ipc.postMessage で受け取り、
//! 状態は evaluate_script で `krbr.<名前>(...)` を呼んで渡す（変わったときだけ）。
//! Tauri 全体は使わず、その中の WebView の部品（wry）だけを使う

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use winit::event_loop::EventLoopProxy;
use winit::window::Window;
use wry::{dpi, Rect, WebContext, WebView, WebViewBuilder};

const STYLE: &str = include_str!("../web/style.css");
const PANEL_STYLE: &str = include_str!("../web/panel.css");
const EDITOR_STYLE: &str = include_str!("../web/editor.css");
const ABOUT_STYLE: &str = include_str!("../web/about.css");
const TOOLBAR_HTML: &str = include_str!("../web/toolbar.html");
const STATUS_HTML: &str = include_str!("../web/status.html");
const PANEL_HTML: &str = include_str!("../web/panel.html");
const BANNER_HTML: &str = include_str!("../web/banner.html");
const MENU_HTML: &str = include_str!("../web/menu.html");
const TOOLTIP_HTML: &str = include_str!("../web/tooltip.html");
const EDITOR_HTML: &str = include_str!("../web/editor.html");
const ABOUT_HTML: &str = include_str!("../web/about.html");
const VIDEO_HTML: &str = include_str!("../web/video.html");
const KPLUG_HTML: &str = include_str!("../web/kicad_plugin.html");
/// どのページにも入れる、説明の吹き出しの仕掛け
const TIP_JS: &str = include_str!("../web/tip.js");

/// ページに渡す画像（材質の見本・部品のプレビュー。PNG）。名前は URL のパス。ページからは IMAGE_URL + 名前 で読む
pub type Images = Arc<Mutex<HashMap<String, Vec<u8>>>>;
/// 画像の置き場の URL（WebView2 では、独自のスキーム krbr を http://krbr.localhost/ で読む）
pub const IMAGE_URL: &str = "http://krbr.localhost/";

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
    /// プルダウンの中身の大きさ（CSS の px）：見せているところ（サブメニューを開くと変わる）と、どのサブメニューを開いても収まる大きさ
    MenuSize { shown: (f32, f32), full: (f32, f32) },
    /// 説明の吹き出しを出す（text が空なら消す）。rect は説明のある要素の位置（そのページの中の CSS の px：左・上・右・下）。
    /// pane はどのページからか（受け取ったところで入れる）
    Tip { pane: Pane, text: String, rect: [f32; 4] },
    /// 吹き出しの大きさ（CSS の px）
    TipSize(f32, f32),
    /// リンクを押した（既定のブラウザで開く）
    Open(String),
    /// ファイルを WebView の上に落とした
    Dropped(Vec<std::path::PathBuf>),
    /// 新しい大きさで描き終えた（重ねる WebView を、画面の外で描いてから置くため。pane は受け取ったところで入れる）
    Painted(Pane),
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
        "size" => {
            let shown = (f("width")?, f("height")?);
            Some(Msg::MenuSize { shown, full: (f("fullWidth").unwrap_or(shown.0).max(shown.0), f("fullHeight").unwrap_or(shown.1).max(shown.1)) })
        }
        "tip" => Some(Msg::Tip { pane: Pane::Toolbar, text: s("text")?, rect: [f("left").unwrap_or(0.0), f("top").unwrap_or(0.0), f("right").unwrap_or(0.0), f("bottom").unwrap_or(0.0)] }),
        "tipsize" => Some(Msg::TipSize(f("width")?, f("height")?)),
        "painted" => Some(Msg::Painted(Pane::Toolbar)),
        "open" => s("url").map(Msg::Open),
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
    /// 材質エディタ（3D 表示の上に重ねる、動かせる窓）
    Editor,
    /// 「KRB Renderer について」の窓（真ん中に重ねる）
    About,
    /// 「動画を書き出す」の窓（真ん中に重ねる）
    Video,
    /// 「KiCad プラグイン」の窓（真ん中に重ねる）
    Plugin,
    /// メニューのプルダウン（3D 表示などの上に重ねる）
    Menu,
    /// 説明の吹き出し（いちばん上に重ねる）
    Tooltip,
}

impl Pane {
    /// 作る順（後のものほど上に重なる）
    pub const ALL: [Pane; 10] = [Pane::Toolbar, Pane::Status, Pane::Panel, Pane::Banner, Pane::Editor, Pane::About, Pane::Video, Pane::Plugin, Pane::Menu, Pane::Tooltip];

    /// ページの window.krbr の関数の名前
    fn func(self) -> &'static str {
        match self {
            Pane::Toolbar => "toolbar",
            Pane::Status => "status",
            Pane::Panel => "panel",
            Pane::Banner => "banner",
            Pane::Editor => "editor",
            Pane::About => "about",
            Pane::Video => "video",
            Pane::Plugin => "kplug",
            Pane::Menu => "menu",
            Pane::Tooltip => "tooltip",
        }
    }

    /// ほかの上に重ねて、出したり消したりするもの（説明の吹き出し・メニューのプルダウン）。WebView2 は隠した WebView を
    /// 出すときや大きさを変えたときに一瞬白く描くので、隠さずに画面の外へ動かし、新しい大きさで描き終えてから置く
    fn overlay(self) -> bool {
        matches!(self, Pane::Tooltip | Pane::Menu)
    }

    fn html(self) -> &'static str {
        match self {
            Pane::Toolbar => TOOLBAR_HTML,
            Pane::Status => STATUS_HTML,
            Pane::Panel => PANEL_HTML,
            Pane::Banner => BANNER_HTML,
            Pane::Editor => EDITOR_HTML,
            Pane::About => ABOUT_HTML,
            Pane::Video => VIDEO_HTML,
            Pane::Plugin => KPLUG_HTML,
            Pane::Menu => MENU_HTML,
            Pane::Tooltip => TOOLTIP_HTML,
        }
    }
}

struct View {
    view: WebView,
    /// 置き場所。None なら隠している
    bounds: Option<PxRect>,
    /// 最後に送った状態（同じなら送らない）
    sent: String,
    /// 重ねるもの：WebView を見せる状態にしたか、今の大きさ、画面の外で描き終わるのを待っている置き場所と待ち始めた時刻
    shown: bool,
    size: (u32, u32),
    pending: Option<(PxRect, Instant)>,
    /// 見せるところ（左上からの幅・高さの物理ピクセル。外は切り抜く）。None なら全体
    clip: Option<(u32, u32)>,
}

/// ページの WebView。1 つ作るのに 0.1〜0.6 秒かかり、その間ウィンドウが止まるので、フレームの合間に 1 つずつ作る（step）。
/// 起動中の画面は、いつも見えるもの（ボタン列・ステータスバー・設定パネル）を作ったら終え、残りは見せてから作る。
/// まだ作っていないページへの操作（置き場所・送信など）は何もしない（作ったあとのフレームで送られる）
pub struct WebUi {
    /// Pane::ALL の順に、作ったところまで
    views: Vec<View>,
    rx: mpsc::Receiver<Msg>,
    tx: mpsc::Sender<Msg>,
    proxy: EventLoopProxy<Wake>,
    images: Images,
    style: String,
    /// 最後に WebView を動かした・消した時刻（その下の 3D 表示を描き直してもらう）
    moved: Instant,
    /// WebView で共有する WebView2 の環境（裏のプロセスを 1 組にする）。データは全体用キャッシュの webview2\
    /// （何も指定しないと exe の横に krbr.exe.WebView2 を作る）。WebView より後に捨てる（フィールドの順）
    context: WebContext,
}

/// 起動中の画面を終えるのに要る（いつも見えている）ページの数（Pane::ALL の先頭から）
const CORE_PANES: usize = 3;

impl WebUi {
    /// まだ WebView を 1 つも作っていない状態（step で作る）
    pub fn start(proxy: EventLoopProxy<Wake>) -> Self {
        let (tx, rx) = mpsc::channel();
        let context = WebContext::new(Some(krbr_core::platform::cache_dir().join("webview2")));
        let style = format!("{STYLE}\n{PANEL_STYLE}\n{EDITOR_STYLE}\n{ABOUT_STYLE}");
        Self { views: Vec::new(), rx, tx, proxy, images: Images::default(), style, moved: Instant::now(), context }
    }

    /// 全部の WebView を一度に作る
    pub fn new(window: &Window, proxy: EventLoopProxy<Wake>) -> Result<Self, String> {
        let mut w = Self::start(proxy);
        while !w.step(window)? {}
        Ok(w)
    }

    /// 作った数と、全部の数
    pub fn progress(&self) -> (usize, usize) {
        (self.views.len(), Pane::ALL.len())
    }

    /// いつも見えているページのうち、作った数と全部の数
    pub fn core_progress(&self) -> (usize, usize) {
        (self.views.len().min(CORE_PANES), CORE_PANES)
    }

    /// いつも見えているページを作り終えたか（起動中の画面を終えてよい）
    pub fn core_ready(&self) -> bool {
        self.views.len() >= CORE_PANES
    }

    /// 全部作り終えたか
    pub fn complete(&self) -> bool {
        self.views.len() == Pane::ALL.len()
    }

    /// 次の WebView を 1 つ作る。全部作り終えたら（もう作るものが無ければ）true
    pub fn step(&mut self, window: &Window) -> Result<bool, String> {
        let Some(&pane) = Pane::ALL.get(self.views.len()) else { return Ok(true) };
        let t0 = Instant::now();
        let (tx, proxy, store) = (self.tx.clone(), self.proxy.clone(), self.images.clone());
        let (tx_drop, proxy_drop) = (self.tx.clone(), self.proxy.clone());
        let view = WebViewBuilder::new_with_web_context(&mut self.context)
            .with_html(pane.html().replace("/*STYLE*/", &self.style).replace("</body>", &format!("<script>{TIP_JS}</script>\n</body>")))
            // 読み込みの間も、まわりと同じ暗い色（白く光らない）
            .with_background_color((17, 19, 22, 255))
            .with_bounds(rect((0, 0, 1, 1)))
            .with_visible(false)
            .with_custom_protocol("krbr".into(), move |_id, req| {
                let name = req.uri().path().trim_start_matches('/');
                let body = store.lock().ok().and_then(|m| m.get(name).cloned());
                // URL は描き直すたびに変わるので、同じ URL は覚えておいてよい
                let r = wry::http::Response::builder().header("Cache-Control", "max-age=3600").header("Access-Control-Allow-Origin", "*");
                match body {
                    Some(b) => r.header("Content-Type", "image/png").body(Cow::Owned(b)),
                    None => r.status(404).body(Cow::Borrowed(&[][..])),
                }
                .unwrap()
            })
            // ファイルを落とした：ビューアに渡す（WebView がファイルを開いてしまわないように、いつも自分で受ける）
            .with_drag_drop_handler(move |e| {
                if let wry::DragDropEvent::Drop { paths, .. } = e {
                    let _ = tx_drop.send(Msg::Dropped(paths));
                    let _ = proxy_drop.send_event(Wake);
                }
                true
            })
            .with_ipc_handler(move |req| {
                if let Some(mut m) = parse(req.body()) {
                    if let Msg::Tip { pane: p, .. } | Msg::Painted(p) = &mut m {
                        *p = pane;
                    }
                    let _ = tx.send(m);
                    let _ = proxy.send_event(Wake);
                }
            })
            .build_as_child(window)
            .map_err(|e| format!("WebView: {e}"))?;
        self.views.push(View { view, bounds: None, sent: String::new(), shown: false, size: (1, 1), pending: None, clip: None });
        krbr_core::debug!("web UI: {pane:?} in {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
        Ok(self.complete())
    }

    fn view(&mut self, pane: Pane) -> Option<&mut View> {
        let i = Pane::ALL.iter().position(|p| *p == pane)?;
        self.views.get_mut(i)
    }

    /// 置き場所（ウィンドウの中の物理ピクセル）。None なら隠す。変わったときだけ動かす
    pub fn layout(&mut self, pane: Pane, r: Option<PxRect>) {
        let Some(v) = self.view(pane) else { return };
        let before = (v.bounds, v.pending.map(|p| p.0), v.size);
        self.place(pane, r);
        let Some(v) = self.view(pane) else { return };
        if before != (v.bounds, v.pending.map(|p| p.0), v.size) {
            self.moved = Instant::now();
        }
    }

    /// 最後に WebView を動かした・消した時刻。WebView の位置の変化は少し遅れて反映されるので、その直後に 3D 表示を
    /// 描いても、WebView がいた場所は描かれない（白く残る）。この時刻から少しの間は、休まずに描き直すこと
    pub fn moved(&self) -> Instant {
        self.moved
    }

    fn place(&mut self, pane: Pane, r: Option<PxRect>) {
        if pane.overlay() {
            return self.layout_overlay(pane, r);
        }
        let Some(v) = self.view(pane) else { return };
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

    /// 重ねるものの置き場所：隠すときは画面の外へ動かすだけ。大きさが変わるときは、画面の外で新しい大きさにして、
    /// ページが描き終えた知らせ（painted）で置く（知らせが来なくても少し待ったら置く）
    fn layout_overlay(&mut self, pane: Pane, r: Option<PxRect>) {
        const AWAY: i32 = -30000;
        let Some(v) = self.view(pane) else { return };
        let Some(r) = r else {
            v.pending = None;
            if v.bounds.take().is_some() {
                let _ = v.view.set_bounds(rect((AWAY, AWAY, v.size.0, v.size.1)));
            }
            return;
        };
        if v.bounds == Some(r) {
            return;
        }
        if let Some((p, t)) = v.pending {
            if p == r {
                if t.elapsed() > Duration::from_millis(150) {
                    v.pending = None;
                    let _ = v.view.set_bounds(rect(r));
                    v.bounds = Some(r);
                }
                return;
            }
        }
        if v.shown && v.size == (r.2, r.3) {
            // 大きさが同じ：そのまま動かす
            v.pending = None;
            let _ = v.view.set_bounds(rect(r));
            v.bounds = Some(r);
            return;
        }
        let _ = v.view.set_bounds(rect((AWAY, AWAY, r.2, r.3)));
        if !v.shown {
            let _ = v.view.set_visible(true);
            v.shown = true;
        }
        v.size = (r.2, r.3);
        v.bounds = None;
        v.pending = Some((r, Instant::now()));
        let _ = v.view.evaluate_script("requestAnimationFrame(() => requestAnimationFrame(() => window.ipc.postMessage('{\"type\":\"painted\"}')))");
    }

    /// 見せるところを左上からの幅・高さ（物理ピクセル）に切り抜く（None なら全体）。WebView の大きさを変えると、新しい大きさで
    /// 描き終えるまで見えなくなるので、大きさの変わる中身（メニューのサブメニュー）は、大きな WebView のまま見せるところだけを変える
    pub fn clip(&mut self, pane: Pane, c: Option<(u32, u32)>) {
        let Some(v) = self.view(pane) else { return };
        if v.clip == c {
            return;
        }
        v.clip = c;
        #[cfg(windows)]
        {
            use windows::Win32::Graphics::Gdi::{CreateRectRgn, SetWindowRgn, HRGN};
            use wry::WebViewExtWindows;
            // 安全性：WebView の窓に、作った領域を渡す（渡した領域は窓のものになるので消さない）
            unsafe {
                let rgn = match c {
                    Some((w, h)) => CreateRectRgn(0, 0, w as i32, h as i32),
                    None => HRGN::default(),
                };
                SetWindowRgn(v.view.hwnd(), Some(rgn), true);
            }
        }
        self.moved = Instant::now();
    }

    /// 画面の外で描き終えた：待っていた場所に置く
    fn painted(&mut self, pane: Pane) {
        let Some(v) = self.view(pane) else { return };
        if let Some((r, _)) = v.pending.take() {
            let _ = v.view.set_bounds(rect(r));
            v.bounds = Some(r);
            self.moved = Instant::now();
        }
    }

    /// キーボードの入力をこのページへ（プルダウンを開いたとき。外をクリックしたら閉じるのは、ページがフォーカスを失った知らせで）
    pub fn focus(&mut self, pane: Pane) {
        if let Some(v) = self.view(pane) {
            let _ = v.view.focus();
        }
    }

    /// キーボードの入力をビューアのウィンドウへ戻す
    pub fn focus_parent(&mut self, pane: Pane) {
        if let Some(v) = self.view(pane) {
            let _ = v.view.focus_parent();
        }
    }

    /// ページに渡す画像の置き場
    pub fn images(&self) -> &Images {
        &self.images
    }

    /// 次の send を、前と同じ内容でも送る（ページに描き直して、大きさを測り直してもらう）
    pub fn resend(&mut self, pane: Pane) {
        if let Some(v) = self.view(pane) {
            v.sent.clear();
        }
    }

    /// 届いた操作。ページを読み終えた知らせがあれば、次の送信で状態を送り直す
    pub fn messages(&mut self) -> Vec<Msg> {
        let mut msgs: Vec<Msg> = self.rx.try_iter().collect();
        for m in &msgs {
            if let Msg::Painted(p) = m {
                self.painted(*p);
            }
        }
        msgs.retain(|m| !matches!(m, Msg::Painted(_)));
        if msgs.contains(&Msg::Ready) {
            for v in &mut self.views {
                v.sent.clear();
            }
        }
        msgs
    }

    /// ページの window.krbr の関数を呼ぶ（引数なし。読み終える前なら何もしない）
    pub fn call(&mut self, pane: Pane, func: &str) {
        if let Some(v) = self.view(pane) {
            let _ = v.view.evaluate_script(&format!("window.krbr && window.krbr.{func} && window.krbr.{func}()"));
        }
    }

    /// 状態を送る（JSON。前に送ったものと同じなら送らない）
    pub fn send(&mut self, pane: Pane, json: &str) {
        let Some(v) = self.view(pane) else { return };
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
        assert_eq!(parse(r#"{"type":"size","width":240,"height":300.5}"#), Some(Msg::MenuSize { shown: (240.0, 300.5), full: (240.0, 300.5) }));
        assert_eq!(parse(r#"{"type":"size","width":240,"height":300,"fullWidth":480,"fullHeight":320}"#), Some(Msg::MenuSize { shown: (240.0, 300.0), full: (480.0, 320.0) }));
        assert_eq!(parse(r#"{"type":"tip","text":"a","left":1,"top":2,"right":3,"bottom":4}"#), Some(Msg::Tip { pane: Pane::Toolbar, text: "a".into(), rect: [1.0, 2.0, 3.0, 4.0] }));
        assert_eq!(parse(r#"{"type":"tipsize","width":100,"height":20}"#), Some(Msg::TipSize(100.0, 20.0)));
        assert_eq!(parse(r#"{"type":"open","url":"https://example.com/"}"#), Some(Msg::Open("https://example.com/".into())));
        assert_eq!(parse(r#"{"type":"ready"}"#), Some(Msg::Ready));
        assert_eq!(parse(r#"{"type":"other"}"#), None);
        assert_eq!(parse("not json"), None);
        // ページに埋め込む CSS の目印がある
        for p in Pane::ALL {
            assert!(p.html().contains("/*STYLE*/"), "{p:?}");
        }
    }
}
