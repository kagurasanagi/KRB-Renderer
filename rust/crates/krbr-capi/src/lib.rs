//! 描画のコア（krbr-render の Renderer）の C ABI。Blender などのラッパーは krbr_renderer.dll を読み込んで、
//! include/krbr_renderer.h の関数を呼ぶ（Python なら ctypes）。関数の説明・引数の約束はヘッダが正本。
//!
//! - 失敗は戻り値（-1 か NULL）で返し、理由は krbr_last_error()（呼んだスレッドごと）で読む
//! - パニックは関数の境界で止めて失敗にする（呼び出し元のアプリを落とさない）
//! - 座標は mm、Y が上の右手系。色はリニア。画像は RGBA の float で、1 行目が上

// 安全性の約束（ポインタの有効性・要素数）はヘッダに書く
#![allow(clippy::missing_safety_doc)]

use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};

use glam::{Mat4, Vec3};
use krbr_pbr::{IntegratorKind, LensModel, Material, Mesh, RenderInstance, RenderScene};
use krbr_render::{DenoiseQuality, ImageF, Renderer, RendererOptions};

/// ヘッダの KRBR_API_VERSION と同じ値。関数を足したり意味を変えたりしたら上げる
pub const API_VERSION: u32 = 1;

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(msg: &str) {
    krbr_core::warn!("krbr_renderer: {msg}");
    LAST_ERROR.with(|e| *e.borrow_mut() = CString::new(msg.replace('\0', " ")).unwrap_or_default());
}

/// f を実行し、Err ならその理由を、パニックならその内容を krbr_last_error に入れて fail を返す
fn guard<T>(fail: T, f: impl FnOnce() -> Result<T, String>) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            set_error(&e);
            fail
        }
        Err(p) => {
            let msg = p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "unknown".into());
            set_error(&format!("internal error (panic): {msg}"));
            fail
        }
    }
}

/// 成功なら 0、失敗なら -1
fn status(f: impl FnOnce() -> Result<(), String>) -> c_int {
    guard(-1, || f().map(|_| 0))
}

unsafe fn text<'a>(p: *const c_char, what: &str) -> Result<&'a str, String> {
    if p.is_null() {
        return Err(format!("{what} is NULL"));
    }
    CStr::from_ptr(p).to_str().map_err(|_| format!("{what} is not UTF-8"))
}

unsafe fn get<'a, T>(p: *const T, what: &str) -> Result<&'a T, String> {
    p.as_ref().ok_or_else(|| format!("{what} is NULL"))
}

unsafe fn get_mut<'a, T>(p: *mut T, what: &str) -> Result<&'a mut T, String> {
    p.as_mut().ok_or_else(|| format!("{what} is NULL"))
}

/// n 要素の配列（n が 0 なら NULL でもよい）
unsafe fn slice<'a, T>(p: *const T, n: usize, what: &str) -> Result<&'a [T], String> {
    if n == 0 {
        return Ok(&[]);
    }
    if p.is_null() {
        return Err(format!("{what} is NULL"));
    }
    Ok(std::slice::from_raw_parts(p, n))
}

fn vec3(p: &[f32]) -> Vec3 {
    Vec3::new(p[0], p[1], p[2])
}

fn index(n: usize) -> Result<i32, String> {
    i32::try_from(n).map_err(|_| "too many items".to_string())
}

// ---------------------------------------------------------------- 全体

#[no_mangle]
pub extern "C" fn krbr_api_version() -> u32 {
    API_VERSION
}

/// 呼んだスレッドで最後に失敗した理由（UTF-8）。次にこのスレッドで失敗するまで有効
#[no_mangle]
pub extern "C" fn krbr_last_error() -> *const c_char {
    LAST_ERROR.with(|e| e.borrow().as_ptr())
}

/// ログ（標準エラー出力）の細かさ：0 debug, 1 info, 2 warn, 3 error
#[no_mangle]
pub extern "C" fn krbr_set_log_level(level: c_int) {
    use krbr_core::log::{set_level, Level};
    set_level(match level {
        i32::MIN..=0 => Level::Debug,
        1 => Level::Info,
        2 => Level::Warn,
        _ => Level::Error,
    });
}

/// ノイズ除去（OIDN の DLL）が使えそうなら 1
#[no_mangle]
pub extern "C" fn krbr_denoiser_available() -> c_int {
    guard(0, || Ok(krbr_render::denoiser_available() as c_int))
}

// ---------------------------------------------------------------- 材質

pub struct KrbrMaterial(Material);

#[no_mangle]
pub unsafe extern "C" fn krbr_material_create(name: *const c_char) -> *mut KrbrMaterial {
    guard(std::ptr::null_mut(), || {
        let name = if name.is_null() { "" } else { text(name, "name")? };
        Ok(Box::into_raw(Box::new(KrbrMaterial(Material { name: name.into(), ..Default::default() }))))
    })
}

#[no_mangle]
pub unsafe extern "C" fn krbr_material_destroy(m: *mut KrbrMaterial) {
    if !m.is_null() {
        guard((), || {
            drop(Box::from_raw(m));
            Ok(())
        })
    }
}

#[no_mangle]
pub unsafe extern "C" fn krbr_material_set_float(m: *mut KrbrMaterial, param: *const c_char, value: f32) -> c_int {
    status(|| get_mut(m, "material")?.0.set_float(text(param, "param")?, value))
}

#[no_mangle]
pub unsafe extern "C" fn krbr_material_set_color(m: *mut KrbrMaterial, param: *const c_char, r: f32, g: f32, b: f32) -> c_int {
    status(|| get_mut(m, "material")?.0.set_color(text(param, "param")?, Vec3::new(r, g, b)))
}

#[no_mangle]
pub unsafe extern "C" fn krbr_material_set_string(m: *mut KrbrMaterial, param: *const c_char, value: *const c_char) -> c_int {
    status(|| get_mut(m, "material")?.0.set_string(text(param, "param")?, text(value, "value")?))
}

// ---------------------------------------------------------------- シーン

pub struct KrbrScene(RenderScene);

#[no_mangle]
pub extern "C" fn krbr_scene_create() -> *mut KrbrScene {
    guard(std::ptr::null_mut(), || Ok(Box::into_raw(Box::new(KrbrScene(RenderScene::default())))))
}

#[no_mangle]
pub unsafe extern "C" fn krbr_scene_destroy(s: *mut KrbrScene) {
    if !s.is_null() {
        guard((), || {
            drop(Box::from_raw(s));
            Ok(())
        })
    }
}

#[no_mangle]
pub unsafe extern "C" fn krbr_scene_add_material(s: *mut KrbrScene, m: *const KrbrMaterial) -> i32 {
    guard(-1, || {
        let s = get_mut(s, "scene")?;
        let m = get(m, "material")?;
        index(s.0.add_material(m.0.clone()) as usize)
    })
}

#[no_mangle]
pub unsafe extern "C" fn krbr_scene_set_material(s: *mut KrbrScene, index: u32, m: *const KrbrMaterial) -> c_int {
    status(|| {
        let s = get_mut(s, "scene")?;
        let m = get(m, "material")?;
        let n = s.0.materials.len();
        let dst = s.0.materials.get_mut(index as usize).ok_or_else(|| format!("material {index} does not exist ({n} materials)"))?;
        *dst = m.0.clone();
        Ok(())
    })
}

/// 頂点ごとの法線（三角形の面積で重みを付けた平均）
fn smooth_normals(positions: &[Vec3], indices: &[u32]) -> Vec<Vec3> {
    let mut n = vec![Vec3::ZERO; positions.len()];
    for t in indices.chunks_exact(3) {
        let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
        let face = (positions[b] - positions[a]).cross(positions[c] - positions[a]);
        for i in [a, b, c] {
            n[i] += face;
        }
    }
    n.into_iter().map(|v| v.try_normalize().unwrap_or(Vec3::Y)).collect()
}

#[no_mangle]
pub unsafe extern "C" fn krbr_scene_add_mesh(
    s: *mut KrbrScene,
    positions: *const f32,
    normals: *const f32,
    vertex_count: u32,
    indices: *const u32,
    triangle_count: u32,
    triangle_materials: *const u32,
) -> i32 {
    guard(-1, || {
        let s = get_mut(s, "scene")?;
        let (nv, nt) = (vertex_count as usize, triangle_count as usize);
        let positions: Vec<Vec3> = slice(positions, nv * 3, "positions")?.chunks_exact(3).map(vec3).collect();
        let indices = slice(indices, nt * 3, "indices")?.to_vec();
        if let Some(&i) = indices.iter().find(|&&i| i >= vertex_count) {
            return Err(format!("index {i} out of range ({nv} vertices)"));
        }
        let normals = if normals.is_null() { smooth_normals(&positions, &indices) } else { slice(normals, nv * 3, "normals")?.chunks_exact(3).map(vec3).collect() };
        let tri_material = if triangle_materials.is_null() { vec![0; nt] } else { slice(triangle_materials, nt, "triangle_materials")?.to_vec() };
        index(s.0.add_mesh(Mesh { positions, normals, indices, tri_material }) as usize)
    })
}

#[no_mangle]
pub unsafe extern "C" fn krbr_scene_add_instance(s: *mut KrbrScene, mesh: u32, matrix: *const f32, material_base: u32, mask: u8) -> i32 {
    guard(-1, || {
        let s = get_mut(s, "scene")?;
        if mesh as usize >= s.0.meshes.len() {
            return Err(format!("mesh {mesh} does not exist ({} meshes)", s.0.meshes.len()));
        }
        let xform = if matrix.is_null() { Mat4::IDENTITY } else { Mat4::from_cols_slice(slice(matrix, 16, "matrix")?) };
        s.0.instances.push(RenderInstance { mesh, xform, material_base, mask });
        index(s.0.instances.len() - 1)
    })
}

// ---------------------------------------------------------------- 描画

pub struct KrbrRenderer {
    r: Renderer,
    device_name: CString,
}

#[no_mangle]
pub extern "C" fn krbr_renderer_create(validation: c_int) -> *mut KrbrRenderer {
    guard(std::ptr::null_mut(), || {
        let r = Renderer::new(&RendererOptions { validation: validation != 0 })?;
        let device_name = CString::new(r.device_info().name.replace('\0', " ")).unwrap_or_default();
        Ok(Box::into_raw(Box::new(KrbrRenderer { r, device_name })))
    })
}

#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_destroy(r: *mut KrbrRenderer) {
    if !r.is_null() {
        guard((), || {
            drop(Box::from_raw(r));
            Ok(())
        })
    }
}

/// GPU の名前（UTF-8）。r を捨てるまで有効
#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_device_name(r: *const KrbrRenderer) -> *const c_char {
    guard(std::ptr::null(), || Ok(get(r, "renderer")?.device_name.as_ptr()))
}

#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_set_scene(r: *mut KrbrRenderer, s: *const KrbrScene) -> c_int {
    status(|| {
        let r = get_mut(r, "renderer")?;
        let s = get(s, "scene")?;
        s.0.validate()?;
        r.r.set_scene(&s.0)
    })
}

#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_update_materials(r: *mut KrbrRenderer, s: *const KrbrScene) -> c_int {
    status(|| get_mut(r, "renderer")?.r.update_materials(&get(s, "scene")?.0.materials))
}

#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_set_visible_mask(r: *mut KrbrRenderer, mask: u8) -> c_int {
    status(|| get_mut(r, "renderer")?.r.set_visible_mask(mask))
}

#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_set_environment(r: *mut KrbrRenderer, rgba: *const f32, width: u32, height: u32) -> c_int {
    status(|| {
        let r = get_mut(r, "renderer")?;
        let n = width as usize * height as usize * 4;
        let px = slice(rgba, n, "rgba")?;
        r.r.set_environment(ImageF { width, height, rgba: px.to_vec() })
    })
}

/// .hdr / .exr（と sRGB の画像）、または内蔵の "builtin:studio"
#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_set_environment_file(r: *mut KrbrRenderer, path: *const c_char) -> c_int {
    status(|| {
        let r = get_mut(r, "renderer")?;
        let env = krbr_render::load_environment_map(text(path, "path")?)?;
        r.r.set_environment(env)
    })
}

#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_set_environment_color(r: *mut KrbrRenderer, red: f32, green: f32, blue: f32) -> c_int {
    status(|| get_mut(r, "renderer")?.r.set_environment_color([red, green, blue]))
}

#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_set_resolution(r: *mut KrbrRenderer, width: u32, height: u32) -> c_int {
    status(|| get_mut(r, "renderer")?.r.set_resolution(width, height))
}

/// position から target を見るカメラ。up は画面の上のおおよその向き、fov_y_deg は縦の画角。縦横比は解像度から決める
#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_set_camera(r: *mut KrbrRenderer, position: *const f32, target: *const f32, up: *const f32, fov_y_deg: f32) -> c_int {
    status(|| {
        let r = get_mut(r, "renderer")?;
        let (p, t, u) = (vec3(slice(position, 3, "position")?), vec3(slice(target, 3, "target")?), vec3(slice(up, 3, "up")?));
        if !(fov_y_deg > 0.0 && fov_y_deg < 180.0) {
            return Err(format!("bad field of view {fov_y_deg}"));
        }
        let (w, h) = r.r.resolution();
        let mut frame = r.r.frame().clone();
        frame.camera = krbr_pbr::Camera::look_at(p, t, u, fov_y_deg, w as f32 / h as f32);
        r.r.set_frame(frame);
        Ok(())
    })
}

fn flag(v: f32) -> bool {
    v != 0.0
}

#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_set_float(r: *mut KrbrRenderer, name: *const c_char, value: f32) -> c_int {
    status(|| {
        let r = get_mut(r, "renderer")?;
        let name = text(name, "name")?;
        if name == "bump_intensity" {
            r.r.set_bump_intensity(value);
            return Ok(());
        }
        let mut f = r.r.frame().clone();
        let count = || -> Result<u32, String> {
            if value >= 0.0 && value.fract() == 0.0 {
                Ok(value as u32)
            } else {
                Err(format!("'{name}' must be a whole number >= 0 (got {value})"))
            }
        };
        match name {
            "env_intensity" => f.env_intensity = value,
            "env_rotation_deg" => f.env_rotation_rad = value.to_radians(),
            "radiance_clamp" => f.radiance_clamp = value,
            "max_depth" => f.max_depth = count()?,
            "rr_start_depth" => f.rr_start_depth = count()?,
            "seed" => f.seed = count()?,
            "spectral" => f.spectral = flag(value),
            "transparent_background" => f.transparent_background = flag(value),
            "vcm_radius" => f.vcm_radius = value,
            "vcm_alpha" => f.vcm_alpha = value,
            "focal_length_mm" => f.lens.focal_length_mm = value,
            "f_number" => f.lens.f_number = value,
            "focus_distance_mm" => f.lens.focus_distance_mm = value,
            "blades" => f.lens.blades = count()? as i32,
            "blade_rotation_deg" => f.lens.blade_rotation_deg = value,
            "blade_roundness" => f.lens.blade_roundness = value,
            "natural_vignetting" => f.lens.natural_vignetting = flag(value),
            "optical_vignetting" => f.lens.optical_vignetting = value,
            "abbe_number" => f.lens.abbe_number = value,
            "lateral_ca" => f.lens.lateral_ca = value,
            _ => return Err(format!("unknown number option '{name}'")),
        }
        r.r.set_frame(f);
        Ok(())
    })
}

#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_set_string(r: *mut KrbrRenderer, name: *const c_char, value: *const c_char) -> c_int {
    status(|| {
        let r = get_mut(r, "renderer")?;
        let (name, value) = (text(name, "name")?, text(value, "value")?);
        match name {
            "integrator" => r.r.set_integrator(value.parse::<IntegratorKind>()?),
            "lens_model" | "lens_file" => {
                let mut f = r.r.frame().clone();
                if name == "lens_model" {
                    f.lens.model = value.parse::<LensModel>()?;
                } else {
                    f.lens.lens_file = value.into();
                }
                r.r.set_frame(f);
                Ok(())
            }
            _ => Err(format!("unknown text option '{name}'")),
        }
    })
}

/// samples サンプルを積み増す（終わるまで戻らない）。積んだ後のサンプル数を返す
#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_render(r: *mut KrbrRenderer, samples: u32) -> i64 {
    guard(-1, || get_mut(r, "renderer")?.r.render_samples(samples).map(i64::from))
}

#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_samples(r: *const KrbrRenderer) -> u32 {
    guard(0, || Ok(get(r, "renderer")?.r.samples()))
}

#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_reset(r: *mut KrbrRenderer) -> c_int {
    status(|| {
        get_mut(r, "renderer")?.r.reset();
        Ok(())
    })
}

unsafe fn write_image(img: &ImageF, out: *mut f32, count: usize) -> Result<(), String> {
    if count != img.rgba.len() {
        return Err(format!("the image is {}x{} = {} floats, the buffer holds {count}", img.width, img.height, img.rgba.len()));
    }
    if out.is_null() {
        return Err("out is NULL".into());
    }
    std::slice::from_raw_parts_mut(out, count).copy_from_slice(&img.rgba);
    Ok(())
}

/// 今の累積を読む。aov：0 色、1 アルベド、2 法線。out は 幅 × 高さ × 4 個の float（count はその数）
#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_read(r: *const KrbrRenderer, aov: c_int, out: *mut f32, count: usize) -> c_int {
    status(|| {
        let a = get(r, "renderer")?.r.readback()?;
        let img = match aov {
            0 => &a.color,
            1 => &a.albedo,
            2 => &a.normal,
            _ => return Err(format!("unknown aov {aov} (0 color, 1 albedo, 2 normal)")),
        };
        write_image(img, out, count)
    })
}

/// 今の累積をノイズ除去して読む。quality：0 fast、1 balanced、2 high。strength：元の画像と混ぜる割合（0..1、1 で OIDN のまま）
#[no_mangle]
pub unsafe extern "C" fn krbr_renderer_denoise(r: *const KrbrRenderer, quality: c_int, strength: f32, out: *mut f32, count: usize) -> c_int {
    status(|| {
        let q = match quality {
            0 => DenoiseQuality::Fast,
            1 => DenoiseQuality::Balanced,
            2 => DenoiseQuality::High,
            _ => return Err(format!("unknown denoise quality {quality} (0 fast, 1 balanced, 2 high)")),
        };
        let img = get(r, "renderer")?.r.denoise(q, strength.clamp(0.0, 1.0))?;
        write_image(&img, out, count)
    })
}
