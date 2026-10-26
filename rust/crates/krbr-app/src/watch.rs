//! ヘッドレスの保存監視（`krbr watch`、C++ 版 app/watch）。ウィンドウは開かず、保存のたびに描き直して PNG を書き出す

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ash::vk;
use krbr_core::{info, warn, FileWatcher};
use krbr_render::tonemapper::{DisplayMode, Tonemapper};
use krbr_render::vk::memory_barrier;
use krbr_render::{image_io, Context, DenoiseQuality, Denoiser, FrameParams, GpuScene};
use krbr_scene::scene::MaterialFilesWatch;
use krbr_scene::IntegratorKind;

use crate::kicad_cli::StrokeTextJob;
use crate::prepare::{frame_params, initial_camera, prepare_scene_and_context, progressive_or_pt, ReloadStats, RenderOptions};

#[derive(Debug, Clone, Default)]
pub struct WatchOptions {
    /// spp は 1 回の更新で積むサンプル数（省略時 32）。out は書き出す PNG
    pub common: RenderOptions,
    /// この回数だけ更新したら終わる（0 なら止めるまで）
    pub max_updates: u32,
}

/// 1 回の更新の内訳（C++ 版 WatchUpdate）
#[derive(Debug, Clone, Default)]
pub struct WatchUpdate {
    /// 保存による更新か（false：裏で分割していた 3D モデルが揃った、または線フォントの文字を書き出し直した）
    pub saved: bool,
    /// kicad-cli で書き出し直した線フォントの文字を反映した
    pub stroke_text: bool,
    /// ファイルの更新時刻から、画像を書き終えるまで
    pub since_save_ms: f64,
    pub reload_ms: f64,
    pub gpu_ms: f64,
    pub render_ms: f64,
    pub write_ms: f64,
    /// ノイズ除去（OIDN）にかかった時間（render_ms の内訳。しなければ 0）
    pub denoise_ms: f64,
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
    let (prep, ctx) = prepare_scene_and_context(&common, common.validate);
    let Some(mut prep) = prep else { anyhow::bail!("scene preparation failed") };
    let ctx = ctx?;
    let out = common.out.clone();
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let (w, h, spp) = (prep.cfg.width, prep.cfg.height, prep.cfg.spp);
    // カメラは最初の構図で固定する（基板の形が変わっても視点は動かさない）
    let frame = frame_params(&prep.cfg, initial_camera(&prep, &common, w, h), common.seed);

    let mut gpu = GpuScene::new(&ctx);
    gpu.build(&prep.scene, &prep.env, &prep.dist, prep.cfg.scene.bump_intensity).map_err(|e| anyhow::anyhow!("GPU scene: {e}"))?;
    prep.env.rgba = Vec::new();
    // 最初の描画の間の保存も取りこぼさないよう、先に監視を始める
    let mut watcher = FileWatcher::with_defaults(&prep.board.file);
    // ノイズ除去は既定で有効（C++ 版と同じ。--no-denoise で無効）
    let denoise = common.denoise.unwrap_or(true);
    let mut r = Renderer { ctx: &ctx, integrator: prep.cfg.integrator, width: w, height: h, spp, exposure: prep.cfg.exposure_ev, glow: crate::prepare::glow_params(&prep.cfg), frame, denoise, denoise_strength: common.denoise_strength.unwrap_or(1.0), denoiser: None };
    let (render_ms, write_ms, _) = r.render_and_write(&gpu, &out)?;
    info!("watch: wrote {} ({}x{}, {} spp, render {:.0} ms, write {:.0} ms); watching {} for saves", out.display(), w, h, spp, render_ms, write_ms, prep.board.file.display());
    if !on_event(WatchEvent::Ready(&out)) {
        return Ok(());
    }

    let mut models_seen = prep.source.as_ref().map(|s| s.library.completed()).unwrap_or(0);
    let mut material_files = MaterialFilesWatch::new(&prep.board.file);
    let mut idle_since = Instant::now();
    // 線フォントの文字の書き出し（kicad-cli、裏のスレッド）
    let mut text_job = StrokeTextJob::new(prep.kicad_cli.clone(), &prep.board.file, &prep.board.project_name);
    loop {
        let mut stroke_text = false;
        if let Some(plot) = text_job.poll() {
            prep.cfg.scene.layer_art.stroke_text = plot.side;
            stroke_text = true;
        }
        let source = prep.source.as_mut().unwrap();
        let saved = watcher.poll();
        let models_done = source.library.completed() != models_seen;
        // 材質の設定（基板の設定ファイルの [ref.*] / [[model]]、model_map.toml、materials/*.toml）を書き換えたら組み直す
        let materials = match material_files.poll(&prep.board.file, &source.materials) {
            Some(m) => {
                info!("watch: material settings changed; rebuilding");
                source.materials = m;
                true
            }
            None => false,
        };
        if !saved && !models_done && !stroke_text && !materials {
            if idle_since.elapsed() >= poll {
                if !on_event(WatchEvent::Idle) {
                    return Ok(());
                }
                idle_since = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let mut u = WatchUpdate { saved, stroke_text, ..Default::default() };
        let t0 = Instant::now();
        if stroke_text {
            source.tiles.invalidate_all(); // シルクが変わったので、どのタイルも作り直す
        }
        let scene = if saved {
            match source.reload(&prep.board, &prep.cfg.scene) {
                Ok((board, scene, stats)) => {
                    prep.board = board;
                    if stats.diff.stroke_text_changed {
                        text_job.request(); // 書き出しの間にまた変わったら、終わった後にもう一度書き出す
                    }
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
        if let Err(e) = gpu.rebuild_geometry(&prep.scene, prep.cfg.scene.bump_intensity) {
            warn!("watch: cannot send the board to the GPU ({e}); waiting for the next save");
            continue;
        }
        u.gpu_ms = ms_since(t1);
        (u.render_ms, u.write_ms, u.denoise_ms) = r.render_and_write(&gpu, &out)?;
        if saved {
            u.since_save_ms = watcher.saved_at().and_then(|t| t.elapsed().ok()).map(|e| e.as_secs_f64() * 1000.0).unwrap_or(0.0);
            let s = &u.stats;
            let meshing = if u.pending_models > 0 { format!(", {} model(s) meshing in background", u.pending_models) } else { String::new() };
            info!(
                "watch: updated {:.0} ms after save (reload {:.0} [parse {:.0}, {} tiles], gpu {:.0}, render {:.0} [denoise {:.0}], write {:.0}); {} added, {} changed, {} removed{}",
                u.since_save_ms, u.reload_ms, s.parse_ms, s.tiles_rebuilt, u.gpu_ms, u.render_ms, u.denoise_ms, u.write_ms, s.diff.added, s.diff.changed, s.diff.removed, meshing
            );
        } else if stroke_text {
            info!("watch: updated (background stroke-font text plot finished)");
        } else if materials {
            info!("watch: updated (material settings)");
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
    integrator: IntegratorKind,
    width: u32,
    height: u32,
    spp: u32,
    exposure: f32,
    glow: krbr_render::glow::GlowParams,
    frame: FrameParams,
    /// ノイズ除去をする・強さ・OIDN（最初の描画で作り、以後は使い回す。作れなければ None のまま）
    denoise: bool,
    denoise_strength: f32,
    denoiser: Option<Denoiser<'a>>,
}

impl<'a> Renderer<'a> {
    /// 戻り値：(描画と読み戻しの ms, 書き出しの ms, そのうちノイズ除去の ms)
    fn render_and_write(&mut self, gpu: &GpuScene<'_>, out: &PathBuf) -> anyhow::Result<(f64, f64, f64)> {
        let t0 = Instant::now();
        // 形状を作り直すと積分器の参照先も変わるので、更新のたびに作る（C++ 版の rebuildIntegrator と同じ）
        let mut prog = progressive_or_pt(self.ctx, gpu, self.integrator, self.width, self.height, self.frame.clone()).map_err(|e| anyhow::anyhow!(e))?;
        // PT に代えたら、次の更新からも PT にする（毎回作り直して失敗しない）
        self.integrator = prog.integrator_kind();
        let tone = Tonemapper::new(self.ctx, prog.targets()).map_err(|e| anyhow::anyhow!(e))?;
        // 8 サンプルずつ投入する（GPU を長く占有しない）
        let mut left = self.spp;
        while left > 0 {
            let n = left.min(8);
            self.ctx.submit_and_wait(|_, cmd| prog.record(cmd, n)).map_err(|e| anyhow::anyhow!(e))?;
            left -= n;
        }
        // ノイズ除去（OIDN のデバイスとバッファは使い回し、累積の画像だけ参照し直す）
        if self.denoise && self.denoiser.is_none() {
            self.denoise = false; // 作れなければ以後は試さない
            match Denoiser::new(self.ctx, prog.targets(), DenoiseQuality::High) {
                Ok(d) => {
                    self.denoiser = Some(d);
                    self.denoise = true;
                }
                Err(e) => warn!("watch: denoiser not available ({e}); writing the noisy image"),
            }
        }
        let mut denoise_ms = 0.0;
        let mut tone_denoised = None;
        // 分光モードの累積は XYZ なので、OIDN にはリニア sRGB にして渡し、結果は XYZ に戻す（トーンマッピングは XYZ として読む）
        let xyz = prog.xyz_output();
        if let Some(d) = &self.denoiser {
            d.rebind(prog.targets());
            match self.ctx.submit_and_wait(|_, cmd| d.record_prepare(cmd, xyz)).and_then(|()| d.execute()) {
                Ok(ms) => {
                    denoise_ms = ms;
                    let t = prog.targets();
                    tone_denoised = Some(Tonemapper::with_views(self.ctx, d.output().view, t.albedo.view, t.normal.view, t.width, t.height).map_err(|e| anyhow::anyhow!(e))?);
                }
                Err(e) => warn!("watch: {e}"),
            }
        }
        let (tm, strength) = match &tone_denoised {
            Some(t) => (t, self.denoise_strength),
            None => (&tone, 0.0),
        };
        let img = tm.output();
        let (w, h) = (img.width, img.height);
        let mut rb = self.ctx.create_buffer(w as u64 * h as u64 * 4, vk::BufferUsageFlags::TRANSFER_DST, true, true).map_err(|e| anyhow::anyhow!(e))?;
        let (image, rbb) = (img.image, rb.buffer);
        let denoiser = self.denoiser.as_ref().filter(|_| tone_denoised.is_some());
        let copied = self.ctx.submit_and_wait(|dev, cmd| {
            if let Some(d) = denoiser {
                d.record_output(cmd, strength, xyz);
            }
            tm.record(cmd, self.exposure, DisplayMode::Beauty, xyz, &self.glow);
            memory_barrier(dev, cmd, vk::PipelineStageFlags2::COMPUTE_SHADER, vk::AccessFlags2::SHADER_STORAGE_WRITE, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_READ);
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 })
                .image_extent(vk::Extent3D { width: w, height: h, depth: 1 });
            unsafe { dev.cmd_copy_image_to_buffer(cmd, image, vk::ImageLayout::GENERAL, rbb, &[region]) };
            memory_barrier(dev, cmd, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE, vk::PipelineStageFlags2::HOST, vk::AccessFlags2::HOST_READ);
        });
        // 確保した大きさは求めた大きさより大きいことがあるので、画像の分（mapped_bytes）だけを使う
        let px = copied.map(|()| rb.mapped_bytes().map(<[u8]>::to_vec).unwrap_or_default());
        self.ctx.destroy_buffer(&mut rb);
        let mut px = px.map_err(|e| anyhow::anyhow!(e))?;
        for c in px.chunks_exact_mut(4) {
            c[3] = 255;
        }
        let render_ms = ms_since(t0);
        let t1 = Instant::now();
        image_io::save_png_rgba8_fast(out, w, h, &px).map_err(|e| anyhow::anyhow!("{}: {e}", out.display()))?;
        Ok((render_ms, ms_since(t1), denoise_ms))
    }
}
