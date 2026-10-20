//! 表示用のトーンマッピング（累積画像 -> RGBA8。shaders/tonemap.comp）

use std::ffi::CString;

use ash::vk;
use glam::Vec4;

use crate::integrator::RenderTargets;
use crate::vk::{image_barrier, Context, Image};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum DisplayMode {
    Beauty = 0,
    Albedo = 1,
    Normal = 2,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct TonemapPush {
    exposure_scale: f32,
    mode: u32,
    width: u32,
    height: u32,
    xyz_to_rgb: [Vec4; 3],
}

pub struct Tonemapper<'a> {
    ctx: &'a Context,
    width: u32,
    height: u32,
    out: Image,
    set_layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}

impl<'a> Tonemapper<'a> {
    /// 出力（RGBA8、GENERAL レイアウト）を作る。targets が変わったら作り直す
    pub fn new(ctx: &'a Context, targets: &RenderTargets) -> Result<Self, String> {
        Self::with_views(ctx, targets.color.view, targets.albedo.view, targets.normal.view, targets.width, targets.height)
    }

    /// 色の画像を差し替えて作る（ノイズ除去の結果を表示するとき。A が割る数になっている画像）
    pub fn with_views(ctx: &'a Context, color: vk::ImageView, albedo: vk::ImageView, normal: vk::ImageView, width: u32, height: u32) -> Result<Self, String> {
        let dev = &ctx.device;
        let out = ctx.create_image_2d(width, height, 1, vk::Format::R8G8B8A8_UNORM, vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC);
        let img = out.image;
        ctx.submit_and_wait(|dev, cmd| {
            image_barrier(dev, cmd, img, vk::ImageLayout::UNDEFINED, vk::ImageLayout::GENERAL, vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE, vk::PipelineStageFlags2::COMPUTE_SHADER, vk::AccessFlags2::SHADER_STORAGE_WRITE, 1);
        });
        let lb: Vec<vk::DescriptorSetLayoutBinding> = (0..4).map(|i| vk::DescriptorSetLayoutBinding::default().binding(i).descriptor_type(vk::DescriptorType::STORAGE_IMAGE).descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE)).collect();
        let set_layout = unsafe { dev.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&lb), None) }.map_err(|e| format!("{e:?}"))?;
        let sizes = [vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_IMAGE, descriptor_count: 4 }];
        let pool = unsafe { dev.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes), None) }.map_err(|e| format!("{e:?}"))?;
        let layouts = [set_layout];
        let set = unsafe { dev.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(pool).set_layouts(&layouts)) }.map_err(|e| format!("{e:?}"))?[0];
        let infos: Vec<vk::DescriptorImageInfo> = [color, albedo, normal, out.view].iter().map(|v| vk::DescriptorImageInfo { sampler: vk::Sampler::null(), image_view: *v, image_layout: vk::ImageLayout::GENERAL }).collect();
        let writes: Vec<vk::WriteDescriptorSet> = (0..4).map(|i| vk::WriteDescriptorSet::default().dst_set(set).dst_binding(i as u32).descriptor_type(vk::DescriptorType::STORAGE_IMAGE).image_info(std::slice::from_ref(&infos[i]))).collect();
        unsafe { dev.update_descriptor_sets(&writes, &[]) };
        let pcr = [vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::COMPUTE, offset: 0, size: std::mem::size_of::<TonemapPush>() as u32 }];
        let layout = unsafe { dev.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts).push_constant_ranges(&pcr), None) }.map_err(|e| format!("{e:?}"))?;
        let code = std::fs::read(crate::shader_dir().join("tonemap.comp.spv")).map_err(|_| "cannot read tonemap.comp.spv".to_string())?;
        let words: Vec<u32> = code.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        let sm = unsafe { dev.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None) }.map_err(|e| format!("{e:?}"))?;
        let entry = CString::new("main").unwrap();
        let stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE).module(sm).name(&entry);
        let r = unsafe { dev.create_compute_pipelines(vk::PipelineCache::null(), &[vk::ComputePipelineCreateInfo::default().stage(stage).layout(layout)], None) };
        unsafe { dev.destroy_shader_module(sm, None) };
        let pipeline = r.map_err(|(_, e)| format!("vkCreateComputePipelines(tonemap): {e:?}"))?[0];
        Ok(Self { ctx, width, height, out, set_layout, pool, set, layout, pipeline })
    }

    /// xyz_input: 累積が CIE XYZ（分光モード）なら true
    pub fn record(&self, cmd: vk::CommandBuffer, exposure_ev: f32, mode: DisplayMode, xyz_input: bool) {
        let dev = &self.ctx.device;
        let m = krbr_scene::spectrum::xyz_to_linear_srgb();
        let w = if xyz_input { 1.0 } else { 0.0 };
        let pc = TonemapPush { exposure_scale: 2f32.powf(exposure_ev), mode: mode as u32, width: self.width, height: self.height, xyz_to_rgb: [m.col(0).extend(w), m.col(1).extend(w), m.col(2).extend(w)] };
        unsafe {
            dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, self.pipeline);
            dev.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::COMPUTE, self.layout, 0, &[self.set], &[]);
            dev.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::COMPUTE, 0, crate::vk::as_bytes(std::slice::from_ref(&pc)));
            dev.cmd_dispatch(cmd, self.width.div_ceil(8), self.height.div_ceil(8), 1);
        }
    }

    pub fn output(&self) -> &Image {
        &self.out
    }
}

impl Drop for Tonemapper<'_> {
    fn drop(&mut self) {
        let dev = &self.ctx.device;
        self.ctx.wait_idle();
        unsafe {
            dev.destroy_pipeline(self.pipeline, None);
            dev.destroy_pipeline_layout(self.layout, None);
            dev.destroy_descriptor_pool(self.pool, None);
            dev.destroy_descriptor_set_layout(self.set_layout, None);
        }
        self.ctx.destroy_image(&mut self.out);
    }
}
