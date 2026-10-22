//! KiCad のプラグイン「KRB Render でプレビュー」（PCB エディタのツールバーのボタン）のインストール。
//! プラグインのファイル（<repo>/kicad_plugin/krbr_preview）は exe に埋め込んであり、KiCad のユーザーフォルダの
//! scripting/plugins/krbr_preview に書き出す。あわせて、この krbr.exe の場所を krbr_path.txt に書く

use std::path::{Path, PathBuf};

use krbr_core::info;

const FILES: &[(&str, &[u8])] = &[
    ("__init__.py", include_bytes!("../../../../kicad_plugin/krbr_preview/__init__.py")),
    ("krbr_preview_action.py", include_bytes!("../../../../kicad_plugin/krbr_preview/krbr_preview_action.py")),
    ("krbr_preview.png", include_bytes!("../../../../kicad_plugin/krbr_preview/krbr_preview.png")),
    ("krbr_preview_48.png", include_bytes!("../../../../kicad_plugin/krbr_preview/krbr_preview_48.png")),
];
const DIR_NAME: &str = "krbr_preview";

/// KiCad 9 以降のユーザーフォルダ（ドキュメント\KiCad\<版>）の scripting\plugins。版の新しい順
pub fn kicad_plugin_dirs() -> Vec<PathBuf> {
    let docs = std::env::var_os("KICAD_DOCUMENTS_HOME").map(PathBuf::from).or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Documents").join("KiCad")));
    let Some(docs) = docs else { return Vec::new() };
    let mut out: Vec<(f64, PathBuf)> = std::fs::read_dir(&docs)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let v: f64 = e.file_name().to_string_lossy().parse().ok()?;
                    (v >= 9.0 && e.path().is_dir()).then(|| (v, e.path().join("scripting").join("plugins")))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    out.into_iter().map(|(_, p)| p).collect()
}

/// plugins（scripting\plugins）の下にプラグインを書き、書いたフォルダを返す
pub fn install_into(plugins: &Path, exe: &Path) -> std::io::Result<PathBuf> {
    let dir = plugins.join(DIR_NAME);
    std::fs::create_dir_all(&dir)?;
    for (name, data) in FILES {
        std::fs::write(dir.join(name), data)?;
    }
    std::fs::write(dir.join("krbr_path.txt"), exe.to_string_lossy().as_bytes())?;
    Ok(dir)
}

/// `krbr kicad-plugin install`。dir を省けば、見つかった KiCad 9 以降のすべての版に入れる
pub fn cmd_install(dir: Option<&Path>) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    let exe = std::fs::canonicalize(&exe).map(strip_verbatim).unwrap_or(exe);
    let targets = match dir {
        Some(d) => vec![d.to_path_buf()],
        None => kicad_plugin_dirs(),
    };
    if targets.is_empty() {
        anyhow::bail!("no KiCad 9+ user folder found under Documents\\KiCad (use --dir <...\\scripting\\plugins>)");
    }
    for t in targets {
        let d = install_into(&t, &exe)?;
        info!("installed the KiCad plugin into {} (krbr: {})", d.display(), exe.display());
    }
    info!("restart KiCad, or in the PCB editor use Tools > External Plugins > Refresh Plugins; the toolbar gets a 'Preview in KRB Render' button");
    Ok(())
}

/// `krbr kicad-plugin uninstall`
pub fn cmd_uninstall(dir: Option<&Path>) -> anyhow::Result<()> {
    let targets = match dir {
        Some(d) => vec![d.to_path_buf()],
        None => kicad_plugin_dirs(),
    };
    let mut removed = 0;
    for t in &targets {
        let d = t.join(DIR_NAME);
        if d.is_dir() {
            std::fs::remove_dir_all(&d)?;
            info!("removed {}", d.display());
            removed += 1;
        }
    }
    if removed == 0 {
        let looked: Vec<String> = targets.iter().map(|t| t.display().to_string()).collect();
        if looked.is_empty() {
            info!("the KiCad plugin is not installed (no KiCad 9+ user folder found under Documents\\KiCad); nothing removed");
        } else {
            info!("the KiCad plugin is not installed in {}; nothing removed", looked.join(", "));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_writes_the_plugin_and_the_exe_path() {
        let dir = std::env::temp_dir().join(format!("krbr_plugin_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let d = install_into(&dir, Path::new(r"C:\x\krbr.exe")).unwrap();
        for (name, data) in FILES {
            assert_eq!(&std::fs::read(d.join(name)).unwrap(), data);
        }
        assert_eq!(std::fs::read_to_string(d.join("krbr_path.txt")).unwrap(), r"C:\x\krbr.exe");
        cmd_uninstall(Some(&dir)).unwrap();
        assert!(!d.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
