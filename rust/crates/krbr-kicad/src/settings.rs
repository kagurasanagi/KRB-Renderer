//! KiCad のユーザー設定（%APPDATA%\kicad\<版>\kicad_common.json など）を読む。読むだけで書き換えない。

use std::path::PathBuf;

/// KiCad の設定フォルダの置き場所（KICAD_CONFIG_HOME があればそれ）。この下に版ごとのフォルダ（"9.0"、"10.0"）がある。
/// KiCad（wxStandardPaths::GetUserConfigDir）と同じ場所：Windows は %APPDATA%\kicad、macOS は ~/Library/Preferences/kicad、
/// そのほかは $XDG_CONFIG_HOME/kicad（未設定なら ~/.config/kicad）
fn config_base() -> Option<PathBuf> {
    let non_empty = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(v) = non_empty("KICAD_CONFIG_HOME") {
        return Some(v);
    }
    if cfg!(windows) {
        non_empty("APPDATA").map(|a| a.join("kicad"))
    } else if cfg!(target_os = "macos") {
        non_empty("HOME").map(|h| h.join("Library").join("Preferences").join("kicad"))
    } else {
        non_empty("XDG_CONFIG_HOME").or_else(|| non_empty("HOME").map(|h| h.join(".config"))).map(|c| c.join("kicad"))
    }
}

/// フォルダ名の版（"10.0" → [10, 0]）。数字で始まらなければ None
fn version_of(name: &str) -> Option<Vec<u32>> {
    let parts: Vec<u32> = name.split('.').map_while(|s| s.parse().ok()).collect();
    (!parts.is_empty()).then_some(parts)
}

/// kicad_common.json を、古い版から新しい版の順に（"9.0" は "10.0" より前）
pub fn kicad_common_files() -> Vec<PathBuf> {
    let Some(base) = config_base() else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&base) else { return Vec::new() };
    let mut files: Vec<(Vec<u32>, PathBuf)> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| Some((version_of(&e.file_name().to_string_lossy())?, e.path().join("kicad_common.json"))))
        .filter(|(_, p)| p.is_file())
        .collect();
    files.sort();
    files.into_iter().map(|(_, p)| p).collect()
}

/// KiCad の言語の表示名（kicad_common.json の system.language。KiCad の pgm_base.cpp の LanguagesList）と言語タグ
const LANGUAGES: &[(&str, &str)] = &[
    ("العربية", "ar"),
    ("فارسی", "fa"),
    ("Bahasa Indonesia", "id"),
    ("Български", "bg"),
    ("Català", "ca"),
    ("Čeština", "cs"),
    ("Dansk", "da"),
    ("Deutsch", "de"),
    ("Ελληνικά", "el"),
    ("Eesti", "et"),
    ("English", "en"),
    ("Español", "es"),
    ("Español (Latinoamericano)", "es-MX"),
    ("Français", "fr"),
    ("עברית", "he"),
    ("हिन्दी", "hi"),
    ("Hrvatski", "hr"),
    ("한국어", "ko"),
    ("Italiano", "it"),
    ("ພາສາລາວ", "lo"),
    ("Latviešu", "lv"),
    ("Lietuvių", "lt"),
    ("Magyar", "hu"),
    ("Nederlands", "nl"),
    ("Norsk Bokmål", "nb"),
    ("日本語", "ja"),
    ("ქართული", "ka"),
    ("ภาษาไทย", "th"),
    ("Polski", "pl"),
    ("Português", "pt"),
    ("Português (Brasil)", "pt-BR"),
    ("Română", "ro"),
    ("Русский", "ru"),
    ("Српски", "sr"),
    ("Slovenčina", "sk"),
    ("Slovenščina", "sl"),
    ("Suomi", "fi"),
    ("Svenska", "sv"),
    ("Tiếng Việt", "vi"),
    ("தமிழ்", "ta"),
    ("తెలుగు", "te"),
    ("Türkçe", "tr"),
    ("Українська", "uk"),
    ("简体中文", "zh-CN"),
    ("繁體中文", "zh-TW"),
];

/// KiCad の言語の表示名を言語タグにする。"Default"（OS に従う）や知らない名前は None
pub fn language_tag_of(label: &str) -> Option<&'static str> {
    LANGUAGES.iter().find(|(l, _)| *l == label.trim()).map(|(_, t)| *t)
}

/// KiCad で選んだ表示の言語（一番新しい版の設定）。"Default"（OS に従う）や読めないときは None
pub fn ui_language() -> Option<&'static str> {
    let file = kicad_common_files().pop()?;
    let text = std::fs::read_to_string(&file).ok()?;
    let j: serde_json::Value = serde_json::from_str(&text).ok()?;
    language_tag_of(j.pointer("/system/language")?.as_str()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_labels() {
        assert_eq!(language_tag_of("日本語"), Some("ja"));
        assert_eq!(language_tag_of("English"), Some("en"));
        assert_eq!(language_tag_of("Português (Brasil)"), Some("pt-BR"));
        assert_eq!(language_tag_of("Default"), None);
        assert_eq!(language_tag_of(""), None);
    }

    #[test]
    fn versions_sort_numerically() {
        let mut v = vec![version_of("10.0").unwrap(), version_of("9.0").unwrap(), version_of("9.99").unwrap()];
        v.sort();
        assert_eq!(v, vec![vec![9, 0], vec![9, 99], vec![10, 0]]);
        assert_eq!(version_of("colors"), None);
    }
}
