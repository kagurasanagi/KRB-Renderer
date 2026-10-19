//! 積分器で共通の部品：レイを飛ばすカーネル（計算シェーダか ray tracing pipeline）、シーンの資源の記述子、フレームの定数

use std::ffi::CString;
use std::sync::{Mutex, OnceLock};

use ash::vk;
use glam::{UVec4, Vec3, Vec4};
use krbr_scene::lens::{LensModel, LensParams, LensSystem};
use krbr_scene::spectrum;

use crate::gpu_scene::GpuScene;
use crate::gpu_types::{FrameUbo, PushConstants};
use crate::integrator::{FrameParams, RenderTargets};
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

/// 全積分器で共通の記述子（0〜19：シーン・環境マップ・出力先・定数・光源）
pub fn scene_bindings(sc: &GpuScene, targets: &RenderTargets, frame_ubo: &Buffer) -> Vec<Binding> {
    let mut b = vec![
        Binding::Tlas { binding: 0, tlas: sc.tlas },
        buffer_binding(1, &sc.vertices),
        buffer_binding(2, &sc.indices),
        buffer_binding(3, &sc.tri_mats),
        buffer_binding(4, &sc.mesh_infos),
        buffer_binding(5, &sc.materials),
        Binding::Image { binding: 6, ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER, sampler: sc.env_sampler, view: sc.env_image.view, layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL },
        buffer_binding(7, &sc.env_marginal),
        buffer_binding(8, &sc.env_conditional),
        buffer_binding(9, &sc.env_pdf),
        Binding::Image { binding: 10, ty: vk::DescriptorType::COMBINED_IMAGE_SAMPLER, sampler: sc.board_sampler, view: sc.board_image.view, layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL },
    ];
    for (i, img) in [&targets.color, &targets.albedo, &targets.normal].iter().enumerate() {
        b.push(Binding::Image { binding: 11 + i as u32, ty: vk::DescriptorType::STORAGE_IMAGE, sampler: vk::Sampler::null(), view: img.view, layout: vk::ImageLayout::GENERAL });
    }
    b.push(Binding::Buffer { binding: 14, ty: vk::DescriptorType::UNIFORM_BUFFER, buffer: frame_ubo.buffer });
    b.push(buffer_binding(15, &sc.instance_infos));
    b.push(buffer_binding(16, &sc.rgb2spec));
    b.push(buffer_binding(17, &sc.rgb2spec_scale));
    b.push(buffer_binding(18, &sc.emitters));
    b.push(buffer_binding(19, &sc.emitter_cdf));
    b
}

/// フレームの定数（カメラ・環境光・光源の選び方など）
pub fn make_frame_ubo(sc: &GpuScene, targets: &RenderTargets, fp: &FrameParams) -> FrameUbo {
    let cam = &fp.camera;
    let mut u = FrameUbo {
        cam_pos: cam.position.extend(0.0),
        cam_right: (cam.right * (cam.tan_half_fov_y * cam.aspect)).extend(0.0),
        cam_up: (cam.up * cam.tan_half_fov_y).extend(0.0),
        cam_forward: cam.forward.extend(0.0),
        board_map: sc.board_map,
        env_params: Vec4::new(fp.env_intensity, fp.env_rotation_rad, fp.radiance_clamp, 0.0),
        dims: UVec4::new(targets.width, targets.height, sc.env_dist_w, sc.env_dist_h),
        counts: UVec4::new(fp.max_depth, fp.rr_start_depth, 0, 0),
        board_mats: UVec4::new(sc.board_mats.mask, sc.board_mats.copper, sc.board_mats.finish, sc.board_mats.fr4),
        board_mats2: UVec4::new(sc.board_mats.silk, sc.board_mats.copper, sc.board_mats.fr4, 0),
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
    let mut p_env = 1.0f32;
    if sc.emitter_count > 0 {
        p_env = if env_power > 0.0 { (env_power / (env_power + tri_power)).clamp(0.1, 0.9) as f32 } else { 0.0 };
    }
    u.light_params = Vec4::new(p_env, sc.emitter_power, sc.scene_radius, sc.emitter_count as f32);
    u.scene_center = sc.scene_center.extend(0.0);
    let radius = if fp.vcm_radius > 0.0 { fp.vcm_radius } else { 0.003 * sc.scene_radius };
    u.vcm_params = Vec4::new(radius, fp.vcm_alpha, targets.width as f32 * targets.height as f32, 0.0);
    fill_lens(&mut u, fp);
    u
}

/// 実レンズのデータは、ファイル名ごとに一度だけ読む
fn lens_system_for(file: &str) -> LensSystem {
    static CACHE: OnceLock<Mutex<std::collections::BTreeMap<String, LensSystem>>> = OnceLock::new();
    let mut c = CACHE.get_or_init(|| Mutex::new(Default::default())).lock().unwrap();
    if let Some(s) = c.get(file) {
        return s.clone();
    }
    let sys = if file.is_empty() {
        LensSystem::double_gauss50()
    } else {
        match LensSystem::load(std::path::Path::new(file)) {
            Ok(s) => s,
            Err(e) => {
                krbr_core::error!("lens: {}; using {}", e, LensSystem::double_gauss50().name);
                LensSystem::double_gauss50()
            }
        }
    };
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
        let front_to_focus = focus.max(1.0);
        let film_dist = sys.film_distance_for(front_to_focus);
        let fe = sys.effective_focal_length();
        let zs = sys.vertex_z();
        let n = sys.surfaces.len().min(16);
        for i in 0..n {
            u.lens_surfaces[i] = Vec4::new(sys.surfaces[i].radius, zs[i], sys.surfaces[i].ior, 0.5 * sys.surfaces[i].aperture);
        }
        // フィルムの大きさは、ピントの距離での倍率で設定の画角になるように
        let half_h = fe * cam.tan_half_fov_y * if front_to_focus > fe * 1.01 { front_to_focus / (front_to_focus - fe) } else { 1.0 };
        u.lens3.z = 1.0 / sys.center_weight(film_dist).max(1e-12);
        u.lens3.w = n as f32;
        u.lens4 = Vec4::new(half_h * cam.aspect, half_h, zs[n - 1] - film_dist, 0.5 * sys.surfaces[n - 1].aperture);
    }
}

fn load_shader(dev: &ash::Device, p: &std::path::Path) -> Result<vk::ShaderModule, String> {
    let code = std::fs::read(p).map_err(|_| format!("cannot read shader {}", p.display()))?;
    if code.len() % 4 != 0 {
        return Err(format!("bad shader {}", p.display()));
    }
    let words: Vec<u32> = code.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    let ci = vk::ShaderModuleCreateInfo::default().code(&words);
    unsafe { dev.create_shader_module(&ci, None) }.map_err(|e| format!("vkCreateShaderModule: {e:?}"))
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
        let stages = if rt { vk::ShaderStageFlags::RAYGEN_KHR | vk::ShaderStageFlags::CLOSEST_HIT_KHR | vk::ShaderStageFlags::MISS_KHR } else { vk::ShaderStageFlags::COMPUTE };
        let lb: Vec<vk::DescriptorSetLayoutBinding> = bindings.iter().map(|b| vk::DescriptorSetLayoutBinding::default().binding(b.binding()).descriptor_type(b.ty()).descriptor_count(1).stage_flags(stages)).collect();
        let set_layout = unsafe { dev.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&lb), None) }.map_err(|e| format!("{e:?}"))?;
        let mut counts: std::collections::BTreeMap<vk::DescriptorType, u32> = Default::default();
        for b in bindings {
            *counts.entry(b.ty()).or_default() += 1;
        }
        let sizes: Vec<vk::DescriptorPoolSize> = counts.iter().map(|(t, n)| vk::DescriptorPoolSize { ty: *t, descriptor_count: *n }).collect();
        let pool = unsafe { dev.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes), None) }.map_err(|e| format!("{e:?}"))?;
        let layouts = [set_layout];
        let set = unsafe { dev.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(pool).set_layouts(&layouts)) }.map_err(|e| format!("{e:?}"))?[0];

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
        let layout = unsafe { dev.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts).push_constant_ranges(&pcr), None) }.map_err(|e| format!("{e:?}"))?;

        let mut k = Self { ctx, rt_pipeline: rt, set_layout, pool, set, layout, pipeline: vk::Pipeline::null(), sbt: Buffer::default(), sbt_raygen: Default::default(), sbt_miss: Default::default(), sbt_hit: Default::default() };
        let dir = crate::shader_dir();
        let entry = CString::new("main").unwrap();
        if rt {
            k.create_ray_tracing_pipeline(name, &dir, &entry)?;
        } else {
            let sm = load_shader(dev, &dir.join(format!("{name}.comp.spv")))?;
            let stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE).module(sm).name(&entry);
            let cci = vk::ComputePipelineCreateInfo::default().stage(stage).layout(layout);
            let r = unsafe { dev.create_compute_pipelines(vk::PipelineCache::null(), &[cci], None) };
            unsafe { dev.destroy_shader_module(sm, None) };
            k.pipeline = r.map_err(|(_, e)| format!("vkCreateComputePipelines: {e:?}"))?[0];
        }
        Ok(k)
    }

    /// ray tracing pipeline：レイ生成 1 本 ＋ ミス 2 本（最近接用・影用）＋ 三角形のヒットグループ 1 つ。再帰はしない
    fn create_ray_tracing_pipeline(&mut self, name: &str, dir: &std::path::Path, entry: &CString) -> Result<(), String> {
        let dev = &self.ctx.device;
        let rt_dev = self.ctx.rt_device.as_ref().ok_or("no ray tracing pipeline device")?;
        let files = [format!("{name}.rgen.spv"), "rt_miss.rmiss.spv".into(), "rt_shadow.rmiss.spv".into(), "rt_hit.rchit.spv".into()];
        let stage_of = [vk::ShaderStageFlags::RAYGEN_KHR, vk::ShaderStageFlags::MISS_KHR, vk::ShaderStageFlags::MISS_KHR, vk::ShaderStageFlags::CLOSEST_HIT_KHR];
        let mut modules = Vec::new();
        for f in &files {
            modules.push(load_shader(dev, &dir.join(f))?);
        }
        let stages: Vec<vk::PipelineShaderStageCreateInfo> = (0..4).map(|i| vk::PipelineShaderStageCreateInfo::default().stage(stage_of[i]).module(modules[i]).name(entry)).collect();
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
        self.sbt = self.ctx.create_device_buffer(&table, vk::BufferUsageFlags::SHADER_BINDING_TABLE_KHR | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS);
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
        unsafe {
            if self.pipeline != vk::Pipeline::null() {
                dev.destroy_pipeline(self.pipeline, None);
            }
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

pub fn xyz_to_srgb_columns() -> [Vec3; 3] {
    let m = spectrum::xyz_to_linear_srgb();
    [m.col(0), m.col(1), m.col(2)]
}
