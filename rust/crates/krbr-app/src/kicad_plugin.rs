//! KiCad のプラグイン「KRB Renderer でプレビュー」（PCB エディタのツールバーのボタン）の登録・解除。
//! プラグインのファイル（<repo>/kicad_plugin/krbr_preview）は exe に埋め込んであり、KiCad のユーザーフォルダの
//! <版>\scripting\plugins\krbr_preview に書き出す。あわせて、この krbr.exe の場所を krbr_path.txt に書く。
//! KiCad は版（9.0、10.0、nightly の 10.99 など）ごとに別のユーザーフォルダを使い、その版のフォルダのプラグインだけを読むので、
//! 登録・解除は版ごとに行う。ビューアの「設定 → KiCad プラグイン」と `krbr kicad-plugin` から使う

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use krbr_core::{info, warn};

use crate::fl;

const FILES: &[(&str, &[u8])] = &[
    ("__init__.py", include_bytes!("../../../../kicad_plugin/krbr_preview/__init__.py")),
    ("krbr_preview_action.py", include_bytes!("../../../../kicad_plugin/krbr_preview/krbr_preview_action.py")),
    ("krbr_preview.png", include_bytes!("../../../../kicad_plugin/krbr_preview/krbr_preview.png")),
    ("krbr_preview_48.png", include_bytes!("../../../../kicad_plugin/krbr_preview/krbr_preview_48.png")),
];
const DIR_NAME: &str = "krbr_preview";
const PATH_FILE: &str = "krbr_path.txt";
/// プラグイン（SWIG の Action Plugin）が動く最初の版
const FIRST_SUPPORTED: [u32; 2] = [9, 0];
/// SWIG の Action Plugin が無くなる予定の KiCad 11 の開発版（nightly の 10.99）から先は、動かないことがある
const SWIG_MAY_BE_GONE: [u32; 2] = [10, 99];

/// 見つかった KiCad の版
#[derive(Debug, Clone)]
pub struct KicadVersion {
    /// 版（"10.0"。ユーザーフォルダの名前と同じ）
    pub name: String,
    version: [u32; 2],
    /// <ドキュメント>\KiCad\<版>\scripting\plugins（まだ無いこともある。登録するときに作る）
    pub plugins: PathBuf,
    /// KiCad の本体のインストール先（見つからなければ None：ユーザーフォルダだけが残っている）
    pub program: Option<PathBuf>,
}

/// この版でプラグインが動くか
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// KiCad 8 以前（入れられない）
    TooOld,
    Yes,
    /// SWIG が無くなる予定の版（入れられるが、動かないことがある）
    MayBreak,
}

/// この版での登録の状態
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    NotInstalled,
    /// この krbr.exe を指していて、ファイルも新しい
    Installed,
    /// この krbr.exe を指しているが、プラグインのファイルが古い（krbr.exe を更新した）
    Outdated,
    /// 別の krbr.exe（まだ有る）を指している
    OtherExe(PathBuf),
    /// 指している krbr.exe が無い（フォルダを移した・消した）。空なら krbr_path.txt が無い
    MissingExe(PathBuf),
}

impl KicadVersion {
    pub fn support(&self) -> Support {
        if self.version < FIRST_SUPPORTED {
            Support::TooOld
        } else if self.version >= SWIG_MAY_BE_GONE {
            Support::MayBreak
        } else {
            Support::Yes
        }
    }

    /// プラグインのフォルダ（scripting\plugins\krbr_preview）
    pub fn plugin_dir(&self) -> PathBuf {
        self.plugins.join(DIR_NAME)
    }

    pub fn state(&self, exe: &Path) -> State {
        state_of(&self.plugin_dir(), exe)
    }

    pub fn install(&self, exe: &Path) -> std::io::Result<PathBuf> {
        install_into(&self.plugins, exe)
    }

    /// 解除する。入っていたら true
    pub fn uninstall(&self) -> std::io::Result<bool> {
        let d = self.plugin_dir();
        if !d.is_dir() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&d)?;
        Ok(true)
    }
}

/// フォルダ名の版（"10.0" → [10, 0]、"9.0.3" → [9, 0]）。数字で始まらなければ None
fn version_of(name: &str) -> Option<[u32; 2]> {
    let mut parts = name.trim().split('.').map(|s| s.parse::<u32>().ok());
    let major = parts.next()??;
    Some([major, parts.next().flatten().unwrap_or(0)])
}

/// KiCad のユーザーフォルダの置き場所（KICAD_DOCUMENTS_HOME、無ければ <ドキュメント>\KiCad）。この下に版ごとのフォルダがある
fn documents_home() -> Option<PathBuf> {
    std::env::var_os("KICAD_DOCUMENTS_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| krbr_core::platform::documents_dir().map(|d| d.join("KiCad")))
}

/// KiCad の本体のインストール先（版, フォルダ）：レジストリのアンインストールの情報（"KiCad 10.0" の InstallLocation。
/// Program Files 以外に入れたものも見つかる）と、Program Files\KiCad\<版>
pub(crate) fn installed_programs() -> Vec<([u32; 2], PathBuf)> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        use krbr_core::platform::registry::{Key, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
        const UNINSTALL: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall";
        for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            let Some(key) = Key::open(root, UNINSTALL) else { continue };
            for sub in key.subkeys() {
                let Some(rest) = sub.strip_prefix("KiCad ") else { continue };
                let Some(dir) = key.string(&sub, "InstallLocation").filter(|s| !s.is_empty()).map(PathBuf::from) else { continue };
                // 版はキーの名前から。読めなければインストール先のフォルダ名（Program Files\KiCad\10.99）から
                let v = version_of(rest).or_else(|| dir.file_name().and_then(|n| version_of(&n.to_string_lossy())));
                if let Some(v) = v.filter(|_| dir.is_dir()) {
                    out.push((v, dir));
                }
            }
        }
        for env in ["ProgramFiles", "ProgramW6432"] {
            let Some(pf) = std::env::var_os(env) else { continue };
            let Ok(rd) = std::fs::read_dir(Path::new(&pf).join("KiCad")) else { continue };
            for e in rd.flatten() {
                if let Some(v) = version_of(&e.file_name().to_string_lossy()).filter(|_| e.path().join("bin").join("kicad.exe").is_file()) {
                    out.push((v, e.path()));
                }
            }
        }
    }
    out
}

/// 見つかった KiCad の版（本体かユーザーフォルダのどちらかが有るもの）。新しい版から
pub fn kicad_versions() -> Vec<KicadVersion> {
    let Some(docs) = documents_home() else { return Vec::new() };
    let mut found: BTreeMap<[u32; 2], Option<PathBuf>> = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(&docs) {
        for e in rd.flatten().filter(|e| e.path().is_dir()) {
            if let Some(v) = version_of(&e.file_name().to_string_lossy()) {
                found.entry(v).or_default();
            }
        }
    }
    for (v, dir) in installed_programs() {
        found.entry(v).or_default().get_or_insert(dir);
    }
    found
        .into_iter()
        .rev()
        .map(|(v, program)| {
            let name = format!("{}.{}", v[0], v[1]);
            KicadVersion { plugins: docs.join(&name).join("scripting").join("plugins"), name, version: v, program }
        })
        .collect()
}

/// この krbr.exe の場所（KiCad の Python へ渡すので、canonicalize の \\?\ を外した普通の形）
pub fn current_exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(std::fs::canonicalize(&exe).map(strip_verbatim).unwrap_or(exe))
}

/// 同じファイルを指すパスか（大文字・小文字と \\?\ の違いは同じとみなす）
fn same_file(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| std::fs::canonicalize(p).map(strip_verbatim).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase();
    norm(a) == norm(b)
}

fn state_of(dir: &Path, exe: &Path) -> State {
    if !dir.is_dir() {
        return State::NotInstalled;
    }
    let recorded = std::fs::read_to_string(dir.join(PATH_FILE)).map(|s| PathBuf::from(s.trim())).unwrap_or_default();
    if recorded.as_os_str().is_empty() || !recorded.is_file() {
        State::MissingExe(recorded)
    } else if !same_file(&recorded, exe) {
        State::OtherExe(recorded)
    } else if FILES.iter().all(|(name, data)| std::fs::read(dir.join(name)).is_ok_and(|d| d == *data)) {
        State::Installed
    } else {
        State::Outdated
    }
}

/// plugins（scripting\plugins）の下にプラグインを書き、書いたフォルダを返す
pub fn install_into(plugins: &Path, exe: &Path) -> std::io::Result<PathBuf> {
    let dir = plugins.join(DIR_NAME);
    std::fs::create_dir_all(&dir)?;
    for (name, data) in FILES {
        std::fs::write(dir.join(name), data)?;
    }
    std::fs::write(dir.join(PATH_FILE), exe.to_string_lossy().as_bytes())?;
    Ok(dir)
}

/// ビューアの起動時に、登録済みの版を直す：指している krbr.exe が無くなった版（フォルダを移した）は、この exe を指すように書き直し、
/// この exe を指していてファイルが古い版（krbr.exe を更新した）は、ファイルを書き直す。
/// 未登録の版には入れない。別の krbr.exe（まだ有る）を指している版にも触らない（開発中のビルドと配布用の一式で取り合わないように）
pub fn repair_installed() {
    let Ok(exe) = current_exe() else { return };
    for v in kicad_versions() {
        if v.support() == Support::TooOld {
            continue;
        }
        let why = match v.state(&exe) {
            State::MissingExe(_) => "the krbr.exe it pointed to is gone",
            State::Outdated => "its files were older than this krbr.exe",
            _ => continue,
        };
        match v.install(&exe) {
            Ok(d) => info!("KiCad {} plugin repaired ({why}): {} -> {}", v.name, d.display(), exe.display()),
            Err(e) => warn!("KiCad {} plugin: cannot repair {}: {e}", v.name, v.plugin_dir().display()),
        }
    }
}

/// --kicad で選んだ版（省けば、入れられる版すべて）
fn select_versions(kicad: Option<&str>) -> anyhow::Result<Vec<KicadVersion>> {
    let all = kicad_versions();
    let Some(want) = kicad else { return Ok(all.into_iter().filter(|v| v.support() != Support::TooOld).collect()) };
    let Some(wv) = version_of(want) else { anyhow::bail!("--kicad {want}: give a version such as 10.0") };
    match all.into_iter().find(|v| v.version == wv) {
        Some(v) if v.support() == Support::TooOld => anyhow::bail!("the plugin needs KiCad 9 or later (asked for {})", v.name),
        Some(v) => Ok(vec![v]),
        None => anyhow::bail!("KiCad {want} was not found (see krbr kicad-plugin status)"),
    }
}

/// `krbr kicad-plugin status`
pub fn cmd_status() -> anyhow::Result<()> {
    let exe = current_exe()?;
    let versions = kicad_versions();
    if versions.is_empty() {
        info!("no KiCad found (neither a user folder under {} nor an installation)", documents_home().unwrap_or_default().display());
    }
    for v in &versions {
        let state = match (v.support(), v.state(&exe)) {
            (Support::TooOld, _) => "not supported (needs KiCad 9 or later)".to_string(),
            (_, State::NotInstalled) => "not installed".to_string(),
            (_, State::Installed) => "installed".to_string(),
            (_, State::Outdated) => "installed, files older than this krbr.exe".to_string(),
            (_, State::OtherExe(p)) => format!("installed for another krbr.exe ({})", p.display()),
            (_, State::MissingExe(p)) => format!("installed, but its krbr.exe is gone ({})", p.display()),
        };
        let program = v.program.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "program not found, user folder only".into());
        let may_break = if v.support() == Support::MayBreak { " [may not work: KiCad 11 drops SWIG plugins]" } else { "" };
        info!("KiCad {}: {state}{may_break}\n  program: {program}\n  plugin:  {}", v.name, v.plugin_dir().display());
    }
    info!("this krbr.exe: {}", exe.display());
    Ok(())
}

/// `krbr kicad-plugin install`。dir（scripting\plugins）を指定しなければ、--kicad の版、省けば見つかった KiCad 9 以降のすべての版に入れる
pub fn cmd_install(dir: Option<&Path>, kicad: Option<&str>) -> anyhow::Result<()> {
    let exe = current_exe()?;
    let targets: Vec<(String, PathBuf)> = match dir {
        Some(d) => vec![(d.display().to_string(), d.to_path_buf())],
        None => select_versions(kicad)?.into_iter().map(|v| (format!("KiCad {}", v.name), v.plugins)).collect(),
    };
    if targets.is_empty() {
        anyhow::bail!("no KiCad 9+ found (use --dir <...\\scripting\\plugins>)");
    }
    for (name, plugins) in targets {
        let d = install_into(&plugins, &exe)?;
        info!("{name}: installed the plugin into {} (krbr: {})", d.display(), exe.display());
    }
    info!("restart KiCad, or in the PCB editor use Tools > External Plugins > Refresh Plugins; the toolbar gets a 'Preview in KRB Renderer' button");
    Ok(())
}

/// `krbr kicad-plugin uninstall`
pub fn cmd_uninstall(dir: Option<&Path>, kicad: Option<&str>) -> anyhow::Result<()> {
    let targets: Vec<PathBuf> = match dir {
        Some(d) => vec![d.join(DIR_NAME)],
        None => select_versions(kicad)?.iter().map(KicadVersion::plugin_dir).collect(),
    };
    let mut removed = 0;
    for d in &targets {
        if d.is_dir() {
            std::fs::remove_dir_all(d)?;
            info!("removed {}", d.display());
            removed += 1;
        }
    }
    if removed == 0 {
        info!("the KiCad plugin is not installed; nothing removed");
    }
    Ok(())
}

/// canonicalize の \\?\ を外す（KiCad の Python へ渡すパスは普通の形にする）
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p,
    }
}

/// ビューアの「設定 → KiCad プラグイン」の窓：見つかった KiCad の版ごとに、状態と登録・解除のボタン
#[derive(Default)]
pub struct PluginWindow {
    pub open: bool,
    exe: PathBuf,
    rows: Vec<(KicadVersion, State)>,
    /// 最後にした操作の結果
    message: String,
}

impl PluginWindow {
    /// 開く（開くたびに KiCad の版と状態を調べ直す）
    pub fn show(&mut self) {
        self.open = true;
        self.message.clear();
        self.rescan();
    }

    fn rescan(&mut self) {
        self.exe = current_exe().unwrap_or_default();
        let exe = &self.exe;
        self.rows = kicad_versions()
            .into_iter()
            .map(|v| {
                let s = v.state(exe);
                (v, s)
            })
            .collect();
    }

    /// 行 i の版に登録する（install）か、解除する
    fn run(&mut self, i: usize, install: bool) {
        let Some((v, _)) = self.rows.get(i) else { return };
        let name = v.name.clone();
        self.message = if install {
            match v.install(&self.exe) {
                Ok(d) => {
                    info!("KiCad {name}: installed the plugin into {} (krbr: {})", d.display(), self.exe.display());
                    fl!("kplug-installed", version = name)
                }
                Err(e) => {
                    warn!("KiCad {name}: cannot install the plugin: {e}");
                    fl!("kplug-failed", error = e.to_string())
                }
            }
        } else {
            match v.uninstall() {
                Ok(_) => {
                    info!("KiCad {name}: removed {}", v.plugin_dir().display());
                    fl!("kplug-uninstalled", version = name)
                }
                Err(e) => {
                    warn!("KiCad {name}: cannot remove the plugin: {e}");
                    fl!("kplug-failed", error = e.to_string())
                }
            }
        };
        self.rescan();
    }

    /// WebView の窓（web/kicad_plugin.html）に渡す JSON。ボタンは "kplug.install.<行>" などのクリックで返ってくる
    pub fn json(&self) -> String {
        let rows: Vec<serde_json::Value> = self
            .rows
            .iter()
            .map(|(v, state)| {
                let support = v.support();
                let (text, install) = match state {
                    State::NotInstalled => (fl!("kplug-state-not-installed"), Some(fl!("kplug-install"))),
                    State::Installed => (fl!("kplug-state-installed"), None),
                    State::Outdated => (fl!("kplug-state-outdated"), Some(fl!("kplug-update"))),
                    State::OtherExe(p) => (fl!("kplug-state-other-exe", path = p.display().to_string()), Some(fl!("kplug-switch"))),
                    State::MissingExe(p) if p.as_os_str().is_empty() => (fl!("kplug-state-no-path"), Some(fl!("kplug-switch"))),
                    State::MissingExe(p) => (fl!("kplug-state-missing-exe", path = p.display().to_string()), Some(fl!("kplug-switch"))),
                };
                let too_old = support == Support::TooOld;
                serde_json::json!({
                    "name": format!("KiCad {}", v.name),
                    "dir": v.plugin_dir().display().to_string(),
                    "tooOld": too_old,
                    "state": if too_old { fl!("kplug-too-old") } else { text },
                    "installed": *state == State::Installed,
                    "install": install.filter(|_| !too_old),
                    "removable": !too_old && *state != State::NotInstalled,
                    "noProgram": v.program.is_none().then(|| fl!("kplug-no-program")),
                    "mayBreak": (support == Support::MayBreak).then(|| fl!("kplug-may-break")),
                })
            })
            .collect();
        serde_json::json!({
            "text": {
                "title": fl!("kplug-title"), "intro": fl!("kplug-intro"), "none": fl!("kplug-none"), "uninstall": fl!("kplug-uninstall"),
                "openFolder": fl!("kplug-open-folder"), "rescan": fl!("kplug-rescan"), "restart": fl!("kplug-restart-hint"),
                "exe": fl!("kplug-this-exe", path = self.exe.display().to_string()), "close": fl!("kplug-close"),
            },
            "rows": rows,
            "message": self.message,
        })
        .to_string()
    }

    /// WebView の窓からの操作（"kplug.<操作>[.<行>]"）
    pub fn click(&mut self, id: &str) {
        let (op, row) = match id.strip_prefix("kplug.").and_then(|r| r.rsplit_once('.')) {
            Some((op, n)) if n.parse::<usize>().is_ok() => (op, n.parse::<usize>().ok()),
            _ => (id.strip_prefix("kplug.").unwrap_or(id), None),
        };
        match (op, row) {
            ("close", _) => self.open = false,
            ("rescan", _) => {
                self.message.clear();
                self.rescan();
            }
            ("install", Some(i)) => self.run(i, true),
            ("uninstall", Some(i)) => self.run(i, false),
            ("folder", Some(i)) => {
                if let Some((v, _)) = self.rows.get(i) {
                    krbr_core::platform::reveal_in_file_manager(&v.plugin_dir().join(PATH_FILE));
                }
            }
            _ => {}
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("krbr_plugin_test_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn install_writes_the_plugin_and_the_exe_path() {
        let dir = temp("install");
        let d = install_into(&dir, Path::new(r"C:\x\krbr.exe")).unwrap();
        for (name, data) in FILES {
            assert_eq!(&std::fs::read(d.join(name)).unwrap(), data);
        }
        assert_eq!(std::fs::read_to_string(d.join(PATH_FILE)).unwrap(), r"C:\x\krbr.exe");
        cmd_uninstall(Some(&dir), None).unwrap();
        assert!(!d.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn state_tells_which_exe_the_plugin_points_to() {
        let dir = temp("state");
        let exe = current_exe().unwrap();
        let d = dir.join(DIR_NAME);
        assert_eq!(state_of(&d, &exe), State::NotInstalled);
        install_into(&dir, &exe).unwrap();
        assert_eq!(state_of(&d, &exe), State::Installed);
        std::fs::write(d.join("krbr_preview_action.py"), b"# old").unwrap();
        assert_eq!(state_of(&d, &exe), State::Outdated);
        // 別の（有る）ファイルを指す・無いファイルを指す・場所のファイルが無い
        let other = dir.join("other.exe");
        std::fs::write(&other, b"").unwrap();
        install_into(&dir, &other).unwrap();
        assert_eq!(state_of(&d, &exe), State::OtherExe(other.clone()));
        let gone = dir.join("gone").join("krbr.exe");
        install_into(&dir, &gone).unwrap();
        assert_eq!(state_of(&d, &exe), State::MissingExe(gone));
        std::fs::remove_file(d.join(PATH_FILE)).unwrap();
        assert_eq!(state_of(&d, &exe), State::MissingExe(PathBuf::new()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn versions_and_support() {
        assert_eq!(version_of("10.0"), Some([10, 0]));
        assert_eq!(version_of("9.0.3"), Some([9, 0]));
        assert_eq!(version_of("10"), Some([10, 0]));
        assert_eq!(version_of("Nightly"), None);
        let v = |version: [u32; 2]| KicadVersion { name: String::new(), version, plugins: PathBuf::new(), program: None };
        assert_eq!(v([8, 0]).support(), Support::TooOld);
        assert_eq!(v([9, 0]).support(), Support::Yes);
        assert_eq!(v([10, 0]).support(), Support::Yes);
        assert_eq!(v([10, 99]).support(), Support::MayBreak);
        assert_eq!(v([11, 0]).support(), Support::MayBreak);
    }

    /// 版の探し方と、起動時の修復（KICAD_DOCUMENTS_HOME を使うテストはこれだけ。環境変数を取り合わないように 1 つにまとめる）
    #[test]
    fn versions_are_found_newest_first_and_repaired() {
        let docs = temp("docs");
        for d in ["9.0", "10.0", "10.99", "8.0", "11.0", "notes"] {
            std::fs::create_dir_all(docs.join(d)).unwrap();
        }
        let plugins = |v: &str| docs.join(v).join("scripting").join("plugins");
        let exe = current_exe().unwrap();
        let other = docs.join("other.exe");
        std::fs::write(&other, b"").unwrap();
        let gone = docs.join("gone").join("krbr.exe");
        install_into(&plugins("9.0"), &gone).unwrap(); // フォルダを移した
        install_into(&plugins("10.0"), &exe).unwrap(); // krbr.exe を更新した（ファイルが古い）
        std::fs::write(plugins("10.0").join(DIR_NAME).join("__init__.py"), b"# old").unwrap();
        install_into(&plugins("10.99"), &other).unwrap(); // 別の krbr.exe
        install_into(&plugins("8.0"), &gone).unwrap(); // 対象外の版

        std::env::set_var("KICAD_DOCUMENTS_HOME", &docs);
        let found = kicad_versions();
        repair_installed();
        std::env::remove_var("KICAD_DOCUMENTS_HOME");

        // この PC に入っている KiCad の版が混ざることがあるので、作ったものの並びだけを見る
        let ours: Vec<&str> = found.iter().map(|v| v.name.as_str()).filter(|n| ["11.0", "10.99", "10.0", "9.0", "8.0"].contains(n)).collect();
        assert_eq!(ours, ["11.0", "10.99", "10.0", "9.0", "8.0"]);
        assert_eq!(found.iter().find(|v| v.name == "9.0").unwrap().plugins, plugins("9.0"));
        let state = |v: &str| state_of(&plugins(v).join(DIR_NAME), &exe);
        assert_eq!(state("9.0"), State::Installed);
        assert_eq!(state("10.0"), State::Installed);
        assert_eq!(state("10.99"), State::OtherExe(other.clone()));
        assert_eq!(state("8.0"), State::MissingExe(gone.clone()));
        assert_eq!(state("11.0"), State::NotInstalled);
        let _ = std::fs::remove_dir_all(&docs);
    }
}
