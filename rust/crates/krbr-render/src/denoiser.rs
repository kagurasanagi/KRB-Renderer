//! ノイズ除去（Intel Open Image Denoise 2.5 の RT フィルタ。HDR、補助情報としてアルベドと法線）。流れ：
//!   denoise_prep.comp（累積画像をサンプル数で割ってバッファへ）→ OIDN → denoise_blend.comp（元の画像と strength で混ぜて画像へ）
//! OIDN の学習済みモデルはリニア RGB を前提にするので、分光モード（累積が CIE XYZ）では前処理でリニア sRGB に変換し、
//! 混ぜるときに XYZ へ戻す（出力は累積と同じ色空間のまま。トーンマッピングと読み戻しの扱いは変わらない）
//! OIDN は Vulkan と同じ GPU の CUDA などのデバイスで動かし、バッファを Win32 ハンドルで共有する（CPU を経由しない）。
//! 共有できなければ CPU デバイスで、ホストから見えるバッファを渡す。OIDN の DLL（OpenImageDenoise.dll）は実行時に読む。
//! 同期：OIDN と Vulkan の間にセマフォは無い。呼び出し側は、前処理の完了を待ってから execute し、execute が終わってから
//! 結果を使うコマンドを投入する（どちらも submit_and_wait で区切る）

use std::ffi::{c_char, c_void, CString};
use std::sync::OnceLock;
use std::time::Instant;

use ash::vk;
use glam::Vec4;

use crate::integrator::RenderTargets;
use crate::kernel::compute_pipeline;
use crate::vk::{as_bytes, image_barrier, memory_barrier, Context, GpuPod, Image};

type OidnDevice = *mut c_void;
type OidnBuffer = *mut c_void;
type OidnFilter = *mut c_void;

const OIDN_DEVICE_TYPE_CPU: i32 = 1;
const OIDN_DEVICE_TYPE_CUDA: i32 = 3;
const OIDN_DEVICE_TYPE_HIP: i32 = 4;
const OIDN_FORMAT_FLOAT3: i32 = 3;
const OIDN_EXTERNAL_MEMORY_TYPE_FLAG_OPAQUE_WIN32: i32 = 1 << 2;
const OIDN_ERROR_NONE: i32 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenoiseQuality {
    Fast,
    Balanced,
    High,
}

impl DenoiseQuality {
    fn oidn(self) -> i32 {
        match self {
            DenoiseQuality::Fast => 4,
            DenoiseQuality::Balanced => 5,
            DenoiseQuality::High => 6,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            DenoiseQuality::Fast => "fast",
            DenoiseQuality::Balanced => "balanced",
            DenoiseQuality::High => "high",
        }
    }
}

/// OIDN の C API（使う関数だけ）
struct Oidn {
    _lib: libloading::Library,
    new_device: unsafe extern "C" fn(i32) -> OidnDevice,
    new_device_by_uuid: unsafe extern "C" fn(*const c_void) -> OidnDevice,
    commit_device: unsafe extern "C" fn(OidnDevice),
    get_device_int: unsafe extern "C" fn(OidnDevice, *const c_char) -> i32,
    get_device_error: unsafe extern "C" fn(OidnDevice, *mut *const c_char) -> i32,
    release_device: unsafe extern "C" fn(OidnDevice),
    new_shared_buffer: unsafe extern "C" fn(OidnDevice, *mut c_void, usize) -> OidnBuffer,
    new_shared_buffer_from_win32_handle: unsafe extern "C" fn(OidnDevice, i32, *mut c_void, *const c_void, usize) -> OidnBuffer,
    release_buffer: unsafe extern "C" fn(OidnBuffer),
    new_filter: unsafe extern "C" fn(OidnDevice, *const c_char) -> OidnFilter,
    set_filter_image: unsafe extern "C" fn(OidnFilter, *const c_char, OidnBuffer, i32, usize, usize, usize, usize, usize),
    set_filter_bool: unsafe extern "C" fn(OidnFilter, *const c_char, bool),
    set_filter_int: unsafe extern "C" fn(OidnFilter, *const c_char, i32),
    commit_filter: unsafe extern "C" fn(OidnFilter),
    execute_filter: unsafe extern "C" fn(OidnFilter),
    release_filter: unsafe extern "C" fn(OidnFilter),
}

// 安全性：関数ポインタはライブラリと同じ寿命で、ライブラリは一度読んだら解放しない（OIDN の API はスレッドから呼べる）
unsafe impl Send for Oidn {}
unsafe impl Sync for Oidn {}

fn load_oidn() -> Result<Oidn, String> {
    // 配布用の一式の dll\oidn、exe と同じフォルダ（ほかのアプリに読み込まれた krbr_renderer.dll なら、その DLL の横の
    // dll\oidn と同じフォルダ）、開発中は scripts\get_oidn.ps1 が置く公式のバイナリ（<repo>/third_party/oidn/bin）
    let name = "OpenImageDenoise.dll";
    let (exe, module) = (krbr_core::platform::exe_dir(), krbr_core::platform::module_dir());
    let candidates = [module.join("dll").join("oidn"), module, exe.join("dll").join("oidn"), exe];
    let mut path = candidates.iter().map(|d| d.join(name)).find(|p| p.exists()).unwrap_or_else(|| candidates[0].join(name));
    if !path.exists() {
        // LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR は完全なパスが要るので、.. を含めずに組み立てる
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(3).unwrap_or(std::path::Path::new("."));
        let dev = repo.join("third_party").join("oidn").join("bin").join(name);
        if dev.exists() {
            path = dev;
        }
    }
    if !path.exists() {
        return Err(format!("not found: {}", path.display()));
    }
    // 安全性：OIDN の公式のビルド済みの DLL。同じフォルダの OpenImageDenoise_core.dll などを、そのフォルダから探させる
    let lib = unsafe {
        #[cfg(windows)]
        {
            use libloading::os::windows::{Library, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR};
            Library::load_with_flags(&path, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_DEFAULT_DIRS).map(libloading::Library::from)
        }
        #[cfg(not(windows))]
        {
            libloading::Library::new(&path)
        }
    }
    .map_err(|e| format!("{}: {e}", path.display()))?;
    macro_rules! sym {
        ($name:literal) => {
            // 安全性：関数の型は oidn.h（2.5.1）の宣言どおり
            *unsafe { lib.get(concat!($name, "\0").as_bytes()) }.map_err(|e| format!("{}: {e}", $name))?
        };
    }
    Ok(Oidn {
        new_device: sym!("oidnNewDevice"),
        new_device_by_uuid: sym!("oidnNewDeviceByUUID"),
        commit_device: sym!("oidnCommitDevice"),
        get_device_int: sym!("oidnGetDeviceInt"),
        get_device_error: sym!("oidnGetDeviceError"),
        release_device: sym!("oidnReleaseDevice"),
        new_shared_buffer: sym!("oidnNewSharedBuffer"),
        new_shared_buffer_from_win32_handle: sym!("oidnNewSharedBufferFromWin32Handle"),
        release_buffer: sym!("oidnReleaseBuffer"),
        new_filter: sym!("oidnNewFilter"),
        set_filter_image: sym!("oidnSetFilterImage"),
        set_filter_bool: sym!("oidnSetFilterBool"),
        set_filter_int: sym!("oidnSetFilterInt"),
        commit_filter: sym!("oidnCommitFilter"),
        execute_filter: sym!("oidnExecuteFilter"),
        release_filter: sym!("oidnReleaseFilter"),
        _lib: lib,
    })
}

fn oidn() -> Result<&'static Oidn, String> {
    static OIDN: OnceLock<Result<Oidn, String>> = OnceLock::new();
    OIDN.get_or_init(load_oidn).as_ref().map_err(|e| e.clone())
}

/// OIDN の DLL を読めるか（ノイズ除去が使えるかの目安。デバイスが作れるかは new で分かる）
pub fn denoiser_available() -> bool {
    oidn().is_ok()
}

fn cstr(s: &str) -> CString {
    CString::new(s).unwrap()
}

/// OIDN の直前のエラー（無ければ Ok）
fn check(api: &Oidn, dev: OidnDevice, what: &str) -> Result<(), String> {
    let mut msg: *const c_char = std::ptr::null();
    // 安全性：dev は生きているデバイス。msg は OIDN が持つ文字列（次の呼び出しまで有効）
    let e = unsafe { (api.get_device_error)(dev, &mut msg) };
    if e == OIDN_ERROR_NONE {
        return Ok(());
    }
    let text = if msg.is_null() { "error".to_string() } else { unsafe { std::ffi::CStr::from_ptr(msg) }.to_string_lossy().into_owned() };
    Err(format!("OIDN {what}: {text}"))
}

#[cfg(windows)]
#[link(name = "kernel32")]
extern "system" {
    fn CloseHandle(h: *mut c_void) -> i32;
}

/// OIDN と共有するバッファ（GPU：外部メモリ、CPU：ホストから見えるバッファ）
#[derive(Default)]
struct SharedBuf {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    handle: Option<vk::HANDLE>,
    host: Option<crate::vk::Buffer>,
    oidn: Option<OidnBuffer>,
}

impl SharedBuf {
    fn vk_buffer(&self) -> vk::Buffer {
        self.host.as_ref().map(|h| h.buffer).unwrap_or(self.buffer)
    }
}

/// 計算シェーダ 1 本と、その記述子の組 1 つ（記述子は呼び出し側で書く）。積分器のカーネル（kernel::Kernel）とは別の、
/// 前処理・後処理用の小さなもの
struct ComputeKernel {
    set_layout: vk::DescriptorSetLayout,
    pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
}

impl ComputeKernel {
    const NULL: ComputeKernel = ComputeKernel { set_layout: vk::DescriptorSetLayout::null(), pool: vk::DescriptorPool::null(), set: vk::DescriptorSet::null(), layout: vk::PipelineLayout::null(), pipeline: vk::Pipeline::null() };

    /// 失敗したら Err（作りかけの資源は解放する）
    fn new(ctx: &Context, spv: &str, types: &[vk::DescriptorType], push_size: u32) -> Result<ComputeKernel, String> {
        let mut k = ComputeKernel::NULL;
        let r = k.create(ctx, spv, types, push_size);
        if r.is_err() {
            k.destroy(ctx);
        }
        r.map(|_| k)
    }

    fn create(&mut self, ctx: &Context, spv: &str, types: &[vk::DescriptorType], push_size: u32) -> Result<(), String> {
        let dev = &ctx.device;
        let lb: Vec<vk::DescriptorSetLayoutBinding> = types.iter().enumerate().map(|(i, t)| vk::DescriptorSetLayoutBinding::default().binding(i as u32).descriptor_type(*t).descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE)).collect();
        self.set_layout = unsafe { dev.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&lb), None) }.map_err(|e| format!("{e:?}"))?;
        let pcr = [vk::PushConstantRange { stage_flags: vk::ShaderStageFlags::COMPUTE, offset: 0, size: push_size }];
        let layouts = [self.set_layout];
        self.layout = unsafe { dev.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts).push_constant_ranges(&pcr), None) }.map_err(|e| format!("{e:?}"))?;
        self.pipeline = compute_pipeline(dev, self.layout, spv)?;
        let count = |t: vk::DescriptorType| types.iter().filter(|x| **x == t).count() as u32;
        let sizes: Vec<vk::DescriptorPoolSize> = [vk::DescriptorType::STORAGE_IMAGE, vk::DescriptorType::STORAGE_BUFFER].iter().filter(|t| count(**t) > 0).map(|t| vk::DescriptorPoolSize { ty: *t, descriptor_count: count(*t) }).collect();
        self.pool = unsafe { dev.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes), None) }.map_err(|e| format!("{e:?}"))?;
        self.set = unsafe { dev.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(self.pool).set_layouts(&layouts)) }.map_err(|e| format!("{e:?}"))?[0];
        Ok(())
    }

    fn write(&self, ctx: &Context, images: &[(u32, vk::ImageView)], buffers: &[(u32, vk::Buffer)]) {
        let ii: Vec<vk::DescriptorImageInfo> = images.iter().map(|(_, v)| vk::DescriptorImageInfo { sampler: vk::Sampler::null(), image_view: *v, image_layout: vk::ImageLayout::GENERAL }).collect();
        let bi: Vec<vk::DescriptorBufferInfo> = buffers.iter().map(|(_, b)| vk::DescriptorBufferInfo { buffer: *b, offset: 0, range: vk::WHOLE_SIZE }).collect();
        let mut w: Vec<vk::WriteDescriptorSet> = Vec::new();
        for (k, (binding, _)) in images.iter().enumerate() {
            w.push(vk::WriteDescriptorSet::default().dst_set(self.set).dst_binding(*binding).descriptor_type(vk::DescriptorType::STORAGE_IMAGE).image_info(std::slice::from_ref(&ii[k])));
        }
        for (k, (binding, _)) in buffers.iter().enumerate() {
            w.push(vk::WriteDescriptorSet::default().dst_set(self.set).dst_binding(*binding).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(std::slice::from_ref(&bi[k])));
        }
        unsafe { ctx.device.update_descriptor_sets(&w, &[]) };
    }

    fn dispatch<P: GpuPod>(&self, ctx: &Context, cmd: vk::CommandBuffer, push: &P, w: u32, h: u32) {
        let dev = &ctx.device;
        unsafe {
            dev.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, self.pipeline);
            dev.cmd_bind_descriptor_sets(cmd, vk::PipelineBindPoint::COMPUTE, self.layout, 0, &[self.set], &[]);
            dev.cmd_push_constants(cmd, self.layout, vk::ShaderStageFlags::COMPUTE, 0, as_bytes(std::slice::from_ref(push)));
            dev.cmd_dispatch(cmd, w.div_ceil(8), h.div_ceil(8), 1);
        }
    }

    /// 破棄する（作りかけで一部が null でもよい。vkDestroy* は null を何もせずに通す）
    fn destroy(&mut self, ctx: &Context) {
        let dev = &ctx.device;
        unsafe {
            dev.destroy_pipeline(self.pipeline, None);
            dev.destroy_pipeline_layout(self.layout, None);
            dev.destroy_descriptor_pool(self.pool, None);
            dev.destroy_descriptor_set_layout(self.set_layout, None);
        }
        *self = ComputeKernel::NULL;
    }
}

/// XYZ ⇄ リニア sRGB の行列の列（w = 1 なら変換する、0 ならそのまま）。shaders/postfx.glsl の applyColorMatrix が読む
fn color_matrix(m: glam::Mat3, on: bool) -> [Vec4; 3] {
    let w = if on { 1.0 } else { 0.0 };
    [m.col(0).extend(w), m.col(1).extend(w), m.col(2).extend(w)]
}

/// denoise_prep.comp のプッシュ定数
#[repr(C)]
#[derive(Clone, Copy)]
struct PrepPush {
    width: u32,
    height: u32,
    _pad: [u32; 2],
    /// 累積が XYZ（分光モード）なら、OIDN に渡す前にリニア sRGB へ変換する
    xyz_to_rgb: [Vec4; 3],
}

/// denoise_blend.comp のプッシュ定数
#[repr(C)]
#[derive(Clone, Copy)]
struct BlendPush {
    width: u32,
    height: u32,
    strength: f32,
    _pad: u32,
    /// 累積が XYZ（分光モード）なら、OIDN の結果（リニア sRGB）を XYZ へ戻してから元の画像と混ぜる
    rgb_to_xyz: [Vec4; 3],
}

const _: () = assert!(std::mem::size_of::<PrepPush>() == 64);
const _: () = assert!(std::mem::size_of::<BlendPush>() == 64);
// 安全性：repr(C) で、上の assert により隙間が無い
unsafe impl GpuPod for PrepPush {}
unsafe impl GpuPod for BlendPush {}

pub struct Denoiser<'a> {
    ctx: &'a Context,
    api: &'static Oidn,
    width: u32,
    height: u32,
    /// GPU と外部メモリで共有している（false なら CPU デバイス）
    shared: bool,
    quality: DenoiseQuality,
    name: String,
    device: OidnDevice,
    filter: OidnFilter,
    /// color, albedo, normal, output
    bufs: [SharedBuf; 4],
    out_image: Image,
    prep: ComputeKernel,
    blend: ComputeKernel,
}

impl<'a> Denoiser<'a> {
    /// 累積の targets（大きさが変わったら作り直す。同じ大きさで作り直されたら rebind）に対して用意する
    pub fn new(ctx: &'a Context, targets: &RenderTargets, quality: DenoiseQuality) -> Result<Self, String> {
        let api = oidn()?;
        let (width, height) = (targets.width, targets.height);
        let (device, shared, name) = create_device(ctx, api)?;
        let mut d = Denoiser {
            ctx,
            api,
            width,
            height,
            shared,
            quality,
            name,
            device,
            filter: std::ptr::null_mut(),
            bufs: Default::default(),
            out_image: Image::default(),
            prep: ComputeKernel::NULL,
            blend: ComputeKernel::NULL,
        };
        d.setup(targets)?;
        krbr_core::info!("denoiser: OIDN on {} ({}), {}x{}, quality {}", d.name, if d.shared { "shared memory" } else { "host memory" }, width, height, quality.name());
        Ok(d)
    }

    fn setup(&mut self, targets: &RenderTargets) -> Result<(), String> {
        let size = self.width as u64 * self.height as u64 * 16;
        for i in 0..4 {
            self.bufs[i] = if self.shared { self.create_shared(size)? } else { self.create_host(size)? };
        }
        let api = self.api;
        let filter = unsafe { (api.new_filter)(self.device, cstr("RT").as_ptr()) };
        if filter.is_null() {
            check(api, self.device, "filter")?;
            return Err("OIDN: cannot create the RT filter".into());
        }
        self.filter = filter;
        let row = self.width as usize * 16;
        for (i, name) in ["color", "albedo", "normal", "output"].iter().enumerate() {
            // 安全性：バッファは w*h*16 バイト（vec4 の float）。OIDN は RGB の 3 つを読み書きする
            unsafe { (api.set_filter_image)(filter, cstr(name).as_ptr(), self.bufs[i].oidn.unwrap(), OIDN_FORMAT_FLOAT3, self.width as usize, self.height as usize, 0, 16, row) };
        }
        unsafe {
            (api.set_filter_bool)(filter, cstr("hdr").as_ptr(), true);
            (api.set_filter_int)(filter, cstr("quality").as_ptr(), self.quality.oidn());
            (api.commit_filter)(filter);
        }
        check(api, self.device, "filter")?;

        let ctx = self.ctx;
        self.out_image = ctx.create_image_2d(self.width, self.height, 1, vk::Format::R32G32B32A32_SFLOAT, vk::ImageUsageFlags::STORAGE | vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST)?;
        let img = self.out_image.image;
        ctx.submit_and_wait(|dev, cmd| {
            image_barrier(dev, cmd, img, vk::ImageLayout::UNDEFINED, vk::ImageLayout::GENERAL, vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE, vk::PipelineStageFlags2::COMPUTE_SHADER, vk::AccessFlags2::SHADER_STORAGE_WRITE, 1);
        })?;
        let (si, sb) = (vk::DescriptorType::STORAGE_IMAGE, vk::DescriptorType::STORAGE_BUFFER);
        self.prep = ComputeKernel::new(ctx, "denoise_prep.comp.spv", &[si, si, si, sb, sb, sb, sb], std::mem::size_of::<PrepPush>() as u32)?;
        self.blend = ComputeKernel::new(ctx, "denoise_blend.comp.spv", &[si, sb, si], std::mem::size_of::<BlendPush>() as u32)?;
        self.rebind(targets);
        Ok(())
    }

    /// 累積の targets を作り直した（同じ大きさ）ときに、参照し直す
    pub fn rebind(&self, targets: &RenderTargets) {
        let b = |i: usize| self.bufs[i].vk_buffer();
        self.prep.write(self.ctx, &[(0, targets.color.view), (1, targets.albedo.view), (2, targets.normal.view)], &[(3, b(0)), (4, b(1)), (5, b(2)), (6, b(3))]);
        self.blend.write(self.ctx, &[(0, targets.color.view), (2, self.out_image.view)], &[(1, b(3))]);
    }


    /// "CUDA" / "HIP" / "GPU" / "CPU"
    pub fn device_name(&self) -> &str {
        &self.name
    }

    pub fn quality(&self) -> DenoiseQuality {
        self.quality
    }

    /// 品質を変える。OIDN が受け付けなければ知らせて、元の品質のまま使い続ける（quality() は実際の品質を返す）
    pub fn set_quality(&mut self, q: DenoiseQuality) {
        if q == self.quality {
            return;
        }
        let (api, filter) = (self.api, self.filter);
        let apply = |q: DenoiseQuality| unsafe {
            (api.set_filter_int)(filter, cstr("quality").as_ptr(), q.oidn());
            (api.commit_filter)(filter);
        };
        apply(q);
        match check(api, self.device, "quality") {
            Ok(()) => self.quality = q,
            Err(e) => {
                krbr_core::warn!("{e}; keeping quality {}", self.quality.name());
                apply(self.quality);
                if let Err(e) = check(api, self.device, "quality") {
                    krbr_core::warn!("{e}");
                }
            }
        }
    }

    /// 累積画像から OIDN の入力を作るコマンドを記録する（この後、完了を待ってから execute）。
    /// xyz_input: 累積が CIE XYZ（分光モード。Progressive::xyz_output）なら true、リニア sRGB なら false。
    /// OIDN は RGB を前提にしているので、XYZ はここでリニア sRGB に直して渡す
    pub fn record_prepare(&self, cmd: vk::CommandBuffer, xyz_input: bool) {
        let push = PrepPush { width: self.width, height: self.height, _pad: [0; 2], xyz_to_rgb: color_matrix(krbr_pbr::spectrum::xyz_to_linear_srgb(), xyz_input) };
        self.prep.dispatch(self.ctx, cmd, &push, self.width, self.height);
        // 書き込みを、完了の待機後に OIDN（外部・CPU）から読めるようにする
        memory_barrier(&self.ctx.device, cmd, vk::PipelineStageFlags2::COMPUTE_SHADER, vk::AccessFlags2::SHADER_STORAGE_WRITE, vk::PipelineStageFlags2::HOST, vk::AccessFlags2::HOST_READ);
    }

    /// ノイズ除去を実行する（終わるまで待つ）。戻り値はかかった時間（ms）
    pub fn execute(&self) -> Result<f64, String> {
        let t0 = Instant::now();
        // 安全性：フィルタは生きていて、入出力のバッファは前処理の完了後に読まれる
        unsafe { (self.api.execute_filter)(self.filter) };
        check(self.api, self.device, "execute")?;
        Ok(t0.elapsed().as_secs_f64() * 1000.0)
    }

    /// 結果を、元の画像と strength（0..1、1 で OIDN の結果そのまま）で混ぜて output() の画像へ書くコマンドを記録する。
    /// xyz_input は record_prepare と同じ値を渡す。出力は累積と同じ色空間
    pub fn record_output(&self, cmd: vk::CommandBuffer, strength: f32, xyz_input: bool) {
        let dev = &self.ctx.device;
        memory_barrier(dev, cmd, vk::PipelineStageFlags2::HOST, vk::AccessFlags2::HOST_WRITE, vk::PipelineStageFlags2::COMPUTE_SHADER, vk::AccessFlags2::SHADER_STORAGE_READ);
        let push = BlendPush { width: self.width, height: self.height, strength, _pad: 0, rgb_to_xyz: color_matrix(krbr_pbr::spectrum::linear_srgb_to_xyz(), xyz_input) };
        self.blend.dispatch(self.ctx, cmd, &push, self.width, self.height);
        memory_barrier(dev, cmd, vk::PipelineStageFlags2::COMPUTE_SHADER, vk::AccessFlags2::SHADER_STORAGE_WRITE, vk::PipelineStageFlags2::COMPUTE_SHADER | vk::PipelineStageFlags2::COPY, vk::AccessFlags2::SHADER_STORAGE_READ | vk::AccessFlags2::TRANSFER_READ);
    }

    /// ノイズ除去した画像（RGBA32F、A = 1、GENERAL レイアウト）。トーンマッピングの色の入力にする
    pub fn output(&self) -> &Image {
        &self.out_image
    }

    /// output() の画像を読み戻す（GPU が待機中に呼ぶこと。分光モードなら XYZ のまま）。失敗したら Err
    pub fn read_output(&self) -> Result<crate::image_io::ImageF, String> {
        let (w, h) = (self.width, self.height);
        let bytes = w as u64 * h as u64 * 16;
        let mut rb = self.ctx.create_buffer(bytes, vk::BufferUsageFlags::TRANSFER_DST, true, true)?;
        let (img, rbb) = (self.out_image.image, rb.buffer);
        let r = self.ctx.submit_and_wait(|dev, cmd| {
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: 1 })
                .image_extent(vk::Extent3D { width: w, height: h, depth: 1 });
            unsafe { dev.cmd_copy_image_to_buffer(cmd, img, vk::ImageLayout::GENERAL, rbb, &[region]) };
            memory_barrier(dev, cmd, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE, vk::PipelineStageFlags2::HOST, vk::AccessFlags2::HOST_READ);
        });
        let out = r.map(|()| {
            let src = rb.mapped_bytes().expect("the readback buffer is host-visible");
            let mut out = crate::image_io::ImageF::new(w, h);
            for (d, s) in out.rgba.iter_mut().zip(src.chunks_exact(4)) {
                *d = f32::from_le_bytes([s[0], s[1], s[2], s[3]]);
            }
            out
        });
        self.ctx.destroy_buffer(&mut rb);
        out
    }

    fn create_shared(&self, size: u64) -> Result<SharedBuf, String> {
        let ctx = self.ctx;
        let dev = &ctx.device;
        let ext_mem = ctx.external_memory_win32.as_ref().ok_or("no external memory")?;
        let mut b = SharedBuf::default();
        let mut ext = vk::ExternalMemoryBufferCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32);
        let families = ctx.shared_families();
        let mut bci = vk::BufferCreateInfo::default().size(size).usage(vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST).push_next(&mut ext);
        if !families.is_empty() {
            bci = bci.sharing_mode(vk::SharingMode::CONCURRENT).queue_family_indices(&families);
        }
        b.buffer = unsafe { dev.create_buffer(&bci, None) }.map_err(|e| format!("vkCreateBuffer: {e:?}"))?;
        let req = unsafe { dev.get_buffer_memory_requirements(b.buffer) };
        let mp = unsafe { ctx.instance.get_physical_device_memory_properties(ctx.physical) };
        let ty = (0..mp.memory_type_count).find(|&i| req.memory_type_bits & (1 << i) != 0 && mp.memory_types[i as usize].property_flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)).ok_or("no device-local memory type")?;
        let mut ded = vk::MemoryDedicatedAllocateInfo::default().buffer(b.buffer);
        let mut exp = vk::ExportMemoryAllocateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32);
        let mai = vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty).push_next(&mut exp).push_next(&mut ded);
        let result = (|| -> Result<(), String> {
            b.memory = unsafe { dev.allocate_memory(&mai, None) }.map_err(|e| format!("vkAllocateMemory: {e:?}"))?;
            unsafe { dev.bind_buffer_memory(b.buffer, b.memory, 0) }.map_err(|e| format!("vkBindBufferMemory: {e:?}"))?;
            let gi = vk::MemoryGetWin32HandleInfoKHR::default().memory(b.memory).handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_WIN32);
            let h = unsafe { ext_mem.get_memory_win32_handle(&gi) }.map_err(|e| format!("vkGetMemoryWin32HandleKHR: {e:?}"))?;
            b.handle = Some(h);
            // 安全性：h は今作った外部メモリのハンドル。OIDN はその内容を読み書きするだけで、ハンドルは閉じない
            let ob = unsafe { (self.api.new_shared_buffer_from_win32_handle)(self.device, OIDN_EXTERNAL_MEMORY_TYPE_FLAG_OPAQUE_WIN32, h as *mut c_void, std::ptr::null(), req.size as usize) };
            check(self.api, self.device, "shared buffer")?;
            if ob.is_null() {
                return Err("OIDN: cannot import the shared buffer".into());
            }
            b.oidn = Some(ob);
            Ok(())
        })();
        if let Err(e) = result {
            self.release(&mut b);
            return Err(e);
        }
        Ok(b)
    }

    fn create_host(&self, size: u64) -> Result<SharedBuf, String> {
        let mut b = SharedBuf::default();
        let mut host = self.ctx.create_buffer(size, vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST, true, true)?;
        let ptr = host.mapped_bytes_mut().map(|m| m.as_mut_ptr() as *mut c_void);
        b.host = Some(host);
        let Some(ptr) = ptr else {
            self.release(&mut b);
            return Err("host buffer is not mapped".into());
        };
        // 安全性：ptr はホストから見えるバッファ（GPU と一貫性のあるメモリ）の size バイトで、b と同じ寿命
        let ob = unsafe { (self.api.new_shared_buffer)(self.device, ptr, size as usize) };
        // create_shared と同じく、エラーと null の両方を確かめる
        let r = check(self.api, self.device, "host buffer").and_then(|()| if ob.is_null() { Err("OIDN: cannot wrap the host buffer".to_string()) } else { Ok(()) });
        if let Err(e) = r {
            self.release(&mut b);
            return Err(e);
        }
        b.oidn = Some(ob);
        Ok(b)
    }

    fn release(&self, b: &mut SharedBuf) {
        let dev = &self.ctx.device;
        if let Some(ob) = b.oidn.take() {
            unsafe { (self.api.release_buffer)(ob) };
        }
        #[cfg(windows)]
        if let Some(h) = b.handle.take() {
            unsafe { CloseHandle(h as *mut c_void) };
        }
        if b.buffer != vk::Buffer::null() {
            unsafe { dev.destroy_buffer(b.buffer, None) };
            b.buffer = vk::Buffer::null();
        }
        if b.memory != vk::DeviceMemory::null() {
            unsafe { dev.free_memory(b.memory, None) };
            b.memory = vk::DeviceMemory::null();
        }
        if let Some(mut h) = b.host.take() {
            self.ctx.destroy_buffer(&mut h);
        }
    }
}

impl Drop for Denoiser<'_> {
    fn drop(&mut self) {
        let ctx = self.ctx;
        ctx.wait_idle();
        if !self.filter.is_null() {
            unsafe { (self.api.release_filter)(self.filter) };
        }
        let mut bufs = std::mem::take(&mut self.bufs);
        for b in &mut bufs {
            self.release(b);
        }
        unsafe { (self.api.release_device)(self.device) };
        self.prep.destroy(ctx);
        self.blend.destroy(ctx);
        let mut img = std::mem::take(&mut self.out_image);
        ctx.destroy_image(&mut img);
    }
}

/// Vulkan と同じ GPU の OIDN デバイス（外部メモリを共有できるもの）。できなければ CPU デバイス
fn create_device(ctx: &Context, api: &Oidn) -> Result<(OidnDevice, bool, String), String> {
    // 開発用：KRBR_DENOISE_DEVICE=cpu で CPU デバイスにする（GPU で共有できない PC と同じ道を確かめる）
    let force_cpu = std::env::var("KRBR_DENOISE_DEVICE").is_ok_and(|v| v.eq_ignore_ascii_case("cpu"));
    if ctx.external_memory_win32.is_some() && !force_cpu {
        // 安全性：UUID は 16 バイト（VkPhysicalDeviceIDProperties::deviceUUID）
        let dev = unsafe { (api.new_device_by_uuid)(ctx.info.device_uuid.as_ptr() as *const c_void) };
        if !dev.is_null() {
            unsafe { (api.commit_device)(dev) };
            let types = unsafe { (api.get_device_int)(dev, cstr("externalMemoryTypes").as_ptr()) };
            if check(api, dev, "GPU device").is_ok() && types & OIDN_EXTERNAL_MEMORY_TYPE_FLAG_OPAQUE_WIN32 != 0 {
                let t = unsafe { (api.get_device_int)(dev, cstr("type").as_ptr()) };
                let name = match t {
                    OIDN_DEVICE_TYPE_CUDA => "CUDA",
                    OIDN_DEVICE_TYPE_HIP => "HIP",
                    _ => "GPU",
                };
                return Ok((dev, true, name.into()));
            }
            unsafe { (api.release_device)(dev) };
        }
    }
    let dev = unsafe { (api.new_device)(OIDN_DEVICE_TYPE_CPU) };
    if dev.is_null() {
        return Err("OIDN: no device".into());
    }
    unsafe { (api.commit_device)(dev) };
    if let Err(e) = check(api, dev, "CPU device") {
        unsafe { (api.release_device)(dev) };
        return Err(e);
    }
    Ok((dev, false, "CPU".into()))
}
