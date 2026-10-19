//! 積分器：パストレーサー（pathtrace）と BDPT / VCM（vcm_light → vcm_camera）。
//! Progressive が出力先の画像と積分器を持ち、サンプルを積み増す

use ash::vk;
use krbr_scene::camera::Camera;
use krbr_scene::lens::LensParams;

use crate::gpu_scene::GpuScene;
use crate::gpu_types::{FrameUbo, PushConstants};
use crate::image_io::ImageF;
use crate::kernel::{buffer_binding, compute_barrier, make_frame_ubo, scene_bindings, Kernel};
use crate::vk::{as_bytes, image_barrier, Buffer, Context, Image};

/// 積分器が書き込む先（RGBA32F の累積値、A にサンプル数）
#[derive(Default)]
pub struct RenderTargets {
    pub color: Image,
    pub albedo: Image,
    pub normal: Image,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone)]
pub struct FrameParams {
    pub camera: Camera,
    pub env_intensity: f32,
    pub env_rotation_rad: f32,
    /// 0 で無効
    pub radiance_clamp: f32,
    pub max_depth: u32,
    pub rr_start_depth: u32,
    pub seed: u32,
    /// 分光モード（代表波長方式。累積は CIE XYZ）
    pub spectral: bool,
    /// VCM の近傍の半径の初期値（mm）。0 ならシーンの大きさから決める
    pub vcm_radius: f32,
    pub vcm_alpha: f32,
    pub lens: LensParams,
}

impl Default for FrameParams {
    fn default() -> Self {
        Self { camera: Camera::default(), env_intensity: 1.0, env_rotation_rad: 0.0, radiance_clamp: 0.0, max_depth: 8, rr_start_depth: 3, seed: 0, spectral: false, vcm_radius: 0.0, vcm_alpha: 0.75, lens: LensParams::default() }
    }
}

/// フレームの定数を置くバッファ（CPU から見えるメモリに置く）
struct FrameBuffer<'a> {
    ctx: &'a Context,
    ubo: Buffer,
}

impl<'a> FrameBuffer<'a> {
    fn new(ctx: &'a Context) -> Self {
        Self { ctx, ubo: ctx.create_buffer(std::mem::size_of::<FrameUbo>() as u64, vk::BufferUsageFlags::UNIFORM_BUFFER, true, false) }
    }
    fn write(&mut self, u: &FrameUbo) {
        let bytes = as_bytes(std::slice::from_ref(u));
        self.ubo.mapped().unwrap()[..bytes.len()].copy_from_slice(bytes);
    }
}

impl Drop for FrameBuffer<'_> {
    fn drop(&mut self) {
        self.ctx.destroy_buffer(&mut self.ubo);
    }
}

pub trait Integrator {
    fn name(&self) -> &'static str;
    fn update_frame(&mut self, fp: &FrameParams);
    /// sample_index 番目のサンプルを cmd に記録する（0 番目で累積を初期化）
    fn record_sample(&self, cmd: vk::CommandBuffer, sample_index: u32);
}

struct PathTracer<'a> {
    ctx: &'a Context,
    scene: &'a GpuScene<'a>,
    width: u32,
    height: u32,
    targets_ref: (u32, u32),
    seed: u32,
    frame: FrameBuffer<'a>,
    kernel: Kernel<'a>,
}

impl<'a> PathTracer<'a> {
    fn new(ctx: &'a Context, sc: &'a GpuScene<'a>, targets: &RenderTargets, fp: &FrameParams) -> Result<Self, String> {
        let mut frame = FrameBuffer::new(ctx);
        frame.write(&make_frame_ubo(sc, targets, fp));
        let kernel = Kernel::new(ctx, "pathtrace", &scene_bindings(sc, targets, &frame.ubo))?;
        Ok(Self { ctx, scene: sc, width: targets.width, height: targets.height, targets_ref: (targets.width, targets.height), seed: fp.seed, frame, kernel })
    }
}

impl Integrator for PathTracer<'_> {
    fn name(&self) -> &'static str {
        "pt"
    }
    fn update_frame(&mut self, fp: &FrameParams) {
        self.seed = fp.seed;
        let t = RenderTargets { width: self.targets_ref.0, height: self.targets_ref.1, ..Default::default() };
        self.frame.write(&make_frame_ubo(self.scene, &t, fp));
    }
    fn record_sample(&self, cmd: vk::CommandBuffer, sample_index: u32) {
        self.kernel.dispatch(cmd, PushConstants { sample_index, seed: self.seed }, self.width, self.height);
        compute_barrier(self.ctx, cmd); // 次のサンプルが累積を読む前に書き込みを完了させる
    }
}

/// BDPT（merging なし）と VCM（merging あり）
struct Bidirectional<'a> {
    ctx: &'a Context,
    scene: &'a GpuScene<'a>,
    merging: bool,
    width: u32,
    height: u32,
    seed: u32,
    capacity: u32,
    frame: FrameBuffer<'a>,
    light_verts: Buffer,
    path_heads: Buffer,
    cell_heads: Buffer,
    counters: Buffer,
    splat: Buffer,
    light: Kernel<'a>,
    camera: Kernel<'a>,
}

impl<'a> Bidirectional<'a> {
    fn new(ctx: &'a Context, sc: &'a GpuScene<'a>, targets: &RenderTargets, fp: &FrameParams, merging: bool) -> Result<Self, String> {
        if !ctx.info.atomic_float_add {
            return Err("bdpt / vcm need VK_EXT_shader_atomic_float (shaderBufferFloat32AtomicAdd)".into());
        }
        let mut frame = FrameBuffer::new(ctx);
        let mut u = make_frame_ubo(sc, targets, fp);
        u.vcm_params.w = if merging { 1.0 } else { 0.0 };
        frame.write(&u);
        // 光源側の頂点：1 本の経路あたり平均 4 個まで、全体で 512 MB まで
        let paths = targets.width as u64 * targets.height as u64;
        let capacity = (paths * 4).min((512u64 << 20) / 96) as u32;
        let usage = vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_DST;
        let light_verts = ctx.create_buffer(capacity as u64 * 96, usage, false, false);
        let path_heads = ctx.create_buffer(paths * 4, usage, false, false);
        let cell_heads = ctx.create_buffer((1u64 << 21) * 4, usage, false, false);
        let counters = ctx.create_buffer(16, usage, false, false);
        let splat = ctx.create_buffer(paths * 16, usage, false, false);
        let mut b = scene_bindings(sc, targets, &frame.ubo);
        b.push(buffer_binding(20, &light_verts));
        b.push(buffer_binding(21, &path_heads));
        b.push(buffer_binding(22, &cell_heads));
        b.push(buffer_binding(23, &counters));
        b.push(buffer_binding(24, &splat));
        let light = Kernel::new(ctx, "vcm_light", &b)?;
        let camera = Kernel::new(ctx, "vcm_camera", &b)?;
        krbr_core::info!("{}: up to {} light vertices ({} MB)", if merging { "vcm" } else { "bdpt" }, capacity, (capacity as u64 * 96) >> 20);
        Ok(Self { ctx, scene: sc, merging, width: targets.width, height: targets.height, seed: fp.seed, capacity, frame, light_verts, path_heads, cell_heads, counters, splat, light, camera })
    }
}

impl Integrator for Bidirectional<'_> {
    fn name(&self) -> &'static str {
        if self.merging {
            "vcm"
        } else {
            "bdpt"
        }
    }
    fn update_frame(&mut self, fp: &FrameParams) {
        self.seed = fp.seed;
        let t = RenderTargets { width: self.width, height: self.height, ..Default::default() };
        let mut u = make_frame_ubo(self.scene, &t, fp);
        u.vcm_params.w = if self.merging { 1.0 } else { 0.0 };
        self.frame.write(&u);
    }
    fn record_sample(&self, cmd: vk::CommandBuffer, sample_index: u32) {
        let dev = &self.ctx.device;
        unsafe {
            // 前の繰り返しの頂点・格子・画面への足し込みを空にする
            dev.cmd_fill_buffer(cmd, self.path_heads.buffer, 0, vk::WHOLE_SIZE, 0xFFFF_FFFF);
            if self.merging {
                dev.cmd_fill_buffer(cmd, self.cell_heads.buffer, 0, vk::WHOLE_SIZE, 0xFFFF_FFFF);
            }
            dev.cmd_fill_buffer(cmd, self.splat.buffer, 0, vk::WHOLE_SIZE, 0);
            let counters = [0u32, self.capacity, 0, 0];
            dev.cmd_update_buffer(cmd, self.counters.buffer, 0, as_bytes(&counters));
        }
        compute_barrier(self.ctx, cmd);
        self.light.dispatch(cmd, PushConstants { sample_index, seed: self.seed }, self.width, self.height);
        compute_barrier(self.ctx, cmd);
        self.camera.dispatch(cmd, PushConstants { sample_index, seed: self.seed }, self.width, self.height);
        compute_barrier(self.ctx, cmd);
    }
}

impl Drop for Bidirectional<'_> {
    fn drop(&mut self) {
        self.ctx.wait_idle();
        for b in [&mut self.light_verts, &mut self.path_heads, &mut self.cell_heads, &mut self.counters, &mut self.splat] {
            self.ctx.destroy_buffer(b);
        }
    }
}

fn create_integrator<'a>(ctx: &'a Context, sc: &'a GpuScene<'a>, targets: &RenderTargets, fp: &FrameParams, name: &str) -> Result<Box<dyn Integrator + 'a>, String> {
    match name {
        "pt" | "path" => Ok(Box::new(PathTracer::new(ctx, sc, targets, fp)?)),
        "bdpt" => Ok(Box::new(Bidirectional::new(ctx, sc, targets, fp, false)?)),
        "vcm" => Ok(Box::new(Bidirectional::new(ctx, sc, targets, fp, true)?)),
        other => Err(format!("unknown integrator: {other}")),
    }
}

/// 読み戻した画像（サンプル数で割った値。色とアルベドはリニア sRGB）
pub struct AovImages {
    pub color: ImageF,
    pub albedo: ImageF,
    pub normal: ImageF,
}

/// CIE XYZ（分光モードの累積）→ リニア sRGB（その場で書き換える）
pub fn xyz_to_linear_srgb(img: &mut ImageF) {
    let m = krbr_scene::spectrum::xyz_to_linear_srgb();
    for px in img.rgba.chunks_exact_mut(4) {
        let c = m * glam::Vec3::new(px[0], px[1], px[2]);
        px[0] = c.x;
        px[1] = c.y;
        px[2] = c.z;
    }
}

/// 累積レンダリングの管理（出力先の画像と積分器を持ち、サンプルを積み増す）
pub struct Progressive<'a> {
    ctx: &'a Context,
    scene: &'a GpuScene<'a>,
    integrator_name: String,
    integrator: Option<Box<dyn Integrator + 'a>>,
    targets: RenderTargets,
    frame: FrameParams,
    samples: u32,
}

impl<'a> Progressive<'a> {
    pub fn new(ctx: &'a Context, scene: &'a GpuScene<'a>, integrator: &str, width: u32, height: u32, frame: FrameParams) -> Result<Self, String> {
        let mut p = Self { ctx, scene, integrator_name: integrator.to_string(), integrator: None, targets: RenderTargets { width, height, ..Default::default() }, frame, samples: 0 };
        p.create_targets()?;
        Ok(p)
    }

    fn create_targets(&mut self) -> Result<(), String> {
        let usage = vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC;
        let (w, h) = (self.targets.width, self.targets.height);
        self.targets.color = self.ctx.create_image_2d(w, h, 1, vk::Format::R32G32B32A32_SFLOAT, usage);
        self.targets.albedo = self.ctx.create_image_2d(w, h, 1, vk::Format::R32G32B32A32_SFLOAT, usage);
        self.targets.normal = self.ctx.create_image_2d(w, h, 1, vk::Format::R32G32B32A32_SFLOAT, usage);
        let imgs = [self.targets.color.image, self.targets.albedo.image, self.targets.normal.image];
        let trace = self.ctx.trace_stages();
        self.ctx.submit_and_wait(|dev, cmd| {
            for img in imgs {
                image_barrier(dev, cmd, img, vk::ImageLayout::UNDEFINED, vk::ImageLayout::GENERAL, vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE, trace, vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE, 1);
            }
        });
        self.integrator = Some(create_integrator(self.ctx, self.scene, &self.targets, &self.frame, &self.integrator_name)?);
        self.samples = 0;
        Ok(())
    }

    fn destroy_targets(&mut self) {
        self.integrator = None;
        self.ctx.destroy_image(&mut self.targets.color);
        self.ctx.destroy_image(&mut self.targets.albedo);
        self.ctx.destroy_image(&mut self.targets.normal);
    }

    /// 解像度を変える（出力先と積分器を作り直し、累積は 0 から）
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), String> {
        if width == self.targets.width && height == self.targets.height {
            return Ok(());
        }
        self.ctx.wait_idle();
        self.destroy_targets();
        self.targets.width = width;
        self.targets.height = height;
        self.create_targets()
    }

    /// 積分器を作り直す（シーンの資源を差し替えた後）。累積は 0 から
    pub fn rebuild_integrator(&mut self) -> Result<(), String> {
        self.ctx.wait_idle();
        self.integrator = None;
        self.integrator = Some(create_integrator(self.ctx, self.scene, &self.targets, &self.frame, &self.integrator_name)?);
        self.samples = 0;
        Ok(())
    }

    pub fn set_integrator(&mut self, name: &str) -> Result<(), String> {
        if name == self.integrator_name {
            return Ok(());
        }
        self.integrator_name = name.to_string();
        self.rebuild_integrator()
    }

    /// カメラなどを変える（累積は 0 から）
    pub fn set_frame(&mut self, frame: FrameParams) {
        self.frame = frame;
        if let Some(i) = &mut self.integrator {
            i.update_frame(&self.frame);
        }
        self.samples = 0;
    }

    pub fn reset(&mut self) {
        self.samples = 0;
    }

    /// count サンプルを cmd に記録する
    pub fn record(&mut self, cmd: vk::CommandBuffer, count: u32) {
        if let Some(i) = &self.integrator {
            for _ in 0..count {
                i.record_sample(cmd, self.samples);
                self.samples += 1;
            }
        }
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }
    pub fn targets(&self) -> &RenderTargets {
        &self.targets
    }
    pub fn frame(&self) -> &FrameParams {
        &self.frame
    }
    /// 累積が CIE XYZ か（分光モード）
    pub fn xyz_output(&self) -> bool {
        self.frame.spectral
    }
    pub fn integrator_name(&self) -> &str {
        self.integrator.as_ref().map(|i| i.name()).unwrap_or("?")
    }

    /// 現在の累積を読み戻す（GPU が待機中に呼ぶこと）。分光モードでも RGB に変換して返す
    pub fn readback(&self) -> AovImages {
        let (w, h) = (self.targets.width, self.targets.height);
        let bytes = w as u64 * h as u64 * 16;
        let mut rb = self.ctx.create_buffer(bytes * 3, vk::BufferUsageFlags::TRANSFER_DST, true, true);
        let imgs = [self.targets.color.image, self.targets.albedo.image, self.targets.normal.image];
        let rbb = rb.buffer;
        self.ctx.submit_and_wait(|dev, cmd| {
            for (k, img) in imgs.iter().enumerate() {
                let region = vk::BufferImageCopy::default()
                    .buffer_offset(bytes * k as u64)
                    .image_subresource(vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 })
                    .image_extent(vk::Extent3D { width: w, height: h, depth: 1 });
                unsafe { dev.cmd_copy_image_to_buffer(cmd, *img, vk::ImageLayout::GENERAL, rbb, &[region]) };
            }
        });
        let src = rb.mapped_ref().unwrap();
        let n = (w * h) as usize;
        let mut out = [ImageF::new(w, h), ImageF::new(w, h), ImageF::new(w, h)];
        for (k, dst) in out.iter_mut().enumerate() {
            let base = n * 16 * k;
            for i in 0..n {
                let px = |c: usize| f32::from_le_bytes([src[base + i * 16 + c * 4], src[base + i * 16 + c * 4 + 1], src[base + i * 16 + c * 4 + 2], src[base + i * 16 + c * 4 + 3]]);
                let cnt = px(3); // A にサンプル数が入っている
                let inv = if cnt > 0.0 { 1.0 / cnt } else { 0.0 };
                for c in 0..3 {
                    dst.rgba[i * 4 + c] = px(c) * inv;
                }
                dst.rgba[i * 4 + 3] = 1.0;
            }
        }
        self.ctx.destroy_buffer(&mut rb);
        let [mut color, mut albedo, normal] = out;
        if self.xyz_output() {
            xyz_to_linear_srgb(&mut color);
            xyz_to_linear_srgb(&mut albedo);
        }
        AovImages { color, albedo, normal }
    }
}

impl Drop for Progressive<'_> {
    fn drop(&mut self) {
        self.ctx.wait_idle();
        self.destroy_targets();
    }
}
