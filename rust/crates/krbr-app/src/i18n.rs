//! ビューアの表示の言語（Fluent。翻訳は `i18n/<言語>/krbr-app.ftl` で、exe に埋め込む。英語が基準で、訳の無い文は英語で出す）。
//! 言語の決め方：`--lang` / 環境変数 KRBR_LANG、ビューアのメニューで選んだもの（state.toml）、KiCad で選んだ言語、OS の言語、英語。
//! ログとコマンドラインの説明は英語のまま。

use std::sync::LazyLock;

use i18n_embed::fluent::{fluent_language_loader, FluentLanguageLoader};
use i18n_embed::LanguageLoader;
use krbr_core::{debug, warn};
use rust_embed::RustEmbed;
use unic_langid::LanguageIdentifier;

#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Localizations;

pub static LOADER: LazyLock<FluentLanguageLoader> = LazyLock::new(|| {
    let loader = fluent_language_loader!();
    // 英語だけは必ず読む（訳の無い文の代わり）
    if let Err(e) = loader.load_fallback_language(&Localizations) {
        warn!("translations: {e}");
    }
    loader.set_use_isolating(false); // egui は双方向の制御文字を描けないので、差し込みを囲まない
    loader
});

/// 翻訳した文（`fl!("id")`、`fl!("id", name = value)`）。id と引数は、英語の .ftl にあるかをコンパイル時に確かめる
#[macro_export]
macro_rules! fl {
    ($id:literal) => {{
        i18n_embed_fl::fl!($crate::i18n::LOADER, $id)
    }};
    ($id:literal, $($args:expr),*) => {{
        i18n_embed_fl::fl!($crate::i18n::LOADER, $id, $($args),*)
    }};
}

/// id を実行時に組み立てる文（無ければ None）
pub fn get(id: &str) -> Option<String> {
    LOADER.has(id).then(|| LOADER.get(id))
}

/// 選べる言語（言語タグ, その言語での名前）
pub const LANGUAGES: &[(&str, &str)] = &[("en", "English"), ("ja", "日本語")];

/// 自動のときの言語：KiCad で選んだ言語、OS の言語の順（どれも翻訳が無ければ英語になる）
fn automatic() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if let Some(k) = krbr_kicad::settings::ui_language() {
        debug!("language: KiCad uses {k}");
        v.push(k.to_string());
    }
    v.extend(sys_locale::get_locales());
    v
}

/// 言語を決めて読み込む。explicit：`--lang` / KRBR_LANG、chosen：メニューで選んだもの（None なら自動）。決まった言語タグを返す
pub fn select(explicit: Option<&str>, chosen: Option<&str>) -> String {
    let env = std::env::var("KRBR_LANG").ok();
    let mut wanted: Vec<String> = [explicit.or(env.as_deref()), chosen].into_iter().flatten().filter(|s| !s.trim().is_empty()).map(|s| s.trim().to_string()).collect();
    wanted.extend(automatic());
    let ids: Vec<LanguageIdentifier> = wanted.iter().filter_map(|s| s.replace('_', "-").parse().ok()).collect();
    // 先頭に近いものから、翻訳のある言語を 1 つ選ぶ（"ja-JP" は "ja" に当てる）
    let lang = ids.iter().find_map(|id| LANGUAGES.iter().find(|(t, _)| *t == id.language.as_str())).map(|(t, _)| *t).unwrap_or("en");
    let id: LanguageIdentifier = lang.parse().unwrap();
    if let Err(e) = LOADER.load_languages(&Localizations, &[id]) {
        warn!("translations ({lang}): {e}");
    }
    LOADER.set_use_isolating(false);
    debug!("language: {lang} (requested: {})", wanted.join(", "));
    lang.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// どの言語にも、英語と同じ id の文がそろっている
    #[test]
    fn translations_are_complete() {
        let ids = |lang: &str| -> Vec<String> {
            let f = Localizations::get(&format!("{lang}/krbr-app.ftl")).unwrap();
            let text = std::str::from_utf8(&f.data).unwrap().to_string();
            let mut v: Vec<String> = text.lines().filter(|l| !l.starts_with([' ', '#', '.']) && l.contains(" =")).map(|l| l.split(" =").next().unwrap().trim().to_string()).collect();
            v.sort();
            v
        };
        let en = ids("en");
        for (lang, _) in LANGUAGES {
            assert_eq!(ids(lang), en, "{lang}");
        }
    }

    #[test]
    fn explicit_language_wins() {
        assert_eq!(select(Some("ja-JP"), Some("en")), "ja");
        assert_eq!(fl!("menu-file"), "ファイル");
        assert_eq!(select(Some("en"), Some("ja")), "en");
        assert_eq!(fl!("menu-file"), "File");
        // 翻訳の無い言語は、次の候補か英語
        assert_eq!(select(Some("xx"), Some("ja")), "ja");
    }
}
