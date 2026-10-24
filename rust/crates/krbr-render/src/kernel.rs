//! 積分器で共通の部品：レイを飛ばすカーネル（計算シェーダか ray tracing pipeline）、シーンの資源の記述子、フレームの定数

use std::ffi::CString;
use std::sync::{Mutex, OnceLock};

use ash::vk;
use glam::{UVec4, Vec4};
use krbr_scene::lens::{LensModel, LensParams, LensSystem};
use krbr_scene::spectrum;

use crate::gpu_scene::GpuScene;
use crate::gpu_types::{FrameUbo, PushConstants};
use crate::integrator::{FrameParams, RenderTargets};
use crate::shader_consts::*;
use crate::vk::{memory_barrier, Buffer, Context};

/// 記述子 1 つ分（バッファ・画像・加速構造のどれか）
#[derive(Clone)]
pub enum Binding {
    Buffer { binding: u32, ty: vk::DescriptorType, buffer: vk::Buffer },
    Image { binding: u32, ty: vk::DescriptorType, sampler: vk::Sampler, view: vk::ImageView, layout: vk::ImageLayout },
    Tlas { binding: u32, tlas: vk::AccelerationStructureKHR },
}

impl Binding {
    fn binding(&self) -> u32 {
        match self {
            Binding::Buffer { binding, .. } | Binding::Image { binding, .. } | Binding::Tlas { binding, .. } => *binding,
        }
    }
    fn ty(&self) -> vk::DescriptorType {
        match self {
            Binding::Buffer { ty, .. } | Binding::Image { ty, .. } => *ty,
            Binding::Tlas { .. } => vk::DescriptorType::ACCELERATION_STRUCTURE_KHR,
        }
    }
}

pub fn buffer_binding(binding: u32, b: &Buffer) -> Binding {
    Binding::Buffer { binding, ty: vk::DescriptorType::STORAGE_BUFFER, buffer: b.buffer }
}

/// 全積分器で共通の記述子（シーン・環境マップ・出力先・定数・光源。番号は shader_consts）
pub fn scene_bindings(sc: &GpuScene, targets: &RenderTargets, frame_ubo: &Buffer) -> Vec<Binding> {
    let sampled = |binding: u32, sampler: vk::Sampler, view: vk::ImageView| Binding::Image { binding, ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER, sampler, view, layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL };
    let storage = |binding: u32, view: vk::ImageView| Binding::Image { binding, ty: vk::DescriptorType::STORAGE_IMAGE, sampler: vk::Sampler::null(), view, layout: vk::ImageLayout::GENERAL };
    vec![
        Binding::Tlas { binding: BINDING_TLAS, tlas: sc.tlas },
        buffer_binding(BINDING_VERTICES, &sc.vertices),
        buffer_binding(BINDING_INDICES, &sc.indices),
        buffer_binding(BINDING_TRI_MATS, &sc.tri_mats),
        buffer_binding(BINDING_MESHES, &sc.mesh_infos),
        buffer_binding(BINDING_MATERIALS, &sc.materials),
        sampled(BINDING_ENV_TEX, sc.env_sampler, sc.env_image.view),
        buffer_binding(BINDING_ENV_MARGINAL, &sc.env_marginal),
        buffer_binding(BINDING_ENV_CONDITIONAL, &sc.env_conditional),
        buffer_binding(BINDING_ENV_PDF, &sc.env_pdf),
        sampled(BINDING_BOARD_TEX, sc.board_sampler, sc.board_image.view),
        storage(BINDING_ACCUM_COLOR, targets.color.view),
        storage(BINDING_ACCUM_ALBEDO, targets.albedo.view),
        storage(BINDING_ACCUM_NORMAL, targets.normal.view),
        Binding::Buffer { binding: BINDING_FRAME, ty: vk::DescriptorType::UNIFORM_BUFFER, buffer: frame_ubo.buffer },
        buffer_binding(BINDING_INSTANCES, &sc.instance_infos),
        buffer_binding(BINDING_RGB2SPEC, &sc.rgb2spec),
        buffer_binding(BINDING_RGB2SPEC_SCALE, &sc.rgb2spec_scale),
        buffer_binding(BINDING_EMITTERS, &sc.emitters),
        buffer_binding(BINDING_EMITTER_CDF, &sc.emitter_cdf),
        buffer_binding(BINDING_CONDUCTORS, &sc.conductors),
    ]
}

/// フレームの定数（カメラ・環境光・光源の選び方など）。width × height は出力の解像度、merging は VCM の近傍のまとめを使うか
pub fn make_frame_ubo(sc: &GpuScene, width: u32, height: u32, fp: &FrameParams, merging: bool) -> FrameUbo {
    let cam = &fp.camera;
    let mut u = FrameUbo {
        cam_pos: cam.position.extend(0.0),
        cam_right: (cam.right * (cam.tan_half_fov_y * cam.aspect)).extend(0.0),
        cam_up: (cam.up * cam.tan_half_fov_y).extend(0.0),
        cam_forward: cam.forward.extend(0.0),
        board_map: sc.board_map,
        env_params: Vec4::new(fp.env_intensity, fp.env_rotation_rad, fp.radiance_clamp, 0.0),
        dims: UVec4::new(width, height, sc.env_dist_w, sc.env_dist_h),
        // z: 光線の cull mask（見せるインスタンスの種類）
        counts: UVec4::new(fp.max_depth, fp.rr_start_depth, sc.visible_mask as u32, 0),
        mask_params: sc.mask_params,
        mask_params2: sc.mask_params2,
        spectral: Vec4::new(if fp.spectral { 1.0 } else { 0.0 }, 1.0 / spectrum::cmf_y_integral(), 0.0, 0.0),
        ..Default::default()
    };
    let to_xyz = spectrum::linear_srgb_to_xyz();
    for i in 0..3 {
        u.rgb_to_xyz[i] = to_xyz.col(i).extend(0.0);
    }
    // 光源の選び方：シーンに届く放射束の見積もりに比例させる（どちらも 10% 以上は選ぶ）
    let pi = std::f64::consts::PI;
    let r = sc.scene_radius as f64;
    let env_power = pi * pi * r * r * sc.env_average_luminance as f64 * fp.env_intensity as f64;
    let tri_power = pi * sc.emitter_power as f64;
    // 見えている発光体が無ければ（発光体が無い、または全部を隠した）、発光体を選ばない（数を 0 として渡す）
    let emitters = if sc.emitter_power > 0.0 { sc.emitter_count } else { 0 };
    let mut p_env = 1.0f32;
    if emitters > 0 {
        p_env = if env_power > 0.0 { (env_power / (env_power + tri_power)).clamp(0.1, 0.9) as f32 } else { 0.0 };
    }
    u.light_params = Vec4::new(p_env, sc.emitter_power, sc.scene_radius, emitters as f32);
    u.scene_center = sc.scene_center.extend(0.0);
    let radius = if fp.vcm_radius > 0.0 { fp.vcm_radius } else { 0.003 * sc.scene_radius };
    u.vcm_params = Vec4::new(radius, fp.vcm_alpha, width as f32 * height as f32, if merging { 1.0 } else { 0.0 });
    fill_lens(&mut u, fp);
    u
}

/// 実レンズのピントの距離の下限（焦点距離の何倍か）
const MIN_FOCUS_IN_FOCAL_LENGTHS: f32 = 1.25;

/// フィルムの中央の明るさ（LensSystem::center_weight）と、画角に合うフィルムの高さ（film_height_for_angle）。
/// 光線を数千本追うので、レンズ・F 値・フィルムの位置・画角が同じなら覚えた値を使う（fill_lens はフレームごとに呼ばれる）
fn film_constants_cached(file: &str, f_number: f32, sys: &LensSystem, film_dist: f32, tan_half: f32) -> (f32, Option<f32>) {
    type Key = (String, u32, u32, u32);
    static LAST: Mutex<Option<(Key, (f32, Option<f32>))>> = Mutex::new(None);
    let key: Key = (file.to_string(), f_number.to_bits(), film_dist.to_bits(), tan_half.to_bits());
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, v)) = last.as_ref() {
        if *k == key {
            return *v;
        }
    }
    let v = (sys.center_weight(film_dist), sys.film_height_for_angle(film_dist, tan_half as f64));
    *last = Some((key, v));
    v
}

/// 射出瞳の表（同じレンズ・F 値・フィルムの位置・大きさなら前の結果。ピントが変わるとフィルムの位置が変わる）
fn exit_pupil_cached(file: &str, f_number: f32, sys: &LensSystem, film_dist: f32, r_max: f32) -> Vec<[f32; 4]> {
    type Key = (String, u32, u32, u32);
    static LAST: Mutex<Option<(Key, Vec<[f32; 4]>)>> = Mutex::new(None);
    let key: Key = (file.to_string(), f_number.to_bits(), film_dist.to_bits(), r_max.to_bits());
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, v)) = last.as_ref() {
        if *k == key {
            return v.clone();
        }
    }
    let t0 = std::time::Instant::now();
    let v = sys.exit_pupil_table(film_dist, r_max, crate::shader_consts::LENS_PUPIL_BINS);
    krbr_core::debug!("lens: exit pupil table in {:.1} ms", t0.elapsed().as_secs_f64() * 1000.0);
    *last = Some((key, v.clone()));
    v
}

/// GPU で描ける実レンズか（面の数が FrameUbo::lens_surfaces に収まる）。エラーにはファイルの名前を含めない（呼び出し側が付ける）
fn check_lens_for_gpu(sys: LensSystem) -> Result<LensSystem, String> {
    if sys.surfaces.len() > MAX_LENS_SURFACES {
        return Err(format!("{} surfaces; the renderer supports at most {}", sys.surfaces.len(), MAX_LENS_SURFACES));
    }
    Ok(sys)
}

/// 実レンズのデータは、ファイル名ごとに一度だけ読む。読めないか GPU で描けないレンズなら、知らせて既定のレンズにする
fn lens_system_for(file: &str) -> LensSystem {
    static CACHE: OnceLock<Mutex<std::collections::BTreeMap<String, LensSystem>>> = OnceLock::new();
    let mut c = CACHE.get_or_init(|| Mutex::new(Default::default())).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = c.get(file) {
        return s.clone();
    }
    let fallback = LensSystem::double_gauss50;
    let sys = if file.is_empty() {
        fallback()
    } else {
        // LensSystem::load のエラーはすでに「パス: line N: ...」の形なので、そのまま出す（パスを重ねない）。
        // 面の数の検査のエラーは同じ形になるようにパスを付ける
        let path = std::path::Path::new(file);
        match LensSystem::load(path).and_then(|s| check_lens_for_gpu(s).map_err(|e| format!("{}: {e}", path.display()))) {
            Ok(s) => s,
            Err(e) => {
                krbr_core::error!("lens: {}; using {}", e, fallback().name);
                fallback()
            }
        }
    };
    debug_assert!(sys.surfaces.len() <= MAX_LENS_SURFACES, "the built-in lens must fit the frame constants");
    c.insert(file.to_string(), sys.clone());
    sys
}

/// レンズの値（make_frame_ubo から呼ぶ）
pub fn fill_lens(u: &mut FrameUbo, fp: &FrameParams) {
    let l: &LensParams = &fp.lens;
    let cam = &fp.camera;
    let focus = if l.focus_distance_mm > 0.0 { l.focus_distance_mm } else { cam.distance };
    let f = l.focal_length_mm.max(1.0);
    let a = 0.5 * f / l.f_number.max(0.5);
    u.lens0 = Vec4::new(l.model as u32 as f32, a, focus, if l.blades >= 3 { l.blades as f32 } else { 0.0 });
    // 口径食：画面の隅で、前の開口（半径 a）が絞りに対して直径 × optical_vignetting だけずれる距離に置く
    let tan_corner = cam.tan_half_fov_y * (1.0 + cam.aspect * cam.aspect).sqrt();
    let d = if l.optical_vignetting > 0.0 { l.optical_vignetting * 2.0 * a / tan_corner.max(1e-4) } else { 0.0 };
    u.lens1 = Vec4::new(l.blade_rotation_deg.to_radians(), l.blade_roundness.clamp(0.0, 1.0), d, a);
    let v = if focus > f * 1.0001 { 1.0 / (1.0 / f - 1.0 / focus) } else { 1e9 };
    u.lens2 = Vec4::new(if l.natural_vignetting { 1.0 } else { 0.0 }, l.lateral_ca, v, f);
    const ND: f32 = 1.5168; // BK7 相当
    u.lens3 = Vec4::new(ND, if l.abbe_number > 0.0 { (ND - 1.0) / l.abbe_number } else { 0.0 }, 0.0, 0.0);
    if l.model == LensModel::Realistic {
        // F 値は設計値（開放）から絞る方向にだけ効く。絞りの面の有効径を縮める
        let sys = lens_system_for(&l.lens_file).stopped_down(l.f_number);
        let fe = sys.effective_focal_length();
        // 焦点距離に近いほどフィルムが遠くなり、焦点距離より近いと実像ができない。焦点距離の 1.25 倍より近くには合わせない
        // （Ctrl + クリックで手前の物を選んでも、像が壊れて真っ白にならないように）
        let front_to_focus = focus.max(MIN_FOCUS_IN_FOCAL_LENGTHS * fe).max(1.0);
        let film_dist = sys.film_distance_for(front_to_focus);
        let zs = sys.vertex_z();
        // 面の数は lens_system_for が MAX_LENS_SURFACES 以下に限っている（切り詰めると光学系が食い違うので、そうしない）
        let n = sys.surfaces.len();
        for i in 0..n {
            u.lens_surfaces[i] = Vec4::new(sys.surfaces[i].radius, zs[i], sys.surfaces[i].ior, 0.5 * sys.surfaces[i].aperture);
        }
        // フィルムの大きさは、端の主光線（絞りの中心を通る光線）が設定の画角の向きになるように決める。ピントを変えても
        // 画角（基板の見かけの大きさ）が変わらない。求まらなければ、薄いレンズの近似（ピントの距離での倍率）
        let (center_weight, film_half_h) = film_constants_cached(&l.lens_file, l.f_number, &sys, film_dist, cam.tan_half_fov_y);
        let half_h = film_half_h.unwrap_or_else(|| fe * cam.tan_half_fov_y * if front_to_focus > fe * 1.01 { front_to_focus / (front_to_focus - fe) } else { 1.0 });
        u.lens3.z = 1.0 / center_weight.max(1e-12);
        u.lens3.w = n as f32;
        u.lens4 = Vec4::new(half_h * cam.aspect, half_h, zs[n - 1] - film_dist, 0.5 * sys.surfaces[n - 1].aperture);
        // 射出瞳の表（フィルムの隅まで）
        let r_max = half_h * (1.0 + cam.aspect * cam.aspect).sqrt();
        let table = exit_pupil_cached(&l.lens_file, l.f_number, &sys, film_dist, r_max);
        // 開発用：KRBR_LENS_FULL_DISK を付けると、射出瞳を使わずに後ろの面全体から選ぶ（前の方法。比べるため）
        let full_disk = if std::env::var_os("KRBR_LENS_FULL_DISK").is_some() { 1.0 } else { 0.0 };
        u.lens5 = Vec4::new(r_max, full_disk, 0.0, 0.0);
        for (dst, b) in u.lens_pupil.iter_mut().zip(table.iter()) {
            *dst = Vec4::from_array(*b);
        }
    }
}

/// SPIR-V（shader_dir() の name）を読んでシェーダモジュールを作る
pub(crate) fn load_shader(dev: &ash::Device, name: &str) -> Result<vk::ShaderModule, String> {
    let p = crate::shader_dir().join(name);
    let code = std::fs::read(&p).map_err(|e| format!("cannot read shader {}: {e}", p.display()))?;
    if code.len() % 4 != 0 {
        return Err(format!("bad shader {} ({} bytes, not a multiple of 4)", p.display(), code.len()));
    }
    let words: Vec<u32> = code.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    let ci = vk::ShaderModuleCreateInfo::default().code(&words);
    unsafe { dev.create_shader_module(&ci, None) }.map_err(|e| format!("vkCreateShaderModule({name}): {e:?}"))
}

/// 計算シェーダ 1 本のパイプライン（入口は main）
pub(crate) fn compute_pipeline(dev: &ash::Device, layout: vk::PipelineLayout, spv: &str) -> Result<vk::Pipeline, String> {
    let sm = load_shader(dev, spv)?;
    let entry = CString::new("main").unwrap();
    let stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE).module(sm).name(&entry);
    let r = unsafe { dev.create_compute_pipelines(vk::PipelineCache::null(), &[vk::ComputePipelineCreateInfo::default().stage(stage).layout(layout)], None) };
    unsafe { dev.destroy_shader_module(sm, None) };
    Ok(r.map_err(|(_, e)| format!("vkCreateComputePipelines({spv}): {e:?}"))?[0])
}

fn align_up(v: u32, a: u32) -> u32 {
    v.div_ceil(a) * a
}

/// レイを飛ばすカーネル 1 本（記述子の組は 1 つ、プッシュ定数は PushConstants）。
/// 画素ごとに 1 スレッドで、GPU によって次のどちらかで作る（シェーダの本文は同じ）：
///   ray query            : 計算シェーダ shaders/<name>.comp.spv
///   ray tracing pipeline : レイ生成シェーダ shaders/<name>.rgen.spv ＋ rt_hit.rchit / rt_miss.rmiss / rt_shadow.rmiss
pub struct Kernel<'a> {
    ctx: &'a Context,
    rt_pipeline: bool,
    set_layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    sbt: Buffer,
    sbt_raygen: vk::StridedDeviceAddressRegionKHR,
    sbt_miss: vk::StridedDeviceAddressRegionKHR,
    sbt_hit: vk::StridedDeviceAddressRegionKHR,
}

impl<'a> Kernel<'a> {
    pub fn new(ctx: &'a Context, name: &str, bindings: &[Binding]) -> Result<Self, String> {
        let dev = &ctx.device;
        let rt = ctx.use_rt_pipeline;
        // 資源は 1 つ作るたびにフィールドへ入れる。途中で失敗して返ると、Drop が作った分だけを破棄する（null は何もしない）
        let mut k = Self {
            ctx,
            rt_pipeline: rt,
            set_layout: vk::DescriptorSetLayout::null(),
            pool: vk::DescriptorPool::null(),
            set: vk::DescriptorSet::null(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            sbt: Buffer::default(),
            sbt_raygen: Default::default(),
            sbt_miss: Default::default(),
            sbt_hit: Default::default(),
        };
        let stages = if rt { vk::ShaderStageFlags::RAYGEN_KHR | vk::ShaderStageFlags::CLOSEST_HIT_KHR | vk::ShaderStageFlags::MISS_KHR } else { vk::ShaderStageFlags::COMPUTE };
        let lb: Vec<vk::DescriptorSetLayoutBinding> = bindings.iter().map(|b| vk::DescriptorSetLayoutBinding::default().binding(b.binding()).descriptor_type(b.ty()).descriptor_count(1).stage_flags(stages)).collect();
        k.set_layout = unsafe { dev.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&lb), None) }.map_err(|e| format!("vkCreateDescriptorSetLayout({name}): {e:?}"))?;
        let mut counts: std::collections::BTreeMap<vk::DescriptorType, u32> = Default::default();
        for b in bindings {
            *counts.entry(b.ty()).or_default() += 1;
        }
        let sizes: Vec<vk::DescriptorPoolSize> = counts.iter().map(|(t, n)| vk::DescriptorPoolSize { ty: *t, descriptor_count: *n }).collect();
        k.pool = unsafe { dev.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes), None) }.map_err(|e| format!("vkCreateDescriptorPool({name}): {e:?}"))?;
        let layouts = [k.set_layout];
        let set = unsafe { dev.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(k.pool).set_layouts(&layouts)) }.map_err(|e| format!("vkAllocateDescriptorSets({name}): {e:?}"))?[0];
        k.set = set;

        // 記述子の書き込み（参照先の構造体は書き込みが終わるまで生かしておく）
        let mut buffer_infos: Vec<vk::DescriptorBufferInfo> = Vec::new();
        let mut image_infos: Vec<vk::DescriptorImageInfo> = Vec::new();
        let mut tlases: Vec<vk::AccelerationStructureKHR> = Vec::new();
        for b in bindings {
            match b {
                Binding::Buffer { buffer, .. } => buffer_infos.push(vk::DescriptorBufferInfo { buffer: *buffer, offset: 0, range: vk::WHOLE_SIZE }),
                Binding::Image { sampler, view, layout, .. } => image_infos.push(vk::DescriptorImageInfo { sampler: *sampler, image_view: *view, image_layout: *layout }),
                Binding::Tlas { tlas, .. } => tlases.push(*tlas),
            }
        }
        let mut as_writes: Vec<vk::WriteDescriptorSetAccelerationStructureKHR> = tlases.iter().map(|t| vk::WriteDescriptorSetAccelerationStructureKHR::default().acceleration_structures(std::slice::from_ref(t))).collect();
        let mut writes: Vec<vk::WriteDescriptorSet> = Vec::new();
        let (mut bi, mut ii) = (0, 0);
        for b in bindings {
            let w = vk::WriteDescriptorSet::default().dst_set(set).dst_binding(b.binding()).descriptor_type(b.ty());
            match b {
                Binding::Buffer { .. } => {
                    writes.push(w.buffer_info(std::slice::from_ref(&buffer_infos[bi])));
                    bi += 1;
                }
                Binding::Image { .. } => {
                    writes.push(w.image_info(std::slice::from_ref(&image_infos[ii])));
                    ii += 1;
                }
                Binding::Tlas { .. } => {}
            }
        }
        // 加速構造の記述子は pNext で渡す（要素ごとに別の可変借用にする）
        let tlas_bindings: Vec<u32> = bindings.iter().filter_map(|b| if let Binding::Tlas { binding, .. } = b { Some(*binding) } else { None }).collect();
        for (aw, binding) in as_writes.iter_mut().zip(tlas_bindings) {
            let mut w = vk::WriteDescriptorSet::default().dst_set(set).dst_binding(binding).descriptor_type(vk::DescriptorType::ACCELERATION_STRUCTURE_KHR).push_next(aw);
            w.descriptor_count = 1;
            writes.push(w);
        }
        unsafe { dev.update_descriptor_sets(&writes, &[]) };

        let pcr = [vk::PushConstantRange { stage_flags: stages, offset: 0, size: std::mem::size_of::<PushConstants>() as u32 }];
        k.layout = unsafe { dev.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts).push_constant_ranges(&pcr), None) }.map_err(|e| format!("vkCreatePipelineLayout({name}): {e:?}"))?;

        if rt {
            k.create_ray_tracing_pipeline(name)?;
        } else {
            k.pipeline = compute_pipeline(dev, k.layout, &format!("{name}.comp.spv"))?;
        }
        Ok(k)
    }

    /// ray tracing pipeline：レイ生成 1 本 ＋ ミス 2 本（最近接用・影用）＋ 三角形のヒットグループ 1 つ。再帰はしない
    fn create_ray_tracing_pipeline(&mut self, name: &str) -> Result<(), String> {
        let dev = &self.ctx.device;
        let rt_dev = self.ctx.rt_device.as_ref().ok_or("no ray tracing pipeline device")?;
        let entry = CString::new("main").unwrap();
        let files = [format!("{name}.rgen.spv"), "rt_miss.rmiss.spv".into(), "rt_shadow.rmiss.spv".into(), "rt_hit.rchit.spv".into()];
        let stage_of = [vk::ShaderStageFlags::RAYGEN_KHR, vk::ShaderStageFlags::MISS_KHR, vk::ShaderStageFlags::MISS_KHR, vk::ShaderStageFlags::CLOSEST_HIT_KHR];
        let mut modules = Vec::new();
        for f in &files {
            match load_shader(dev, f) {
                Ok(m) => modules.push(m),
                Err(e) => {
                    for m in modules {
                        unsafe { dev.destroy_shader_module(m, None) };
                    }
                    return Err(e);
                }
            }
        }
        let stages: Vec<vk::PipelineShaderStageCreateInfo> = (0..4).map(|i| vk::PipelineShaderStageCreateInfo::default().stage(stage_of[i]).module(modules[i]).name(&entry)).collect();
        let mut groups: Vec<vk::RayTracingShaderGroupCreateInfoKHR> = (0..3)
            .map(|i| vk::RayTracingShaderGroupCreateInfoKHR::default().ty(vk::RayTracingShaderGroupTypeKHR::GENERAL).general_shader(i).closest_hit_shader(vk::SHADER_UNUSED_KHR).any_hit_shader(vk::SHADER_UNUSED_KHR).intersection_shader(vk::SHADER_UNUSED_KHR))
            .collect();
        groups.push(vk::RayTracingShaderGroupCreateInfoKHR::default().ty(vk::RayTracingShaderGroupTypeKHR::TRIANGLES_HIT_GROUP).general_shader(vk::SHADER_UNUSED_KHR).closest_hit_shader(3).any_hit_shader(vk::SHADER_UNUSED_KHR).intersection_shader(vk::SHADER_UNUSED_KHR));
        let ci = vk::RayTracingPipelineCreateInfoKHR::default().stages(&stages).groups(&groups).max_pipeline_ray_recursion_depth(1).layout(self.layout);
        let r = unsafe { rt_dev.create_ray_tracing_pipelines(vk::DeferredOperationKHR::null(), vk::PipelineCache::null(), &[ci], None) };
        for m in modules {
            unsafe { dev.destroy_shader_module(m, None) };
        }
        self.pipeline = r.map_err(|(_, e)| format!("vkCreateRayTracingPipelinesKHR: {e:?}"))?[0];

        // シェーダバインディングテーブル：[レイ生成][ミス×2][ヒット×1]。各領域の先頭は baseAlignment に揃える
        let rp = &self.ctx.rt_props;
        let handle = rp.shader_group_handle_size;
        let stride = align_up(handle, rp.shader_group_handle_alignment);
        let base = rp.shader_group_base_alignment;
        let (raygen_size, miss_size, hit_size) = (align_up(stride, base), align_up(2 * stride, base), align_up(stride, base));
        let handles = unsafe { rt_dev.get_ray_tracing_shader_group_handles(self.pipeline, 0, 4, 4 * handle as usize) }.map_err(|e| format!("{e:?}"))?;
        let mut table = vec![0u8; (raygen_size + miss_size + hit_size) as usize];
        let h = handle as usize;
        table[..h].copy_from_slice(&handles[..h]);
        let m0 = raygen_size as usize;
        table[m0..m0 + h].copy_from_slice(&handles[h..2 * h]);
        let m1 = m0 + stride as usize;
        table[m1..m1 + h].copy_from_slice(&handles[2 * h..3 * h]);
        let hg = (raygen_size + miss_size) as usize;
        table[hg..hg + h].copy_from_slice(&handles[3 * h..4 * h]);
        self.sbt = self.ctx.create_device_buffer(&table, vk::BufferUsageFlags::SHADER_BINDING_TABLE_KHR | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS)?;
        self.sbt_raygen = vk::StridedDeviceAddressRegionKHR { device_address: self.sbt.address, stride: stride as u64, size: stride as u64 };
        self.sbt_miss = vk::StridedDeviceAddressRegionKHR { device_address: self.sbt.address + raygen_size as u64, stride: stride as u64, size: 2 * stride as u64 };
        self.sbt_hit = vk::StridedDeviceAddressRegionKHR { device_address: self.sbt.address + (raygen_size + miss_size) as u64, stride: stride as u64, size: stride as u64 };
        krbr_core::debug!("{}: ray tracing pipeline, SBT {} bytes (handle {}, stride {}, base {})", name, table.len(), handle, stride, base);
        Ok(())
    }

    pub fn dispatch(&self, cmd: vk::CommandBuffer, pc: PushConstants, width: u32, height: u32) {
        let dev = &self.ctx.device;
        let bytes = crate::vk::as_bytes(std::slice::from_ref(&pc));
        unsafe {
            if self.rt_pipeline {
                dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::RAY_TRACING_KHR, self.pipeline);
                dev.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::RAY_TRACING_KHR, self.layout, 0, &[self.set], &[]);
                dev.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::RAYGEN_KHR | vk::ShaderStageFlags::CLOSEST_HIT_KHR | vk::ShaderStageFlags::MISS_KHR, 0, bytes);
                let callable = vk::StridedDeviceAddressRegionKHR::default();
                self.ctx.rt_device.as_ref().unwrap().cmd_trace_rays(cmd, &self.sbt_raygen, &self.sbt_miss, &self.sbt_hit, &callable, width, height, 1);
            } else {
                dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, self.pipeline);
                dev.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::COMPUTE, self.layout, 0, &[self.set], &[]);
                dev.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::COMPUTE, 0, bytes);
                dev.cmd_dispatch(cmd, width.div_ceil(8), height.div_ceil(8), 1);
            }
        }
    }
}

impl Drop for Kernel<'_> {
    fn drop(&mut self) {
        let dev = &self.ctx.device;
        self.ctx.destroy_buffer(&mut self.sbt);
        // 作りかけ（Kernel::new の途中で失敗）なら一部が null。vkDestroy* は null を何もせずに通す
        unsafe {
            dev.destroy_pipeline(self.pipeline, None);
            dev.destroy_pipeline_layout(self.layout, None);
            dev.destroy_descriptor_pool(self.pool, None);
            dev.destroy_descriptor_set_layout(self.set_layout, None);
        }
    }
}

/// レイを飛ばすカーネルの書き込みを、次のカーネル・計算シェーダ・転送が読めるようにする
pub fn compute_barrier(ctx: &Context, cmd: vk::CommandBuffer) {
    let trace = ctx.trace_stages();
    memory_barrier(
        &ctx.device,
        cmd,
        trace | vk::PipelineStageFlags2::TRANSFER,
        vk::AccessFlags2::SHADER_STORAGE_WRITE | vk::AccessFlags2::TRANSFER_WRITE,
        trace | vk::PipelineStageFlags2::COPY,
        vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE | vk::AccessFlags2::TRANSFER_READ,
    );
}
