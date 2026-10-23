//! 材質の見本の画像のキャッシュ。見本は材質のパラメータと描き方（分光か RGB か）のハッシュで引く：
//!   1. 焼き付けた見本（crates/krbr-app/swatches/<ハッシュ>.png。組み込みの材質を多くのサンプルで描いてノイズ除去したもの。
//!      `krbr material bake-swatches` で作り、exe に埋め込む）
//!   2. その場で描いた見本のキャッシュ（全体用キャッシュの swatches/<ハッシュ>.png）
//!
//! どちらにも無ければ材質エディタがその場で描いて 2 に書く。パラメータが変われば別のハッシュになるので、古い画像は使われない

use std::path::{Path, PathBuf};
use std::time::Instant;

use krbr_render::Context;
use krbr_scene::material_library::builtin_order;
use krbr_scene::{Material, MaterialLibrary};
use rust_embed::RustEmbed;

use crate::preview::{swatch_camera, swatch_materials, swatch_scene, Preview, PreviewLight, SWATCH_PX};

#[derive(RustEmbed)]
#[folder = "swatches/"]
struct Baked;

/// 見本の描き方（preview.rs の見本のシーン・照明・カメラ・トーンマッピング）を変えたら上げる。焼き付けた見本とキャッシュが
/// 使われなくなるので、`krbr material bake-swatches` で焼き直す
pub const SWATCH_VERSION: u32 = 1;

/// 焼き付けの既定のサンプル数
pub const BAKE_SPP: u32 = 16384;

/// キャッシュに残す見本の数（自分の材質を編集するたびに増えるので、古いものから消す）
const CACHE_KEEP: usize = 500;

/// 見本のハッシュ（材質の名前と分類は描画に関わらないので含めない）
pub fn swatch_key(m: &Material, spectral: bool) -> String {
    let mut m = m.clone();
    m.name.clear();
    m.category = None;
    let text = format!("v{SWATCH_VERSION} {SWATCH_PX} {} {m:?}", if spectral { "spectral" } else { "rgb" });
    krbr_core::to_hex(krbr_core::xxh64(text.as_bytes()))
}

fn cache_dir() -> PathBuf {
    krbr_core::platform::cache_dir().join("swatches")
}

/// 焼き付けた見本か、キャッシュの見本
pub fn load(key: &str) -> Option<egui::ColorImage> {
    let file = format!("{key}.png");
    let bytes = match Baked::get(&file) {
        Some(f) => f.data.into_owned(),
        None => std::fs::read(cache_dir().join(&file)).ok()?,
    };
    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).ok()?.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
}

/// その場で描いた見本をキャッシュに書く（書けなくても困らないので、警告だけ）。起動して初めて書くときに、古い見本を消す
pub fn store(key: &str, img: &egui::ColorImage) {
    let dir = cache_dir();
    static PRUNED: std::sync::Once = std::sync::Once::new();
    PRUNED.call_once(|| prune(&dir, CACHE_KEEP));
    let r = std::fs::create_dir_all(&dir).map_err(|e| e.to_string()).and_then(|()| save_png(&dir.join(format!("{key}.png")), img));
    if let Err(e) = r {
        krbr_core::warn!("swatch cache {}: {e}", dir.display());
    }
}

/// フォルダの .png を、新しいものから keep 個だけ残す
fn prune(dir: &Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "png"))
        .map(|p| (std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH), p))
        .collect();
    if files.len() <= keep {
        return;
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.0));
    for (_, p) in &files[keep..] {
        let _ = std::fs::remove_file(p);
    }
}

fn save_png(path: &Path, img: &egui::ColorImage) -> Result<(), String> {
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
    krbr_render::image_io::save_png_rgba8(path, img.size[0] as u32, img.size[1] as u32, &rgba)
}

/// 見本 1 枚を、spp サンプル積んでノイズ除去して描く。GPU が待機中に呼ぶこと
pub fn render(ctx: &'static Context, preview: &mut Preview, m: &Material, spectral: bool, spp: u32, chunk: u32) -> Result<egui::ColorImage, String> {
    preview.set_view(swatch_camera(), spectral);
    preview.set_materials(swatch_materials(m))?;
    preview.reset();
    while preview.samples() < spp {
        preview.add_samples(ctx, chunk.min(spp - preview.samples()))?;
    }
    preview.image_denoised(ctx, 0.0)
}

/// 組み込みの材質の見本を、RGB と分光の両方で焼き付ける（out に <ハッシュ>.png を書き、ほかの .png は消す）。
/// 焼き付けた見本は次のビルドで exe に埋め込まれる
pub fn bake_builtin(out: &Path, spp: u32) -> Result<(), String> {
    if !krbr_render::denoiser_available() {
        return Err("OIDN DLLs not found (third_party/oidn/bin or next to the exe); the baked swatches must be denoised".into());
    }
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let ctx: &'static Context = Box::leak(Box::new(Context::new(&krbr_render::ContextOptions::default())?));
    let lib = MaterialLibrary::builtin();
    let light = PreviewLight::new();
    let first = lib.find(&builtin_order()[0]).ok_or("no built-in materials")?;
    let mut preview = Preview::new(ctx, swatch_scene(first), &light, SWATCH_PX, SWATCH_PX, swatch_camera(), false)?;
    let mut written = std::collections::BTreeSet::new();
    let t0 = Instant::now();
    let total = builtin_order().len() * 2;
    let mut done = 0;
    for name in builtin_order() {
        let m = lib.find(name).ok_or_else(|| format!("built-in material {name} not found"))?;
        for spectral in [false, true] {
            done += 1;
            let key = swatch_key(m, spectral);
            if !written.insert(key.clone()) {
                continue; // 同じパラメータの材質は 1 枚だけ
            }
            let t = Instant::now();
            let img = render(ctx, &mut preview, m, spectral, spp, 256)?;
            save_png(&out.join(format!("{key}.png")), &img)?;
            krbr_core::info!("[{done}/{total}] {name} ({}): {key}.png, {:.1} s", if spectral { "spectral" } else { "RGB" }, t.elapsed().as_secs_f64());
        }
    }
    // 使われなくなった見本を消す
    for e in std::fs::read_dir(out).map_err(|e| format!("{}: {e}", out.display()))?.flatten() {
        let p = e.path();
        let stale = p.extension().is_some_and(|x| x == "png") && p.file_stem().is_some_and(|s| !written.contains(&*s.to_string_lossy()));
        if stale {
            std::fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        }
    }
    krbr_core::info!("baked {} swatches ({SWATCH_PX} px, {spp} spp, denoised) into {} in {:.0} s", written.len(), out.display(), t0.elapsed().as_secs_f64());
    Ok(())
}

/// 焼き付けの既定の書き込み先（ソースの crates/krbr-app/swatches。開発用の PC でだけ在る）
pub fn default_bake_dir() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("swatches");
    p.is_dir().then_some(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_follow_the_parameters_not_the_name() {
        let lib = MaterialLibrary::builtin();
        let gold = lib.find("metal_gold").unwrap();
        let mut renamed = gold.clone();
        renamed.name = "my gold".into();
        renamed.category = Some(krbr_scene::Category::Other);
        assert_eq!(swatch_key(gold, false), swatch_key(&renamed, false));
        assert_ne!(swatch_key(gold, false), swatch_key(gold, true));
        renamed.specular_roughness += 0.01;
        assert_ne!(swatch_key(gold, false), swatch_key(&renamed, false));
    }

    /// 組み込みの材質（builtin_materials.toml）か Material の項目、見本の描き方を変えたら、焼き直すこと：
    ///   cargo run --release -- material bake-swatches
    #[test]
    fn baked_swatches_cover_the_builtin_materials() {
        let lib = MaterialLibrary::builtin();
        for name in builtin_order() {
            for spectral in [false, true] {
                let key = swatch_key(lib.find(name).unwrap(), spectral);
                assert!(Baked::get(&format!("{key}.png")).is_some(), "no baked swatch for {name} (spectral {spectral}); run `krbr material bake-swatches`");
            }
        }
        let img = load(&swatch_key(lib.find("metal_gold").unwrap(), true)).unwrap();
        assert_eq!(img.size, [SWATCH_PX as usize, SWATCH_PX as usize]);
    }

    #[test]
    fn pruning_keeps_the_newest() {
        let dir = std::env::temp_dir().join(format!("krbr_swatch_prune_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..5 {
            std::fs::write(dir.join(format!("{i}.png")), b"x").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        prune(&dir, 2);
        let mut left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["3.png", "4.png"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
