//! ヘッドレスの保存監視（`krbr watch`、C++ 版 app/watch）。ウィンドウは開かず、保存のたびに描き直して PNG を書き出す

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ash::vk;
use krbr_core::{info, warn, FileWatcher};
use krbr_render::tonemapper::{DisplayMode, Tonemapper};
use krbr_render::vk::memory_barrier;
use krbr_render::{image_io, Context, ContextOptions, FrameParams, GpuScene, Progressive};

use crate::prepare::{frame_params, initial_camera, prepare_scene, ReloadStats, RenderOptions};

#[derive(Debug, Clone, Default)]
pub struct WatchOptions {
    /// spp は 1 回の更新で積むサンプル数（省略時 32）。out は書き出す PNG
    pub common: RenderOptions,
    /// この回数だけ更新したら終わる（0 なら止めるまで）
    pub max_updates: u32,
}

/// 1 回の更新の内訳（C++ 版 WatchUpdate。ノイズ除去は Rust 版では未対応）
#[derive(Debug, Clone, Default)]
pub struct WatchUpdate {
    /// 保存による更新か（false：裏で分割していた 3D モデルが揃った）
    pub saved: bool,
    /// ファイルの更新時刻から、画像を書き終えるまで
    pub since_save_ms: f64,
    pub reload_ms: f64,
    pub gpu_ms: f64,
    pub render_ms: f64,
    pub write_ms: f64,
    pub stats: ReloadStats,
    pub pending_models: usize,
}

/// 監視の途中で呼び出し側へ知らせること
pub enum WatchEvent<'a> {
    /// 最初の画像を書き出した
    Ready(&'a Path),
    /// 保存（または 3D モデルの分割の完了）を受けて描き直した
    Updated(&'a WatchUpdate),
    /// poll の間、何も起きなかった
    Idle,
}

/// 基板を開いて最初の画像を書き出し、保存のたびに描き直す。on_event が false を返したら終わる。
/// poll：何も起きないときに on_event(Idle) を呼ぶ間隔
pub fn run_watch(o: &WatchOptions, poll: Duration, mut on_event: impl FnMut(WatchEvent) -> bool) -> anyhow::Result<()> {
    let mut common = o.common.clone();
    common.spp = Some(common.spp.unwrap_or(32));
    let Some(mut prep) = prepare_scene(&common) else { anyhow::bail!("scene preparation failed") };
    let out = common.out.clone();
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let (w, h, spp) = (prep.cfg.width, prep.cfg.height, prep.cfg.spp);
    // カメラは最初の構図で固定する（基板の形が変わっても視点は動かさない）
    let frame = frame_params(&prep.cfg, initial_camera(&prep, &common, w, h), common.seed);

    let ctx = Context::new(&ContextOptions { validation: common.validate, low_queue_priority: true, ..Default::default() }).map_err(|e| anyhow::anyhow!(e))?;
    let mut gpu = GpuScene::new(&ctx);
    gpu.build(&prep.scene, &prep.env, &prep.dist, prep.cfg.scene.bump_intensity);
    prep.env.rgba = Vec::new();
    // 最初の描画の間の保存も取りこぼさないよう、先に監視を始める
    let mut watcher = FileWatcher::with_defaults(&prep.board.file);
    let mut r = Renderer { ctx: &ctx, integrator: prep.cfg.integrator.clone(), width: w, height: h, spp, exposure: prep.cfg.exposure_ev, frame };
    let (render_ms, write_ms) = r.render_and_write(&gpu, &out)?;
    info!("watch: wrote {} ({}x{}, {} spp, render {:.0} ms, write {:.0} ms); watching {} for saves", out.display(), w, h, spp, render_ms, write_ms, prep.board.file.display());
    if !on_event(WatchEvent::Ready(&out)) {
        return Ok(());
    }

    let mut models_seen = prep.source.as_ref().map(|s| s.library.completed()).unwrap_or(0);
    let mut idle_since = Instant::now();
    loop {
        let source = prep.source.as_mut().unwrap();
        let saved = watcher.poll();
        let models_done = source.library.completed() != models_seen;
        if !saved && !models_done {
            if idle_since.elapsed() >= poll {
                if !on_event(WatchEvent::Idle) {
                    return Ok(());
                }
                idle_since = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let mut u = WatchUpdate { saved, ..Default::default() };
        let t0 = Instant::now();
        let scene = if saved {
            match source.reload(&prep.board, &prep.cfg.scene) {
                Ok((board, scene, stats)) => {
                    prep.board = board;
                    u.stats = stats;
                    scene
                }
                Err(e) => {
                    warn!("watch: cannot read the board ({e}); waiting for the next save");
                    continue;
                }
            }
        } else {
            models_seen = source.library.completed();
            source.build(&prep.board, &prep.cfg.scene, true)
        };
        u.reload_ms = ms_since(t0);
        u.pending_models = scene.pending_models;
        let t1 = Instant::now();
        prep.scene = scene;
        gpu.rebuild_geometry(&prep.scene, prep.cfg.scene.bump_intensity);
        u.gpu_ms = ms_since(t1);
        (u.render_ms, u.write_ms) = r.render_and_write(&gpu, &out)?;
        if saved {
            u.since_save_ms = watcher.saved_at().and_then(|t| t.elapsed().ok()).map(|e| e.as_secs_f64() * 1000.0).unwrap_or(0.0);
            let s = &u.stats;
            let meshing = if u.pending_models > 0 { format!(", {} model(s) meshing in background", u.pending_models) } else { String::new() };
            info!(
                "watch: updated {:.0} ms after save (reload {:.0} [parse {:.0}, {} tiles], gpu {:.0}, render {:.0}, write {:.0}); {} added, {} changed, {} removed{}",
                u.since_save_ms, u.reload_ms, s.parse_ms, s.tiles_rebuilt, u.gpu_ms, u.render_ms, u.write_ms, s.diff.added, s.diff.changed, s.diff.removed, meshing
            );
        } else {
            info!("watch: updated (background model meshing finished, {} pending)", u.pending_models);
        }
        idle_since = Instant::now();
        if !on_event(WatchEvent::Updated(&u)) {
            return Ok(());
        }
    }
}

/// `krbr watch`：止めるまで（または --max-updates 回）保存のたびに描き直す
pub fn cmd_watch(o: &WatchOptions) -> anyhow::Result<()> {
    let mut updates = 0;
    run_watch(o, Duration::from_secs(1), |e| {
        if let WatchEvent::Updated(_) = e {
            updates += 1;
        }
        o.max_updates == 0 || updates < o.max_updates
    })
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// 1 回分の描画：積分器とトーンマッピングを作り、spp を積んで RGBA8 を読み戻し、PNG に書く
struct Renderer<'a> {
    ctx: &'a Context,
    integrator: String,
    width: u32,
    height: u32,
    spp: u32,
    exposure: f32,
    frame: FrameParams,
}

impl Renderer<'_> {
    /// 戻り値：(描画と読み戻しの ms, 書き出しの ms)
    fn render_and_write(&mut self, gpu: &GpuScene<'_>, out: &PathBuf) -> anyhow::Result<(f64, f64)> {
        let t0 = Instant::now();
        // 形状を作り直すと積分器の参照先も変わるので、更新のたびに作る（C++ 版の rebuildIntegrator と同じ）
        let mut prog = Progressive::new(self.ctx, gpu, &self.integrator, self.width, self.height, self.frame.clone()).map_err(|e| anyhow::anyhow!(e))?;
        if prog.integrator_name() != self.integrator {
            self.integrator = prog.integrator_name().to_string();
        }
        let tone = Tonemapper::new(self.ctx, prog.targets()).map_err(|e| anyhow::anyhow!(e))?;
        // 8 サンプルずつ投入する（GPU を長く占有しない）
        let mut left = self.spp;
        while left > 0 {
            let n = left.min(8);
            self.ctx.submit_and_wait(|_, cmd| prog.record(cmd, n));
            left -= n;
        }
        let img = tone.output();
        let (w, h) = (img.width, img.height);
        let mut rb = self.ctx.create_buffer(w as u64 * h as u64 * 4, vk::BufferUsageFlags::TRANSFER_DST, true, true);
        let (image, rbb, xyz) = (img.image, rb.buffer, prog.xyz_output());
        self.ctx.submit_and_wait(|dev, cmd| {
            tone.record(cmd, self.exposure, DisplayMode::Beauty, xyz);
            memory_barrier(dev, cmd, vk::PipelineStageFlags2::COMPUTE_SHADER, vk::AccessFlags2::SHADER_STORAGE_WRITE, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_READ);
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 })
                .image_extent(vk::Extent3D { width: w, height: h, depth: 1 });
            unsafe { dev.cmd_copy_image_to_buffer(cmd, image, vk::ImageLayout::GENERAL, rbb, &[region]) };
        });
        let mut px = rb.mapped_ref().unwrap().to_vec();
        self.ctx.destroy_buffer(&mut rb);
        for c in px.chunks_exact_mut(4) {
            c[3] = 255;
        }
        let render_ms = ms_since(t0);
        let t1 = Instant::now();
        image_io::save_png_rgba8_fast(out, w, h, &px).map_err(|e| anyhow::anyhow!("{}: {e}", out.display()))?;
        Ok((render_ms, ms_since(t1)))
    }
}
