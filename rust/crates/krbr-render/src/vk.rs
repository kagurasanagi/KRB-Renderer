//! ヘッドレス Vulkan コンテキスト（ash）。サーフェス／スワップチェーン系の拡張は有効にしない。
//! 交差判定は ray query（計算シェーダ）を優先し、無ければ ray tracing pipeline（GTX 10 系など）

use std::ffi::{c_char, CStr, CString};
use std::sync::{Arc, Mutex};

use ash::khr;
use ash::vk;
use gpu_allocator::vulkan::{Allocation, AllocationCreateDesc, AllocationScheme, Allocator, AllocatorCreateDesc};
use gpu_allocator::MemoryLocation;

#[derive(Debug, Clone, Default)]
pub struct DeviceInfo {
    pub name: String,
    pub api_version: u32,
    pub driver_version: u32,
    pub discrete: bool,
    pub ray_query: bool,
    pub acceleration_structure: bool,
    pub ray_tracing_pipeline: bool,
    pub global_priority: bool,
    pub device_local_bytes: u64,
    pub atomic_float_add: bool,
    pub device_uuid: [u8; 16],
}

/// サーフェスを作る関数（ウィンドウ表示のときだけ）。インスタンスができた後に呼ばれる
pub type SurfaceFactory = Box<dyn Fn(&ash::Entry, &ash::Instance) -> Result<vk::SurfaceKHR, String>>;

#[derive(Default)]
pub struct ContextOptions {
    pub validation: bool,
    /// 検証レイヤーの同期の検証（コマンドの間のバリアの抜け）も有効にする（validation のときだけ。遅い。テスト用）
    pub sync_validation: bool,
    /// VK_KHR_global_priority が使えれば LOW で作る。画面表示するときは、計算専用のファミリーがあれば、サンプルを積む
    /// キュー（render_queue）だけを LOW にして、表示のキュー（queue）は普通の優先度にする（重い描画の間も画面が滑らかに動く）
    pub low_queue_priority: bool,
    /// 画面表示する場合だけ指定する（指定しなければサーフェス・スワップチェーン系は一切有効にしない）
    pub instance_extensions: Vec<*const c_char>,
    pub create_surface: Option<SurfaceFactory>,
}

#[derive(Debug, Default)]
pub struct Buffer {
    pub buffer: vk::Buffer,
    pub allocation: Option<Allocation>,
    pub size: u64,
    pub address: u64,
}

impl Buffer {
    /// 割り当てたメモリ全体（アロケータの都合で、作るときに求めた大きさ size より長いことがある）
    fn mapped(&mut self) -> Option<&mut [u8]> {
        self.allocation.as_mut().and_then(|a| a.mapped_slice_mut())
    }
    /// 割り当てたメモリ全体（mapped と同じく size より長いことがある）。読み戻しには mapped_bytes を使う
    fn mapped_ref(&self) -> Option<&[u8]> {
        self.allocation.as_ref().and_then(|a| a.mapped_slice())
    }
    /// CPU から見えるメモリのうち、作るときに求めた大きさ（size バイト）だけ。読み戻した内容はこれで見る
    pub fn mapped_bytes(&self) -> Option<&[u8]> {
        let n = self.size as usize;
        self.mapped_ref().and_then(|m| m.get(..n))
    }
    /// mapped_bytes の書き込み用
    pub fn mapped_bytes_mut(&mut self) -> Option<&mut [u8]> {
        let n = self.size as usize;
        self.mapped().and_then(|m| m.get_mut(..n))
    }
}

#[derive(Debug, Default)]
pub struct Image {
    pub image: vk::Image,
    pub allocation: Option<Allocation>,
    pub view: vk::ImageView,
    pub format: vk::Format,
    pub width: u32,
    pub height: u32,
    pub layers: u32,
}

fn has_ext(exts: &[vk::ExtensionProperties], name: &CStr) -> bool {
    exts.iter().any(|e| e.extension_name_as_c_str().map(|n| n == name).unwrap_or(false))
}

fn query_info(instance: &ash::Instance, pd: vk::PhysicalDevice) -> DeviceInfo {
    // 安全性：有効なインスタンスと物理デバイス
    unsafe {
        let props = instance.get_physical_device_properties(pd);
        let exts = instance.enumerate_device_extension_properties(pd).unwrap_or_default();
        let mut rq = vk::PhysicalDeviceRayQueryFeaturesKHR::default();
        let mut af = vk::PhysicalDeviceShaderAtomicFloatFeaturesEXT::default();
        let mut rtp = vk::PhysicalDeviceRayTracingPipelineFeaturesKHR::default();
        let mut asf = vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default();
        let mut f2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut asf).push_next(&mut rq).push_next(&mut af).push_next(&mut rtp);
        instance.get_physical_device_features2(pd, &mut f2);
        let mut idp = vk::PhysicalDeviceIDProperties::default();
        let mut p2 = vk::PhysicalDeviceProperties2::default().push_next(&mut idp);
        instance.get_physical_device_properties2(pd, &mut p2);
        let mem = instance.get_physical_device_memory_properties(pd);
        let mut local = 0u64;
        for h in &mem.memory_heaps[..mem.memory_heap_count as usize] {
            if h.flags.contains(vk::MemoryHeapFlags::DEVICE_LOCAL) {
                local = local.max(h.size);
            }
        }
        DeviceInfo {
            name: props.device_name_as_c_str().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            api_version: props.api_version,
            driver_version: props.driver_version,
            discrete: props.device_type == vk::PhysicalDeviceType::DISCRETE_GPU,
            ray_query: has_ext(&exts, khr::ray_query::NAME) && rq.ray_query == vk::TRUE,
            acceleration_structure: has_ext(&exts, khr::acceleration_structure::NAME) && asf.acceleration_structure == vk::TRUE,
            ray_tracing_pipeline: has_ext(&exts, khr::ray_tracing_pipeline::NAME) && rtp.ray_tracing_pipeline == vk::TRUE,
            global_priority: has_ext(&exts, khr::global_priority::NAME) || has_ext(&exts, ash::ext::global_priority::NAME),
            device_local_bytes: local,
            atomic_float_add: has_ext(&exts, ash::ext::shader_atomic_float::NAME) && af.shader_buffer_float32_atomic_add == vk::TRUE,
            device_uuid: idp.device_uuid,
        }
    }
}

/// 検証レイヤーが報告したエラーの数（プロセス全体。テストで、描画の前後の差を見る）
static VALIDATION_ERRORS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// 検証レイヤーが報告したエラーの数（プロセスを始めてからの合計）
pub fn validation_error_count() -> usize {
    VALIDATION_ERRORS.load(std::sync::atomic::Ordering::Relaxed)
}

unsafe extern "system" fn debug_callback(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user: *mut std::ffi::c_void,
) -> vk::Bool32 {
    let msg = if data.is_null() || (*data).p_message.is_null() { String::new() } else { CStr::from_ptr((*data).p_message).to_string_lossy().into_owned() };
    if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        VALIDATION_ERRORS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        krbr_core::error!("[vulkan] {}", msg);
    } else if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
        krbr_core::warn!("[vulkan] {}", msg);
    }
    vk::FALSE
}

fn create_instance(entry: &ash::Entry, validation: bool, sync_validation: bool, extra_exts: &[*const c_char]) -> Result<(ash::Instance, bool), String> {
    let app_name = CString::new("krbr").unwrap();
    let app = vk::ApplicationInfo::default().application_name(&app_name).application_version(vk::make_api_version(0, 0, 1, 0)).engine_name(&app_name).api_version(vk::API_VERSION_1_3);
    let mut layers: Vec<*const c_char> = Vec::new();
    let mut exts: Vec<*const c_char> = extra_exts.to_vec();
    let mut with_validation = false;
    let layer_name = CString::new("VK_LAYER_KHRONOS_validation").unwrap();
    if validation {
        // 安全性：entry は有効
        let avail = unsafe { entry.enumerate_instance_layer_properties() }.unwrap_or_default();
        if avail.iter().any(|l| l.layer_name_as_c_str().map(|n| n == layer_name.as_c_str()).unwrap_or(false)) {
            layers.push(layer_name.as_ptr());
            exts.push(ash::ext::debug_utils::NAME.as_ptr());
            with_validation = true;
        } else {
            krbr_core::warn!("validation layer not available");
        }
    }
    let enables = [vk::ValidationFeatureEnableEXT::SYNCHRONIZATION_VALIDATION];
    let mut features = vk::ValidationFeaturesEXT::default().enabled_validation_features(&enables);
    let mut ci = vk::InstanceCreateInfo::default().application_info(&app).enabled_layer_names(&layers).enabled_extension_names(&exts);
    if with_validation && sync_validation {
        ci = ci.push_next(&mut features);
    }
    // 安全性：引数の文字列は関数の終わりまで生きている
    let instance = unsafe { entry.create_instance(&ci, None) }.map_err(|e| format!("vkCreateInstance failed ({e:?})"))?;
    Ok((instance, with_validation))
}

pub struct Context {
    pub entry: ash::Entry,
    pub instance: ash::Instance,
    pub physical: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub queue_family: u32,
    /// サンプルを積むキュー（画面表示のときに、計算専用ファミリーの LOW のキューを別に作れたら別。作れなければ queue と同じ）
    pub render_queue: vk::Queue,
    pub render_family: u32,
    /// render_queue 用のコマンドプール（同じファミリーなら pool と同じ）
    pub render_pool: vk::CommandPool,
    pub allocator: Arc<Mutex<Allocator>>,
    pub pool: vk::CommandPool,
    pub fence: vk::Fence,
    pub info: DeviceInfo,
    pub as_props: vk::PhysicalDeviceAccelerationStructurePropertiesKHR<'static>,
    pub rt_props: vk::PhysicalDeviceRayTracingPipelinePropertiesKHR<'static>,
    pub as_device: khr::acceleration_structure::Device,
    pub rt_device: Option<khr::ray_tracing_pipeline::Device>,
    pub use_rt_pipeline: bool,
    pub surface: vk::SurfaceKHR,
    pub surface_loader: Option<khr::surface::Instance>,
    pub swapchain_loader: Option<khr::swapchain::Device>,
    /// VK_KHR_external_memory_win32（ノイズ除去 OIDN とバッファを共有する）。使えなければ None
    pub external_memory_win32: Option<khr::external_memory_win32::Device>,
    debug: Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
}

impl Context {
    /// 全物理デバイスの情報を列挙（デバイスは作らない）
    pub fn enumerate() -> Result<Vec<DeviceInfo>, String> {
        // 安全性：Vulkan ローダーの読み込み
        let entry = unsafe { ash::Entry::load() }.map_err(|e| format!("Vulkan loader not found: {e}"))?;
        let (instance, _) = create_instance(&entry, false, false, &[])?;
        let out = unsafe { instance.enumerate_physical_devices() }.unwrap_or_default().into_iter().map(|pd| query_info(&instance, pd)).collect();
        unsafe { instance.destroy_instance(None) };
        Ok(out)
    }

    pub fn new(opts: &ContextOptions) -> Result<Self, String> {
        let entry = unsafe { ash::Entry::load() }.map_err(|e| format!("Vulkan loader not found: {e}"))?;
        let (instance, with_validation) = create_instance(&entry, opts.validation, opts.sync_validation, &opts.instance_extensions)?;
        let (surface, surface_loader) = match &opts.create_surface {
            Some(f) => (f(&entry, &instance)?, Some(khr::surface::Instance::new(&entry, &instance))),
            None => (vk::SurfaceKHR::null(), None),
        };
        let debug = if with_validation {
            let du = ash::ext::debug_utils::Instance::new(&entry, &instance);
            let mci = vk::DebugUtilsMessengerCreateInfoEXT::default()
                .message_severity(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR)
                .message_type(vk::DebugUtilsMessageTypeFlagsEXT::GENERAL | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE)
                .pfn_user_callback(Some(debug_callback));
            let m = unsafe { du.create_debug_utils_messenger(&mci, None) }.ok();
            m.map(|m| (du, m))
        } else {
            None
        };

        // 物理デバイス：ray query が第一候補。無ければ ray tracing pipeline
        let pds = unsafe { instance.enumerate_physical_devices() }.map_err(|e| format!("{e:?}"))?;
        let mut best: Option<(i64, vk::PhysicalDevice, DeviceInfo)> = None;
        for pd in pds {
            let di = query_info(&instance, pd);
            if (!di.ray_query && !di.ray_tracing_pipeline) || !di.acceleration_structure || di.api_version < vk::API_VERSION_1_3 {
                continue;
            }
            let score = (if di.discrete { 1000 } else { 0 }) + (if di.ray_query { 100 } else { 0 }) + (di.device_local_bytes >> 30) as i64;
            if best.as_ref().map(|b| score > b.0).unwrap_or(true) {
                best = Some((score, pd, di));
            }
        }
        let Some((_, physical, info)) = best else {
            return Err("no Vulkan 1.3 device with VK_KHR_acceleration_structure + (VK_KHR_ray_query or VK_KHR_ray_tracing_pipeline)".into());
        };
        let use_rt_pipeline = !info.ray_query;

        // キュー：グラフィックスを持たない計算専用ファミリーを優先（デスクトップ描画と競合しにくい）。
        // 画面表示するときは、計算と表示の両方ができるファミリー
        let qprops = unsafe { instance.get_physical_device_queue_family_properties(physical) };
        let (mut compute_only, mut any_compute, mut present) = (None, None, None);
        for (i, q) in qprops.iter().enumerate() {
            if !q.queue_flags.contains(vk::QueueFlags::COMPUTE) {
                continue;
            }
            if any_compute.is_none() {
                any_compute = Some(i as u32);
            }
            if !q.queue_flags.contains(vk::QueueFlags::GRAPHICS) && compute_only.is_none() {
                compute_only = Some(i as u32);
            }
            if let Some(sl) = &surface_loader {
                if present.is_none() && q.queue_flags.contains(vk::QueueFlags::GRAPHICS) && unsafe { sl.get_physical_device_surface_support(physical, i as u32, surface) }.unwrap_or(false) {
                    present = Some(i as u32);
                }
            }
        }
        let queue_family = if surface_loader.is_some() {
            present.ok_or("no queue family can present to the window")?
        } else {
            compute_only.or(any_compute).ok_or("no compute queue")?
        };

        let exts = unsafe { instance.enumerate_device_extension_properties(physical) }.unwrap_or_default();
        let mut dev_exts: Vec<*const c_char> = vec![khr::acceleration_structure::NAME.as_ptr(), khr::deferred_host_operations::NAME.as_ptr()];
        dev_exts.push(if use_rt_pipeline { khr::ray_tracing_pipeline::NAME.as_ptr() } else { khr::ray_query::NAME.as_ptr() });
        if surface_loader.is_some() {
            dev_exts.push(khr::swapchain::NAME.as_ptr());
        }
        if info.atomic_float_add {
            dev_exts.push(ash::ext::shader_atomic_float::NAME.as_ptr());
        }
        let external_memory = cfg!(windows) && has_ext(&exts, khr::external_memory_win32::NAME);
        if external_memory {
            dev_exts.push(khr::external_memory_win32::NAME.as_ptr());
        }
        let prio_ext = if opts.low_queue_priority {
            if has_ext(&exts, khr::global_priority::NAME) {
                Some(khr::global_priority::NAME)
            } else if has_ext(&exts, ash::ext::global_priority::NAME) {
                Some(ash::ext::global_priority::NAME)
            } else {
                None
            }
        } else {
            None
        };

        // 使う機能だけを有効にする。シェーダの SPIR-V が宣言するのは Shader、RayQueryKHR か RayTracingKHR、
        // AtomicFloat32AddEXT（BDPT / VCM の画面への足し込み）だけで、64 ビット整数・記述子の配列・scalar レイアウトは使わない。
        // bufferDeviceAddress（加速構造の入力）・synchronization2・maintenance4・dynamicRendering（表示）は Vulkan 1.3 で
        // 必須の機能なので、1.3 のデバイスを選んだ時点で使える。加速構造・ray query・ray tracing pipeline・atomic float は
        // query_info で対応を調べてから要求する
        // timelineSemaphore も 1.3 で必須（ビューアが、サンプルを積むキューと表示のキューの間で使う）
        let mut f12 = vk::PhysicalDeviceVulkan12Features::default().buffer_device_address(true).timeline_semaphore(true);
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default().synchronization2(true).maintenance4(true).dynamic_rendering(true);
        let mut fas = vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default().acceleration_structure(true);
        let mut frq = vk::PhysicalDeviceRayQueryFeaturesKHR::default().ray_query(true);
        let mut frtp = vk::PhysicalDeviceRayTracingPipelineFeaturesKHR::default().ray_tracing_pipeline(true);
        let mut faf = vk::PhysicalDeviceShaderAtomicFloatFeaturesEXT::default().shader_buffer_float32_atomic_add(true);
        let mut f2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut f12).push_next(&mut f13).push_next(&mut fas);
        if use_rt_pipeline {
            f2 = f2.push_next(&mut frtp);
        } else {
            f2 = f2.push_next(&mut frq);
        }
        if info.atomic_float_add {
            f2 = f2.push_next(&mut faf);
        }

        // 画面表示で LOW にできるなら、サンプルを積むキューを計算専用ファミリーに分けて、そちらだけ LOW にする
        let split = if surface_loader.is_some() && prio_ext.is_some() { compute_only.filter(|&c| c != queue_family) } else { None };
        let prio = [0.5f32];
        let mut gp = vk::DeviceQueueGlobalPriorityCreateInfoKHR::default().global_priority(vk::QueueGlobalPriorityKHR::LOW);
        let mut make_device = |with_prio: bool, split: Option<u32>| -> Result<ash::Device, vk::Result> {
            let mut e = dev_exts.clone();
            let mut qcis = vec![vk::DeviceQueueCreateInfo::default().queue_family_index(queue_family).queue_priorities(&prio)];
            if with_prio {
                if let Some(pe) = prio_ext {
                    e.push(pe.as_ptr());
                    match split {
                        Some(c) => qcis.push(vk::DeviceQueueCreateInfo::default().queue_family_index(c).queue_priorities(&prio).push_next(&mut gp)),
                        None => qcis[0] = qcis[0].push_next(&mut gp),
                    }
                }
            }
            let dci = vk::DeviceCreateInfo::default().queue_create_infos(&qcis).enabled_extension_names(&e).push_next(&mut f2);
            unsafe { instance.create_device(physical, &dci, None) }
        };
        let (device, low_prio, split) = match make_device(prio_ext.is_some(), split) {
            Ok(d) => (d, prio_ext.is_some(), split),
            Err(e) if prio_ext.is_some() => {
                krbr_core::debug!("device creation with low global priority failed ({:?}), retrying", e);
                (make_device(false, None).map_err(|e| format!("vkCreateDevice failed ({e:?})"))?, false, None)
            }
            Err(e) => return Err(format!("vkCreateDevice failed ({e:?})")),
        };
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        let render_family = split.unwrap_or(queue_family);
        let render_queue = unsafe { device.get_device_queue(render_family, 0) };

        let mut as_props = vk::PhysicalDeviceAccelerationStructurePropertiesKHR::default();
        let mut rt_props = vk::PhysicalDeviceRayTracingPipelinePropertiesKHR::default();
        {
            let mut p2 = vk::PhysicalDeviceProperties2::default().push_next(&mut as_props);
            if use_rt_pipeline {
                p2 = p2.push_next(&mut rt_props);
            }
            unsafe { instance.get_physical_device_properties2(physical, &mut p2) };
        }
        // pNext の鎖を切って 'static にする
        let as_props = vk::PhysicalDeviceAccelerationStructurePropertiesKHR { p_next: std::ptr::null_mut(), ..as_props };
        let rt_props = vk::PhysicalDeviceRayTracingPipelinePropertiesKHR { p_next: std::ptr::null_mut(), ..rt_props };

        let allocator = Allocator::new(&AllocatorCreateDesc {
            instance: instance.clone(),
            device: device.clone(),
            physical_device: physical,
            debug_settings: Default::default(),
            buffer_device_address: true,
            allocation_sizes: Default::default(),
        })
        .map_err(|e| format!("allocator: {e}"))?;

        let pool = unsafe { device.create_command_pool(&vk::CommandPoolCreateInfo::default().flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER).queue_family_index(queue_family), None) }.map_err(|e| format!("{e:?}"))?;
        let render_pool = if render_family == queue_family {
            pool
        } else {
            unsafe { device.create_command_pool(&vk::CommandPoolCreateInfo::default().flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER).queue_family_index(render_family), None) }.map_err(|e| format!("{e:?}"))?
        };
        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }.map_err(|e| format!("{e:?}"))?;
        let as_device = khr::acceleration_structure::Device::new(&instance, &device);
        let rt_device = use_rt_pipeline.then(|| khr::ray_tracing_pipeline::Device::new(&instance, &device));
        let swapchain_loader = surface_loader.as_ref().map(|_| khr::swapchain::Device::new(&instance, &device));
        let external_memory_win32 = external_memory.then(|| khr::external_memory_win32::Device::new(&instance, &device));

        krbr_core::info!(
            "GPU: {} (Vulkan {}.{}.{}), queue family {}{}{}, {}",
            info.name,
            vk::api_version_major(info.api_version),
            vk::api_version_minor(info.api_version),
            vk::api_version_patch(info.api_version),
            queue_family,
            match split {
                Some(c) => format!(" (display) + {c} (compute-only, rendering)"),
                None if compute_only == Some(queue_family) => " (compute-only)".to_string(),
                None => String::new(),
            },
            match (low_prio, split) {
                (true, Some(_)) => ", low global priority for rendering",
                (true, None) => ", low global priority",
                _ => "",
            },
            if use_rt_pipeline { "ray tracing pipeline (no ray_query)" } else { "ray query" }
        );
        Ok(Self { entry, instance, physical, device, queue, queue_family, render_queue, render_family, render_pool, allocator: Arc::new(Mutex::new(allocator)), pool, fence, info, as_props, rt_props, as_device, rt_device, use_rt_pipeline, surface, surface_loader, swapchain_loader, external_memory_win32, debug })
    }

    /// レイを飛ばすシェーダの段階（バリア用）
    pub fn trace_stages(&self) -> vk::PipelineStageFlags2 {
        if self.use_rt_pipeline {
            vk::PipelineStageFlags2::COMPUTE_SHADER | vk::PipelineStageFlags2::RAY_TRACING_SHADER_KHR
        } else {
            vk::PipelineStageFlags2::COMPUTE_SHADER
        }
    }

    /// 表示とサンプルを積むキューのファミリーが違えば、その 2 つ（資源を両方のキューで使えるように共有する）。同じなら空
    pub fn shared_families(&self) -> Vec<u32> {
        if self.render_family == self.queue_family {
            Vec::new()
        } else {
            vec![self.queue_family, self.render_family]
        }
    }

    /// バッファを作る。VRAM が足りないなどで作れなければ Err（作りかけの資源は解放する）
    pub fn create_buffer(&self, size: u64, usage: vk::BufferUsageFlags, host_visible: bool, host_readback: bool) -> Result<Buffer, String> {
        let families = self.shared_families();
        let mut ci = vk::BufferCreateInfo::default().size(size.max(16)).usage(usage | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS);
        if !families.is_empty() {
            ci = ci.sharing_mode(vk::SharingMode::CONCURRENT).queue_family_indices(&families);
        }
        let buffer = unsafe { self.device.create_buffer(&ci, None) }.map_err(|e| format!("vkCreateBuffer ({}): {e:?}", mib(size)))?;
        let req = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let location = if host_readback {
            MemoryLocation::GpuToCpu
        } else if host_visible {
            MemoryLocation::CpuToGpu
        } else {
            MemoryLocation::GpuOnly
        };
        let allocation = match self.allocate(&AllocationCreateDesc { name: "buffer", requirements: req, location, linear: true, allocation_scheme: AllocationScheme::GpuAllocatorManaged }) {
            Ok(a) => a,
            Err(e) => {
                unsafe { self.device.destroy_buffer(buffer, None) };
                return Err(format!("buffer of {} ({location:?}): {e}", mib(size)));
            }
        };
        if let Err(e) = unsafe { self.device.bind_buffer_memory(buffer, allocation.memory(), allocation.offset()) } {
            self.free(allocation);
            unsafe { self.device.destroy_buffer(buffer, None) };
            return Err(format!("vkBindBufferMemory: {e:?}"));
        }
        let address = unsafe { self.device.get_buffer_device_address(&vk::BufferDeviceAddressInfo::default().buffer(buffer)) };
        Ok(Buffer { buffer, allocation: Some(allocation), size, address })
    }

    fn allocate(&self, desc: &AllocationCreateDesc<'_>) -> Result<Allocation, String> {
        let mut a = self.allocator.lock().map_err(|_| "the GPU memory allocator is poisoned".to_string())?;
        a.allocate(desc).map_err(|e| e.to_string())
    }

    fn free(&self, a: Allocation) {
        if let Ok(mut al) = self.allocator.lock() {
            let _ = al.free(a);
        }
    }

    pub fn destroy_buffer(&self, b: &mut Buffer) {
        if let Some(a) = b.allocation.take() {
            self.free(a);
        }
        if b.buffer != vk::Buffer::null() {
            unsafe { self.device.destroy_buffer(b.buffer, None) };
        }
        *b = Buffer::default();
    }

    /// device-local バッファを作ってステージング経由で転送する。失敗したら Err（作ったバッファは解放する）
    pub fn create_device_buffer(&self, data: &[u8], usage: vk::BufferUsageFlags) -> Result<Buffer, String> {
        let mut b = self.create_buffer(data.len() as u64, usage | vk::BufferUsageFlags::TRANSFER_DST, false, false)?;
        if let Err(e) = self.upload(&mut b, data, 0) {
            self.destroy_buffer(&mut b);
            return Err(e);
        }
        Ok(b)
    }

    /// data を dst の dst_offset へ転送して、完了まで待つ
    pub fn upload(&self, dst: &mut Buffer, data: &[u8], dst_offset: u64) -> Result<(), String> {
        if data.is_empty() {
            return Ok(());
        }
        let mut staging = self.create_buffer(data.len() as u64, vk::BufferUsageFlags::TRANSFER_SRC, true, false)?;
        let r = (|| {
            staging.mapped_bytes_mut().ok_or("the staging buffer is not mapped")?.copy_from_slice(data);
            let (src, dstb, n) = (staging.buffer, dst.buffer, data.len() as u64);
            self.submit_and_wait(|dev, cmd| unsafe {
                dev.cmd_copy_buffer(cmd, src, dstb, &[vk::BufferCopy { src_offset: 0, dst_offset, size: n }]);
            })
        })();
        self.destroy_buffer(&mut staging);
        r
    }

    /// 2D の画像（layers > 1 なら配列）とビューを作る。失敗したら Err（作りかけの資源は解放する）
    pub fn create_image_2d(&self, w: u32, h: u32, layers: u32, format: vk::Format, usage: vk::ImageUsageFlags) -> Result<Image, String> {
        let families = self.shared_families();
        let mut ici = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width: w, height: h, depth: 1 })
            .mip_levels(1)
            .array_layers(layers)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        if !families.is_empty() {
            ici = ici.sharing_mode(vk::SharingMode::CONCURRENT).queue_family_indices(&families);
        }
        let image = unsafe { self.device.create_image(&ici, None) }.map_err(|e| format!("vkCreateImage ({w}x{h}x{layers} {format:?}): {e:?}"))?;
        let req = unsafe { self.device.get_image_memory_requirements(image) };
        let mut img = Image { image, allocation: None, view: vk::ImageView::null(), format, width: w, height: h, layers };
        let r = (|| {
            let allocation = self
                .allocate(&AllocationCreateDesc { name: "image", requirements: req, location: MemoryLocation::GpuOnly, linear: false, allocation_scheme: AllocationScheme::GpuAllocatorManaged })
                .map_err(|e| format!("image {w}x{h}x{layers} {format:?} ({}): {e}", mib(req.size)))?;
            // 安全性：memory は bind にだけ使い、allocation（img が持つ）より長く使わない
            let (memory, offset) = (unsafe { allocation.memory() }, allocation.offset());
            img.allocation = Some(allocation);
            unsafe { self.device.bind_image_memory(image, memory, offset) }.map_err(|e| format!("vkBindImageMemory: {e:?}"))?;
            let vci = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(if layers > 1 { vk::ImageViewType::TYPE_2D_ARRAY } else { vk::ImageViewType::TYPE_2D })
                .format(format)
                .subresource_range(vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: layers });
            img.view = unsafe { self.device.create_image_view(&vci, None) }.map_err(|e| format!("vkCreateImageView: {e:?}"))?;
            Ok::<(), String>(())
        })();
        match r {
            Ok(()) => Ok(img),
            Err(e) => {
                self.destroy_image(&mut img);
                Err(e)
            }
        }
    }

    pub fn destroy_image(&self, img: &mut Image) {
        if img.view != vk::ImageView::null() {
            unsafe { self.device.destroy_image_view(img.view, None) };
        }
        if let Some(a) = img.allocation.take() {
            self.free(a);
        }
        if img.image != vk::Image::null() {
            unsafe { self.device.destroy_image(img.image, None) };
        }
        *img = Image::default();
    }

    /// 画像全体にデータを転送して最終レイアウトへ遷移する
    pub fn upload_image(&self, img: &Image, data: &[u8], final_layout: vk::ImageLayout) -> Result<(), String> {
        self.upload_image_parts(img, &[data], final_layout)
    }

    /// upload_image の、データを幾つかに分けて渡すもの（層ごとなど。つないだものが画像全体）。転送用のバッファへ直接写すので、
    /// 呼ぶ側で 1 本につなぎ直さなくてよい（基板の距離場は表・裏で 100 MB 近い）
    pub fn upload_image_parts(&self, img: &Image, parts: &[&[u8]], final_layout: vk::ImageLayout) -> Result<(), String> {
        let total: usize = parts.iter().map(|p| p.len()).sum();
        let mut staging = self.create_buffer(total as u64, vk::BufferUsageFlags::TRANSFER_SRC, true, false)?;
        let r = (|| {
            let dst = staging.mapped_bytes_mut().ok_or("the staging buffer is not mapped")?;
            let mut at = 0;
            for p in parts {
                dst[at..at + p.len()].copy_from_slice(p);
                at += p.len();
            }
            let (sb, image, w, h, layers) = (staging.buffer, img.image, img.width, img.height, img.layers);
            let trace = self.trace_stages();
            self.submit_and_wait(|dev, cmd| {
                image_barrier(dev, cmd, image, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE, layers);
                let region = vk::BufferImageCopy::default()
                    .image_subresource(vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: layers })
                    .image_extent(vk::Extent3D { width: w, height: h, depth: 1 });
                unsafe { dev.cmd_copy_buffer_to_image(cmd, sb, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[region]) };
                image_barrier(dev, cmd, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, final_layout, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE, trace, vk::AccessFlags2::SHADER_SAMPLED_READ | vk::AccessFlags2::SHADER_STORAGE_READ, layers);
            })
        })();
        self.destroy_buffer(&mut staging);
        r
    }

    /// 1 回きりのコマンドを記録して完了まで待つ。投入や待機に失敗したら Err（デバイスを失ったときなど）
    /// 検証レイヤーを有効にして作ったか（レイヤーが無ければ、指定しても無効のまま）
    pub fn validating(&self) -> bool {
        self.debug.is_some()
    }

    pub fn submit_and_wait(&self, record: impl FnOnce(&ash::Device, vk::CommandBuffer)) -> Result<(), String> {
        let dev = &self.device;
        let ai = vk::CommandBufferAllocateInfo::default().command_pool(self.pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1);
        let cmd = unsafe { dev.allocate_command_buffers(&ai) }.map_err(|e| format!("vkAllocateCommandBuffers: {e:?}"))?[0];
        let r = (|| unsafe {
            dev.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)).map_err(|e| format!("vkBeginCommandBuffer: {e:?}"))?;
            record(dev, cmd);
            dev.end_command_buffer(cmd).map_err(|e| format!("vkEndCommandBuffer: {e:?}"))?;
            let cmds = [cmd];
            let si = vk::SubmitInfo::default().command_buffers(&cmds);
            dev.reset_fences(&[self.fence]).map_err(|e| format!("vkResetFences: {e:?}"))?;
            dev.queue_submit(self.queue, &[si], self.fence).map_err(|e| format!("vkQueueSubmit: {e:?}"))?;
            dev.wait_for_fences(&[self.fence], true, u64::MAX).map_err(|e| format!("vkWaitForFences: {e:?}"))
        })();
        unsafe { dev.free_command_buffers(self.pool, &[cmd]) };
        r
    }

    pub fn wait_idle(&self) {
        let _ = unsafe { self.device.device_wait_idle() };
    }
}

impl Context {
    /// アロケータ（とそれが持つメモリ）をデバイスより先に解放する。
    /// allocator は Arc<Mutex<Allocator>> のまま表示（egui_ash_renderer の with_gpu_allocator）へ渡すので、型を Option にして
    /// take() することができない。そこで中身を空のアロケータと入れ替えて、古い方をここで解放する。
    /// 空のアロケータを作れなかったときは、壊した後のデバイスでメモリを解放しないよう、古い方を解放せずに残す（Arc を 1 つ忘れる）
    fn release_allocator(&mut self) {
        let empty = Allocator::new(&AllocatorCreateDesc {
            instance: self.instance.clone(),
            device: self.device.clone(),
            physical_device: self.physical,
            debug_settings: Default::default(),
            buffer_device_address: false,
            allocation_sizes: Default::default(),
        });
        match (self.allocator.lock(), empty) {
            (Ok(mut a), Ok(e)) => drop(std::mem::replace(&mut *a, e)),
            _ => {
                krbr_core::warn!("cannot release the GPU memory allocator before the device; leaving its memory allocated");
                std::mem::forget(self.allocator.clone());
            }
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_fence(self.fence, None);
            if self.render_pool != self.pool {
                self.device.destroy_command_pool(self.render_pool, None);
            }
            self.device.destroy_command_pool(self.pool, None);
            self.release_allocator();
            self.device.destroy_device(None);
            if let (Some(sl), true) = (&self.surface_loader, self.surface != vk::SurfaceKHR::null()) {
                sl.destroy_surface(self.surface, None);
            }
            if let Some((du, m)) = self.debug.take() {
                du.destroy_debug_utils_messenger(m, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}

/// 画像のレイアウトの移行とメモリのバリア（VkImageMemoryBarrier2 の項目をそのまま引数にする）
#[allow(clippy::too_many_arguments)]
pub fn image_barrier(dev: &ash::Device, cmd: vk::CommandBuffer, image: vk::Image, old: vk::ImageLayout, new: vk::ImageLayout, src_stage: vk::PipelineStageFlags2, src_access: vk::AccessFlags2, dst_stage: vk::PipelineStageFlags2, dst_access: vk::AccessFlags2, layers: u32) {
    let b = vk::ImageMemoryBarrier2::default()
        .src_stage_mask(src_stage)
        .src_access_mask(src_access)
        .dst_stage_mask(dst_stage)
        .dst_access_mask(dst_access)
        .old_layout(old)
        .new_layout(new)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: layers });
    let bs = [b];
    let di = vk::DependencyInfo::default().image_memory_barriers(&bs);
    unsafe { dev.cmd_pipeline_barrier2(cmd, &di) };
}

pub fn memory_barrier(dev: &ash::Device, cmd: vk::CommandBuffer, src_stage: vk::PipelineStageFlags2, src_access: vk::AccessFlags2, dst_stage: vk::PipelineStageFlags2, dst_access: vk::AccessFlags2) {
    let b = vk::MemoryBarrier2::default().src_stage_mask(src_stage).src_access_mask(src_access).dst_stage_mask(dst_stage).dst_access_mask(dst_access);
    let bs = [b];
    let di = vk::DependencyInfo::default().memory_barriers(&bs);
    unsafe { dev.cmd_pipeline_barrier2(cmd, &di) };
}

/// GPU へそのまま送れる型：どのバイトも初期化されている（repr(C) でパディングが無く、参照を持たない）。
/// GPU 用の構造体は、隙間をパディングのフィールドで明示的に埋め、size_of の const assert で隙間が無いことを確かめてから実装する
///
/// # Safety
/// 実装する型は、どのバイトも初期化済みであること（as_bytes がバイト列として読む）
pub unsafe trait GpuPod: Copy + 'static {}

// 安全性：プリミティブと、glam の repr(C) のベクトル（成分だけでパディングが無い）
unsafe impl GpuPod for u8 {}
unsafe impl GpuPod for u32 {}
unsafe impl GpuPod for i32 {}
unsafe impl GpuPod for f32 {}
unsafe impl GpuPod for glam::Vec3 {}
unsafe impl GpuPod for glam::Vec4 {}
unsafe impl GpuPod for glam::UVec4 {}
unsafe impl<T: GpuPod, const N: usize> GpuPod for [T; N] {}
// 安全性：変換行列（f32 x 12）、2 つの Packed24_8（u32）、デバイスアドレス（u64）の 64 バイトで、隙間が無い
unsafe impl GpuPod for vk::AccelerationStructureInstanceKHR {}
const _: () = assert!(std::mem::size_of::<vk::AccelerationStructureInstanceKHR>() == 64);

/// GPU へ送る値の並びをバイト列として見る
pub fn as_bytes<T: GpuPod>(v: &[T]) -> &[u8] {
    // 安全性：T: GpuPod なので、どのバイトも初期化されている。読み取りのみ
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

/// ログ用：バイト数を MB で
fn mib(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1u64 << 20) as f64)
}
