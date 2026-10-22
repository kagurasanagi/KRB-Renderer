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
    /// 指定と違う拡張子のファイルに差し替えた（.wrl -> .step など）
    pub substituted: bool,
}

pub struct ModelResolver {
    project_dir: PathBuf,
    vars: BTreeMap<String, String>,
    /// 見つかった KiCad のうち一番新しい版の 3D ライブラリ。定義の無い KICADn_3DMODEL_DIR / KISYS3DMOD はここを指す
    default_models: Option<String>,
    search_dirs: Vec<PathBuf>,
    re: Regex,
}

fn is_dir(p: &Path) -> bool {
    p.is_dir()
}

fn is_file(p: &Path) -> bool {
    p.is_file()
}

/// 3D ライブラリの場所を指す変数か（KICADn_3DMODEL_DIR・KISYS3DMOD・KICAD_USER_3DMODEL_DIR）
fn is_model_dir_var(name: &str) -> bool {
    if name == "KISYS3DMOD" || name == "KICAD_USER_3DMODEL_DIR" {
        return true;
    }
    name.strip_prefix("KICAD").and_then(|r| r.strip_suffix("_3DMODEL_DIR")).is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
}

/// 版で引ける KiCad の標準ライブラリの変数か（KICADn_3DMODEL_DIR・KISYS3DMOD。ユーザーのライブラリは除く）
fn is_stock_model_dir_var(name: &str) -> bool {
    is_model_dir_var(name) && name != "KICAD_USER_3DMODEL_DIR"
}

/// インストールされた KiCad の 3D ライブラリ：(主版, フォルダ)。主版が分からない配置は None
///   Windows: <Program Files>\KiCad\<版>\share\kicad\3dmodels（版ごとに並べて入る）
///   Linux:   /usr/share/kicad/3dmodels など（パッケージは 1 つの版だけを入れ、フォルダに版が無い）
///   macOS:   /Applications/KiCad/KiCad.app/Contents/SharedSupport/3dmodels
fn installed_model_dirs() -> Vec<(Option<i32>, PathBuf)> {
    let mut out = Vec::new();
    if cfg!(windows) {
        for env in ["ProgramFiles", "ProgramW6432"] {
            let Ok(pf) = std::env::var(env) else { continue };
            let Ok(entries) = std::fs::read_dir(PathBuf::from(pf).join("KiCad")) else { continue };
            for e in entries.flatten() {
                let ver = e.file_name().to_string_lossy().into_owned();
                let major: i32 = ver.split(|c: char| !c.is_ascii_digit()).next().and_then(|s| s.parse().ok()).unwrap_or(0);
                let models = e.path().join("share").join("kicad").join("3dmodels");
                if major > 0 && is_dir(&models) {
                    out.push((Some(major), models));
                }
            }
        }
    } else {
        let candidates = if cfg!(target_os = "macos") {
            vec![PathBuf::from("/Applications/KiCad/KiCad.app/Contents/SharedSupport/3dmodels")]
        } else {
            vec![PathBuf::from("/usr/share/kicad/3dmodels"), PathBuf::from("/usr/local/share/kicad/3dmodels")]
        };
        out.extend(candidates.into_iter().filter(|d| is_dir(d)).map(|d| (None, d)));
    }
    out
}

/// インストール先から KICADn_3DMODEL_DIR の既定値を作り、一番新しい版のライブラリを返す（版の分からない配置は、版の分かるものより後回し）
fn detect_install_vars(vars: &mut BTreeMap<String, String>) -> Option<String> {
    let mut best: Option<(i32, String)> = None;
    for (major, models) in installed_model_dirs() {
        let models_str = models.to_string_lossy().into_owned();
        if let Some(m) = major {
            vars.entry(format!("KICAD{m}_3DMODEL_DIR")).or_insert_with(|| models_str.clone());
        }
        let rank = major.unwrap_or(0);
        if best.as_ref().is_none_or(|(r, _)| rank > *r) {
            best = Some((rank, models_str));
        }
    }
    best.map(|(_, d)| d)
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
        // 優先順（後ほど優先）：インストール先からの既定値 < kicad_common.json < 環境変数 < 設定ファイル
        let mut vars = BTreeMap::new();
        read_kicad_common(&mut vars);
        let default_models = detect_install_vars(&mut vars);
        // 3D ライブラリの変数は、版を決め打ちせずに環境変数から拾う（今後の版の KICADn_3DMODEL_DIR も同じ扱い）
        for (name, v) in std::env::vars() {
            if is_model_dir_var(&name) {
                vars.insert(name, v);
            }
        }
        for (k, v) in extra_vars {
            vars.insert(k.clone(), v.clone());
        }
        vars.insert("KIPRJMOD".into(), project_dir.to_string_lossy().into_owned());
        Self {
            project_dir: project_dir.to_path_buf(),
            vars,
            default_models,
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
            } else if let Some(d) = self.default_models.as_ref().filter(|_| is_stock_model_dir_var(name)) {
                // 定義の無い版（古い版の変数や、インストールより新しい版の変数）も、一番新しい標準ライブラリを指す
                out.push_str(d);
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

        let found = |c: PathBuf, expanded: String| {
            let substituted = c.extension() != p.extension();
            ResolvedModel { found: true, path: c, expanded, reason: String::new(), substituted }
        };
        if let Some(c) = candidates(&p).into_iter().find(|c| is_file(c)) {
            return found(c, expanded);
        }
        // 追加の検索フォルダをファイル名で探す（"Lib.3dshapes/name.step" の形も試す）。拡張子の優先順は上と同じ
        if let Some(fname) = p.file_name() {
            let lib_and_name = p.parent().and_then(|d| d.file_name()).map(|lib| PathBuf::from(lib).join(fname));
            for dir in &self.search_dirs {
                for rel in lib_and_name.iter().chain(std::iter::once(&PathBuf::from(fname))) {
                    if let Some(c) = candidates(&dir.join(rel)).into_iter().find(|c| is_file(c)) {
                        return found(c, expanded);
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

/// STEP の拡張子（大文字のファイル名もあるので両方）
const STEP_EXTS: [&str; 4] = ["step", "stp", "STEP", "STP"];

/// p を探すときの候補の順。VRML（.wrl / .wrz）は読み込み器が無いので、同名の STEP を先に試す。
/// それ以外は指定どおりのファイルを先に、無ければ同名の STEP を試す
fn candidates(p: &Path) -> Vec<PathBuf> {
    let ext = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let steps = STEP_EXTS.iter().map(|e| p.with_extension(e));
    if ext == "wrl" || ext == "wrz" {
        steps.chain(std::iter::once(p.to_path_buf())).collect()
    } else {
        std::iter::once(p.to_path_buf()).chain(steps).collect()
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
        // 相対パスはプロジェクトのフォルダから。検索フォルダは "Lib/name" でも探す。拡張子が同じなら差し替えではない
        let r2 = ModelResolver::new(&std::env::temp_dir(), &BTreeMap::new(), &[dir.clone()]);
        let m2 = r2.resolve("elsewhere/Lib.3dshapes/part.step");
        assert!(m2.found && !m2.substituted);
        // 検索フォルダに .wrl と .step が並んでいても、.wrl の指定には読める .step を返す
        std::fs::write(lib.join("part.wrl"), "x").unwrap();
        let m3 = r2.resolve("elsewhere/Lib.3dshapes/part.wrl");
        assert!(m3.found && m3.substituted && m3.path.extension().unwrap() == "step", "{:?}", m3.path);
    }

    #[test]
    fn model_dir_variable_names() {
        assert!(is_model_dir_var("KICAD5_3DMODEL_DIR") && is_model_dir_var("KICAD11_3DMODEL_DIR") && is_model_dir_var("KISYS3DMOD"));
        assert!(is_model_dir_var("KICAD_USER_3DMODEL_DIR") && !is_stock_model_dir_var("KICAD_USER_3DMODEL_DIR"));
        assert!(!is_model_dir_var("KICAD_3DMODEL_DIR") && !is_model_dir_var("KICADX_3DMODEL_DIR") && !is_model_dir_var("KICAD9_SYMBOL_DIR"));
    }

    #[test]
    fn normalize_resolves_dots() {
        let p = normalize(Path::new("C:/a/b/../c/./d.step"));
        assert_eq!(p, PathBuf::from("C:/a/c/d.step"));
    }
}
