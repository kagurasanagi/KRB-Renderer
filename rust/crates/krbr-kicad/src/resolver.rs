//! 3D モデル指定（${KICAD10_3DMODEL_DIR}/... など）を実ファイルに解決する。KiCad の設定は読むだけで書き換えない。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use regex::Regex;

#[derive(Debug, Clone, Default)]
pub struct ResolvedModel {
    pub found: bool,
    /// 見つかったファイル（見つからなければ、探した正規化済みのパス）
    pub path: PathBuf,
    /// 変数展開後の文字列
    pub expanded: String,
    /// 見つからない理由
    pub reason: String,
    /// .wrl -> .step などの差し替えをした
    pub substituted: bool,
}

pub struct ModelResolver {
    project_dir: PathBuf,
    vars: BTreeMap<String, String>,
    search_dirs: Vec<PathBuf>,
    re: Regex,
}

fn is_dir(p: &Path) -> bool {
    p.is_dir()
}

fn is_file(p: &Path) -> bool {
    p.is_file()
}

/// KiCad のインストール先を探して KICADn_3DMODEL_DIR の既定値を作る
fn detect_install_vars(vars: &mut BTreeMap<String, String>) {
    let mut roots: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for env in ["ProgramFiles", "ProgramW6432"] {
            if let Ok(pf) = std::env::var(env) {
                roots.push(PathBuf::from(pf).join("KiCad"));
            }
        }
    } else {
        roots = vec![PathBuf::from("/usr/share/kicad"), PathBuf::from("/usr/local/share/kicad")];
    }
    let mut best_major = -1i32;
    let mut best = String::new();
    for root in &roots {
        if !is_dir(root) {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(root) else { continue };
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            let ver = e.file_name().to_string_lossy().into_owned();
            let major: i32 = ver.split(|c: char| !c.is_ascii_digit()).next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let models = e.path().join("share").join("kicad").join("3dmodels");
            if major <= 0 || !is_dir(&models) {
                continue;
            }
            let var = format!("KICAD{major}_3DMODEL_DIR");
            let models_str = models.to_string_lossy().into_owned();
            vars.entry(var).or_insert_with(|| models_str.clone());
            if major > best_major {
                best_major = major;
                best = models_str;
            }
        }
    }
    if best_major > 0 {
        // 古い版の変数や KISYS3DMOD も、見つからなければ最新のライブラリを指す
        for v in 5..=best_major {
            vars.entry(format!("KICAD{v}_3DMODEL_DIR")).or_insert_with(|| best.clone());
        }
        vars.entry("KISYS3DMOD".into()).or_insert_with(|| best.clone());
    }
}

/// %APPDATA%/kicad/<ver>/kicad_common.json の environment.vars を読む（読み取りのみ）
fn read_kicad_common(vars: &mut BTreeMap<String, String>) {
    // 新しい版を後に読んで優先
    let files = crate::settings::kicad_common_files();
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else { continue };
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(j) => {
                if let Some(v) = j.pointer("/environment/vars").and_then(|v| v.as_object()) {
                    for (k, val) in v {
                        if let Some(s) = val.as_str() {
                            vars.insert(k.clone(), s.to_string());
                        }
                    }
                }
            }
            Err(e) => krbr_core::debug!("ignoring {}: {}", f.display(), e),
        }
    }
}

impl ModelResolver {
    /// extra_vars: 設定ファイルで与えた変数（最優先）。search_dirs: ファイル名で探す追加フォルダ
    pub fn new(project_dir: &Path, extra_vars: &BTreeMap<String, String>, search_dirs: &[PathBuf]) -> Self {
        let mut vars = BTreeMap::new();
        read_kicad_common(&mut vars);
        detect_install_vars(&mut vars);
        for name in [
            "KISYS3DMOD",
            "KICAD6_3DMODEL_DIR",
            "KICAD7_3DMODEL_DIR",
            "KICAD8_3DMODEL_DIR",
            "KICAD9_3DMODEL_DIR",
            "KICAD10_3DMODEL_DIR",
            "KICAD_USER_3DMODEL_DIR",
        ] {
            if let Ok(v) = std::env::var(name) {
                vars.insert(name.to_string(), v);
            }
        }
        for (k, v) in extra_vars {
            vars.insert(k.clone(), v.clone());
        }
        vars.insert("KIPRJMOD".into(), project_dir.to_string_lossy().into_owned());
        Self {
            project_dir: project_dir.to_path_buf(),
            vars,
            search_dirs: search_dirs.to_vec(),
            re: Regex::new(r"\$\{([^}]+)\}|\$\(([^)]+)\)").unwrap(),
        }
    }

    pub fn vars(&self) -> &BTreeMap<String, String> {
        &self.vars
    }

    fn expand(&self, s: &str, undefined: &mut Option<String>) -> String {
        let mut out = String::new();
        let mut last = 0;
        for m in self.re.captures_iter(s) {
            let whole = m.get(0).unwrap();
            out.push_str(&s[last..whole.start()]);
            let name = m.get(1).or_else(|| m.get(2)).map(|g| g.as_str()).unwrap_or("");
            if let Some(v) = self.vars.get(name) {
                out.push_str(v);
            } else if let Ok(e) = std::env::var(name) {
                out.push_str(&e);
            } else {
                if undefined.is_none() {
                    *undefined = Some(name.to_string());
                }
                out.push_str(whole.as_str());
            }
            last = whole.end();
        }
        out.push_str(&s[last..]);
        out
    }

    pub fn resolve(&self, spec: &str) -> ResolvedModel {
        let mut undefined = None;
        let expanded = self.expand(spec, &mut undefined);
        let mut p = PathBuf::from(&expanded);
        if p.is_relative() {
            p = self.project_dir.join(p);
        }
        let p = normalize(&p);

        let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let is_vrml = ext == "wrl" || ext == "wrz";
        // 候補：STEP を優先（VRML は読み込み器が無いので、同名の STEP があれば差し替える）
        let step_exts = ["step", "stp", "STEP", "STP"];
        let mut candidates: Vec<PathBuf> = Vec::new();
        if is_vrml {
            candidates.extend(step_exts.iter().map(|e| p.with_extension(e)));
            candidates.push(p.clone());
        } else {
            candidates.push(p.clone());
            candidates.extend(step_exts.iter().map(|e| p.with_extension(e)));
        }
        for c in &candidates {
            if is_file(c) {
                return ResolvedModel { found: true, path: c.clone(), expanded, reason: String::new(), substituted: *c != p };
            }
        }
        // 追加の検索フォルダをファイル名で探す（"Lib.3dshapes/name.step" の形も試す）
        if let Some(fname) = p.file_name() {
            let lib_and_name = p.parent().and_then(|d| d.file_name()).map(|lib| PathBuf::from(lib).join(fname));
            for dir in &self.search_dirs {
                for rel in lib_and_name.iter().chain(std::iter::once(&PathBuf::from(fname))) {
                    let q = dir.join(rel);
                    for e in ["", "step", "stp"] {
                        let c = if e.is_empty() { q.clone() } else { q.with_extension(e) };
                        if is_file(&c) {
                            return ResolvedModel { found: true, path: c, expanded, reason: String::new(), substituted: true };
                        }
                    }
                }
            }
        }
        let reason = match undefined {
            Some(v) => format!("undefined variable ${{{v}}}"),
            None => "file not found".to_string(),
        };
        ResolvedModel { found: false, path: p, expanded, reason, substituted: false }
    }
}

/// `..` と `.` を解決する（ファイルの存在は要らない。C++ の lexically_normal 相当）
fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_vars_and_substitutes_step_for_wrl() {
        let dir = std::env::temp_dir().join("krbr_rust_resolver_test");
        let lib = dir.join("Lib.3dshapes");
        std::fs::create_dir_all(&lib).unwrap();
        std::fs::write(lib.join("part.step"), "x").unwrap();
        let mut extra = BTreeMap::new();
        extra.insert("MYLIB".to_string(), dir.to_string_lossy().into_owned());
        let r = ModelResolver::new(&dir, &extra, &[]);
        let m = r.resolve("${MYLIB}/Lib.3dshapes/part.wrl");
        assert!(m.found && m.substituted);
        assert_eq!(m.path, lib.join("part.step"));
        let missing = r.resolve("${NOPE_UNDEFINED}/x.step");
        assert!(!missing.found);
        assert!(missing.reason.contains("NOPE_UNDEFINED"));
        // 相対パスはプロジェクトのフォルダから。検索フォルダは "Lib/name" でも探す
        let r2 = ModelResolver::new(&std::env::temp_dir(), &BTreeMap::new(), &[dir.clone()]);
        let m2 = r2.resolve("elsewhere/Lib.3dshapes/part.step");
        assert!(m2.found && m2.substituted);
    }

    #[test]
    fn normalize_resolves_dots() {
        let p = normalize(Path::new("C:/a/b/../c/./d.step"));
        assert_eq!(p, PathBuf::from("C:/a/c/d.step"));
    }
}
