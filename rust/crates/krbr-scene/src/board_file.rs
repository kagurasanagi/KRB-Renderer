//! 基板ごとの設定ファイル `<基板の名前>.krbr`（1 つの TOML）。
//!   [board] [relief] [surface] [placeholder] [paths] [kicad_cli]  基板の設定（手で書く）
//!   [environment] [camera] [render] [lens]                         照明・カメラ・レンダリングの設定（手で書く）
//!   [ref.<部品番号>] / [[model]]                                    材質の上書き（ビューアと krbr material set が書く。crate::model_map）
//!   [remembered]                                                    ビューアで最後に使った設定（ビューアが書く。krbr-app の remember）
//! 置き場所は全体設定（`<全体設定>\settings.toml` の board_settings）で選ぶ：基板の横（既定）か `<全体設定>\boards\`。
//! どちらに置いても、中の相対パスは基板のフォルダから。置き場所を切り替えると、次に開いたときに今の置き場所へ移す。
//! 書き換えるときは toml_edit で、手で書いたコメントと並びを残したまま、その表だけを差し替える。
//! 以前の形（`<名前>.krbr\` フォルダの board.toml / environment.toml / overrides.toml）は、見つけたら 1 つのファイルにまとめ、
//! フォルダは `<名前>.krbr.old` に名前を変えて残す。

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item};

use crate::config_templates::{BOARD_TOML, ENVIRONMENT_TOML, HEADER};

/// 基板の設定ファイルの置き場所
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// 基板の横（`<基板のフォルダ>\<名前>.krbr`）
    NextToBoard,
    /// 全体設定のフォルダ（`<全体設定>\boards\<名前>_<基板のパスのハッシュ>.krbr`）。KiCad のフォルダにファイルを増やさない
    ConfigDir,
}

fn settings_file() -> PathBuf {
    krbr_core::platform::config_dir().join("settings.toml")
}

/// 今の置き場所（全体設定）
pub fn location() -> Location {
    let file = settings_file();
    let Ok(text) = std::fs::read_to_string(&file) else { return Location::NextToBoard };
    match text.parse::<toml::Table>() {
        Ok(t) => match t.get("board_settings").and_then(|v| v.as_str()) {
            Some("config_dir") => Location::ConfigDir,
            _ => Location::NextToBoard,
        },
        Err(e) => {
            krbr_core::warn!("{}: {}", file.display(), e);
            Location::NextToBoard
        }
    }
}

/// 置き場所を変える（全体設定に書く。基板のファイルは次に開いたときに移す）
pub fn set_location(loc: Location) -> Result<(), String> {
    let v = match loc {
        Location::NextToBoard => "board",
        Location::ConfigDir => "config_dir",
    };
    let new = "# krbr の全体設定\n\n# 基板ごとの設定ファイル <名前>.krbr の置き場所：board（基板の横）/ config_dir（このフォルダの boards\\）\nboard_settings = \"board\"\n";
    edit_toml(&settings_file(), new, |doc| doc["board_settings"] = toml_edit::value(v))
}

/// 基板を見分ける鍵：絶対パス、区切りは /、Windows では大文字・小文字を区別しない
pub fn board_key(pcb: &Path) -> String {
    let abs = std::path::absolute(pcb).unwrap_or_else(|_| pcb.to_path_buf());
    let s = abs.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

fn stem(pcb: &Path) -> String {
    pcb.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
}

/// 基板のフォルダ（設定ファイルの中の相対パスの基準）
pub fn base_dir(pcb: &Path) -> PathBuf {
    match pcb.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// その置き場所での設定ファイルのパス
pub fn file_in(pcb: &Path, loc: Location) -> PathBuf {
    match loc {
        Location::NextToBoard => base_dir(pcb).join(format!("{}.krbr", stem(pcb))),
        Location::ConfigDir => {
            let h = krbr_core::to_hex(krbr_core::xxh64(board_key(pcb).as_bytes()));
            krbr_core::platform::config_dir().join("boards").join(format!("{}_{}.krbr", stem(pcb), &h[..8.min(h.len())]))
        }
    }
}

/// この基板の設定ファイル（今の置き場所）。以前の形のフォルダや、別の置き場所にあるファイルは、ここへ移す。
/// まだ無ければ、そのパス（書くときに作る）
pub fn resolve(pcb: &Path) -> PathBuf {
    let loc = location();
    let want = file_in(pcb, loc);
    let legacy = file_in(pcb, Location::NextToBoard);
    if legacy.is_dir() {
        migrate_legacy(&legacy, &want);
    }
    if want.is_file() {
        return want;
    }
    let other = file_in(pcb, if loc == Location::NextToBoard { Location::ConfigDir } else { Location::NextToBoard });
    if other.is_file() {
        match move_file(&other, &want) {
            Ok(()) => krbr_core::info!("board settings: moved {} -> {}", other.display(), want.display()),
            Err(e) => {
                krbr_core::warn!("board settings: cannot move {} to {} ({}); using it where it is", other.display(), want.display(), e);
                return other;
            }
        }
    }
    want
}

fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(d) = to.parent() {
        std::fs::create_dir_all(d)?;
    }
    // ドライブをまたぐと rename できないので、写してから消す
    std::fs::rename(from, to).or_else(|_| std::fs::copy(from, to).and_then(|_| std::fs::remove_file(from)))
}

/// 以前の形（<名前>.krbr\ の board.toml / environment.toml / overrides.toml）を 1 つのファイルにまとめる
fn migrate_legacy(dir: &Path, to: &Path) {
    if to.is_file() {
        krbr_core::warn!("board settings: both {} and the old folder {} exist; using the file (the folder is ignored)", to.display(), dir.display());
        return;
    }
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
    let mut env = read("environment.toml");
    // environment.toml の相対パスはフォルダからだった。基板のフォルダからに変わるので、絶対パスにしておく
    if let Some(text) = &env {
        if let Ok(mut doc) = text.parse::<DocumentMut>() {
            let mut changed = false;
            for (table, key) in [("environment", "hdr"), ("lens", "lens_file")] {
                let Some(s) = doc.get(table).and_then(|t| t.get(key)).and_then(|v| v.as_str()).map(String::from) else { continue };
                if s.is_empty() || s.starts_with("builtin:") || Path::new(&s).is_absolute() {
                    continue;
                }
                let abs = std::path::absolute(dir.join(&s)).unwrap_or_else(|_| dir.join(&s));
                doc[table][key] = toml_edit::value(abs.to_string_lossy().replace('\\', "/"));
                changed = true;
            }
            if changed {
                env = Some(doc.to_string());
            }
        }
    }
    // 無かったファイルの分は、ひな形（説明つきの既定値）で埋める
    let board = read("board.toml").unwrap_or_else(|| BOARD_TOML.to_string());
    let env = env.unwrap_or_else(|| ENVIRONMENT_TOML.to_string());
    let parts: Vec<String> = [Some(board), Some(env), read("overrides.toml")].into_iter().flatten().collect();
    let mut text = format!("{HEADER}\n# （{} から移した）\n", dir.display());
    for p in &parts {
        text.push('\n');
        text.push_str(p.trim_end());
        text.push('\n');
    }
    // 同じ表が 2 つのファイルにあってつなげられないときは、表ごとに重ねる（コメントは一部消える）
    if text.parse::<DocumentMut>().is_err() {
        let mut doc: DocumentMut = HEADER.parse().unwrap_or_default();
        for p in &parts {
            if let Ok(d) = p.parse::<DocumentMut>() {
                for (k, item) in d.iter() {
                    if !doc.contains_key(k) {
                        doc.insert(k, item.clone());
                    }
                }
            }
        }
        text = doc.to_string();
    }
    // 基板の横に置くなら同じ名前になるので、先にフォルダの名前を変える
    let mut old = dir.with_extension("krbr.old");
    let mut n = 2;
    while old.exists() {
        old = dir.with_extension(format!("krbr.old{n}"));
        n += 1;
    }
    if let Err(e) = std::fs::rename(dir, &old) {
        krbr_core::warn!("board settings: cannot rename {} ({}); keeping the old folder", dir.display(), e);
        return;
    }
    match write_atomic(to, &text) {
        Ok(()) => krbr_core::info!("board settings: merged {} into {} (the folder was renamed to {})", dir.display(), to.display(), old.display()),
        Err(e) => {
            krbr_core::warn!("board settings: cannot write {} ({}); restoring {}", to.display(), e, dir.display());
            let _ = std::fs::rename(&old, dir);
        }
    }
}

/// 新しく作るときの中身（説明とひな形。値は既定値と同じ）
pub fn template() -> String {
    format!("{HEADER}{BOARD_TOML}{ENVIRONMENT_TOML}")
}

/// この基板の設定ファイルを読む。(ファイル, 中身)。無い・読めないときは中身が None（読めないときは警告）
pub fn read(pcb: &Path) -> (PathBuf, Option<toml::Table>) {
    let file = resolve(pcb);
    let text = match std::fs::read_to_string(&file) {
        Ok(s) => Some(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            krbr_core::warn!("{}: cannot read ({e}; save it as UTF-8) - ignored", file.display());
            None
        }
    };
    let t = text.and_then(|s| match s.parse::<toml::Table>() {
        Ok(t) => Some(t),
        Err(e) => {
            krbr_core::warn!("{}: {} - ignored", file.display(), e);
            None
        }
    });
    (file, t)
}

/// この基板の設定ファイルを書き換える（無ければひな形から作る）。書いたファイルを返す
pub fn edit(pcb: &Path, f: impl FnOnce(&mut DocumentMut)) -> Result<PathBuf, String> {
    let file = resolve(pcb);
    edit_toml(&file, &template(), f)?;
    Ok(file)
}

/// TOML のファイルを、コメントと並びを残したまま書き換える（無ければ new_text から作る）。
/// 既にあって書式の誤りで読めないときは、触らずにエラーにする
pub fn edit_toml(file: &Path, new_text: &str, f: impl FnOnce(&mut DocumentMut)) -> Result<(), String> {
    try_edit_toml(file, new_text, |doc| {
        f(doc);
        Ok(())
    })
}

/// edit_toml の、書き換えが失敗しうる版：f が Err を返したらファイルを書かずにそのエラーを返す
pub fn try_edit_toml(file: &Path, new_text: &str, f: impl FnOnce(&mut DocumentMut) -> Result<(), String>) -> Result<(), String> {
    let mut doc: DocumentMut = match std::fs::read_to_string(file) {
        Ok(s) => s.parse().map_err(|e| format!("cannot read {} ({e}); not overwriting it - fix or remove the file first", file.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => new_text.parse().map_err(|e| format!("template: {e}"))?,
        // UTF-8 でない（Shift-JIS で保存した）、使用中、権限が無い、など：ひな形で上書きしない
        Err(e) => return Err(format!("cannot read {} ({e}); not overwriting it - save it as UTF-8 or remove it first", file.display())),
    };
    f(&mut doc).map_err(|e| format!("{}: {e}; not written", file.display()))?;
    write_atomic(file, &doc.to_string()).map_err(|e| format!("cannot write {}: {e}", file.display()))
}

/// 一時ファイルに書いてから置き換える（書き込み中のファイルを読まれないように）
fn write_atomic(file: &Path, text: &str) -> std::io::Result<()> {
    if let Some(d) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(d)?;
    }
    let tmp = file.with_extension("krbr.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, file)
}

/// 最上位の表 keys を src の内容で差し替える（src に無い鍵は消す）。今ある表は、その場で値だけを変える（表・項目に
/// 手で書いたコメントと、表の場所は残る）。新しい表はファイルの最後に置く。
/// 新しい内容を作れなかったとき（実際にはまず起きない）は、doc に触らずにエラーを記録する。
/// 失敗をファイルを書く側に伝えるには try_replace_tables と try_edit_toml を使う
pub fn replace_tables(doc: &mut DocumentMut, src: &toml::Table, keys: &[&str]) {
    if let Err(e) = try_replace_tables(doc, src, keys) {
        krbr_core::error!("{e}");
    }
}

/// replace_tables の本体。新しい内容を先に作ってから古い表を消すので、失敗しても doc は元のまま（上書きの内容が消えない）
pub fn try_replace_tables(doc: &mut DocumentMut, src: &toml::Table, keys: &[&str]) -> Result<(), String> {
    let mut part = toml::Table::new();
    for k in keys {
        if let Some(v) = src.get(*k) {
            part.insert(k.to_string(), v.clone());
        }
    }
    let src_doc = if part.is_empty() {
        None
    } else {
        let text = toml::to_string(&part).map_err(|e| format!("cannot write the tables [{}]: {e}", keys.join("], [")))?;
        Some(text.parse::<DocumentMut>().map_err(|e| format!("cannot write the tables [{}]: {e}", keys.join("], [")))?)
    };
    let mut next = max_position(doc.as_item()) + 1;
    let mut appended = Vec::new();
    for k in keys {
        let new = src_doc.as_ref().and_then(|d| d.get(k));
        // 今ある表と新しい表が同じ種類（表どうし・表の配列どうし）なら、その場で差し替える
        // （以前は表ごと作り直して最後に置いたので、[ref.D1] などの中に書いたコメントが消えていた）
        if let (Some(old), Some(new)) = (doc.get_mut(k), new) {
            if same_kind(old, new) {
                merge_item(old, new, &mut next);
                continue;
            }
        }
        if let Some(item) = doc.remove(k) {
            // 足したときに前に付けた、ファイルの最後のコメントは残す
            let prefix = first_table_prefix(&item);
            if prefix.lines().any(|l| l.trim_start().starts_with('#')) {
                let t = format!("{}{}", raw(doc.trailing()), prefix.trim_end_matches('\n').to_string() + "\n");
                doc.set_trailing(t);
            }
        }
        if let Some(new) = new {
            appended.push((k.to_string(), new.clone()));
        }
    }
    if !appended.is_empty() {
        append_items(doc, appended);
    }
    Ok(())
}

fn same_kind(a: &Item, b: &Item) -> bool {
    (a.is_table() && b.is_table()) || (a.is_array_of_tables() && b.is_array_of_tables())
}

/// 値の見た目（前後の空白・コメントを除いたもの）
fn value_text(v: &toml_edit::Value) -> String {
    let mut v = v.clone();
    v.decor_mut().clear();
    v.to_string()
}

/// dst を src の内容にする。残る鍵・表は、付いているコメント（decor）と場所を保つ。新しく足す表は next からの番号で後ろに置く
fn merge_item(dst: &mut Item, src: &Item, next: &mut usize) {
    match (dst, src) {
        (Item::Table(d), Item::Table(s)) => merge_table(d, s, next),
        (Item::ArrayOfTables(d), Item::ArrayOfTables(s)) => {
            while d.len() > s.len() {
                d.remove(d.len() - 1);
            }
            for (i, st) in s.iter().enumerate() {
                match d.get_mut(i) {
                    Some(dt) => merge_table(dt, st, next),
                    None => {
                        let mut it = Item::Table(st.clone());
                        renumber(&mut it, next);
                        if let Item::Table(t) = it {
                            d.push(t);
                        }
                    }
                }
            }
        }
        (Item::Value(d), Item::Value(s)) => {
            if value_text(d) != value_text(s) {
                let decor = d.decor().clone();
                *d = s.clone();
                *d.decor_mut() = decor;
            }
        }
        (d, s) => {
            let mut it = s.clone();
            renumber(&mut it, next);
            *d = it;
        }
    }
}

fn merge_table(d: &mut toml_edit::Table, s: &toml_edit::Table, next: &mut usize) {
    let gone: Vec<String> = d.iter().map(|(k, _)| k.to_string()).filter(|k| !s.contains_key(k)).collect();
    for k in gone {
        d.remove(&k);
    }
    for (k, si) in s.iter() {
        match d.get_mut(k) {
            Some(di) => merge_item(di, si, next),
            None => {
                let mut it = si.clone();
                renumber(&mut it, next);
                d.insert(k, it);
            }
        }
    }
}

fn raw(r: &toml_edit::RawString) -> String {
    r.as_str().unwrap_or("").to_string()
}

fn first_table_prefix(item: &Item) -> String {
    match item {
        // 値の無い暗黙の表は見出しが出ないので、最初の子の表（set_lead が付けた所）を見る
        Item::Table(t) if t.is_implicit() => t.iter().find(|(_, i)| i.is_table() || i.is_array_of_tables()).map(|(_, i)| first_table_prefix(i)).unwrap_or_default(),
        Item::Table(t) => t.decor().prefix().map(raw).unwrap_or_default(),
        Item::ArrayOfTables(a) => a.iter().next().and_then(|t| t.decor().prefix().map(raw)).unwrap_or_default(),
        _ => String::new(),
    }
}

/// 表をファイルの最後に足す。表の順番は position で決まるので、今ある表より後ろの番号を振り直す。
/// ファイルの最後のコメント（最後の表の後ろ）は、足した表より前に残し、表の前には空行を入れる
fn append_items(doc: &mut DocumentMut, items: Vec<(String, Item)>) {
    let mut next = max_position(doc.as_item()) + 1;
    let mut trailing = raw(doc.trailing());
    doc.set_trailing("");
    for (k, mut item) in items {
        renumber(&mut item, &mut next);
        let lead = if trailing.trim().is_empty() { "\n".to_string() } else { format!("{}\n", trailing.trim_end_matches('\n').to_string() + "\n") };
        set_lead(&mut item, &lead);
        trailing.clear();
        doc.insert(&k, item);
    }
}

/// 表の前に lead（ファイルの最後のコメントと空行）を付ける。表に付いていたコメント（ひな形の説明など）は残す。
/// 値の無い暗黙の表（[ref] の下の [ref.U1] だけ、など）は見出しが出ず、付けたコメントも出ないので、最初の子の表に付ける
/// （以前は暗黙の表に付けて、ファイルの最後のコメントが消えていた）
fn set_lead(item: &mut Item, lead: &str) {
    let with_own = |t: &mut toml_edit::Table, lead: &str| {
        let own = t.decor().prefix().map(raw).unwrap_or_default();
        t.decor_mut().set_prefix(format!("{lead}{}", own.trim_start_matches('\n')));
    };
    match item {
        Item::Table(t) if t.is_implicit() => {
            let first = t.iter().find(|(_, i)| i.is_table() || i.is_array_of_tables()).map(|(k, _)| k.to_string());
            match first {
                Some(k) => set_lead(t.get_mut(&k).unwrap(), lead),
                None => with_own(t, lead),
            }
        }
        Item::Table(t) => with_own(t, lead),
        Item::ArrayOfTables(a) => {
            for (i, t) in a.iter_mut().enumerate() {
                with_own(t, if i == 0 { lead } else { "\n" });
            }
        }
        _ => {}
    }
}

fn max_position(item: &Item) -> usize {
    match item {
        Item::Table(t) => t.iter().map(|(_, i)| max_position(i)).max().unwrap_or(0).max(t.position().unwrap_or(0)),
        Item::ArrayOfTables(a) => a.iter().map(|t| t.iter().map(|(_, i)| max_position(i)).max().unwrap_or(0).max(t.position().unwrap_or(0))).max().unwrap_or(0),
        _ => 0,
    }
}

fn renumber(item: &mut Item, next: &mut usize) {
    match item {
        Item::Table(t) => {
            t.set_position(*next);
            *next += 1;
            for (_, i) in t.iter_mut() {
                renumber(i, next);
            }
        }
        Item::ArrayOfTables(a) => {
            for t in a.iter_mut() {
                t.set_position(*next);
                *next += 1;
                for (_, i) in t.iter_mut() {
                    renumber(i, next);
                }
            }
        }
        _ => {}
    }
}

/// `--init`：ひな形を書く。ファイルが既にあれば、無い表だけをひな形から足す（書いてある値は変えない）
pub fn init(pcb: &Path) {
    let Ok(tpl) = template().parse::<DocumentMut>() else { return };
    let mut added = Vec::new();
    let r = edit(pcb, |doc| {
        let items: Vec<(String, Item)> = tpl.iter().filter(|(k, _)| !doc.contains_key(k)).map(|(k, i)| (k.to_string(), i.clone())).collect();
        added = items.iter().map(|(k, _)| k.clone()).collect();
        append_items(doc, items);
    });
    match r {
        Ok(f) if added.is_empty() => krbr_core::info!("--init: {} already has every section", f.display()),
        Ok(f) => krbr_core::info!("--init: wrote {} ({})", f.display(), added.join(", ")),
        Err(e) => krbr_core::warn!("--init: {}", e),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    /// 全体設定のフォルダ（KRBR_CONFIG_DIR）を差し替えるテストは 1 つずつ走らせる
    pub(crate) fn config_lock() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// テスト用の作業フォルダ（基板のフォルダと、全体設定のフォルダ）
    pub(crate) fn workspace(name: &str) -> (PathBuf, PathBuf) {
        let ws = std::env::temp_dir().join(format!("krbr_board_file_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(ws.join("proj")).unwrap();
        std::env::set_var("KRBR_CONFIG_DIR", ws.join("config"));
        (ws.join("proj"), ws)
    }

    #[test]
    fn edits_keep_comments_and_new_tables_go_last() {
        let _g = config_lock();
        let (proj, ws) = workspace("edit");
        let pcb = proj.join("b.kicad_pcb");
        // 無ければひな形から作る
        let f = edit(&pcb, |_| {}).unwrap();
        assert_eq!(f, proj.join("b.krbr"));
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.starts_with("# krbr: この基板の設定") && text.contains("[render]") && text.contains("# 1 画素あたりのサンプル数"));
        // 手で書いた値とコメントは、別の表を書き換えても残る
        std::fs::write(&f, text.replace("spp = 64 ", "spp = 16 ")).unwrap();
        let mut src = toml::Table::new();
        src.insert("remembered".into(), toml::Value::Table(toml::toml! { exposure_ev = 1.5 }));
        edit(&pcb, |doc| replace_tables(doc, &src, &["remembered"])).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("spp = 16                     # 1 画素あたりのサンプル数"), "{text}");
        assert!(text.trim_end().ends_with("double Gauss 50mm f/2\n\n[remembered]\nexposure_ev = 1.5"), "{text}");
        // 何度差し替えても、ファイルの最後のコメントは消えず、空行も増えない
        edit(&pcb, |doc| replace_tables(doc, &src, &["remembered"])).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), text);
        // 消すと表ごと無くなる（コメントは残る）
        edit(&pcb, |doc| replace_tables(doc, &toml::Table::new(), &["remembered"])).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(!text.contains("\n[remembered]\n") && text.contains("# lens_file"), "{text}");
        // 読めないファイルは書き換えない
        std::fs::write(&f, "[broken").unwrap();
        assert!(edit(&pcb, |_| {}).is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "[broken");
        // UTF-8 でない（Shift-JIS で保存した）ファイルも、ひな形で上書きしない
        let sjis = b"# \x8a\xee\x94\xc2\n[render]\nspp = 16\n";
        std::fs::write(&f, sjis).unwrap();
        assert!(edit(&pcb, |_| {}).is_err());
        assert_eq!(std::fs::read(&f).unwrap(), sjis);
        let _ = std::fs::remove_dir_all(&ws);
    }

    /// 差し替える表の中のコメントは残る（[ref.D1] の見出しと項目のコメント）。新しく足す表が値の無い暗黙の表
    /// （[ref] の下の [ref.U5] だけ）でも、ファイルの最後のコメントは消えない
    #[test]
    fn replaced_tables_keep_their_comments() {
        let _g = config_lock();
        let (proj, ws) = workspace("ref_comments");
        let pcb = proj.join("b.kicad_pcb");
        let f = proj.join("b.krbr");
        std::fs::write(&f, "[render]\nspp = 16\n\n[ref.D1]  # status LED\nmaterial = \"led\"  # keep me\n\n# the end\n").unwrap();
        let src: toml::Table = toml::from_str("[ref.D1]\nmaterial = \"led\"\n[ref.U5]\nmaterial = \"abs\"\n").unwrap();
        edit(&pcb, |doc| replace_tables(doc, &src, &["ref"])).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("[ref.D1]  # status LED\nmaterial = \"led\"  # keep me\n") && text.contains("[ref.U5]\nmaterial = \"abs\"") && text.contains("# the end"), "{text}");
        // 値を変えても、項目のコメントは残る。消した表は無くなる
        let src: toml::Table = toml::from_str("[ref.D1]\nmaterial = \"glass\"\n").unwrap();
        edit(&pcb, |doc| replace_tables(doc, &src, &["ref"])).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("material = \"glass\"  # keep me") && !text.contains("U5") && text.contains("# the end"), "{text}");

        // 表の無いファイルに、値の無い暗黙の表を足しても、最後のコメントは残る
        std::fs::write(&f, "[render]\nspp = 16\n# the end\n").unwrap();
        let src: toml::Table = toml::from_str("[ref.U5]\nmaterial = \"abs\"\n").unwrap();
        edit(&pcb, |doc| replace_tables(doc, &src, &["ref"])).unwrap();
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(text.contains("# the end") && text.contains("[ref.U5]"), "{text}");
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn location_switch_moves_the_file() {
        let _g = config_lock();
        let (proj, ws) = workspace("location");
        let pcb = proj.join("b.kicad_pcb");
        edit(&pcb, |doc| doc["render"]["spp"] = toml_edit::value(7)).unwrap();
        assert!(proj.join("b.krbr").is_file());
        set_location(Location::ConfigDir).unwrap();
        assert_eq!(location(), Location::ConfigDir);
        let moved = resolve(&pcb);
        assert!(moved.starts_with(ws.join("config").join("boards")) && moved.is_file() && !proj.join("b.krbr").exists());
        assert_eq!(read(&pcb).1.unwrap()["render"]["spp"].as_integer(), Some(7));
        set_location(Location::NextToBoard).unwrap();
        assert_eq!(resolve(&pcb), proj.join("b.krbr"));
        assert!(proj.join("b.krbr").is_file() && !moved.exists());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn the_old_folder_is_merged_into_one_file() {
        let _g = config_lock();
        let (proj, ws) = workspace("legacy");
        let pcb = proj.join("b.kicad_pcb");
        let dir = proj.join("b.krbr");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("board.toml"), "# my board\n[board]\nmask_color = \"jlcpcb-white\"   # white\n").unwrap();
        std::fs::write(dir.join("environment.toml"), "[environment]\nhdr = \"../hdr/x.exr\"\n[render]\nspp = 16\n").unwrap();
        std::fs::write(dir.join("overrides.toml"), "[ref.D1]\nmaterial = \"plastic\"\n").unwrap();
        let (file, t) = read(&pcb);
        let t = t.unwrap();
        assert_eq!(file, proj.join("b.krbr"));
        assert!(file.is_file() && proj.join("b.krbr.old").join("board.toml").is_file());
        assert_eq!(t["board"]["mask_color"].as_str(), Some("jlcpcb-white"));
        assert_eq!(t["render"]["spp"].as_integer(), Some(16));
        assert_eq!(t["ref"]["D1"]["material"].as_str(), Some("plastic"));
        // フォルダからの相対パスは、絶対パスにして意味を保つ
        let hdr = t["environment"]["hdr"].as_str().unwrap();
        assert_eq!(Path::new(hdr), proj.join("hdr").join("x.exr"));
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("# my board") && text.contains("# white"));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn init_adds_only_missing_sections() {
        let _g = config_lock();
        let (proj, ws) = workspace("init");
        let pcb = proj.join("b.kicad_pcb");
        std::fs::write(proj.join("b.krbr"), "[render]\nspp = 16\n").unwrap();
        init(&pcb);
        let t = read(&pcb).1.unwrap();
        assert_eq!(t["render"]["spp"].as_integer(), Some(16));
        assert!(t["render"].get("width").is_none(), "an existing table is not touched");
        assert!(t.contains_key("board") && t.contains_key("lens"));
        let _ = std::fs::remove_dir_all(&ws);
    }
}
