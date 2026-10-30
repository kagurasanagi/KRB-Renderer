//! はんだの平衡形状のくり返しを GPU で行う（krbr-scene の solder_sim::Relaxer。式は solder_sim::relax_cpu と同じ）。
//! シェーダは shaders/solder_*.comp（krbr-render に埋め込み）で、バッファはデバイスアドレスでプッシュ定数から渡す。
//! 1 回のくり返し：はんだの割合の畳み込み（3 つの軸）→ φ と候補の φ のヒストグラム → CPU でしきい値 λ（ヒストグラムの上の二分法）
//! → λ を当てて新しい割合と変化の合計。次のくり返しの φ までを 1 回の投入にまとめ、GPU を待つのは 1 回だけ。
//! 描画とは別の Vulkan のデバイスを、初めて解くときに作る（作れなければ CPU に戻す）。パッドは 1 つずつ解く

use std::sync::{Arc, Mutex};

use ash::vk;
use krbr_render::vk::{as_bytes, memory_barrier, Buffer, Context, ContextOptions, GpuPod};
use krbr_scene::solder_sim::{set_relaxer, stage_done, Relax, Relaxer};

/// しきい値を求めるヒストグラムの区間の数と、φ の範囲（φ = 2 G * χ + 固体の項 − 重力 は −1.1〜4 くらい。外は端の区間に数える）
const HIST_BINS: usize = 16384;
const HIST_LO: f32 = -2.0;
const HIST_HI: f32 = 5.0;
/// プッシュ定数の大きさ（どのシェーダもこれ以下）
const PUSH_SIZE: u32 = 128;
const GROUP: u32 = 256;

#[repr(C)]
#[derive(Clone, Copy)]
struct BlurPush {
    src: [u32; 2],
    dst: [u32; 2],
    nx: u32,
    ny: u32,
    nz: u32,
    axis: u32,
    sigma: f32,
    replicate: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct UtilPush {
    src: [u32; 2],
    dst: [u32; 2],
    n: u32,
    mode: u32,
    p: u32,
    a: f32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PhiPush {
    fill: [u32; 2],
    conv: [u32; 2],
    ws: [u32; 2],
    phase: [u32; 2],
    phi: [u32; 2],
    hist: [u32; 2],
    nx: u32,
    ny: u32,
    nz: u32,
    nbins: u32,
    grav: f32,
    z0: f32,
    cell: f32,
    hist_lo: f32,
    hist_scale: f32,
    pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct ApplyPush {
    fill: [u32; 2],
    before: [u32; 2],
    next: [u32; 2],
    phi: [u32; 2],
    phase: [u32; 2],
    sums: [u32; 2],
    n: u32,
    lam: f32,
    width: f32,
    pad: u32,
}

const _: () = assert!(std::mem::size_of::<BlurPush>() == 40 && std::mem::size_of::<UtilPush>() == 32);
const _: () = assert!(std::mem::size_of::<PhiPush>() == 88 && std::mem::size_of::<ApplyPush>() == 64);
// 安全性：u32 と f32 だけの repr(C) で、隙間が無い（上の大きさの確認）
unsafe impl GpuPod for BlurPush {}
unsafe impl GpuPod for UtilPush {}
unsafe impl GpuPod for PhiPush {}
unsafe impl GpuPod for ApplyPush {}

fn addr(b: &Buffer) -> [u32; 2] {
    [b.address as u32, (b.address >> 32) as u32]
}

struct Gpu {
    ctx: Context,
    layout: vk::PipelineLayout,
    blur: vk::Pipeline,
    util: vk::Pipeline,
    phi: vk::Pipeline,
    apply: vk::Pipeline,
}

impl Gpu {
    fn new() -> Result<Gpu, String> {
        let ctx = Context::new(&ContextOptions { low_queue_priority: true, ..Default::default() })?;
        let dev = &ctx.device;
        let pcr = [vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::COMPUTE, offset: 0, size: PUSH_SIZE }];
        let layout = unsafe { dev.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().push_constant_ranges(&pcr), None) }.map_err(|e| format!("{e:?}"))?;
        let pipeline = |name: &str| -> Result<vk::Pipeline, String> {
            let code = krbr_render::shader_spirv(name)?;
            let words: Vec<u32> = code.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
            let sm = unsafe { dev.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&words), None) }.map_err(|e| format!("{name}: {e:?}"))?;
            let entry = c"main";
            let stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE).module(sm).name(entry);
            let r = unsafe { dev.create_compute_pipelines(vk::PipelineCache::null(), &[vk::ComputePipelineCreateInfo::default().stage(stage).layout(layout)], None) };
            unsafe { dev.destroy_shader_module(sm, None) };
            Ok(r.map_err(|(_, e)| format!("{name}: {e:?}"))?[0])
        };
        let gpu = Gpu { blur: pipeline("solder_blur.comp.spv")?, util: pipeline("solder_util.comp.spv")?, phi: pipeline("solder_phi.comp.spv")?, apply: pipeline("solder_apply.comp.spv")?, layout, ctx };
        krbr_core::info!("solder: solving on the GPU ({})", gpu.ctx.info.name);
        Ok(gpu)
    }

    fn dispatch<P: GpuPod>(&self, cmd: vk::CommandBuffer, pipeline: vk::Pipeline, push: &P, n: usize) {
        let dev = &self.ctx.device;
        unsafe {
            dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, pipeline);
            dev.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::COMPUTE, 0, as_bytes(std::slice::from_ref(push)));
            dev.cmd_dispatch(cmd, (n as u32).div_ceil(GROUP), 1, 1);
        }
    }

    /// 前のコマンドの書き込みを、次のコマンドが読めるようにする
    fn barrier(&self, cmd: vk::CommandBuffer) {
        memory_barrier(&self.ctx.device, cmd, vk::PipelineStageFlags2::ALL_COMMANDS, vk::AccessFlags2::MEMORY_WRITE, vk::PipelineStageFlags2::ALL_COMMANDS, vk::AccessFlags2::MEMORY_READ | vk::AccessFlags2::MEMORY_WRITE);
    }

    /// src を σ で畳み込んで dst へ（3 つの軸。t1, t2 は作業用）
    #[allow(clippy::too_many_arguments)]
    fn blur3(&self, cmd: vk::CommandBuffer, g: &krbr_scene::solder_sim::Grid, src: &Buffer, t1: &Buffer, t2: &Buffer, dst: &Buffer, sigma: f64, replicate: bool) {
        for (axis, a, b) in [(0u32, src, t1), (1, t1, t2), (2, t2, dst)] {
            let push = BlurPush { src: addr(a), dst: addr(b), nx: g.nx as u32, ny: g.ny as u32, nz: g.nz as u32, axis, sigma: sigma as f32, replicate: replicate as u32 };
            self.dispatch(cmd, self.blur, &push, g.len());
            self.barrier(cmd);
        }
    }

    fn copy(&self, cmd: vk::CommandBuffer, src: &Buffer, dst: &Buffer, bytes: u64) {
        unsafe { self.ctx.device.cmd_copy_buffer(cmd, src.buffer, dst.buffer, &[vk::BufferCopy { src_offset: 0, dst_offset: 0, size: bytes }]) };
    }

    fn relax(&self, r: &Relax, fill: &mut [f32]) -> Result<Vec<(f64, usize, usize)>, String> {
        let ctx = &self.ctx;
        let g = r.grid;
        let n = g.len();
        let fbytes = (n * 4) as u64;
        let usage = vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST;
        let mut bufs: Vec<Buffer> = Vec::new();
        let result = (|| -> Result<Vec<(f64, usize, usize)>, String> {
            let mut make = |bytes: u64, readback: bool| -> Result<usize, String> {
                bufs.push(ctx.create_buffer(bytes, usage, false, readback)?);
                Ok(bufs.len() - 1)
            };
            let (phase, fillb, before, next, conv, t0, t1, t2, ws, phi) =
                (make(fbytes, false)?, make(fbytes, false)?, make(fbytes, false)?, make(fbytes, false)?, make(fbytes, false)?, make(fbytes, false)?, make(fbytes, false)?, make(fbytes, false)?, make(fbytes, false)?, make(fbytes, false)?);
            let (hist, sums) = (make((HIST_BINS * 4) as u64, false)?, make(16, false)?);
            let (hist_host, sums_host, fill_host) = (make((HIST_BINS * 4) as u64, true)?, make(16, true)?, make(fbytes, true)?);
            let codes: Vec<u32> = r.phase.iter().zip(r.sticky).map(|(&p, &s)| p as u32 | if s { 0x100 } else { 0 }).collect();
            ctx.upload(&mut bufs[phase], as_bytes(&codes), 0)?;
            ctx.upload(&mut bufs[fillb], as_bytes(fill), 0)?;
            let b = &bufs;
            let forced = r.sticky.iter().filter(|&&s| s).count() as f64;
            let need = (r.target as f64 - forced).max(0.0);
            let scale = HIST_BINS as f32 / (HIST_HI - HIST_LO);
            let mut log = Vec::new();
            for st in &r.stages {
                // 固体の項 ws = G_σ * χ_固体 + Σ (cos θ / η) G_{ησ} * χ_s（範囲の外は端の値が続く）
                ctx.submit_and_wait(|_, cmd| {
                    self.dispatch(cmd, self.util, &UtilPush { src: addr(&b[phase]), dst: addr(&b[t0]), n: n as u32, mode: 0, p: 255, a: 0.0 }, n);
                    self.barrier(cmd);
                    self.blur3(cmd, &g, &b[t0], &b[t1], &b[t2], &b[ws], st.sigma, true);
                    for &(p, coef, e) in &r.terms {
                        self.dispatch(cmd, self.util, &UtilPush { src: addr(&b[phase]), dst: addr(&b[t0]), n: n as u32, mode: 0, p: p as u32, a: 0.0 }, n);
                        self.barrier(cmd);
                        self.blur3(cmd, &g, &b[t0], &b[t1], &b[t2], &b[conv], st.sigma * e, true);
                        self.dispatch(cmd, self.util, &UtilPush { src: addr(&b[conv]), dst: addr(&b[ws]), n: n as u32, mode: 1, p: 0, a: coef }, n);
                        self.barrier(cmd);
                    }
                    self.copy(cmd, &b[fillb], &b[before], fbytes);
                    self.barrier(cmd);
                })?;
                let mut lam = 0.0f32;
                let (mut iterations, mut change) = (0usize, 0.0f64);
                for step in 0..=r.max_iterations {
                    let last = step == r.max_iterations;
                    ctx.submit_and_wait(|dev, cmd| unsafe {
                        if step > 0 {
                            // 前の φ に λ を当てる → next、変化の合計。before ← fill、fill ← next
                            dev.cmd_fill_buffer(cmd, b[sums].buffer, 0, 16, 0);
                            self.barrier(cmd);
                            let push = ApplyPush { fill: addr(&b[fillb]), before: addr(&b[before]), next: addr(&b[next]), phi: addr(&b[phi]), phase: addr(&b[phase]), sums: addr(&b[sums]), n: n as u32, lam, width: st.width, pad: 0 };
                            self.dispatch(cmd, self.apply, &push, n);
                            self.barrier(cmd);
                            self.copy(cmd, &b[fillb], &b[before], fbytes);
                            self.copy(cmd, &b[next], &b[fillb], fbytes);
                            self.barrier(cmd);
                            self.copy(cmd, &b[sums], &b[sums_host], 16);
                        }
                        if !last {
                            self.blur3(cmd, &g, &b[fillb], &b[t1], &b[t2], &b[conv], st.sigma, false);
                            dev.cmd_fill_buffer(cmd, b[hist].buffer, 0, (HIST_BINS * 4) as u64, 0);
                            self.barrier(cmd);
                            let push = PhiPush {
                                fill: addr(&b[fillb]),
                                conv: addr(&b[conv]),
                                ws: addr(&b[ws]),
                                phase: addr(&b[phase]),
                                phi: addr(&b[phi]),
                                hist: addr(&b[hist]),
                                nx: g.nx as u32,
                                ny: g.ny as u32,
                                nz: g.nz as u32,
                                nbins: HIST_BINS as u32,
                                grav: st.grav as f32,
                                z0: g.center(0, 0, 0).z as f32,
                                cell: g.cell as f32,
                                hist_lo: HIST_LO,
                                hist_scale: scale,
                                pad: 0,
                            };
                            self.dispatch(cmd, self.phi, &push, n);
                            self.barrier(cmd);
                            self.copy(cmd, &b[hist], &b[hist_host], (HIST_BINS * 4) as u64);
                        }
                    })?;
                    if step > 0 {
                        let s = b[sums_host].mapped_bytes().ok_or("sums not mapped")?;
                        let word = |i: usize| u32::from_le_bytes(s[i * 4..i * 4 + 4].try_into().unwrap());
                        let (d1, d2, interface) = (word(0) as f64 / 256.0, word(1) as f64 / 256.0, word(2) as usize);
                        iterations += 1;
                        change = d1;
                        if last || stage_done(step - 1, d1, d2, interface) {
                            break;
                        }
                    }
                    // 体積が合うしきい値 λ：Σ_区間 個数 · clamp((φ − λ) / width + ½, 0, 1) = 残りの体積（二分法）
                    let h = b[hist_host].mapped_bytes().ok_or("histogram not mapped")?;
                    let bins: Vec<(f32, f64)> = (0..HIST_BINS)
                        .filter_map(|i| {
                            let c = u32::from_le_bytes(h[i * 4..i * 4 + 4].try_into().unwrap());
                            (c > 0).then(|| (HIST_LO + (i as f32 + 0.5) / scale, c as f64))
                        })
                        .collect();
                    let Some(&(first, _)) = bins.first() else { break };
                    let (mut a, mut bb) = (first - st.width, bins.last().unwrap().0 + st.width);
                    let amount = |l: f32| -> f64 { bins.iter().map(|&(x, c)| c * ((x - l) / st.width + 0.5).clamp(0.0, 1.0) as f64).sum() };
                    for _ in 0..28 {
                        let mid = 0.5 * (a + bb);
                        if amount(mid) > need {
                            a = mid;
                        } else {
                            bb = mid;
                        }
                    }
                    lam = 0.5 * (a + bb);
                }
                log.push((st.sigma, iterations, change.round() as usize));
            }
            ctx.submit_and_wait(|_, cmd| self.copy(cmd, &b[fillb], &b[fill_host], fbytes))?;
            let out = b[fill_host].mapped_bytes().ok_or("fill not mapped")?;
            for (k, v) in fill.iter_mut().enumerate() {
                *v = f32::from_le_bytes(out[k * 4..k * 4 + 4].try_into().unwrap());
            }
            Ok(log)
        })();
        for mut buf in bufs {
            ctx.destroy_buffer(&mut buf);
        }
        result
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        let dev = &self.ctx.device;
        self.ctx.wait_idle();
        unsafe {
            for p in [self.blur, self.util, self.phi, self.apply] {
                dev.destroy_pipeline(p, None);
            }
            dev.destroy_pipeline_layout(self.layout, None);
        }
    }
}

/// GPU でくり返しを行う Relaxer。デバイスは初めて解くときに作る
#[derive(Default)]
pub struct GpuRelaxer {
    gpu: Mutex<Option<Result<Gpu, String>>>,
}

impl Relaxer for GpuRelaxer {
    fn relax(&self, r: &Relax, fill: &mut Vec<f32>) -> Result<Vec<(f64, usize, usize)>, String> {
        let mut g = self.gpu.lock().map_err(|_| "the GPU solver is poisoned".to_string())?;
        match g.get_or_insert_with(Gpu::new) {
            Ok(gpu) => gpu.relax(r, fill),
            Err(e) => {
                let e = e.clone();
                // デバイスを作れない：以後は CPU で解く
                set_relaxer(None);
                Err(e)
            }
        }
    }
}

/// 表面実装のはんだのシミュレーションを GPU で行うようにする（KRBR_SOLDER_CPU があれば CPU のまま）
pub fn install() {
    if krbr_scene::solder::env_flag("KRBR_SOLDER_CPU") {
        return;
    }
    set_relaxer(Some(Arc::new(GpuRelaxer::default())));
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{DVec2, DVec3};
    use krbr_scene::solder_sim::{solve, Params, Problem};

    /// GPU と CPU で同じ形になる（金属の壁際のはんだ。GPU が要る）
    #[test]
    #[ignore]
    fn gpu_matches_cpu() {
        let pad = vec![vec![DVec2::new(-0.4, -0.3), DVec2::new(0.4, -0.3), DVec2::new(0.4, 0.3), DVec2::new(-0.4, 0.3)]];
        let (lo, hi) = (DVec3::new(-0.6, -0.4, 0.012), DVec3::new(-0.05, 0.4, 0.4));
        let c = |x: usize, y: usize, z: usize| DVec3::new([lo.x, hi.x][x], [lo.y, hi.y][y], [lo.z, hi.z][z]);
        let quads = [
            [c(0, 0, 0), c(0, 1, 0), c(1, 1, 0), c(1, 0, 0)],
            [c(0, 0, 1), c(1, 0, 1), c(1, 1, 1), c(0, 1, 1)],
            [c(0, 0, 0), c(1, 0, 0), c(1, 0, 1), c(0, 0, 1)],
            [c(0, 1, 0), c(0, 1, 1), c(1, 1, 1), c(1, 1, 0)],
            [c(0, 0, 0), c(0, 0, 1), c(0, 1, 1), c(0, 1, 0)],
            [c(1, 0, 0), c(1, 1, 0), c(1, 1, 1), c(1, 0, 1)],
        ];
        let tris: Vec<[DVec3; 3]> = quads.iter().flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect();
        let part = vec![0u32; tris.len()];
        let pr = Problem { pad: &pad, opening: &pad, tris: &tris, tri_part: &part, part_metal: &[true], volume: 0.48 * 0.06, copper: 0.035, mask_on_substrate: 0.025 };
        let prm = Params { voxels: 400_000, max_voxels: 400_000, ..Default::default() };
        // CPU と GPU で解く（準備は同じ。くり返しだけが違う）
        set_relaxer(None);
        let t0 = std::time::Instant::now();
        let cpu = solve(&pr, &prm).unwrap();
        let t_cpu = t0.elapsed().as_secs_f64();
        set_relaxer(Some(Arc::new(GpuRelaxer::default())));
        let _ = solve(&pr, &prm); // デバイスを作る分を測らない
        let t0 = std::time::Instant::now();
        let gpu = solve(&pr, &prm).unwrap();
        let t_gpu = t0.elapsed().as_secs_f64();
        set_relaxer(None);
        let g = cpu.grid;
        let differ = cpu.liquid.iter().zip(&gpu.liquid).filter(|(a, b)| a != b).count();
        let liquid = cpu.liquid.iter().filter(|&&b| b).count();
        let top = |s: &krbr_scene::solder_sim::Solution, x: f64| {
            let i = ((x - g.origin.x) / g.cell) as usize;
            (0..g.nz).filter(|&l| s.liquid[g.index(i, g.ny / 2, l)]).map(|l| g.center(i, g.ny / 2, l).z).fold(0.0, f64::max)
        };
        let (wc, wg) = (top(&cpu, -0.05 + 1.5 * g.cell), top(&gpu, -0.05 + 1.5 * g.cell));
        eprintln!("cpu {t_cpu:.2} s {:?}, gpu {t_gpu:.2} s {:?}; {differ} of {liquid} cells differ; wall {wc:.3} / {wg:.3}", cpu.log, gpu.log);
        assert!((differ as f64) < 0.02 * liquid as f64, "{differ} of {liquid}");
        assert!((wc - wg).abs() <= 2.0 * g.cell, "wall {wc} vs {wg}");
    }
}
