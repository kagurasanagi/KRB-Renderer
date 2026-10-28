//! プロジェクト設定（基板の設定ファイル <名前>.krbr。crate::board_file）。項目は C++ 版の board.toml / environment.toml と同じ

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use glam::Vec3;
use toml::Value;

use crate::camera::CameraParams;
use crate::lens::LensParams;
use crate::mask_presets::{find_mask_preset, mask_presets};
use crate::scene::{Finish, SceneOptions};

/// 積分器（レンダリングの方式）。設定ファイルの [render] integrator、コマンドラインの --integrator、ビューアの選択の名前と対応する
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IntegratorKind {
    /// パストレーサー
    #[default]
    Pt,
    /// 双方向パストレーシング
    Bdpt,
    /// 頂点の接続と併合（透明な物体越しの集光模様も扱う）
    Vcm,
}

impl IntegratorKind {
    /// すべての種類（ビューアの選択肢の並び）
    pub const ALL: [IntegratorKind; 3] = [IntegratorKind::Pt, IntegratorKind::Bdpt, IntegratorKind::Vcm];

    /// 設定ファイル・コマンドライン・覚えた設定での名前
    pub fn as_str(self) -> &'static str {
        match self {
            IntegratorKind::Pt => "pt",
            IntegratorKind::Bdpt => "bdpt",
            IntegratorKind::Vcm => "vcm",
        }
    }
}

/// as_str の名前で出す（幅の指定も効く）
impl std::fmt::Display for IntegratorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.as_str())
    }
}

impl std::str::FromStr for IntegratorKind {
    type Err = String;
    /// 名前（pt / bdpt / vcm。"path" は "pt" の別名）から。知らない名前は Err
    fn from_str(s: &str) -> Result<Self, String> {
        if s == "path" {
            return Ok(IntegratorKind::Pt);
        }
        Self::ALL.into_iter().find(|k| k.as_str() == s).ok_or_else(|| format!("unknown integrator '{s}' (pt / bdpt / vcm)"))
    }
}

#[derive(Debug, Clone)]
pub struct ProjectConfig {
    // 基板の設定（[board] [relief] [surface] [placeholder] [paths] [kicad_cli]）
    pub scene: SceneOptions,
    /// ${VAR} の追加・上書き
    pub path_vars: BTreeMap<String, String>,
    pub model_search_dirs: Vec<PathBuf>,
    pub use_kicad_cli_text: bool,
    /// 空なら自動検出
    pub kicad_cli: String,

    // 照明・カメラ・レンダリングの設定（[environment] [camera] [render] [lens]）
    pub hdr: String,
    pub env_intensity: f32,
    pub env_rotation_deg: f32,
    pub elevation_deg: f64,
    pub azimuth_deg: f64,
    pub fov_deg: f64,
    pub margin: f64,
    pub width: u32,
    pub height: u32,
    pub spp: u32,
    pub max_depth: u32,
    pub exposure_ev: f32,
    /// グロー（明るい光のにじみ。krbr_render::glow）：強さ（0 でなし）、しきい値（露出後の RGB の最大。1 が画面の白）、広がり（0..1）
    pub glow: f32,
    pub glow_threshold: f32,
    pub glow_spread: f32,
    /// 間接光だけに効く上限（ホタル対策）
    pub radiance_clamp: f32,
    /// 分光モード（代表波長方式、4 波長）
    pub spectral: bool,
    /// 背景を透明にする（カメラの光線が何にも当たらない画素を透明に。PNG はアルファ付き、EXR は A に入れる）
    pub transparent_background: bool,
    pub integrator: IntegratorKind,
    /// VCM の近傍の半径の初期値（mm）。0 で自動
    pub vcm_radius: f32,
    pub lens: LensParams,

    pub loaded_files: Vec<PathBuf>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        // カメラの既定値は CameraParams::default に 1 つだけ持つ
        let cam = CameraParams::default();
        Self {
            scene: SceneOptions::default(),
            path_vars: BTreeMap::new(),
            model_search_dirs: Vec::new(),
            use_kicad_cli_text: true,
            kicad_cli: String::new(),
            hdr: String::new(),
            env_intensity: 1.0,
            env_rotation_deg: 0.0,
            elevation_deg: cam.elevation_deg,
            azimuth_deg: cam.azimuth_deg,
            fov_deg: cam.fov_deg,
            margin: cam.margin,
            width: cam.width,
            height: cam.height,
            spp: 64,
            max_depth: 8,
            exposure_ev: 0.7,
            glow: 1.0,
            glow_threshold: 4.0,
            glow_spread: 0.7,
            radiance_clamp: 10.0,
            spectral: false,
            transparent_background: false,
            integrator: IntegratorKind::default(),
            vcm_radius: 0.0,
            lens: LensParams::default(),
            loaded_files: Vec::new(),
        }
    }
}

/// レジストの色の指定（プリセット名 "jlcpcb-purple" など、または "r,g,b"）を SceneOptions に反映する
pub fn apply_mask_color_spec(o: &mut SceneOptions, spec: &str) -> Result<(), String> {
    if let Some(p) = find_mask_preset(spec) {
        o.mask_color = p.srgb;
        o.mask_color_set = true;
        o.mask_roughness = p.roughness;
        o.mask_scatter = p.scatter;
        o.mask_preset = p.name.to_string();
        if !o.silk_color_set {
            o.silk_color = p.silk_srgb; // 黄・白のレジストでは黒いシルク
        }
        return Ok(());
    }
    // 3 つとも数でなければ受け付けない（数にならない欄を黙って捨てると、残りの欄がずれて別の色になる）
    let v: Vec<f32> = spec.split(',').map(|t| t.trim().parse::<f32>()).collect::<Result<_, _>>().unwrap_or_default();
    if v.len() == 3 && v.iter().all(|x| (0.0..=1.0).contains(x)) {
        o.mask_color = Vec3::new(v[0], v[1], v[2]);
        o.mask_color_set = true;
        o.mask_preset.clear();
        return Ok(());
    }
    let names: Vec<&str> = mask_presets().iter().map(|p| p.name).collect();
    Err(format!("'{spec}' is not a mask preset or r,g,b (sRGB, 0..1). Presets: {}", names.join(" ")))
}

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Float(f) => Some(*f),
        Value::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "a string",
        Value::Integer(_) => "an integer",
        Value::Float(_) => "a number",
        Value::Boolean(_) => "true/false",
        Value::Datetime(_) => "a date",
        Value::Array(_) => "an array",
        Value::Table(_) => "a table",
    }
}

/// 数の許される範囲（端を含むか含まないか）。範囲外の値は警告して使わない（既定値のまま）
#[derive(Debug, Clone, Copy)]
struct Range {
    lo: f64,
    hi: f64,
    /// true なら lo を含まない（> lo）
    lo_open: bool,
    hi_open: bool,
}

impl Range {
    const ANY: Range = Range { lo: f64::NEG_INFINITY, hi: f64::INFINITY, lo_open: false, hi_open: false };
    const NON_NEGATIVE: Range = Range { lo: 0.0, ..Range::ANY };
    const POSITIVE: Range = Range { lo: 0.0, lo_open: true, ..Range::ANY };
    const UNIT: Range = Range::closed(0.0, 1.0);

    const fn closed(lo: f64, hi: f64) -> Range {
        Range { lo, hi, lo_open: false, hi_open: false }
    }

    const fn open(lo: f64, hi: f64) -> Range {
        Range { lo, hi, lo_open: true, hi_open: true }
    }

    fn contains(&self, v: f64) -> bool {
        // NaN はどの範囲にも入らない
        (if self.lo_open { v > self.lo } else { v >= self.lo }) && (if self.hi_open { v < self.hi } else { v <= self.hi })
    }

    fn describe(&self) -> String {
        match (self.lo.is_finite(), self.hi.is_finite()) {
            (true, false) => format!("{} {}", if self.lo_open { ">" } else { ">=" }, self.lo),
            (false, true) => format!("{} {}", if self.hi_open { "<" } else { "<=" }, self.hi),
            _ => format!("{}{}, {}{}", if self.lo_open { "(" } else { "[" }, self.lo, self.hi, if self.hi_open { ")" } else { "]" }),
        }
    }
}

/// 基板の設定ファイルの表のうち、ここ（load_project_config）では読まないもの：材質の上書き（crate::model_map）と、
/// ビューアが覚えた設定（krbr-app の remember）
const TABLES_READ_ELSEWHERE: &[&str] = &["ref", "model", "remembered"];
/// ここで読む表
const SECTIONS: &[&str] = &["board", "relief", "surface", "solder", "placeholder", "paths", "kicad_cli", "environment", "camera", "render", "lens"];

/// 設定ファイルの読み取り。読んだ鍵を覚えておき、型の誤り・範囲外の値をファイル名つきで警告する（値は使わず既定値のまま）。
/// 最後に、読まなかった鍵（綴りの誤り、置き場所の違う鍵）を警告する。材質ライブラリが未知のパラメータを警告するのと同じ方針
struct Reader<'a> {
    t: &'a toml::Table,
    file: &'a Path,
    /// 読んだ鍵（"render.spp" の形）
    seen: BTreeSet<String>,
}

impl<'a> Reader<'a> {
    fn new(t: &'a toml::Table, file: &'a Path) -> Self {
        Self { t, file, seen: BTreeSet::new() }
    }

    /// 鍵の値（書いてあれば）。読んだ鍵として覚える
    fn get(&mut self, path: &[&str]) -> Option<&'a Value> {
        self.seen.insert(path.join("."));
        let mut cur: &'a Value = self.t.get(path[0])?;
        for k in &path[1..] {
            cur = cur.as_table()?.get(*k)?;
        }
        Some(cur)
    }

    fn warn(&self, path: &[&str], msg: &str) {
        krbr_core::warn!("{}: {}: {}", self.file.display(), path.join("."), msg);
    }

    fn wrong_type(&self, path: &[&str], v: &Value, expected: &str) {
        self.warn(path, &format!("expected {expected}, found {}; ignored", type_name(v)));
    }

    /// 数（範囲を確かめる）。書いていない・使えないなら None
    fn number(&mut self, path: &[&str], range: Range) -> Option<f64> {
        let v = self.get(path)?;
        let Some(x) = as_f64(v) else {
            self.wrong_type(path, v, "a number");
            return None;
        };
        if !range.contains(x) {
            self.warn(path, &format!("{x} is out of range (must be {}); ignored", range.describe()));
            return None;
        }
        Some(x)
    }

    fn num(&mut self, path: &[&str], range: Range, dst: &mut f32) {
        if let Some(x) = self.number(path, range) {
            *dst = x as f32;
        }
    }

    fn num_f64(&mut self, path: &[&str], range: Range, dst: &mut f64) {
        if let Some(x) = self.number(path, range) {
            *dst = x;
        }
    }

    /// 数。lo..=hi の外なら警告して端に丸める（範囲外でも効き方が決まっている項目用。num_f64 は範囲外を捨てる）
    fn num_f64_clamped(&mut self, path: &[&str], lo: f64, hi: f64, dst: &mut f64) {
        // NaN は number が警告して捨てる
        let Some(x) = self.number(path, Range::ANY) else { return };
        let c = x.clamp(lo, hi);
        if c != x {
            self.warn(path, &format!("{x} is out of range (must be {lo} to {hi}); using {c}"));
        }
        *dst = c;
    }

    /// 整数（小数部のない数も受け付ける）。以前は `v as u32` で、負の値が黙って 0 になっていた
    fn integer(&mut self, path: &[&str], min: i64, max: i64) -> Option<i64> {
        let x = self.number(path, Range::closed(min as f64, max as f64))?;
        if x.fract() != 0.0 {
            self.warn(path, &format!("{x} is not an integer; ignored"));
            return None;
        }
        Some(x as i64)
    }

    fn num_u32(&mut self, path: &[&str], min: u32, dst: &mut u32) {
        if let Some(x) = self.integer(path, min as i64, u32::MAX as i64) {
            *dst = x as u32;
        }
    }

    fn num_i32(&mut self, path: &[&str], min: i32, dst: &mut i32) {
        if let Some(x) = self.integer(path, min as i64, i32::MAX as i64) {
            *dst = x as i32;
        }
    }

    fn boolean(&mut self, path: &[&str], dst: &mut bool) {
        match self.get(path) {
            Some(Value::Boolean(b)) => *dst = *b,
            Some(v) => self.wrong_type(path, v, "true/false"),
            None => {}
        }
    }

    fn string(&mut self, path: &[&str]) -> Option<&'a str> {
        match self.get(path) {
            Some(Value::String(s)) => Some(s.as_str()),
            Some(v) => {
                self.wrong_type(path, v, "a string");
                None
            }
            None => None,
        }
    }

    /// 名前で選ぶ項目（FromStr）。知らない名前は警告して既定値のまま
    fn choice<T: std::str::FromStr<Err = String>>(&mut self, path: &[&str], dst: &mut T) {
        if let Some(s) = self.string(path) {
            match s.parse() {
                Ok(v) => *dst = v,
                Err(e) => self.warn(path, &format!("{e}; ignored")),
            }
        }
    }

    /// パスの項目：空なら省略と同じ（None）、"builtin:" で始まればそのまま、それ以外は基板のフォルダからの相対パス
    fn path(&mut self, path: &[&str], base: &Path) -> Option<String> {
        let s = self.string(path)?;
        if s.is_empty() {
            return None;
        }
        Some(if s.starts_with("builtin:") { s.to_string() } else { resolve_rel(base, s).to_string_lossy().into_owned() })
    }

    /// 色：sRGB の [r, g, b]（各 0..1）。材質ライブラリ（materials/*.toml）の色はリニアで、スカラー 1 つ（灰色）も書けるが、
    /// 基板の設定ファイルには画面で見る色を書くので、sRGB の配列だけにしている（crate::material_io::read_color）
    fn color(&mut self, path: &[&str], dst: &mut Vec3, set: Option<&mut bool>) {
        let Some(v) = self.get(path) else { return };
        let rgb = match v {
            Value::Array(a) if a.len() == 3 => (as_f64(&a[0]), as_f64(&a[1]), as_f64(&a[2])),
            _ => (None, None, None),
        };
        let (Some(r), Some(g), Some(b)) = rgb else {
            return self.wrong_type(path, v, "[r, g, b] (sRGB, 0..1)");
        };
        let c = Vec3::new(r as f32, g as f32, b as f32);
        if ![r, g, b].iter().all(|x| Range::UNIT.contains(*x)) {
            return self.warn(path, &format!("[{r}, {g}, {b}] is out of range (each component must be in [0, 1]); ignored"));
        }
        *dst = c;
        if let Some(s) = set {
            *s = true;
        }
    }

    /// 読まなかった鍵を警告する（綴りの誤り、[render] に書くべき鍵を最上位に書いた、など）
    fn warn_unread(&self) {
        for (k, v) in self.t {
            if TABLES_READ_ELSEWHERE.contains(&k.as_str()) {
                continue;
            }
            let Value::Table(sec) = v else {
                krbr_core::warn!("{}: '{}' is outside any table (put it under one of [{}]); ignored", self.file.display(), k, SECTIONS.join("] ["));
                continue;
            };
            if !SECTIONS.contains(&k.as_str()) {
                krbr_core::warn!("{}: unknown table [{}]; ignored", self.file.display(), k);
                continue;
            }
            for key in sec.keys() {
                if !self.seen.contains(&format!("{k}.{key}")) {
                    krbr_core::warn!("{}: unknown key '{}' in [{}]; ignored", self.file.display(), key, k);
                }
            }
        }
    }
}

fn resolve_rel(dir: &Path, s: &str) -> PathBuf {
    let p = PathBuf::from(s);
    if p.is_relative() {
        dir.join(p)
    } else {
        p
    }
}

/// `--init`：基板の設定ファイル <名前>.krbr のひな形を書く（既にあれば、無い表だけを足す）
pub fn write_default_configs(pcb: &Path) {
    crate::board_file::init(pcb);
}

/// 既定値に、基板の設定ファイル <名前>.krbr（crate::board_file）の内容を重ねる。読めなければ警告して既定値。
/// 未知の鍵・型の誤り・範囲外の値は、ファイル名つきで警告してその項目だけ既定値のままにする
pub fn load_project_config(pcb: &Path) -> ProjectConfig {
    let mut c = ProjectConfig::default();
    // 中の相対パスは基板のフォルダから
    let base = crate::board_file::base_dir(pcb);
    let (file, table) = crate::board_file::read(pcb);
    if let Some(t) = table {
        c.loaded_files.push(file.clone());
        read_into(&mut c, &mut Reader::new(&t, &file), &base);
    }
    for f in &c.loaded_files {
        krbr_core::info!("config: {}", f.display());
    }
    // ファイルはあるが読めないときは board_file::read が警告している
    if c.loaded_files.is_empty() && !file.exists() {
        krbr_core::info!("config: no {} found, using defaults", file.display());
    }
    c
}

fn read_into(c: &mut ProjectConfig, r: &mut Reader, base: &Path) {
    use Range as R;
    let o = &mut c.scene;
    if let Some(f) = r.string(&["board", "finish"]) {
        // 大小を区別しない（"hasl" も HASL）。知らない名前は警告する（以前は名前だけ finish_<名前> の金めっきになっていた）
        match f.parse::<Finish>() {
            Ok(v) => o.finish = v.as_str().to_string(),
            Err(e) => r.warn(&["board", "finish"], &format!("{e}; using {}", o.finish)),
        }
    }
    match r.get(&["board", "mask_color"]) {
        Some(Value::String(name)) => {
            if let Err(e) = apply_mask_color_spec(o, name) {
                r.warn(&["board", "mask_color"], &e);
            }
        }
        _ => r.color(&["board", "mask_color"], &mut o.mask_color, Some(&mut o.mask_color_set)),
    }
    r.num(&["board", "mask_roughness"], R::UNIT, &mut o.mask_roughness);
    r.num(&["board", "mask_scatter"], R::UNIT, &mut o.mask_scatter);
    r.color(&["board", "silk_color"], &mut o.silk_color, Some(&mut o.silk_color_set));
    r.color(&["board", "fr4_color"], &mut o.fr4_color, None);
    if let Some(v) = r.string(&["board", "substrate"]) {
        o.substrate = v.trim().to_string();
    }
    r.boolean(&["board", "clip_silk_by_mask"], &mut o.layer_art.clip_silk_by_mask);
    r.num_f64(&["board", "texture_px_per_mm"], R::POSITIVE, &mut o.layer_art.px_per_mm);
    r.boolean(&["board", "include_dnp"], &mut o.include_dnp);
    // 層テクスチャ方式（enabled = false）は廃止した。凹凸は常に形状として作る
    let mut relief_enabled = true;
    r.boolean(&["relief", "enabled"], &mut relief_enabled);
    if !relief_enabled {
        r.warn(&["relief", "enabled"], "false is no longer supported (the layer texture mode was removed); the relief is always built as geometry");
    }
    // 厚みは µm で書き、mm で持つ
    if let Some(v) = r.number(&["relief", "copper_um"], R::NON_NEGATIVE) {
        o.relief_params.copper = v / 1000.0;
        o.relief_copper_from_stackup = false;
    }
    for (key, dst) in [("mask_on_copper_um", &mut o.relief_params.mask_on_copper), ("mask_on_substrate_um", &mut o.relief_params.mask_on_substrate), ("silk_um", &mut o.relief_params.silk), ("via_dimple_um", &mut o.relief_params.via_dimple)] {
        if let Some(v) = r.number(&["relief", key], R::NON_NEGATIVE) {
            *dst = v / 1000.0;
        }
    }
    r.num(&["surface", "bump_intensity"], R::NON_NEGATIVE, &mut o.bump_intensity);
    r.boolean(&["solder", "fillet"], &mut o.solder_fillet);
    if let Some(v) = r.string(&["solder", "material"]) {
        o.solder_material = v.trim().to_string();
    }
    r.num_f64(&["placeholder", "height_mm"], R::NON_NEGATIVE, &mut o.placeholder_height);
    r.color(&["placeholder", "color"], &mut o.placeholder_color, None);
    match r.get(&["paths", "vars"]) {
        Some(Value::Table(vars)) => {
            for (k, v) in vars {
                match v {
                    Value::String(s) => {
                        c.path_vars.insert(k.clone(), s.clone());
                    }
                    other => r.wrong_type(&["paths", "vars", k], other, "a string"),
                }
            }
        }
        Some(v) => r.wrong_type(&["paths", "vars"], v, "a table { NAME = \"path\" }"),
        None => {}
    }
    match r.get(&["paths", "model_search_dirs"]) {
        Some(Value::Array(dirs)) => {
            for d in dirs {
                match d {
                    Value::String(s) if !s.is_empty() => c.model_search_dirs.push(resolve_rel(base, s)),
                    Value::String(_) => {}
                    other => r.wrong_type(&["paths", "model_search_dirs"], other, "a string (a folder)"),
                }
            }
        }
        Some(v) => r.wrong_type(&["paths", "model_search_dirs"], v, "an array of folders"),
        None => {}
    }
    r.boolean(&["kicad_cli", "use_for_text"], &mut c.use_kicad_cli_text);
    if let Some(p) = r.string(&["kicad_cli", "path"]) {
        c.kicad_cli = p.to_string(); // 空なら自動検出
    }
    // hdr と lens_file は同じ扱い：空は省略と同じ（以前は hdr = "" が基板のフォルダのパスになり、覚えた環境マップが使われなかった）
    if let Some(h) = r.path(&["environment", "hdr"], base) {
        c.hdr = h;
    }
    r.num(&["environment", "intensity"], R::NON_NEGATIVE, &mut c.env_intensity);
    r.num(&["environment", "rotation_deg"], R::ANY, &mut c.env_rotation_deg);
    r.num_f64(&["camera", "elevation_deg"], R::closed(-90.0, 90.0), &mut c.elevation_deg);
    r.num_f64(&["camera", "azimuth_deg"], R::ANY, &mut c.azimuth_deg);
    r.num_f64(&["camera", "fov_deg"], R::open(0.0, 180.0), &mut c.fov_deg);
    // 以前から、範囲外は frame_camera が端に丸めて使っていた（捨てると構図が変わる）ので、警告して丸める
    r.num_f64_clamped(&["camera", "margin"], 0.0, crate::camera::MAX_MARGIN, &mut c.margin);
    r.num_u32(&["render", "width"], 1, &mut c.width);
    r.num_u32(&["render", "height"], 1, &mut c.height);
    r.num_u32(&["render", "spp"], 1, &mut c.spp);
    r.num_u32(&["render", "max_depth"], 0, &mut c.max_depth);
    r.num(&["render", "exposure_ev"], R::ANY, &mut c.exposure_ev);
    r.num(&["render", "glow"], R::NON_NEGATIVE, &mut c.glow);
    r.num(&["render", "glow_threshold"], R::NON_NEGATIVE, &mut c.glow_threshold);
    r.num(&["render", "glow_spread"], R::UNIT, &mut c.glow_spread);
    r.num(&["render", "radiance_clamp"], R::NON_NEGATIVE, &mut c.radiance_clamp);
    r.boolean(&["render", "spectral"], &mut c.spectral);
    r.boolean(&["render", "transparent_background"], &mut c.transparent_background);
    // 知らない名前は警告して既定のまま（"path" は "pt" の別名）
    r.choice(&["render", "integrator"], &mut c.integrator);
    r.num(&["render", "vcm_radius"], R::NON_NEGATIVE, &mut c.vcm_radius);
    r.choice(&["lens", "model"], &mut c.lens.model);
    r.num(&["lens", "focal_length_mm"], R::POSITIVE, &mut c.lens.focal_length_mm);
    r.num(&["lens", "f_number"], R::POSITIVE, &mut c.lens.f_number);
    r.num(&["lens", "focus_distance_mm"], R::NON_NEGATIVE, &mut c.lens.focus_distance_mm);
    r.num_i32(&["lens", "blades"], 0, &mut c.lens.blades);
    r.num(&["lens", "blade_rotation_deg"], R::ANY, &mut c.lens.blade_rotation_deg);
    r.num(&["lens", "blade_roundness"], R::UNIT, &mut c.lens.blade_roundness);
    r.boolean(&["lens", "natural_vignetting"], &mut c.lens.natural_vignetting);
    r.num(&["lens", "optical_vignetting"], R::UNIT, &mut c.lens.optical_vignetting);
    r.num(&["lens", "abbe_number"], R::NON_NEGATIVE, &mut c.lens.abbe_number);
    r.num(&["lens", "lateral_ca"], R::ANY, &mut c.lens.lateral_ca);
    if let Some(f) = r.path(&["lens", "lens_file"], base) {
        c.lens.lens_file = f;
    }
    r.warn_unread();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board_file::tests::{config_lock, workspace};
    use crate::lens::LensModel;

    #[test]
    fn init_template_is_the_defaults() {
        let _g = config_lock();
        let (proj, ws) = workspace("config_init");
        let pcb = proj.join("b.kicad_pcb");
        write_default_configs(&pcb);
        assert!(proj.join("b.krbr").is_file());
        // ひな形の値は既定値と同じ（書いても見た目が変わらない）。項目を足しても漏れないよう、全体を Debug の表記で比べる
        let (mut c, d) = (load_project_config(&pcb), ProjectConfig::default());
        assert_eq!(c.loaded_files.len(), 1);
        c.loaded_files.clear();
        assert_eq!(format!("{c:#?}"), format!("{d:#?}"));
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn reads_board_and_environment_sections_and_paths_are_from_the_board_folder() {
        let _g = config_lock();
        let (proj, ws) = workspace("config_read");
        std::fs::write(
            proj.join("board.krbr"),
            "[board]\nmask_color = \"jlcpcb-white\"\nmask_scatter = 0.5\n[relief]\nvia_dimple_um = 30\n[paths]\nvars = { X = \"y\" }\nmodel_search_dirs = [\"models\"]\n\
             [environment]\nhdr = \"hdr/studio.exr\"\n[render]\nspp = 16\nwidth = 640\n[lens]\nmodel = \"thin\"\nblades = 6\n",
        )
        .unwrap();
        let c = load_project_config(&proj.join("board.kicad_pcb"));
        assert_eq!(c.scene.mask_preset, "jlcpcb-white");
        assert_eq!(c.scene.mask_scatter, 0.5); // プリセットの後の指定が勝つ
        assert_eq!(c.scene.silk_color, Vec3::new(0.08, 0.08, 0.08));
        assert!((c.scene.relief_params.via_dimple - 0.03).abs() < 1e-9);
        assert_eq!(c.path_vars["X"], "y");
        assert_eq!(PathBuf::from(&c.hdr), proj.join("hdr/studio.exr"));
        assert_eq!(c.model_search_dirs, vec![proj.join("models")]);
        assert_eq!((c.spp, c.width, c.height), (16, 640, 1080));
        assert_eq!(c.lens.model, LensModel::Thin);
        assert_eq!(c.lens.blades, 6);
        assert_eq!(c.loaded_files, vec![proj.join("board.krbr")]);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn invalid_values_are_ignored_and_empty_paths_mean_omitted() {
        let _g = config_lock();
        let (proj, ws) = workspace("config_invalid");
        std::fs::write(
            proj.join("board.krbr"),
            "spp = 4
[board]
finish = \"hasl\"
mask_roughness = 1.5
fr4_color = 0.5
include_dnp = \"no\"
             [render]
spp = -1
width = 640.5
height = 720.0
integrator = \"path\"
sppp = 8
             [lens]
model = \"Thin\"
lens_file = \"\"
[environment]
hdr = \"\"
[rendering]
x = 1
",
        )
        .unwrap();
        let (c, d) = (load_project_config(&proj.join("board.kicad_pcb")), ProjectConfig::default());
        assert_eq!(c.scene.finish, "HASL"); // 大小を区別しない
        assert_eq!(c.scene.mask_roughness, d.scene.mask_roughness); // 範囲外
        assert_eq!(c.scene.fr4_color, d.scene.fr4_color); // 型の誤り
        assert_eq!(c.scene.include_dnp, d.scene.include_dnp);
        assert_eq!((c.spp, c.width, c.height), (d.spp, d.width, 720)); // 負の値・小数は使わない（以前は 0 と 640）
        assert_eq!(c.integrator, IntegratorKind::Pt); // path は pt の別名
        assert_eq!(c.lens.model, d.lens.model); // 知らない名前（大小も区別する）
        assert_eq!((c.hdr.as_str(), c.lens.lens_file.as_str()), ("", "")); // 空は省略と同じ
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn integrator_names() {
        for k in IntegratorKind::ALL {
            assert_eq!(k.as_str().parse::<IntegratorKind>(), Ok(k));
        }
        assert_eq!("path".parse::<IntegratorKind>(), Ok(IntegratorKind::Pt));
        assert!("PT".parse::<IntegratorKind>().is_err());
        assert_eq!(ProjectConfig::default().integrator, IntegratorKind::Pt);
    }

    #[test]
    fn mask_color_spec_needs_three_numbers() {
        let mut o = SceneOptions::default();
        assert!(apply_mask_color_spec(&mut o, "0.1,0.2,0.3").is_ok());
        assert_eq!(o.mask_color, Vec3::new(0.1, 0.2, 0.3));
        for bad in ["0.1,x,0.2,0.3", "0.1,0.2", "0.1,0.2,1.5"] {
            assert!(apply_mask_color_spec(&mut o, bad).is_err(), "{bad}");
        }
    }

    /// ひな形のレジストのプリセット名の行のコメントを外すと、プリセットの色・粗さ・散乱になる
    /// （以前はひな形の mask_roughness / mask_scatter の行が緑の値で上書きし、白いレジストが半透明になった）
    #[test]
    fn template_preset_line_sets_the_whole_preset() {
        let _g = config_lock();
        let (proj, ws) = workspace("config_template_preset");
        let pcb = proj.join("b.kicad_pcb");
        write_default_configs(&pcb);
        let file = proj.join("b.krbr");
        let text = std::fs::read_to_string(&file).unwrap();
        let line = text.lines().find(|l| l.trim_start().starts_with("# mask_color = \"jlcpcb-purple\"")).expect("the template has a preset line");
        std::fs::write(&file, text.replace(line, "mask_color = \"jlcpcb-white\"")).unwrap();
        let c = load_project_config(&pcb);
        let p = find_mask_preset("jlcpcb-white").unwrap();
        assert_eq!((c.scene.mask_preset.as_str(), c.scene.mask_roughness, c.scene.mask_scatter), ("jlcpcb-white", p.roughness, p.scatter));
        let _ = std::fs::remove_dir_all(&ws);
    }
}
