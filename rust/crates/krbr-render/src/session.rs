//! ほかのアプリから描画だけを使うための窓口（Renderer）。Vulkan の初期化、シーンと環境マップの転送、積分器、累積、
//! 読み戻し、ノイズ除去を 1 つにまとめる。C ABI（krbr-capi）もこれを包むだけ。
//!
//! 座標の単位は mm、Y が上の右手系。環境マップは正距円筒（画像の上端が +Y、横の中央が -Z の向き）

use krbr_pbr::{IntegratorKind, Material, SceneSource};

use crate::denoiser::DenoiseQuality;
use crate::env_map::{build_env_distribution, EnvDistribution};
use crate::gpu_scene::GpuScene;
use crate::image_io::ImageF;
use crate::integrator::{AovImages, FrameParams, Progressive};
use crate::renderer::denoise;
use crate::vk::{Context, ContextOptions, DeviceInfo};

/// 環境マップの重点的サンプリングの表の横の画素数の上限（ビューアと同じ）
const ENV_DIST_RESOLUTION: u32 = 2048;

#[derive(Debug, Clone, Default)]
pub struct RendererOptions {
    /// Vulkan の検証レイヤーを使う（開発用。遅い）
    pub validation: bool,
}

/// 描画の窓口。使い方：new → set_scene → set_frame（カメラなど）→ render_samples を繰り返す → readback / denoise
pub struct Renderer {
    // フィールドは宣言の順に捨てられる。prog は gpu を、gpu は ctx を参照するので、この順に並べること
    prog: Option<Progressive<'static>>,
    gpu: Box<GpuScene<'static>>,
    ctx: Box<Context>,
    env: ImageF,
    env_dist: EnvDistribution,
    /// GPU にシーンがあるか（GpuScene::build を済ませた。2 回目からは rebuild_geometry）
    built: bool,
    integrator: IntegratorKind,
    width: u32,
    height: u32,
    frame: FrameParams,
    bump_intensity: f32,
}

impl Renderer {
    /// Vulkan のデバイスを作る（ray tracing のできる GPU を自動で選ぶ）。環境マップは内蔵のスタジオ
    pub fn new(opts: &RendererOptions) -> Result<Self, String> {
        let ctx = Box::new(Context::new(&ContextOptions { validation: opts.validation, low_queue_priority: true, ..Default::default() })?);
        // 安全性：Context は Box の中でムーブされず、それを参照する gpu と prog は ctx より先に捨てる（フィールドの順）
        let ctx_ref: &'static Context = unsafe { &*(&*ctx as *const Context) };
        let gpu = Box::new(GpuScene::new(ctx_ref));
        let env = crate::studio_env::studio_environment(ENV_DIST_RESOLUTION);
        let env_dist = build_env_distribution(&env, ENV_DIST_RESOLUTION);
        let (width, height) = (640, 480);
        let mut frame = FrameParams::default();
        frame.camera.aspect = width as f32 / height as f32;
        Ok(Self { prog: None, gpu, ctx, env, env_dist, built: false, integrator: IntegratorKind::Pt, width, height, frame, bump_intensity: 1.0 })
    }

    pub fn device_info(&self) -> &DeviceInfo {
        &self.ctx.info
    }

    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// シーン（形状・材質・配置）を GPU へ送る。前のシーンは捨て、累積は 0 から。
    /// 送るのに失敗したら（VRAM が足りないなど）、シーンは無い状態に戻る
    pub fn set_scene(&mut self, scene: &dyn SceneSource) -> Result<(), String> {
        let sent = if self.built { self.gpu.rebuild_geometry(scene, self.bump_intensity) } else { self.gpu.build(scene, &self.env, &self.env_dist, self.bump_intensity) };
        if let Err(e) = sent {
            self.discard_gpu();
            return Err(e);
        }
        self.built = true;
        self.rebuild_integrator()
    }

    /// 材質だけを送り直す（数と発光は前のシーンと同じであること。違えば Err で、set_scene で送り直す）。累積は 0 から
    pub fn update_materials(&mut self, materials: &[Material]) -> Result<(), String> {
        self.require_scene()?;
        self.gpu.update_materials(materials, self.bump_intensity)?;
        self.reset();
        Ok(())
    }

    /// 凹凸（材質の bump_strength）に掛ける強さ。次の set_scene / update_materials から効く
    pub fn set_bump_intensity(&mut self, v: f32) {
        self.bump_intensity = v;
    }

    /// 描くインスタンスの表示マスク（RenderInstance::mask と重なるものだけを描く）。累積は 0 から
    pub fn set_visible_mask(&mut self, mask: u8) -> Result<(), String> {
        self.require_scene()?;
        self.gpu.set_visible_mask(mask)?;
        self.reset();
        Ok(())
    }

    /// 環境マップ（リニアの RGBA、正距円筒）を差し替える。累積は 0 から
    pub fn set_environment(&mut self, env: ImageF) -> Result<(), String> {
        if env.width == 0 || env.height == 0 || env.rgba.len() != env.width as usize * env.height as usize * 4 {
            return Err(format!("bad environment image ({}x{}, {} floats)", env.width, env.height, env.rgba.len()));
        }
        self.env_dist = build_env_distribution(&env, ENV_DIST_RESOLUTION);
        self.env = env;
        if self.built {
            if let Err(e) = self.gpu.set_environment(&self.env, &self.env_dist) {
                self.discard_gpu();
                return Err(format!("{e} (the scene was discarded; send it again with set_scene)"));
            }
            self.rebuild_integrator()?;
        }
        Ok(())
    }

    /// 一様な色の環境光
    pub fn set_environment_color(&mut self, rgb: [f32; 3]) -> Result<(), String> {
        let mut env = ImageF::new(64, 32);
        for p in env.rgba.chunks_exact_mut(4) {
            p.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 1.0]);
        }
        self.set_environment(env)
    }

    /// カメラ・レンズ・深さ・分光などのフレームの設定。累積は 0 から
    pub fn set_frame(&mut self, frame: FrameParams) {
        if let Some(p) = &mut self.prog {
            p.set_frame(frame.clone());
        }
        self.frame = frame;
    }

    pub fn frame(&self) -> &FrameParams {
        &self.frame
    }

    /// 積分器を替える。作れなければ Err（その GPU で使えない積分器。別の種類で呼び直せる）
    pub fn set_integrator(&mut self, kind: IntegratorKind) -> Result<(), String> {
        self.integrator = kind;
        match &mut self.prog {
            Some(p) => p.set_integrator(kind),
            None => Ok(()),
        }
    }

    pub fn integrator(&self) -> IntegratorKind {
        self.integrator
    }

    /// 出力の画素数。カメラの縦横比（frame.camera.aspect）も合わせる。累積は 0 から
    pub fn set_resolution(&mut self, width: u32, height: u32) -> Result<(), String> {
        if width == 0 || height == 0 {
            return Err(format!("bad resolution {width}x{height}"));
        }
        self.width = width;
        self.height = height;
        let mut frame = self.frame.clone();
        frame.camera.aspect = width as f32 / height as f32;
        self.set_frame(frame);
        match &mut self.prog {
            Some(p) => p.resize(width, height),
            None => Ok(()),
        }
    }

    pub fn resolution(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// 累積を 0 に戻す
    pub fn reset(&mut self) {
        if let Some(p) = &mut self.prog {
            p.reset();
        }
    }

    /// これまでに積んだサンプル数
    pub fn samples(&self) -> u32 {
        self.prog.as_ref().map_or(0, |p| p.samples())
    }

    /// count サンプルを積み増して、完了を待つ。1 サンプルずつ投入するので、GPU を長く占有しない
    /// （中断できるように、呼ぶ側は少しずつ呼んで間に readback や中断の確認をはさめる）。積んだ後のサンプル数を返す
    pub fn render_samples(&mut self, count: u32) -> Result<u32, String> {
        self.require_scene()?;
        if self.prog.is_none() {
            // 安全性：GpuScene は Box の中でムーブされず、prog は gpu より先に捨てる（フィールドの順）
            let gpu: &'static GpuScene<'static> = unsafe { &*(&*self.gpu as *const GpuScene<'static>) };
            let ctx: &'static Context = gpu.context();
            self.prog = Some(Progressive::new(ctx, gpu, self.integrator, self.width, self.height, self.frame.clone())?);
        }
        let prog = self.prog.as_mut().expect("created above");
        for _ in 0..count {
            self.ctx.submit_and_wait(|_, cmd| prog.record(cmd, 1))?;
        }
        Ok(prog.samples())
    }

    /// 今の累積を読み戻す（色とアルベドはリニア sRGB。背景を透明にしていれば色の A にアルファ）
    pub fn readback(&self) -> Result<AovImages, String> {
        self.progressive()?.readback()
    }

    /// 今の累積をノイズ除去（OIDN）した画像（リニア sRGB）。strength は元の画像と混ぜる割合（0..1）。
    /// OIDN の DLL が無いなどで使えなければ Err
    pub fn denoise(&self, quality: DenoiseQuality, strength: f32) -> Result<ImageF, String> {
        let prog = self.progressive()?;
        let aov = prog.readback()?;
        denoise(&self.ctx, prog, &aov.color, quality, strength)
    }

    fn progressive(&self) -> Result<&Progressive<'static>, String> {
        self.prog.as_ref().ok_or_else(|| "nothing rendered yet (call render_samples first)".to_string())
    }

    fn require_scene(&self) -> Result<(), String> {
        if self.built {
            Ok(())
        } else {
            Err("no scene (call set_scene first)".into())
        }
    }

    /// GPU の資源を作りかけで失敗したとき：積分器と GpuScene を捨てて新しくする（作りかけの資源を描かないように。
    /// 次の set_scene で全部を送り直す）
    fn discard_gpu(&mut self) {
        self.prog = None;
        let ctx: &'static Context = self.gpu.context();
        *self.gpu = GpuScene::new(ctx);
        self.built = false;
    }

    /// GPU の資源（形状・環境マップ）を差し替えた後に、積分器を作り直す
    fn rebuild_integrator(&mut self) -> Result<(), String> {
        match &mut self.prog {
            Some(p) => p.rebuild_integrator(),
            None => Ok(()),
        }
    }
}
