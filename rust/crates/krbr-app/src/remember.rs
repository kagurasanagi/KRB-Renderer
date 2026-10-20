//! 基板ごとに最後に使った環境マップと、最近使った環境マップの一覧を覚えておく（`%APPDATA%\krbr\state.toml`）。
//! プロジェクトのファイル（`<名前>.krbr\environment.toml`）は書き換えない。

use std::path::{Path, PathBuf};

use krbr_core::warn;
use toml::{Table, Value};

const MAX_RECENT: usize = 12;

/// 基板について覚えている環境マップ
#[derive(Debug, Clone, PartialEq)]
pub struct Remembered {
    pub hdr: String,
    /// 覚えたときの environment.toml の hdr。後で設定ファイルの方が書き換えられたら、そちらを優先する
    pub config_hdr: String,
}

fn state_file() -> PathBuf {
    krbr_core::platform::config_dir().join("state.toml")
}

/// 表を引く鍵：絶対パス、区切りは /、Windows では大文字・小文字を区別しない
fn board_key(pcb: &Path) -> String {
    let abs = std::path::absolute(pcb).unwrap_or_else(|_| pcb.to_path_buf());
    let s = abs.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// 環境マップの指定を、作業フォルダによらない形にする（内蔵の名前はそのまま）
pub fn normalize_hdr(spec: &str) -> String {
    if krbr_render::is_builtin(spec) {
        return spec.to_string();
    }
    std::path::absolute(spec).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| spec.to_string())
}

fn load(file: &Path) -> Table {
    match std::fs::read_to_string(file) {
        Ok(s) => s.parse::<Table>().unwrap_or_else(|e| {
            warn!("{}: {}", file.display(), e);
            Table::new()
        }),
        Err(_) => Table::new(),
    }
}

fn save(file: &Path, t: &Table) {
    let tmp = file.with_extension("toml.tmp");
    let r = file
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|_| std::fs::write(&tmp, t.to_string()))
        .and_then(|_| std::fs::rename(&tmp, file));
    if let Err(e) = r {
        warn!("{}: {}", file.display(), e);
    }
}

fn remembered_in(t: &Table, pcb: &Path) -> Option<Remembered> {
    let b = t.get("boards")?.as_table()?.get(&board_key(pcb))?.as_table()?;
    let hdr = b.get("hdr")?.as_str()?.to_string();
    let config_hdr = b.get("config_hdr").and_then(|v| v.as_str()).unwrap_or("").to_string();
    Some(Remembered { hdr, config_hdr })
}

fn remember_in(t: &mut Table, pcb: &Path, hdr: &str, config_hdr: &str) {
    let boards = t.entry("boards").or_insert_with(|| Value::Table(Table::new()));
    if !boards.is_table() {
        *boards = Value::Table(Table::new());
    }
    let mut b = Table::new();
    b.insert("hdr".into(), Value::String(hdr.to_string()));
    b.insert("config_hdr".into(), Value::String(config_hdr.to_string()));
    boards.as_table_mut().unwrap().insert(board_key(pcb), Value::Table(b));

    let mut recent = recent_in(t);
    recent.retain(|r| r != hdr);
    recent.insert(0, hdr.to_string());
    recent.truncate(MAX_RECENT);
    t.insert("recent_hdr".into(), Value::Array(recent.into_iter().map(Value::String).collect()));
}

fn recent_in(t: &Table) -> Vec<String> {
    t.get("recent_hdr").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// この基板で最後に使った環境マップ
pub fn remembered_hdr(pcb: &Path) -> Option<Remembered> {
    remembered_in(&load(&state_file()), pcb)
}

/// この基板で使った環境マップを覚える（最近使った一覧の先頭にも入れる）
pub fn remember_hdr(pcb: &Path, hdr: &str, config_hdr: &str) {
    let file = state_file();
    let mut t = load(&file);
    remember_in(&mut t, pcb, hdr, config_hdr);
    save(&file, &t);
}

/// 最近使った環境マップ（新しい順）
pub fn recent_hdrs() -> Vec<String> {
    recent_in(&load(&state_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_per_board_and_recent() {
        let mut t = Table::new();
        let (a, b) = (Path::new("/x/a.kicad_pcb"), Path::new("/x/b.kicad_pcb"));
        remember_in(&mut t, a, "/hdr/one.exr", "");
        remember_in(&mut t, b, "/hdr/two.exr", "/cfg.exr");
        remember_in(&mut t, a, "/hdr/two.exr", "");
        // 文字列にして読み直しても同じ
        let t: Table = t.to_string().parse().unwrap();
        assert_eq!(remembered_in(&t, a), Some(Remembered { hdr: "/hdr/two.exr".into(), config_hdr: String::new() }));
        assert_eq!(remembered_in(&t, b).unwrap().config_hdr, "/cfg.exr");
        assert_eq!(remembered_in(&t, Path::new("/x/c.kicad_pcb")), None);
        assert_eq!(recent_in(&t), vec!["/hdr/two.exr".to_string(), "/hdr/one.exr".to_string()]);
    }
}
