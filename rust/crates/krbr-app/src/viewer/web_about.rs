//! 「ヘルプ → KRB Renderer について」（--web-ui）：版・コミット・新しい版の有無・GitHub のページ・ライセンスと著作権・
//! 使用ライブラリ・動作環境の窓（web/about.html）に渡す JSON。新しい版の有無は、開いたときにページが GitHub の
//! リリースの API に問い合わせる（起動時には問い合わせない）

use serde_json::{json, Value};

use super::*;

/// GitHub のプロジェクトのページ
pub(super) const REPO_URL: &str = "https://github.com/kagurasanagi/KRB-Renderer";
const LICENSE: &str = include_str!("../../../../../LICENSE");
const THIRD_PARTY: &str = include_str!("../../web/third_party.json");
pub(super) const ICON_PNG: &[u8] = include_bytes!("../../../../krbr/assets/krbr_256.png");
/// 画像の置き場でのアイコンの名前
pub(super) const ICON_PATH: &str = "about/icon.png";

/// 画像の置き場にアイコンを置く（WebView を作ったとき一度）
pub(super) fn publish_icon(images: &crate::web_ui::Images) {
    if let Ok(mut m) = images.lock() {
        m.insert(ICON_PATH.to_string(), ICON_PNG.to_vec());
    }
}

/// Vulkan の版の数（major.minor.patch）
fn vk_version(v: u32) -> String {
    format!("{}.{}.{}", v >> 22, (v >> 12) & 0x3ff, v & 0xfff)
}

/// ドライバの版（NVIDIA は独自の詰め方）
fn driver_version(name: &str, v: u32) -> String {
    if name.contains("NVIDIA") {
        format!("{}.{:02}", v >> 22, (v >> 14) & 0xff)
    } else {
        vk_version(v)
    }
}

/// exe の横の licenses フォルダ（配布の zip。開発中のビルドには無い）
fn license_dir() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.join("licenses");
    dir.is_dir().then_some(dir)
}

fn lib(name: &str, license: &str, url: &str, note: String) -> Value {
    json!({ "name": name, "license": license, "url": url, "note": note })
}

impl App {
    pub(super) fn about_json(&self) -> String {
        let version = env!("CARGO_PKG_VERSION");
        let (hash, date) = (env!("KRBR_GIT_HASH"), env!("KRBR_GIT_DATE"));
        let mut env_rows = vec![(fl!("about-env-app"), format!("KRB Renderer {version}{}", if hash.is_empty() { String::new() } else { format!(" ({hash}, {date})") })), (fl!("about-env-os"), krbr_core::platform::os_description())];
        if let Some(g) = &self.gpu {
            let i = &g.ctx.info;
            env_rows.push((fl!("about-env-gpu"), format!("{}{}", i.name, if i.discrete { "" } else { " (integrated)" })));
            env_rows.push((fl!("about-env-driver"), driver_version(&i.name, i.driver_version)));
            env_rows.push((fl!("about-env-vulkan"), vk_version(i.api_version)));
            env_rows.push((fl!("about-env-trace"), trace_method_name(g.ctx).to_string()));
        }
        if let Ok(v) = wry::webview_version() {
            env_rows.push((fl!("about-env-webview"), v));
        }
        let ja = self.language.starts_with("ja");
        let native = vec![
            lib("Open CASCADE Technology (OCCT)", "LGPL-2.1 with exception", "https://dev.opencascade.org/", if ja { "STEP の 3D モデルの読み込み" } else { "Reading STEP 3D models" }.into()),
            lib("Intel Open Image Denoise", "Apache-2.0", "https://www.openimagedenoise.org/", if ja { "ノイズ除去" } else { "Denoising" }.into()),
            lib("oneTBB", "Apache-2.0", "https://github.com/uxlfoundation/oneTBB", if ja { "Open Image Denoise が使う" } else { "Used by Open Image Denoise" }.into()),
            lib("Microsoft Edge WebView2", "Microsoft Software License", "https://developer.microsoft.com/microsoft-edge/webview2/", if ja { "画面（OS に入っているランタイム）" } else { "User interface (runtime installed with Windows)" }.into()),
            lib("Microsoft Visual C++ Runtime", "Microsoft Software License", "https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist", if ja { "再頒布用のファイル（dll\\vcruntime）" } else { "Redistributable files (dll\\vcruntime)" }.into()),
            lib("Vulkan", "Apache-2.0 (loader)", "https://www.vulkan.org/", if ja { "GPU の描画（ローダーはドライバに付属）" } else { "GPU rendering (the loader comes with the driver)" }.into()),
        ];
        let assets = vec![lib(
            "KiCad Libraries (bench)",
            "CC-BY-SA-4.0 with exception",
            "https://www.kicad.org/libraries/license/",
            if ja { "krbr bench の基板の 3D モデルとフットプリント" } else { "3D models and footprints of the krbr bench board" }.into(),
        )];
        let mut licenses = vec![json!({ "name": "KRB Renderer (MIT)", "text": LICENSE })];
        if let Some(dir) = license_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "txt")).collect();
            files.sort();
            for f in files {
                let name = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                if name.starts_with("KRB-Renderer") {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&f) {
                    licenses.push(json!({ "name": name, "text": text }));
                }
            }
        }
        let crates: Value = serde_json::from_str(THIRD_PARTY).unwrap_or(Value::Array(Vec::new()));
        let count = crates.as_array().map(|a| a.len()).unwrap_or(0);
        json!({
            "icon": format!("{}{ICON_PATH}", crate::web_ui::IMAGE_URL),
            "name": "KRB Renderer", "tagline": fl!("about-tagline"),
            "version": version, "versionText": fl!("about-version", version = version),
            "commitText": (!hash.is_empty()).then(|| fl!("about-commit", hash = hash, date = date)),
            "repo": REPO_URL, "releases": format!("{REPO_URL}/releases"), "issues": format!("{REPO_URL}/issues"),
            "api": "https://api.github.com/repos/kagurasanagi/KRB-Renderer/releases/latest",
            "text": {
                "title": fl!("about-title"), "checking": fl!("about-update-checking"), "latest": fl!("about-update-latest", latest = "{latest}"),
                "newer": fl!("about-update-new", latest = "{latest}"), "none": fl!("about-update-none"), "failed": fl!("about-update-failed", error = "{error}"),
                "download": fl!("about-update-download"), "github": fl!("about-github"), "releases": fl!("about-releases"), "issues": fl!("about-issues"),
                "copy": fl!("about-copy"), "copied": fl!("about-copied"), "ok": fl!("about-ok"), "close": fl!("panel-close-tip"),
                "tabAbout": fl!("about-tab-about"), "tabLibraries": fl!("about-tab-libraries"), "tabLicense": fl!("about-tab-license"), "tabEnv": fl!("about-tab-env"),
                "description": fl!("about-description"), "madeWith": fl!("about-made-with"), "copyright": fl!("about-copyright"), "licenseShort": fl!("about-license-short"),
                "links": fl!("about-links"), "native": fl!("about-native"), "crates": fl!("about-crates", count = count.to_string()), "assets": fl!("about-assets"),
                "licenseFiles": fl!("about-license-files"), "licenseFolder": fl!("about-license-folder"), "webview": fl!("about-env-webview"),
            },
            "hasLicenseFolder": license_dir().is_some(),
            "native": native, "assets": assets, "crates": crates, "licenses": licenses,
            "env": env_rows.into_iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
        })
        .to_string()
    }

    /// egui の画面のときの「について」（版・著作権・ライセンス・GitHub のページだけ）
    pub(super) fn about_egui(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let mut open = true;
        egui::Window::new(fl!("about-title")).open(&mut open).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
            ui.heading("KRB Renderer");
            ui.label(fl!("about-tagline"));
            ui.label(fl!("about-version", version = env!("CARGO_PKG_VERSION")));
            let hash = env!("KRBR_GIT_HASH");
            if !hash.is_empty() {
                ui.weak(fl!("about-commit", hash = hash, date = env!("KRBR_GIT_DATE")));
            }
            ui.separator();
            ui.label(fl!("about-made-with"));
            ui.label(fl!("about-copyright"));
            ui.label(fl!("about-license-short"));
            ui.hyperlink_to(fl!("about-github"), REPO_URL);
            ui.hyperlink_to(fl!("about-releases"), format!("{REPO_URL}/releases"));
        });
        self.about_open = open;
    }

    /// 「について」の窓からの操作
    pub(super) fn apply_about_click(&mut self, id: &str) {
        match id {
            "about.close" => self.about_open = false,
            "about.licenses" => {
                if let Some(d) = license_dir() {
                    if let Err(e) = std::process::Command::new("explorer").arg(&d).spawn() {
                        warn!("cannot open {}: {e}", d.display());
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_bundled_lists() {
        assert_eq!(vk_version((1 << 22) | (3 << 12) | 280), "1.3.280");
        assert_eq!(driver_version("NVIDIA GeForce RTX 3060", (621 << 22) | (10 << 14)), "621.10");
        let crates: Value = serde_json::from_str(THIRD_PARTY).unwrap();
        assert!(crates.as_array().unwrap().iter().any(|c| c["name"] == "wry"));
        assert!(LICENSE.starts_with("MIT License"));
        assert_eq!(&ICON_PNG[1..4], b"PNG");
    }
}
