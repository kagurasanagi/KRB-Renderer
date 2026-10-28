//! 積分器：パストレーサー（pathtrace）と BDPT / VCM（vcm_light → vcm_camera）。
//! Progressive が出力先の画像と積分器を持ち、サンプルを積み増す

use ash::vk;
use krbr_scene::camera::Camera;
use krbr_scene::lens::LensParams;
use krbr_scene::IntegratorKind;

use crate::gpu_scene::GpuScene;
use crate::gpu_types::{FrameUbo, GpuLightVertex, PushConstants};
use crate::image_io::ImageF;
use crate::kernel::{buffer_binding, compute_barrier, make_frame_ubo, scene_bindings, Binding, Kernel};
use crate::shader_consts::*;
use crate::vk::{as_bytes, image_barrier, memory_barrier, Buffer, Context, Image};

/// 積分器が書き込む先（RGBA32F の累積値、A にサンプル数）
#[derive(Default)]
pub struct RenderTargets {
    pub color: Image,
    pub albedo: Image,
    pub normal: Image,
    /// 最初に当たった点の和（xyz）と当たったサンプルの数（w）
    pub position: Image,
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
    /// 最大の深さ。経路の線分の数（カメラ・光源を含めた頂点の数 − 1）は max_depth + 1 まで。全積分器で同じ意味
    pub max_depth: u32,
    /// ロシアンルーレットを始める深さ。パストレーサーだけに効く。BDPT / VCM の続ける確率は MIS の確率密度に含めるので、
    /// 頂点の材質だけで決まる（光源側から数えてもカメラ側から数えても同じ）値にする必要があり、深さによらず最初の頂点から効く
    pub rr_start_depth: u32,
    pub seed: u32,
    /// 分光モード（代表波長方式。累積は CIE XYZ）
    pub spectral: bool,
    /// VCM の近傍の半径の初期値（mm）。0 ならシーンの大きさから決める
    pub vcm_radius: f32,
    pub vcm_alpha: f32,
    pub lens: LensParams,
    /// 背景を透明にする：カメラの光線が何にも当たらない画素は、環境光を足さず透明にする（環境光は映り込みと照明には使う）。
    /// 色はアルファを掛けた値（premultiplied）になり、アルファは当たったサンプルの割合（Progressive::readback）
    pub transparent_background: bool,
}

impl Default for FrameParams {
    fn default() -> Self {
        Self { camera: Camera::default(), env_intensity: 1.0, env_rotation_rad: 0.0, radiance_clamp: 0.0, max_depth: 8, rr_start_depth: 3, seed: 0, spectral: false, vcm_radius: 0.0, vcm_alpha: 0.75, lens: LensParams::default(), transparent_background: false }
    }
}

/// フレームの定数を置くバッファ（CPU から見えるメモリに置く）
struct FrameBuffer<'a> {
    ctx: &'a Context,
    ubo: Buffer,
}

impl<'a> FrameBuffer<'a> {
    fn new(ctx: &'a Context) -> Result<Self, String> {
        Ok(Self { ctx, ubo: ctx.create_buffer(std::mem::size_of::<FrameUbo>() as u64, vk::BufferUsageFlags::UNIFORM_BUFFER, true, false)? })
    }
    fn write(&mut self, u: &FrameUbo) {
        let bytes = as_bytes(std::slice::from_ref(u));
        self.ubo.mapped_bytes_mut().expect("the frame constants are in host-visible memory").copy_from_slice(bytes);
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
    /// 前に記録したサンプルが GPU で終わった後の確かめ（容量の不足など）。Progressive::record が次を記録する前に呼ぶ
    fn review_completed(&mut self) -> Result<(), String> {
        Ok(())
    }
}

struct PathTracer<'a> {
    ctx: &'a Context,
    scene: &'a GpuScene<'a>,
    width: u32,
    height: u32,
    seed: u32,
    frame: FrameBuffer<'a>,
    kernel: Kernel<'a>,
}

impl<'a> PathTracer<'a> {
    fn new(ctx: &'a Context, sc: &'a GpuScene<'a>, targets: &RenderTargets, fp: &FrameParams) -> Result<Self, String> {
        let mut frame = FrameBuffer::new(ctx)?;
        frame.write(&make_frame_ubo(sc, targets.width, targets.height, fp, false));
        let kernel = Kernel::new(ctx, "pathtrace", &scene_bindings(sc, targets, &frame.ubo))?;
        Ok(Self { ctx, scene: sc, width: targets.width, height: targets.height, seed: fp.seed, frame, kernel })
    }
}

impl Integrator for PathTracer<'_> {
    fn name(&self) -> &'static str {
        "pt"
    }
    fn update_frame(&mut self, fp: &FrameParams) {
        self.seed = fp.seed;
        self.frame.write(&make_frame_ubo(self.scene, self.width, self.height, fp, false));
    }
    fn record_sample(&self, cmd: vk::CommandBuffer, sample_index: u32) {
        self.kernel.dispatch(cmd, PushConstants { sample_index, seed: self.seed }, self.width, self.height);
        compute_barrier(self.ctx, cmd); // 次のサンプルが累積を読む前に書き込みを完了させる
    }
}

/// 光源側の頂点：1 本の経路あたり平均この数まで保存できるように確保する（足りなければ review_completed で増やす）
const LIGHT_VERTICES_PER_PATH: u64 = 4;
/// 光源側の頂点のバッファの上限（VRAM の使いすぎを防ぐ）
const LIGHT_VERTEX_BUFFER_MAX_BYTES: u64 = 512 << 20;

/// BDPT / VCM のバッファ（作りかけで失敗しても Drop が作った分を解放する）
struct VcmBuffers<'a> {
    ctx: &'a Context,
    light_verts: Buffer,
    path_heads: Buffer,
    cell_heads: Buffer,
    counters: Buffer,
    /// counters を毎回写して CPU から読む（容量の不足などを知らせるため）
    counters_readback: Buffer,
    splat: Buffer,
}

impl<'a> VcmBuffers<'a> {
    fn new(ctx: &'a Context, paths: u64, capacity: u32) -> Result<Self, String> {
        let usage = vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_DST;
        let mut b = Self { ctx, light_verts: Buffer::default(), path_heads: Buffer::default(), cell_heads: Buffer::default(), counters: Buffer::default(), counters_readback: Buffer::default(), splat: Buffer::default() };
        b.light_verts = ctx.create_buffer(light_vertex_bytes(capacity), usage, false, false)?;
        b.path_heads = ctx.create_buffer(paths * 4, usage, false, false)?;
        b.cell_heads = ctx.create_buffer(VCM_GRID_SIZE as u64 * 4, usage, false, false)?;
        b.counters = ctx.create_buffer(VCM_COUNTER_COUNT as u64 * 4, usage | vk::BufferUsageFlags::TRANSFER_SRC, false, false)?;
        b.counters_readback = ctx.create_buffer(VCM_COUNTER_COUNT as u64 * 4, vk::BufferUsageFlags::TRANSFER_DST, true, true)?;
        b.splat = ctx.create_buffer(paths * VCM_SPLAT_FLOATS_PER_PIXEL as u64 * 4, usage, false, false)?;
        Ok(b)
    }
}

impl Drop for VcmBuffers<'_> {
    fn drop(&mut self) {
        for b in [&mut self.light_verts, &mut self.path_heads, &mut self.cell_heads, &mut self.counters, &mut self.counters_readback, &mut self.splat] {
            self.ctx.destroy_buffer(b);
        }
    }
}

fn light_vertex_bytes(capacity: u32) -> u64 {
    capacity as u64 * std::mem::size_of::<GpuLightVertex>() as u64
}

fn max_light_vertices() -> u64 {
    LIGHT_VERTEX_BUFFER_MAX_BYTES / std::mem::size_of::<GpuLightVertex>() as u64
}

/// BDPT（merging なし）と VCM（merging あり）
struct Bidirectional<'a> {
    ctx: &'a Context,
    scene: &'a GpuScene<'a>,
    merging: bool,
    width: u32,
    height: u32,
    seed: u32,
    /// 光源側の頂点の容量（個）
    capacity: u32,
    frame: FrameBuffer<'a>,
    bindings: Vec<Binding>,
    // カーネルはバッファを記述子で参照するので、バッファより先に破棄する（フィールドは宣言の順に破棄される）
    light: Kernel<'a>,
    camera: Kernel<'a>,
    buffers: VcmBuffers<'a>,
    /// 容量を増やせるか（増やすのに失敗したら false）
    growable: bool,
    /// 容量を使い切った・セルの走査を打ち切った、を一度だけ知らせる
    warned_full: bool,
    warned_cell_walk: bool,
}

impl<'a> Bidirectional<'a> {
    fn new(ctx: &'a Context, sc: &'a GpuScene<'a>, targets: &RenderTargets, fp: &FrameParams, merging: bool) -> Result<Self, String> {
        if !ctx.info.atomic_float_add {
            return Err("bdpt / vcm need VK_EXT_shader_atomic_float (shaderBufferFloat32AtomicAdd)".into());
        }
        let mut frame = FrameBuffer::new(ctx)?;
        frame.write(&make_frame_ubo(sc, targets.width, targets.height, fp, merging));
        let paths = targets.width as u64 * targets.height as u64;
        let capacity = (paths * LIGHT_VERTICES_PER_PATH).min(max_light_vertices()) as u32;
        let buffers = VcmBuffers::new(ctx, paths, capacity)?;
        let mut bindings = scene_bindings(sc, targets, &frame.ubo);
        bindings.push(buffer_binding(BINDING_LIGHT_VERTICES, &buffers.light_verts));
        bindings.push(buffer_binding(BINDING_PATH_HEADS, &buffers.path_heads));
        bindings.push(buffer_binding(BINDING_CELL_HEADS, &buffers.cell_heads));
        bindings.push(buffer_binding(BINDING_COUNTERS, &buffers.counters));
        bindings.push(buffer_binding(BINDING_SPLAT, &buffers.splat));
        let light = Kernel::new(ctx, "vcm_light", &bindings)?;
        let camera = Kernel::new(ctx, "vcm_camera", &bindings)?;
        krbr_core::info!("{}: up to {} light vertices ({} MB)", if merging { "vcm" } else { "bdpt" }, capacity, light_vertex_bytes(capacity) >> 20);
        Ok(Self {
            ctx,
            scene: sc,
            merging,
            width: targets.width,
            height: targets.height,
            seed: fp.seed,
            capacity,
            frame,
            bindings,
            light,
            camera,
            buffers,
            growable: true,
            warned_full: false,
            warned_cell_walk: false,
        })
    }

    /// 光源側の頂点のバッファを capacity 個に作り直す（記述子が変わるのでカーネルも作り直す）。
    /// 失敗したら Err で、今のバッファとカーネルをそのまま使い続ける
    fn grow(&mut self, capacity: u32) -> Result<(), String> {
        self.ctx.wait_idle();
        let usage = vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_DST;
        let mut new_buf = self.ctx.create_buffer(light_vertex_bytes(capacity), usage, false, false)?;
        let mut bindings = self.bindings.clone();
        for b in &mut bindings {
            if let Binding::Buffer { binding: BINDING_LIGHT_VERTICES, buffer, .. } = b {
                *buffer = new_buf.buffer;
            }
        }
        let kernels = Kernel::new(self.ctx, "vcm_light", &bindings).and_then(|l| Ok((l, Kernel::new(self.ctx, "vcm_camera", &bindings)?)));
        let (light, camera) = match kernels {
            Ok(k) => k,
            Err(e) => {
                self.ctx.destroy_buffer(&mut new_buf);
                return Err(e);
            }
        };
        // 古いカーネルを破棄してから、それが参照していた古いバッファを解放する
        self.light = light;
        self.camera = camera;
        let mut old = std::mem::replace(&mut self.buffers.light_verts, new_buf);
        self.ctx.destroy_buffer(&mut old);
        self.bindings = bindings;
        self.capacity = capacity;
        Ok(())
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
        self.frame.write(&make_frame_ubo(self.scene, self.width, self.height, fp, self.merging));
    }
    fn record_sample(&self, cmd: vk::CommandBuffer, sample_index: u32) {
        let dev = &self.ctx.device;
        let b = &self.buffers;
        unsafe {
            // 前の繰り返しの頂点・格子・画面への足し込みを空にする
            dev.cmd_fill_buffer(cmd, b.path_heads.buffer, 0, vk::WHOLE_SIZE, 0xFFFF_FFFF);
            if self.merging {
                dev.cmd_fill_buffer(cmd, b.cell_heads.buffer, 0, vk::WHOLE_SIZE, 0xFFFF_FFFF);
            }
            dev.cmd_fill_buffer(cmd, b.splat.buffer, 0, vk::WHOLE_SIZE, 0);
            let mut counters = [0u32; VCM_COUNTER_COUNT as usize];
            counters[VCM_COUNTER_CAPACITY as usize] = self.capacity;
            dev.cmd_update_buffer(cmd, b.counters.buffer, 0, as_bytes(&counters));
        }
        compute_barrier(self.ctx, cmd);
        self.light.dispatch(cmd, PushConstants { sample_index, seed: self.seed }, self.width, self.height);
        compute_barrier(self.ctx, cmd);
        self.camera.dispatch(cmd, PushConstants { sample_index, seed: self.seed }, self.width, self.height);
        compute_barrier(self.ctx, cmd);
        // この繰り返しの数を CPU から読めるところへ写す（review_completed で見る）
        let n = VCM_COUNTER_COUNT as u64 * 4;
        unsafe { dev.cmd_copy_buffer(cmd, b.counters.buffer, b.counters_readback.buffer, &[vk::BufferCopy { src_offset: 0, dst_offset: 0, size: n }]) };
        // 写し終えてから、CPU が読む・次の繰り返しの初めに数を書き直す（CLEAR の段階の vkCmdUpdateBuffer）
        memory_barrier(dev, cmd, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE, vk::PipelineStageFlags2::HOST | vk::PipelineStageFlags2::CLEAR, vk::AccessFlags2::HOST_READ | vk::AccessFlags2::TRANSFER_WRITE);
    }
    fn review_completed(&mut self) -> Result<(), String> {
        let Some(bytes) = self.buffers.counters_readback.mapped_bytes() else { return Ok(()) };
        let counter = |i: u32| {
            let o = i as usize * 4;
            u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]])
        };
        // 数は容量を超えても数え続ける（保存できなかった頂点も数える）
        let wanted = counter(VCM_COUNTER_STORED) as u64;
        let truncated = counter(VCM_COUNTER_MERGE_TRUNCATED);
        if truncated > 0 && !std::mem::replace(&mut self.warned_cell_walk, true) {
            krbr_core::warn!("vcm: {} merging lookups stopped after {} vertices in one cell; some light is missing there", truncated, VCM_MAX_CELL_WALK);
        }
        if wanted <= self.capacity as u64 {
            return Ok(());
        }
        // 保存できなかった頂点への接続・まとめは行われず、その分の光が欠ける（MIS の重みはその戦略がある前提なので暗くなる）。
        // 上限までは容量を増やして、次の繰り返しから欠けないようにする
        let max = max_light_vertices();
        if self.growable && (self.capacity as u64) < max {
            let capacity = (wanted + wanted / 4).min(max) as u32;
            krbr_core::info!("{}: {} light vertices did not fit in {}; growing to {}", self.name(), wanted, self.capacity, capacity);
            let r = self.grow(capacity);
            // 作れなかったら（VRAM の不足など）、今の容量のまま続ける。毎回試さない
            self.growable = r.is_ok();
            return r;
        }
        if !std::mem::replace(&mut self.warned_full, true) {
            krbr_core::warn!("{}: {} light vertices did not fit in {} ({} MB, the limit); the rest are dropped and some light is missing", self.name(), wanted, self.capacity, light_vertex_bytes(self.capacity) >> 20);
        }
        Ok(())
    }
}

impl Drop for Bidirectional<'_> {
    fn drop(&mut self) {
        self.ctx.wait_idle();
    }
}

fn create_integrator<'a>(ctx: &'a Context, sc: &'a GpuScene<'a>, targets: &RenderTargets, fp: &FrameParams, kind: IntegratorKind) -> Result<Box<dyn Integrator + 'a>, String> {
    match kind {
        IntegratorKind::Pt => Ok(Box::new(PathTracer::new(ctx, sc, targets, fp)?)),
        IntegratorKind::Bdpt => Ok(Box::new(Bidirectional::new(ctx, sc, targets, fp, false)?)),
        IntegratorKind::Vcm => Ok(Box::new(Bidirectional::new(ctx, sc, targets, fp, true)?)),
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
    kind: IntegratorKind,
    integrator: Option<Box<dyn Integrator + 'a>>,
    targets: RenderTargets,
    frame: FrameParams,
    samples: u32,
}

impl<'a> Progressive<'a> {
    pub fn new(ctx: &'a Context, scene: &'a GpuScene<'a>, kind: IntegratorKind, width: u32, height: u32, frame: FrameParams) -> Result<Self, String> {
        let mut p = Self { ctx, scene, kind, integrator: None, targets: RenderTargets { width, height, ..Default::default() }, frame, samples: 0 };
        p.create_targets()?;
        Ok(p)
    }

    /// 出力先の画像と積分器を作る。失敗したら Err（作った画像は destroy_targets・Drop が解放する）
    fn create_targets(&mut self) -> Result<(), String> {
        let usage = vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC;
        let (w, h) = (self.targets.width, self.targets.height);
        let f = vk::Format::R32G32B32A32_SFLOAT;
        self.targets.color = self.ctx.create_image_2d(w, h, 1, f, usage)?;
        self.targets.albedo = self.ctx.create_image_2d(w, h, 1, f, usage)?;
        self.targets.normal = self.ctx.create_image_2d(w, h, 1, f, usage)?;
        self.targets.position = self.ctx.create_image_2d(w, h, 1, f, usage)?;
        let imgs = [self.targets.color.image, self.targets.albedo.image, self.targets.normal.image, self.targets.position.image];
        let trace = self.ctx.trace_stages();
        self.ctx.submit_and_wait(|dev, cmd| {
            for img in imgs {
                image_barrier(dev, cmd, img, vk::ImageLayout::UNDEFINED, vk::ImageLayout::GENERAL, vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE, trace, vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::SHADER_STORAGE_WRITE, 1);
            }
        })?;
        self.integrator = Some(create_integrator(self.ctx, self.scene, &self.targets, &self.frame, self.kind)?);
        self.samples = 0;
        Ok(())
    }

    fn destroy_targets(&mut self) {
        self.integrator = None;
        self.ctx.destroy_image(&mut self.targets.color);
        self.ctx.destroy_image(&mut self.targets.albedo);
        self.ctx.destroy_image(&mut self.targets.normal);
        self.ctx.destroy_image(&mut self.targets.position);
    }

    /// 解像度を変える（出力先と積分器を作り直し、累積は 0 から）。失敗したら Err（積分器は無いままになり、record は何もしない）
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
        self.integrator = Some(create_integrator(self.ctx, self.scene, &self.targets, &self.frame, self.kind)?);
        self.samples = 0;
        Ok(())
    }

    /// 積分器を替える（累積は 0 から）。作れなければ Err（積分器は無いままになり、record は何もしない。別の種類で呼び直せる）
    pub fn set_integrator(&mut self, kind: IntegratorKind) -> Result<(), String> {
        if kind == self.kind && self.integrator.is_some() {
            return Ok(());
        }
        self.kind = kind;
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

    /// count サンプルを cmd に記録する。前に記録したコマンドの完了を待ってから呼ぶこと（その結果を確かめてから記録する）
    pub fn record(&mut self, cmd: vk::CommandBuffer, count: u32) {
        if let Some(i) = &mut self.integrator {
            if self.samples > 0 {
                if let Err(e) = i.review_completed() {
                    krbr_core::error!("{}: {e}", i.name());
                }
            }
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
    /// 使っている積分器の種類（作れていなければ、最後に指定した種類）
    pub fn integrator_kind(&self) -> IntegratorKind {
        self.kind
    }
    pub fn integrator_name(&self) -> &str {
        self.kind.as_str()
    }

    /// 最初に当たった点の平均（ワールド座標）と、当たったサンプルの割合（w。0 なら環境光だけ）を読み戻す
    /// （GPU が待機中に呼ぶこと。動画のフローを求める）
    pub fn readback_position(&self) -> Result<Vec<[f32; 4]>, String> {
        let (w, h) = (self.targets.width, self.targets.height);
        let bytes = w as u64 * h as u64 * 16;
        let mut rb = self.ctx.create_buffer(bytes, vk::BufferUsageFlags::TRANSFER_DST, true, true)?;
        let (img, rbb) = (self.targets.position.image, rb.buffer);
        let r = self.ctx.submit_and_wait(|dev, cmd| {
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 })
                .image_extent(vk::Extent3D { width: w, height: h, depth: 1 });
            unsafe { dev.cmd_copy_image_to_buffer(cmd, img, vk::ImageLayout::GENERAL, rbb, &[region]) };
            memory_barrier(dev, cmd, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE, vk::PipelineStageFlags2::HOST, vk::AccessFlags2::HOST_READ);
        });
        if let Err(e) = r {
            self.ctx.destroy_buffer(&mut rb);
            return Err(e);
        }
        let src = rb.mapped_bytes().expect("the readback buffer is host-visible");
        let n = self.samples.max(1) as f32;
        let out = src
            .chunks_exact(16)
            .map(|c| {
                let f = |k: usize| f32::from_le_bytes([c[k * 4], c[k * 4 + 1], c[k * 4 + 2], c[k * 4 + 3]]);
                let hits = f(3);
                let inv = if hits > 0.0 { 1.0 / hits } else { 0.0 };
                [f(0) * inv, f(1) * inv, f(2) * inv, hits / n]
            })
            .collect();
        self.ctx.destroy_buffer(&mut rb);
        Ok(out)
    }

    /// 現在の累積を読み戻す（GPU が待機中に呼ぶこと）。分光モードでも RGB に変換して返す。
    /// 背景を透明にしているときは、色の A にアルファ（カメラの光線が何かに当たったサンプルの割合）を入れる（色はアルファを
    /// 掛けた値のまま）。それ以外は 1。読み戻し用のバッファが作れないなどで失敗したら Err
    pub fn readback(&self) -> Result<AovImages, String> {
        let (w, h) = (self.targets.width, self.targets.height);
        let bytes = w as u64 * h as u64 * 16;
        let transparent = self.frame.transparent_background;
        let mut imgs = vec![self.targets.color.image, self.targets.albedo.image, self.targets.normal.image];
        if transparent {
            imgs.push(self.targets.position.image);
        }
        let mut rb = self.ctx.create_buffer(bytes * imgs.len() as u64, vk::BufferUsageFlags::TRANSFER_DST, true, true)?;
        let rbb = rb.buffer;
        let r = self.ctx.submit_and_wait(|dev, cmd| {
            for (k, img) in imgs.iter().enumerate() {
                let region = vk::BufferImageCopy::default()
                    .buffer_offset(bytes * k as u64)
                    .image_subresource(vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 })
                    .image_extent(vk::Extent3D { width: w, height: h, depth: 1 });
                unsafe { dev.cmd_copy_image_to_buffer(cmd, *img, vk::ImageLayout::GENERAL, rbb, &[region]) };
            }
            memory_barrier(dev, cmd, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE, vk::PipelineStageFlags2::HOST, vk::AccessFlags2::HOST_READ);
        });
        if let Err(e) = r {
            self.ctx.destroy_buffer(&mut rb);
            return Err(e);
        }
        let src = rb.mapped_bytes().expect("the readback buffer is host-visible");
        let n = (w * h) as usize;
        let at = |k: usize, i: usize, c: usize| {
            let o = n * 16 * k + i * 16 + c * 4;
            f32::from_le_bytes([src[o], src[o + 1], src[o + 2], src[o + 3]])
        };
        let mut out = [ImageF::new(w, h), ImageF::new(w, h), ImageF::new(w, h)];
        for (k, dst) in out.iter_mut().enumerate() {
            for i in 0..n {
                let cnt = at(k, i, 3); // A にサンプル数が入っている
                let inv = if cnt > 0.0 { 1.0 / cnt } else { 0.0 };
                for c in 0..3 {
                    dst.rgba[i * 4 + c] = at(k, i, c) * inv;
                }
                dst.rgba[i * 4 + 3] = 1.0;
            }
        }
        if transparent {
            // アルファ = 当たったサンプルの数 / 数えたサンプルの数（アルベドの A。レンズの中で遮られたサンプルは数えない）。
            // 表示の shaders/tonemap.comp の coverage と同じ
            for i in 0..n {
                let (hits, cnt) = (at(3, i, 3), at(1, i, 3));
                out[0].rgba[i * 4 + 3] = if cnt > 0.0 { (hits / cnt).clamp(0.0, 1.0) } else { 0.0 };
            }
        }
        self.ctx.destroy_buffer(&mut rb);
        let [mut color, mut albedo, normal] = out;
        if self.xyz_output() {
            xyz_to_linear_srgb(&mut color);
            xyz_to_linear_srgb(&mut albedo);
        }
        Ok(AovImages { color, albedo, normal })
    }
}

impl Drop for Progressive<'_> {
    fn drop(&mut self) {
        self.ctx.wait_idle();
        self.destroy_targets();
    }
}
