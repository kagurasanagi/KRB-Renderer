//! 材質の割り当ての CLI（`krbr material list / show / set`）、レジストの色の一覧（`krbr mask list`）、
//! Vulkan のデバイスの一覧（`krbr probe`）。C++ 版 app/material_cmd と commands の cmdMaskList / cmdProbe と同じ表示

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use krbr_core::{info, warn};
use krbr_kicad::{Board, ModelResolver};
use krbr_model::{MeshParams, ModelLibrary};
use krbr_render::{Context, ContextOptions};
use krbr_scene::config::load_project_config;
use krbr_scene::model_map::{GlobalModelMap, PartAssignment, ProjectOverrides};
use krbr_scene::scene::{load_material_setup, MaterialAssignment, MaterialSetup};
use krbr_scene::{mask_presets, MaterialLibrary, MaterialQuery, MaterialResolver};

use crate::prepare::{load_board_or_report, model_resolver};

#[derive(Debug, Clone, Default)]
pub struct MaterialSetOptions {
    pub pcb: PathBuf,
    /// 部品番号ごと（基板の設定ファイル <名前>.krbr の [ref.*]）
    pub reference: String,
    /// 3D モデルごと（展開前のパス、ファイル名、実ファイルのパス）
    pub model: String,
    /// 部位（空か "*" なら全部位）
    pub part: String,
    pub material: String,
    /// 3D モデルごとの設定を全体設定（model_map.toml）に書く
    pub global: bool,
}

/// --model の指定に一致する基板の中のモデル（展開前のパス、ファイル内容のハッシュ）
struct ModelRef {
    spec: String,
    hash: String,
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn find_models(board: &Board, resolver: &ModelResolver, query: &str) -> Vec<ModelRef> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let qp = Path::new(query);
    for fp in &board.footprints {
        for m in &fp.models {
            let r = resolver.resolve(&m.path);
            let fname = Path::new(&m.path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            let mut hit = m.path == query || fname == query;
            if !hit && r.found {
                hit = r.path.file_name().is_some_and(|f| f.to_string_lossy() == query) || (qp.exists() && same_file(qp, &r.path));
            }
            if !hit || !seen.insert(m.path.clone()) {
                continue;
            }
            let hash = if r.found { std::fs::read(&r.path).map(|b| krbr_core::to_hex(krbr_core::xxh64(&b))).unwrap_or_default() } else { String::new() };
            out.push(ModelRef { spec: m.path.clone(), hash });
        }
    }
    out
}

/// 部位（空か "*" なら全部位）に材質を割り当てる。全部位のときは、同じ書き込み先の部位ごとの指定も消す
/// （残すと、そちらが優先されて「全部位」にならない。C++ 版は部位ごとの指定を残す）
fn assign(a: &mut PartAssignment, part: &str, material: &str) {
    if part.is_empty() || part == "*" {
        a.material = material.to_string();
        a.parts.clear();
    } else {
        a.parts.insert(part.to_string(), material.to_string());
    }
}

/// 書き込む先のファイルが既にあって読めないときは止める（C++ 版は読めないファイルを 1 件だけの内容で上書きしてしまう）
fn check_readable<T>(file: &Path, load: impl Fn(&Path) -> Result<T, String>) -> Result<(), String> {
    if file.exists() {
        load(file).map_err(|e| format!("cannot read {} ({e}); not overwriting it - fix or remove the file first", file.display()))?;
    }
    Ok(())
}

/// 書き込む先：部品番号か、基板の中の 3D モデル
enum Target {
    Ref(String),
    Model(ModelRef),
}

/// set_material / clear_material で共通：指定の検証、基板と材質の設定の読み込み、書き込む先の決定（--model に一致するモデルがちょうど 1 つ）
fn resolve_target(o: &MaterialSetOptions) -> Result<(Board, MaterialSetup, Target), String> {
    if o.reference.is_empty() == o.model.is_empty() {
        return Err("specify exactly one of --ref or --model".into());
    }
    if o.global && o.model.is_empty() {
        return Err("--global can only be used with --model (per-reference overrides belong to the project)".into());
    }
    let board = load_board_or_report(&o.pcb).ok_or("cannot load board")?;
    let ms = load_material_setup(&board.file);
    if !o.reference.is_empty() {
        return Ok((board, ms, Target::Ref(o.reference.clone())));
    }
    let cfg = load_project_config(&board.file);
    let mut models = find_models(&board, &model_resolver(&board, &cfg), &o.model);
    match models.len() {
        0 => Err(format!("no 3D model on this board matches '{}'", o.model)),
        1 => Ok((board, ms, Target::Model(models.remove(0)))),
        _ => {
            let list: String = models.iter().map(|m| format!("\n  {}", m.spec)).collect();
            Err(format!("'{}' matches several models; use the full path as written in the board:{list}", o.model))
        }
    }
}

/// 材質を割り当てて設定ファイルに書く。戻り値は書いたファイル
pub fn set_material(o: &MaterialSetOptions) -> Result<PathBuf, String> {
    let (board, mut ms, target) = resolve_target(o)?;
    if ms.library.find_variant(&o.material).is_none() {
        return Err(format!("material '{}' is not in the material library (see: krbr material list)", o.material));
    }
    let m = match target {
        Target::Ref(reference) => {
            if !board.footprints.iter().any(|f| f.reference == reference) {
                warn!("reference {} is not on this board (written anyway)", reference);
            }
            check_readable(&ms.project_file, ProjectOverrides::load)?;
            assign(ms.project.refs.entry(reference).or_default(), &o.part, &o.material);
            ms.project.save(&ms.project_file)?;
            return Ok(ms.project_file);
        }
        Target::Model(m) => m,
    };
    if o.global {
        check_readable(&ms.global_file, GlobalModelMap::load)?;
        assign(&mut ms.global.upsert(&m.spec, &m.hash).assign, &o.part, &o.material);
        ms.global.save(&ms.global_file)?;
        Ok(ms.global_file)
    } else {
        check_readable(&ms.project_file, ProjectOverrides::load)?;
        assign(&mut ms.project.upsert(&m.spec, &m.hash).assign, &o.part, &o.material);
        ms.project.save(&ms.project_file)?;
        Ok(ms.project_file)
    }
}

/// 上書きを消す（part が空か "*" なら全部位）。何か消えたら true
fn clear_assign(a: &mut PartAssignment, part: &str) -> bool {
    if part.is_empty() || part == "*" {
        let had = !a.is_empty();
        *a = PartAssignment::default();
        had
    } else {
        a.parts.remove(part).is_some()
    }
}

/// set_material で書いた割り当てを消して、規則どおりに戻す（o.material は使わない）。
/// 戻り値は書き直したファイル（消すものが無ければ None で、ファイルに触らない）
pub fn clear_material(o: &MaterialSetOptions) -> Result<Option<PathBuf>, String> {
    let (_, mut ms, target) = resolve_target(o)?;
    let m = match target {
        Target::Ref(reference) => {
            check_readable(&ms.project_file, ProjectOverrides::load)?;
            let Some(a) = ms.project.refs.get_mut(&reference) else { return Ok(None) };
            if !clear_assign(a, &o.part) {
                return Ok(None);
            }
            if a.is_empty() {
                ms.project.refs.remove(&reference);
            }
            ms.project.save(&ms.project_file)?;
            return Ok(Some(ms.project_file));
        }
        Target::Model(m) => m,
    };
    let (entries, file) = if o.global {
        check_readable(&ms.global_file, GlobalModelMap::load)?;
        (&mut ms.global.models, ms.global_file.clone())
    } else {
        check_readable(&ms.project_file, ProjectOverrides::load)?;
        (&mut ms.project.models, ms.project_file.clone())
    };
    let mut changed = false;
    for e in entries.iter_mut().filter(|e| e.matches(&m.spec, &m.hash)) {
        changed |= clear_assign(&mut e.assign, &o.part);
    }
    if !changed {
        return Ok(None);
    }
    entries.retain(|e| !e.assign.is_empty());
    if o.global { ms.global.save(&file)? } else { ms.project.save(&file)? }
    Ok(Some(file))
}

/// 部品・部位ごとの材質と、どの段階で決まったか（3D モデルを読む。キャッシュが無ければ分割するので時間がかかる）
pub fn compute_assignments(pcb: &Path) -> Result<Vec<MaterialAssignment>, String> {
    let board = load_board_or_report(pcb).ok_or("cannot load board")?;
    let cfg = load_project_config(&board.file);
    let ms = load_material_setup(&board.file);
    let materials = MaterialResolver::new(&ms.library, &ms.project, &ms.global);
    let resolver = model_resolver(&board, &cfg);
    let library = ModelLibrary::new(krbr_core::platform::cache_dir(), MeshParams::default(), true);
    let mut out = Vec::new();
    for fp in &board.footprints {
        for m in &fp.models {
            if m.hidden {
                continue;
            }
            let r = resolver.resolve(&m.path);
            if !r.found {
                continue;
            }
            let lr = library.get(&r.path);
            let Some(mesh) = &lr.mesh else { continue };
            let model_file = r.path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            for part in &mesh.parts {
                let res = materials.resolve(&MaterialQuery { reference: &fp.reference, model_spec: &m.path, model_hash: &lr.file_hash, model_file: &model_file, part: Some(part) });
                out.push(MaterialAssignment {
                    reference: fp.reference.clone(),
                    footprint: fp.lib_id.clone(),
                    model_spec: m.path.clone(),
                    model_file: model_file.clone(),
                    model_hash: lr.file_hash.clone(),
                    part_keys: part.keys().join(" "),
                    material: res.name,
                    source: res.source,
                });
            }
        }
    }
    Ok(out)
}

pub fn cmd_material_set(o: &MaterialSetOptions) -> anyhow::Result<()> {
    let written = set_material(o).map_err(|e| anyhow::anyhow!(e))?;
    info!("wrote {}", written.display());
    Ok(())
}

/// 表にして出す（列の幅は最も長いものに合わせる）
pub fn cmd_material_show(pcb: &Path, ref_filter: &str) -> anyhow::Result<()> {
    let rows: Vec<MaterialAssignment> = compute_assignments(pcb).map_err(|e| anyhow::anyhow!(e))?.into_iter().filter(|a| ref_filter.is_empty() || a.reference == ref_filter).collect();
    let w = |min: usize, f: fn(&MaterialAssignment) -> &str| rows.iter().map(|a| f(a).len()).max().unwrap_or(0).max(min);
    let (wr, wf, wp, wm) = (w(3, |a| &a.reference), w(5, |a| &a.model_file), w(4, |a| &a.part_keys), w(8, |a| &a.material));
    println!("{:<wr$}  {:<wf$}  {:<wp$}  {:<wm$}  source", "ref", "model", "part", "material");
    for a in &rows {
        println!("{:<wr$}  {:<wf$}  {:<wp$}  {:<wm$}  {}", a.reference, a.model_file, a.part_keys, a.material, a.source);
    }
    Ok(())
}

pub fn cmd_material_list() -> anyhow::Result<()> {
    let lib = MaterialLibrary::load(&krbr_core::platform::config_dir());
    for n in lib.names() {
        println!("{:<20} {}", n, lib.source_of(&n));
    }
    Ok(())
}

pub fn cmd_mask_list() -> anyhow::Result<()> {
    println!("{:<20} {:<18} {:<6} {:<7} note", "name", "sRGB", "rough", "scatter");
    for p in mask_presets() {
        println!("{:<20} {:.2} {:.2} {:.2}     {:<6.2} {:<7.2} {}", p.name, p.srgb.x, p.srgb.y, p.srgb.z, p.roughness, p.scatter, p.note);
    }
    Ok(())
}

/// Vulkan のデバイスと、レイトレーシングへの対応を出し、ウィンドウなしのデバイスを作ってみる
pub fn cmd_probe() -> anyhow::Result<()> {
    let devs = Context::enumerate().map_err(|e| anyhow::anyhow!(e))?;
    if devs.is_empty() {
        anyhow::bail!("no Vulkan devices");
    }
    for d in &devs {
        let v = d.api_version;
        info!("{}: Vulkan {}.{}.{}, {} MiB, {}", d.name, (v >> 22) & 0x7f, (v >> 12) & 0x3ff, v & 0xfff, d.device_local_bytes >> 20, if d.discrete { "discrete" } else { "integrated/other" });
        info!("  ray_query={} acceleration_structure={} ray_tracing_pipeline={} global_priority={}", d.ray_query, d.acceleration_structure, d.ray_tracing_pipeline, d.global_priority);
    }
    let _ctx = Context::new(&ContextOptions { low_queue_priority: true, ..Default::default() }).map_err(|e| anyhow::anyhow!(e))?;
    info!("headless device created (no surface / swapchain)");
    Ok(())
}
