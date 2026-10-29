//! 表示用のトーンマッピング（累積画像 -> RGBA8。shaders/tonemap.comp）と、その前のグロー（shaders/glow.comp。crate::glow と同じ式）

use ash::vk;
use glam::Vec4;

use crate::glow;
use crate::integrator::RenderTargets;
use crate::kernel::compute_pipeline;
use crate::vk::{image_barrier, memory_barrier, Context, GpuPod, Image};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum DisplayMode {
    Beauty = 0,
    Albedo = 1,
    Normal = 2,
}

/// 背景を透明にした画像（FrameParams::transparent_background）の、アルファの扱い
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Background {
    /// アルファを使わない（背景を透明にしていない）
    Opaque,
    /// 透明な所を市松模様にして見せる（表示）
    Checker,
    /// アルファを付けて出す（保存。色はアルファで割った値）
    Alpha,
}

impl Background {
    /// 表示用：背景を透明にしていれば市松模様
    pub fn for_display(transparent: bool) -> Self {
        if transparent {
            Self::Checker
        } else {
            Self::Opaque
        }
    }

    /// 保存用：背景を透明にしていればアルファを付ける
    pub fn for_saving(transparent: bool) -> Self {
        if transparent {
            Self::Alpha
        } else {
            Self::Opaque
        }
    }
}

/// 市松模様の 1 マス（画素）
const CHECKER_PX: f32 = 12.0;

#[repr(C)]
#[derive(Clone, Copy)]
struct TonemapPush {
    exposure_scale: f32,
    mode: u32,
    width: u32,
    height: u32,
    xyz_to_rgb: [Vec4; 3],
    /// x: 強さ / 段の重みの合計（0 ならグローなし）, y, z: グローの最上段の大きさ
    glow: Vec4,
    /// x: 0 = 不透明, 1 = 市松模様の上に合成, 2 = アルファを付ける（Background）。y: 市松の 1 マス（画素）
    background: Vec4,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct GlowPush {
    mode: u32,
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
    exposure_scale: f32,
    threshold: f32,
    knee: f32,
    spread: f32,
    _pad: [f32; 3],
    xyz_to_rgb: [Vec4; 3],
}

// tonemap.comp・glow.comp のプッシュ定数と同じ大きさ（隙間が無いことの確認も兼ねる）
const _: () = assert!(std::mem::size_of::<TonemapPush>() == 96);
const _: () = assert!(std::mem::size_of::<GlowPush>() == 96);
// 安全性：repr(C) で、上の assert により隙間が無い
unsafe impl GpuPod for TonemapPush {}
unsafe impl GpuPod for GlowPush {}

/// グローの 1 回の処理（mode は glow.comp のとおり）
struct GlowPass {
    set: vk::DescriptorSet,
    mode: u32,
    src: (u32, u32),
    dst: (u32, u32),
}

pub struct Tonemapper<'a> {
    ctx: &'a Context,
    width: u32,
    height: u32,
    /// 出力（RGBA8）。ふつうは 1 枚。ビューアは、描いている間に表示が前の画像を読めるように複数枚を順に使う
    outs: Vec<Image>,
    set_layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    /// 出力ごとの記述子の組（入力は同じ）
    sets: Vec<vk::DescriptorSet>,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    // グロー：縮めた段（levels[0] が半分の解像度）と、足し上げた段（sums[i] = levels[i] + 広げた sums[i + 1]）
    levels: Vec<Image>,
    sums: Vec<Image>,
    glow_passes: Vec<GlowPass>,
    glow_set_layout: vk::DescriptorSetLayout,
    glow_layout: vk::PipelineLayout,
    glow_pipeline: vk::Pipeline,
}

fn storage_set_layout(dev: &ash::Device, n: u32) -> Result<vk::DescriptorSetLayout, String> {
    let lb: Vec<vk::DescriptorSetLayoutBinding> = (0..n).map(|i| vk::DescriptorSetLayoutBinding::default().binding(i).descriptor_type(vk::DescriptorType::STORAGE_IMAGE).descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE)).collect();
    unsafe { dev.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&lb), None) }.map_err(|e| format!("{e:?}"))
}

fn write_storage_images(dev: &ash::Device, set: vk::DescriptorSet, views: &[vk::ImageView]) {
    let infos: Vec<vk::DescriptorImageInfo> = views.iter().map(|v| vk::DescriptorImageInfo { sampler: vk::Sampler::null(), image_view: *v, image_layout: vk::ImageLayout::GENERAL }).collect();
    let writes: Vec<vk::WriteDescriptorSet> = (0..infos.len()).map(|i| vk::WriteDescriptorSet::default().dst_set(set).dst_binding(i as u32).descriptor_type(vk::DescriptorType::STORAGE_IMAGE).image_info(std::slice::from_ref(&infos[i]))).collect();
    unsafe { dev.update_descriptor_sets(&writes, &[]) };
}

impl<'a> Tonemapper<'a> {
    /// 出力（RGBA8、GENERAL レイアウト）を作る。targets が変わったら作り直す
    pub fn new(ctx: &'a Context, targets: &RenderTargets) -> Result<Self, String> {
        Self::with_color(ctx, targets.color.view, targets)
    }

    /// 色の画像を差し替えて作る（ノイズ除去の結果を表示するとき。A が割る数になっている画像）。アルベド・法線・
    /// アルファ（最初に当たった点）は targets のもの
    pub fn with_color(ctx: &'a Context, color: vk::ImageView, targets: &RenderTargets) -> Result<Self, String> {
        Self::with_views(ctx, color, targets.albedo.view, targets.normal.view, targets.position.view, targets.width, targets.height)
    }

    /// 出力を outputs 枚持つものを作る（record_to で書く先を選ぶ）
    pub fn with_outputs(ctx: &'a Context, targets: &RenderTargets, outputs: usize) -> Result<Self, String> {
        Self::build(ctx, targets.color.view, targets.albedo.view, targets.normal.view, targets.position.view, targets.width, targets.height, outputs)
    }

    /// 画像を指定して作る。position はアルファ（背景を透明にしたとき）に使う、最初に当たった点の累積。
    /// 失敗したら Err（作った分は Drop が解放する）
    pub fn with_views(ctx: &'a Context, color: vk::ImageView, albedo: vk::ImageView, normal: vk::ImageView, position: vk::ImageView, width: u32, height: u32) -> Result<Self, String> {
        Self::build(ctx, color, albedo, normal, position, width, height, 1)
    }

    #[allow(clippy::too_many_arguments)] // 入力の画像（with_views と同じ）と出力の枚数
    fn build(ctx: &'a Context, color: vk::ImageView, albedo: vk::ImageView, normal: vk::ImageView, position: vk::ImageView, width: u32, height: u32, outputs: usize) -> Result<Self, String> {
        let dev = &ctx.device;
        let outputs = outputs.max(1);
        // 資源は 1 つ作るたびにフィールドへ入れる。途中で失敗して返ると、Drop が作った分だけを破棄する（null は何もしない）
        let mut t = Self {
            ctx,
            width,
            height,
            outs: Vec::new(),
            set_layout: vk::DescriptorSetLayout::null(),
            pool: vk::DescriptorPool::null(),
            sets: Vec::new(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            levels: Vec::new(),
            sums: Vec::new(),
            glow_passes: Vec::new(),
            glow_set_layout: vk::DescriptorSetLayout::null(),
            glow_layout: vk::PipelineLayout::null(),
            glow_pipeline: vk::Pipeline::null(),
        };
        // シェーダが読めないなど失敗しやすいものを先に作る
        t.set_layout = storage_set_layout(dev, 6)?;
        t.glow_set_layout = storage_set_layout(dev, 3)?;
        let layouts = [t.set_layout];
        let pcr = [vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::COMPUTE, offset: 0, size: std::mem::size_of::<TonemapPush>() as u32 }];
        t.layout = unsafe { dev.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts).push_constant_ranges(&pcr), None) }.map_err(|e| format!("{e:?}"))?;
        t.pipeline = compute_pipeline(dev, t.layout, "tonemap.comp.spv")?;
        let gpcr = [vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::COMPUTE, offset: 0, size: std::mem::size_of::<GlowPush>() as u32 }];
        let gl = [t.glow_set_layout];
        t.glow_layout = unsafe { dev.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&gl).push_constant_ranges(&gpcr), None) }.map_err(|e| format!("{e:?}"))?;
        t.glow_pipeline = compute_pipeline(dev, t.glow_layout, "glow.comp.spv")?;

        for _ in 0..outputs {
            t.outs.push(ctx.create_image_2d(width, height, 1, vk::Format::R8G8B8A8_UNORM, vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC)?);
        }
        // グローの段（画像が小さすぎて段が無いときも、tonemap.comp に渡す 1×1 を作る）
        let sizes = glow::level_sizes(width, height);
        let make = |(w, h): (u32, u32)| ctx.create_image_2d(w, h, 1, vk::Format::R32G32B32A32_SFLOAT, vk::ImageUsageFlags::STORAGE);
        for s in if sizes.is_empty() { vec![(1, 1)] } else { sizes.clone() } {
            t.levels.push(make(s)?);
        }
        for &s in sizes.iter().take(sizes.len().saturating_sub(1)) {
            t.sums.push(make(s)?);
        }
        let images: Vec<vk::Image> = t.outs.iter().chain(&t.levels).chain(&t.sums).map(|i| i.image).collect();
        ctx.submit_and_wait(|dev, cmd| {
            for img in &images {
                image_barrier(dev, cmd, *img, vk::ImageLayout::UNDEFINED, vk::ImageLayout::GENERAL, vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE, vk::PipelineStageFlags2::COMPUTE_SHADER, vk::AccessFlags2::SHADER_STORAGE_WRITE, 1);
            }
        })?;
        let n_glow_sets = if sizes.is_empty() { 0 } else { 2 * sizes.len() as u32 - 1 };
        let n_out = outputs as u32;
        let sizes_pool = [vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_IMAGE, descriptor_count: 6 * n_out + 3 * n_glow_sets }];
        t.pool = unsafe { dev.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(n_out + n_glow_sets).pool_sizes(&sizes_pool), None) }.map_err(|e| format!("{e:?}"))?;
        // 最上段：段が 1 つなら縮めた段、2 つ以上なら足し上げた段
        let top = t.sums.first().unwrap_or(&t.levels[0]).view;
        for k in 0..outputs {
            let set = unsafe { dev.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(t.pool).set_layouts(&layouts)) }.map_err(|e| format!("{e:?}"))?[0];
            write_storage_images(dev, set, &[color, albedo, normal, t.outs[k].view, top, position]);
            t.sets.push(set);
        }

        // グローの処理の並び：縮める（0 段目は累積画像から）→ 小さい段から足し上げる
        let n = sizes.len();
        let (pool, levels, sums) = (t.pool, &t.levels, &t.sums);
        let mut glow_passes = Vec::new();
        let mut add_pass = |views: [vk::ImageView; 3], mode: u32, src: (u32, u32), dst: (u32, u32)| -> Result<(), String> {
            let s = unsafe { dev.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(pool).set_layouts(&gl)) }.map_err(|e| format!("{e:?}"))?[0];
            write_storage_images(dev, s, &views);
            glow_passes.push(GlowPass { set: s, mode, src, dst });
            Ok(())
        };
        for i in 0..n {
            let (src, src_size) = if i == 0 { (color, (width, height)) } else { (levels[i - 1].view, sizes[i - 1]) };
            add_pass([src, levels[i].view, src], if i == 0 { 0 } else { 1 }, src_size, sizes[i])?;
        }
        // 一番小さい段は縮めた段をそのまま使う
        for i in (0..n.saturating_sub(1)).rev() {
            let small = if i + 1 == n - 1 { levels[n - 1].view } else { sums[i + 1].view };
            add_pass([small, sums[i].view, levels[i].view], 2, sizes[i + 1], sizes[i])?;
        }
        t.glow_passes = glow_passes;
        Ok(t)
    }

    /// xyz_input: 累積が CIE XYZ（分光モード）なら true。glow: グロー（画像の表示のときだけかける）。
    /// background: 背景を透明にした画像のアルファの扱い（画像の表示のときだけ）。出力は output()（1 枚目）
    pub fn record(&self, cmd: vk::CommandBuffer, exposure_ev: f32, mode: DisplayMode, xyz_input: bool, glow: &glow::GlowParams, background: Background) {
        self.record_to(cmd, 0, exposure_ev, mode, xyz_input, glow, background);
    }

    /// record と同じで、slot 枚目の出力（output_at）へ書く
    #[allow(clippy::too_many_arguments)] // record の引数と書く先
    pub fn record_to(&self, cmd: vk::CommandBuffer, slot: usize, exposure_ev: f32, mode: DisplayMode, xyz_input: bool, glow: &glow::GlowParams, background: Background) {
        let dev = &self.ctx.device;
        let m = krbr_pbr::spectrum::xyz_to_linear_srgb();
        let w = if xyz_input { 1.0 } else { 0.0 };
        let xyz_to_rgb = [m.col(0).extend(w), m.col(1).extend(w), m.col(2).extend(w)];
        let exposure_scale = 2f32.powf(exposure_ev);
        let use_glow = glow.is_on() && mode == DisplayMode::Beauty && !self.glow_passes.is_empty();
        let n_levels = glow::level_sizes(self.width, self.height).len();
        let compute = vk::PipelineStageFlags2::COMPUTE_SHADER;
        unsafe {
            if use_glow {
                // 前の表示がグローの段を読み終えてから書く
                memory_barrier(dev, cmd, compute, vk::AccessFlags2::SHADER_STORAGE_READ, compute, vk::AccessFlags2::SHADER_STORAGE_WRITE);
                dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, self.glow_pipeline);
                for p in &self.glow_passes {
                    let pc = GlowPush { mode: p.mode, src_w: p.src.0, src_h: p.src.1, dst_w: p.dst.0, dst_h: p.dst.1, exposure_scale, threshold: glow.threshold, knee: glow::KNEE * glow.threshold, spread: glow.spread, _pad: [0.0; 3], xyz_to_rgb };
                    dev.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::COMPUTE, self.glow_layout, 0, &[p.set], &[]);
                    dev.cmd_push_constants(cmd, self.glow_layout, vk::ShaderStageFlags::COMPUTE, 0, crate::vk::as_bytes(std::slice::from_ref(&pc)));
                    dev.cmd_dispatch(cmd, p.dst.0.div_ceil(8), p.dst.1.div_ceil(8), 1);
                    memory_barrier(dev, cmd, compute, vk::AccessFlags2::SHADER_STORAGE_WRITE, compute, vk::AccessFlags2::SHADER_STORAGE_READ);
                }
            }
            let top = glow::level_sizes(self.width, self.height).first().copied().unwrap_or((1, 1));
            let glow_v = if use_glow { Vec4::new(glow.strength / glow.weight_sum(n_levels), top.0 as f32, top.1 as f32, 0.0) } else { Vec4::ZERO };
            let bg = match background {
                Background::Opaque => 0.0,
                Background::Checker => 1.0,
                Background::Alpha => 2.0,
            };
            let pc = TonemapPush { exposure_scale, mode: mode as u32, width: self.width, height: self.height, xyz_to_rgb, glow: glow_v, background: Vec4::new(bg, CHECKER_PX, 0.0, 0.0) };
            dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, self.pipeline);
            dev.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::COMPUTE, self.layout, 0, &[self.sets[slot]], &[]);
            dev.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::COMPUTE, 0, crate::vk::as_bytes(std::slice::from_ref(&pc)));
            dev.cmd_dispatch(cmd, self.width.div_ceil(8), self.height.div_ceil(8), 1);
        }
    }

    pub fn output(&self) -> &Image {
        &self.outs[0]
    }

    /// slot 枚目の出力（with_outputs で作ったとき）
    pub fn output_at(&self, slot: usize) -> &Image {
        &self.outs[slot]
    }

    pub fn outputs(&self) -> usize {
        self.outs.len()
    }
}

impl Drop for Tonemapper<'_> {
    fn drop(&mut self) {
        let dev = &self.ctx.device;
        self.ctx.wait_idle();
        unsafe {
            dev.destroy_pipeline(self.pipeline, None);
            dev.destroy_pipeline_layout(self.layout, None);
            dev.destroy_pipeline(self.glow_pipeline, None);
            dev.destroy_pipeline_layout(self.glow_layout, None);
            dev.destroy_descriptor_pool(self.pool, None);
            dev.destroy_descriptor_set_layout(self.set_layout, None);
            dev.destroy_descriptor_set_layout(self.glow_set_layout, None);
        }
        for img in self.outs.iter_mut().chain(self.levels.iter_mut()).chain(self.sums.iter_mut()) {
            self.ctx.destroy_image(img);
        }
    }
}
