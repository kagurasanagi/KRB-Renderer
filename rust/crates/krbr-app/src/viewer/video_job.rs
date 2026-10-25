//! 動画の書き出し（ファイル → 動画を書き出す）：カメラを決まった動き（ターンテーブルなど）で動かしながら、1 コマずつ
//! 決まったサンプル数まで積んで（ノイズ除去して）トーンマッピングし、MP4（H.264）にする。書き出しの間は描画ロックと
//! 同じく、ほかの操作で累積が変わらないようにする。
//! 時間方向のちらつき：OIDN は 1 枚ずつのノイズ除去（時間方向は見ない）。どのコマも同じ乱数の列（画素・サンプル番号・
//! シードが同じ）で積むので、残ったノイズは画面に張り付いた模様になり、コマごとに跳ねない。サンプル数を増やすほど消える

use serde_json::{json, Value};

use super::*;
use crate::video::{bitrate_for, VideoFormat, VideoWriter};

/// カメラの動き
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pattern {
    /// 基板のまわりを 1 周（等速。最初と最後がつながる）
    Turntable,
    /// 左右に首を振る（±30°）
    Swing,
    /// 1 周しながら、上下にゆっくり揺れる（±10°）
    Wave,
}

impl Pattern {
    fn as_str(self) -> &'static str {
        match self {
            Pattern::Turntable => "turntable",
            Pattern::Swing => "swing",
            Pattern::Wave => "wave",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        [Pattern::Turntable, Pattern::Swing, Pattern::Wave].into_iter().find(|p| p.as_str() == s)
    }
}

/// 画質（圧縮）：1 画素・1 フレームあたりのビット数
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Quality {
    High,
    Standard,
    Small,
}

impl Quality {
    fn as_str(self) -> &'static str {
        match self {
            Quality::High => "high",
            Quality::Standard => "standard",
            Quality::Small => "small",
        }
    }

    fn bits_per_pixel(self) -> f64 {
        match self {
            Quality::High => 0.2,
            Quality::Standard => 0.1,
            Quality::Small => 0.05,
        }
    }
}

/// 書き出しの設定（「動画を書き出す」の窓で決める。次に開いたときも同じ）
#[derive(Debug, Clone)]
pub(super) struct VideoSettings {
    pub pattern: Pattern,
    /// 上から見て、基板が時計回りに回る
    pub clockwise: bool,
    pub seconds: f32,
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    pub spp: u32,
    pub denoise: bool,
    pub quality: Quality,
    /// 基板の中心のまわりを回る（false なら今の注視点）
    pub center_board: bool,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self { pattern: Pattern::Turntable, clockwise: false, seconds: 12.0, fps: 30, width: 1920, height: 1080, spp: 128, denoise: true, quality: Quality::Standard, center_board: true }
    }
}

impl VideoSettings {
    pub fn frames(&self) -> u32 {
        ((self.seconds * self.fps as f32).round() as u32).max(1)
    }

    pub fn bitrate(&self) -> u32 {
        bitrate_for(self.width, self.height, self.fps, self.quality.bits_per_pixel())
    }
}

/// 動画のサイズの選択肢
const SIZES: [(u32, u32); 5] = [(1920, 1080), (1280, 720), (1080, 1080), (1080, 1350), (3840, 2160)];
const SPPS: [u32; 6] = [32, 64, 128, 256, 512, 1024];
const FPS: [u32; 3] = [24, 30, 60];

/// 書き出しの前の視点と設定（終わったら戻す）
struct Restore {
    target: Vec3,
    distance: f32,
    elevation: f64,
    azimuth: f64,
    size_mode: Option<(u32, u32)>,
    max_spp: u32,
    denoise: bool,
    paused: bool,
}

pub(super) struct VideoJob {
    s: VideoSettings,
    path: PathBuf,
    total: u32,
    /// 今描いているコマ
    frame: u32,
    /// 回る中心と距離・向きの基準
    target: Vec3,
    distance: f32,
    elevation: f64,
    azimuth: f64,
    restore: Restore,
    writer: Option<VideoWriter>,
    started: Instant,
    /// 前のコマのカメラ（フローを求める）
    prev_cam: Option<krbr_render::flow::FlowCamera>,
    /// 開発用：このコマ数で書き出しを終える（KRBR_TEST_VIDEO_MAXFRAMES）
    max_frames: Option<u32>,
}

/// 「動画を書き出す」の窓の状態
#[derive(Default)]
pub(super) struct VideoDialog {
    pub open: bool,
    pub settings: VideoSettings,
    /// 保存先を選ぶダイアログ
    pub picking: Option<mpsc::Receiver<Option<PathBuf>>>,
    /// 最後の結果（書き出した・失敗した・中止した）
    pub message: String,
    pub error: bool,
}

impl App {
    /// t（0..1）のときのカメラ（方位・仰角）
    fn video_camera(job: &VideoJob, t: f64) -> (f64, f64) {
        let sign = if job.s.clockwise { 1.0 } else { -1.0 };
        let two_pi_t = 2.0 * std::f64::consts::PI * t;
        match job.s.pattern {
            Pattern::Turntable => (job.azimuth + sign * 360.0 * t, job.elevation),
            Pattern::Swing => (job.azimuth + 30.0 * two_pi_t.sin(), job.elevation),
            Pattern::Wave => (job.azimuth + sign * 360.0 * t, (job.elevation + 10.0 * two_pi_t.sin()).clamp(-89.0, 89.0)),
        }
    }

    /// 書き出すコマのカメラにする（累積は、次のフレームでやり直される）
    fn set_video_frame(&mut self) {
        let Some(job) = &self.video else { return };
        let (az, el) = Self::video_camera(job, job.frame as f64 / job.total as f64);
        let (target, distance) = (job.target, job.distance);
        let st = &mut self.st;
        (st.target, st.distance, st.azimuth, st.elevation) = (target, distance, az, el);
        st.changed.frame = true;
    }

    /// 動画の書き出しを始める
    pub(super) fn start_video(&mut self, path: PathBuf) -> Result<(), String> {
        if !self.has_board() {
            return Err(fl!("save-no-board"));
        }
        // 基板を開いた直後で、最初の視点がまだ決まっていなければ、先に決める（その視点から始める）
        if std::mem::take(&mut self.st.req.start_view) {
            self.start_camera(self.st.display.w, self.st.display.h);
        }
        let s = self.video_dialog.settings.clone();
        let writer = VideoWriter::start(&VideoFormat { path: path.clone(), width: s.width, height: s.height, fps: s.fps, bitrate: s.bitrate(), quality: None, hardware: true })?;
        let st = &mut self.st;
        let restore = Restore { target: st.target, distance: st.distance, elevation: st.elevation, azimuth: st.azimuth, size_mode: st.size_mode, max_spp: st.max_spp, denoise: st.denoise, paused: st.paused };
        let b = &self.prep.scene.board;
        let target = if s.center_board { krbr_geom::board_mesh::board_to_world(b.center, 0.0, b.center) } else { st.target };
        st.size_mode = Some((s.width, s.height));
        st.max_spp = s.spp;
        st.denoise = s.denoise;
        st.paused = false;
        let total = s.frames();
        info!("video: {} ({}x{}, {} fps, {} frames, {} spp, {:.1} Mbit/s, {})", path.display(), s.width, s.height, s.fps, total, s.spp, s.bitrate() as f64 / 1e6, s.pattern.as_str());
        self.video = Some(VideoJob { total, frame: 0, target, distance: st.distance, elevation: st.elevation, azimuth: st.azimuth, restore, writer: Some(writer), started: Instant::now(), path, s, prev_cam: None, max_frames: std::env::var("KRBR_TEST_VIDEO_MAXFRAMES").ok().and_then(|v| v.parse().ok()) });
        self.video_dialog.message.clear();
        self.set_video_frame();
        Ok(())
    }

    /// フレームを出した後：今のコマを積み終えていたら（ノイズ除去も）、書いて次のコマへ
    pub(super) fn step_video(&mut self) {
        // 開発用：基板を開いたら、短い動画を書き出す
        if !self.splash && self.has_board() && self.video.is_none() && self.board_job.is_none() {
            if let Some(p) = self.test.o.video.take() {
                let s = &mut self.video_dialog.settings;
                (s.width, s.height, s.seconds, s.fps, s.spp) = (320, 180, 1.0, 10, 16);
                if let Some(n) = std::env::var("KRBR_TEST_VIDEO_SPP").ok().and_then(|v| v.parse().ok()) {
                    (s.width, s.height, s.spp, s.seconds) = (1280, 720, n, 0.2);
                    s.denoise = std::env::var_os("KRBR_TEST_VIDEO_NODENOISE").is_none();
                }
                if let Some(x) = std::env::var("KRBR_TEST_VIDEO_SECONDS").ok().and_then(|v| v.parse().ok()) {
                    (s.seconds, s.fps) = (x, 30);
                }
                if let Err(e) = self.start_video(p) {
                    error!("video (test): {e}");
                }
                self.video_test_exit = true;
            }
        }
        let Some(job) = &self.video else { return };
        let Some(g) = self.gpu.as_ref() else { return };
        let t = g.prog.targets();
        let use_denoise = job.s.denoise && g.denoiser.is_some();
        let ready = (t.width, t.height) == (job.s.width, job.s.height)
            && !self.st.changed.frame
            && g.prog.samples() >= job.s.spp
            && (!use_denoise || (self.denoised_samples == g.prog.samples() && self.denoised_epoch == self.accum_epoch));
        // ノイズ除去を頼んだのに使えない（作れなかった）ときは、そのまま書く
        if !ready {
            return;
        }
        let img = match read_image(g, use_denoise) {
            Ok(i) => i,
            Err(e) => return self.end_video(Some(e)),
        };
        self.note_video_camera();
        let rgba = image_io::tonemap_rgba8(&img, self.st.exposure, &self.st.glow_params());
        let job = self.video.as_mut().unwrap();
        // 開発用：エンコードする前のコマを PNG でも書く（画質の比べ用）
        if let Some(dir) = std::env::var_os("KRBR_VIDEO_RAW_DIR") {
            let _ = image_io::save_png_rgba8(&PathBuf::from(dir).join(format!("raw_{:04}.png", job.frame)), job.s.width, job.s.height, &rgba);
        }
        if let Err(e) = job.writer.as_mut().unwrap().push(rgba) {
            return self.end_video(Some(e));
        }
        job.frame += 1;
        if job.frame >= job.total || job.max_frames.is_some_and(|m| job.frame >= m) {
            self.end_video(None);
        } else {
            self.set_video_frame();
        }
    }

    /// 今のコマのカメラ（フローを求める。レンズの写し方を含む）
    fn video_flow_camera(&self, w: u32, h: u32) -> krbr_render::flow::FlowCamera {
        let cam = orbit_camera(self.st.target, self.st.distance, self.st.elevation, self.st.azimuth, self.st.fov, w as f32 / h.max(1) as f32);
        krbr_render::flow::FlowCamera::new(cam, &self.prep.cfg.lens)
    }

    /// このコマのカメラを覚える（次のコマのフローに使う）。開発用（KRBR_VIDEO_RAW_DIR）：前のコマへの後ろ向きのフローを
    /// flow_<コマ>.bin（float の x, y）にも書く（ちらつきを、前のコマをずらして比べて測る）
    fn note_video_camera(&mut self) {
        let Some(g) = self.gpu.as_ref() else { return };
        let (w, h) = (g.prog.targets().width, g.prog.targets().height);
        let cam = self.video_flow_camera(w, h);
        if let Some(dir) = std::env::var_os("KRBR_VIDEO_RAW_DIR") {
            if let (Some(prev), Ok(pos)) = (self.video.as_ref().and_then(|j| j.prev_cam.clone()), g.prog.readback_position()) {
                let f = krbr_render::flow::backward_flow(&pos, w, h, &cam, &prev);
                let bytes: Vec<u8> = f.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
                let frame = self.video.as_ref().map(|j| j.frame).unwrap_or(0);
                let _ = std::fs::write(PathBuf::from(dir).join(format!("flow_{frame:04}.bin")), bytes);
            }
        }
        if let Some(j) = self.video.as_mut() {
            j.prev_cam = Some(cam);
        }
    }

    /// 書き出しを終える（error があれば失敗、中止は cancel_video）。視点と設定を戻す
    pub(super) fn end_video(&mut self, error: Option<String>) {
        let Some(mut job) = self.video.take() else { return };
        let result = job.writer.take().map(|w| w.finish()).unwrap_or(Ok(0));
        let r = &job.restore;
        let st = &mut self.st;
        (st.target, st.distance, st.elevation, st.azimuth) = (r.target, r.distance, r.elevation, r.azimuth);
        (st.size_mode, st.max_spp, st.denoise, st.paused) = (r.size_mode, r.max_spp, r.denoise, r.paused);
        st.changed.frame = true;
        let secs = job.started.elapsed().as_secs_f64();
        let d = &mut self.video_dialog;
        match (error, result) {
            (None, Ok(bytes)) if job.frame >= job.total => {
                info!("video: wrote {} ({} frames, {:.1} MB) in {}", job.path.display(), job.frame, bytes as f64 / 1e6, format_duration(secs));
                d.message = fl!("video-done", path = job.path.display().to_string(), size = format!("{:.1}", bytes as f64 / 1e6), time = format_duration(secs));
                d.error = false;
            }
            (None, Ok(_)) => {
                info!("video: stopped at frame {} of {}", job.frame, job.total);
                d.message = fl!("video-cancelled", done = job.frame.to_string(), total = job.total.to_string(), path = job.path.display().to_string());
                d.error = false;
            }
            (Some(e), _) | (None, Err(e)) => {
                error!("video: {e}");
                d.message = fl!("video-failed", error = e);
                d.error = true;
            }
        }
        self.last_update = d.message.clone();
        // 開発用の書き出し（--test-video）は、終えたらビューアも閉じる
        if self.video_test_exit {
            self.done = true;
        }
    }

    /// 書き出しを途中でやめる（そこまでのコマで動画を閉じる）
    pub(super) fn cancel_video(&mut self) {
        if self.video.is_some() {
            self.end_video(None);
        }
    }

    /// 書き出しの進み具合（0..1）と、残りの見込み（秒）
    fn video_progress(&self) -> Option<(f64, Option<f64>)> {
        let job = self.video.as_ref()?;
        let g = self.gpu.as_ref()?;
        let within = (g.prog.samples() as f64 / job.s.spp.max(1) as f64).min(1.0);
        let done = (job.frame as f64 + within) / job.total as f64;
        let elapsed = job.started.elapsed().as_secs_f64();
        let remaining = (done > 0.002).then(|| elapsed / done * (1.0 - done));
        Some((done, remaining))
    }

    /// ステータスバーに出す、書き出しの知らせ
    pub(super) fn video_status(&self) -> Option<String> {
        let job = self.video.as_ref()?;
        let (_, remaining) = self.video_progress()?;
        Some(fl!("video-status", frame = (job.frame + 1).min(job.total).to_string(), total = job.total.to_string(), remaining = remaining.map(format_duration).unwrap_or_else(|| "…".into())))
    }

    /// 今の速さから、書き出しにかかる時間の見込み（秒）。まだ測っていなければ None
    fn video_estimate(&self) -> Option<f64> {
        let g = self.gpu.as_ref()?;
        if self.accum_measured == 0 {
            return None;
        }
        let t = g.prog.targets();
        let per_sample = self.accum_wall.as_secs_f64() / self.accum_measured as f64 / (t.width as f64 * t.height as f64).max(1.0);
        let s = &self.video_dialog.settings;
        Some(per_sample * s.width as f64 * s.height as f64 * s.spp as f64 * s.frames() as f64)
    }

    /// 「動画を書き出す」の窓の内容（web/video.html の krbr.video に渡す JSON）
    pub(super) fn video_json(&self) -> String {
        let s = &self.video_dialog.settings;
        let opt = |v: String, label: String| json!({ "v": v, "label": label });
        let running = self.video.as_ref().map(|job| {
            let (done, remaining) = self.video_progress().unwrap_or((0.0, None));
            json!({
                "frame": (job.frame + 1).min(job.total), "total": job.total, "progress": done,
                "elapsed": format_duration(job.started.elapsed().as_secs_f64()),
                "remaining": remaining.map(format_duration),
                "path": job.path.display().to_string(),
            })
        });
        let mb = s.bitrate() as f64 * s.seconds as f64 / 8e6;
        json!({
            "title": fl!("video-title"), "close": fl!("panel-close-tip"),
            "board": self.has_board(),
            "values": {
                "pattern": s.pattern.as_str(), "direction": if s.clockwise { "cw" } else { "ccw" }, "seconds": s.seconds, "fps": s.fps.to_string(),
                "size": format!("{}x{}", s.width, s.height), "spp": s.spp.to_string(), "denoise": s.denoise, "quality": s.quality.as_str(),
                "center": if s.center_board { "board" } else { "target" },
            },
            "options": {
                "pattern": [
                    { "v": "turntable", "label": fl!("video-pattern-turntable"), "tip": fl!("video-pattern-turntable-tip") },
                    { "v": "swing", "label": fl!("video-pattern-swing"), "tip": fl!("video-pattern-swing-tip") },
                    { "v": "wave", "label": fl!("video-pattern-wave"), "tip": fl!("video-pattern-wave-tip") },
                ],
                "direction": [opt("ccw".into(), fl!("video-ccw")), opt("cw".into(), fl!("video-cw"))],
                "fps": FPS.iter().map(|f| opt(f.to_string(), format!("{f} fps"))).collect::<Vec<_>>(),
                "size": SIZES.iter().map(|(w, h)| opt(format!("{w}x{h}"), size_label(*w, *h))).collect::<Vec<_>>(),
                "spp": SPPS.iter().map(|n| opt(n.to_string(), format!("{n} spp"))).collect::<Vec<_>>(),
                "quality": [opt("high".into(), fl!("video-quality-high")), opt("standard".into(), fl!("video-quality-standard")), opt("small".into(), fl!("video-quality-small"))],
                "center": [opt("board".into(), fl!("video-center-board")), opt("target".into(), fl!("video-center-target"))],
            },
            "text": {
                "pattern": fl!("video-pattern"), "direction": fl!("video-direction"), "seconds": fl!("video-seconds"), "fps": fl!("video-fps"), "size": fl!("video-size"),
                "spp": fl!("video-spp"), "sppTip": fl!("video-spp-tip"), "denoise": fl!("video-denoise"), "denoiseTip": fl!("video-denoise-tip"),
                "quality": fl!("video-quality"), "center": fl!("video-center"), "start": fl!("video-start"), "cancel": fl!("video-cancel"),
                "summary": fl!("video-summary", frames = s.frames().to_string(), mb = format!("{:.0}", mb.max(1.0)), mbps = format!("{:.1}", s.bitrate() as f64 / 1e6)),
                "estimate": self.video_estimate().map(|e| fl!("video-estimate", time = format_duration(e))).unwrap_or_else(|| fl!("video-estimate-unknown")),
                "hint": fl!("video-hint"), "noBoard": fl!("save-no-board"), "running": fl!("video-running"), "remaining": fl!("video-remaining"),
            },
            "running": running,
            "message": self.video_dialog.message, "error": self.video_dialog.error,
        })
        .to_string()
    }

    /// 「動画を書き出す」の窓の値を変えた
    pub(super) fn apply_video_set(&mut self, id: &str, v: &Value) {
        let s = &mut self.video_dialog.settings;
        let text = v.as_str().map(str::to_string).or_else(|| v.as_f64().map(|x| x.to_string())).unwrap_or_default();
        match id {
            "video.pattern" => s.pattern = Pattern::parse(&text).unwrap_or(s.pattern),
            "video.direction" => s.clockwise = text == "cw",
            "video.seconds" => {
                if let Ok(x) = text.parse::<f32>() {
                    s.seconds = x.clamp(1.0, 600.0);
                }
            }
            "video.fps" => s.fps = text.parse().ok().filter(|f| FPS.contains(f)).unwrap_or(s.fps),
            "video.size" => {
                if let Some((w, h)) = text.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))) {
                    (s.width, s.height) = (w, h);
                }
            }
            "video.spp" => s.spp = text.parse().ok().filter(|n| SPPS.contains(n)).unwrap_or(s.spp),
            "video.denoise" => s.denoise = v.as_bool().unwrap_or(s.denoise),
            "video.quality" => {
                s.quality = match text.as_str() {
                    "high" => Quality::High,
                    "small" => Quality::Small,
                    _ => Quality::Standard,
                }
            }
            "video.center" => s.center_board = text == "board",
            _ => {}
        }
    }

    /// 「動画を書き出す」の窓のボタン
    pub(super) fn apply_video_click(&mut self, id: &str) {
        match id {
            "video.close" => self.video_dialog.open = false,
            "video.cancel" => self.cancel_video(),
            "video.start" => {
                if self.video.is_some() || self.video_dialog.picking.is_some() {
                    return;
                }
                let stem = self.stem.clone();
                let dir = self.out_dir.clone();
                let mut dlg = rfd::FileDialog::new().set_title(fl!("video-title")).add_filter("MP4", &["mp4"]).set_file_name(format!("{stem}.mp4"));
                if dir.is_dir() {
                    dlg = dlg.set_directory(&dir);
                }
                let (tx, rx) = mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(dlg.save_file().map(|p| if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp4")) { p } else { p.with_extension("mp4") }));
                });
                self.video_dialog.picking = Some(rx);
            }
            _ => {}
        }
    }

    /// 保存先を選んだら書き出しを始める
    pub(super) fn poll_video_dialog(&mut self) {
        let Some(rx) = &self.video_dialog.picking else { return };
        match rx.try_recv() {
            Ok(picked) => {
                self.video_dialog.picking = None;
                if let Some(p) = picked {
                    if let Err(e) = self.start_video(p) {
                        error!("video: {e}");
                        self.video_dialog.message = fl!("video-failed", error = e);
                        self.video_dialog.error = true;
                    }
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => self.video_dialog.picking = None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turntable_loops_and_settings() {
        let s = VideoSettings::default();
        assert_eq!(s.frames(), 360);
        assert!(s.bitrate() > 1_000_000);
        assert_eq!(Pattern::parse("swing"), Some(Pattern::Swing));
    }
}
