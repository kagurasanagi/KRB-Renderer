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
    /// VK_KHR_global_priority が使えれば LOW で作る
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
    pub fn is_valid(&self) -> bool {
        self.buffer != vk::Buffer::null()
    }
    pub fn mapped(&mut self) -> Option<&mut [u8]> {
        self.allocation.as_mut().and_then(|a| a.mapped_slice_mut())
    }
    pub fn mapped_ref(&self) -> Option<&[u8]> {
        self.allocation.as_ref().and_then(|a| a.mapped_slice())
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

impl Image {
    pub fn is_valid(&self) -> bool {
        self.image != vk::Image::null()
    }
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

unsafe extern "system" fn debug_callback(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    _user: *mut std::ffi::c_void,
) -> vk::Bool32 {
    let msg = if data.is_null() || (*data).p_message.is_null() { String::new() } else { CStr::from_ptr((*data).p_message).to_string_lossy().into_owned() };
    if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR) {
        krbr_core::error!("[vulkan] {}", msg);
    } else if severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::WARNING) {
        krbr_core::warn!("[vulkan] {}", msg);
    }
    vk::FALSE
}

fn create_instance(entry: &ash::Entry, validation: bool, extra_exts: &[*const c_char]) -> Result<(ash::Instance, bool), String> {
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
    let ci = vk::InstanceCreateInfo::default().application_info(&app).enabled_layer_names(&layers).enabled_extension_names(&exts);
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
    debug: Option<(ash::ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
}

impl Context {
    /// 全物理デバイスの情報を列挙（デバイスは作らない）
    pub fn enumerate() -> Result<Vec<DeviceInfo>, String> {
        // 安全性：Vulkan ローダーの読み込み
        let entry = unsafe { ash::Entry::load() }.map_err(|e| format!("Vulkan loader not found: {e}"))?;
        let (instance, _) = create_instance(&entry, false, &[])?;
        let out = unsafe { instance.enumerate_physical_devices() }.unwrap_or_default().into_iter().map(|pd| query_info(&instance, pd)).collect();
        unsafe { instance.destroy_instance(None) };
        Ok(out)
    }

    pub fn new(opts: &ContextOptions) -> Result<Self, String> {
        let entry = unsafe { ash::Entry::load() }.map_err(|e| format!("Vulkan loader not found: {e}"))?;
        let (instance, with_validation) = create_instance(&entry, opts.validation, &opts.instance_extensions)?;
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
            compute_only = None;
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

        let mut f12 = vk::PhysicalDeviceVulkan12Features::default().buffer_device_address(true).descriptor_indexing(true).runtime_descriptor_array(true).shader_sampled_image_array_non_uniform_indexing(true).scalar_block_layout(true);
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default().synchronization2(true).maintenance4(true).dynamic_rendering(true);
        let mut fas = vk::PhysicalDeviceAccelerationStructureFeaturesKHR::default().acceleration_structure(true);
        let mut frq = vk::PhysicalDeviceRayQueryFeaturesKHR::default().ray_query(true);
        let mut frtp = vk::PhysicalDeviceRayTracingPipelineFeaturesKHR::default().ray_tracing_pipeline(true);
        let mut faf = vk::PhysicalDeviceShaderAtomicFloatFeaturesEXT::default().shader_buffer_float32_atomic_add(true);
        let features = vk::PhysicalDeviceFeatures::default().shader_int64(true);
        let mut f2 = vk::PhysicalDeviceFeatures2::default().features(features).push_next(&mut f12).push_next(&mut f13).push_next(&mut fas);
        if use_rt_pipeline {
            f2 = f2.push_next(&mut frtp);
        } else {
            f2 = f2.push_next(&mut frq);
        }
        if info.atomic_float_add {
            f2 = f2.push_next(&mut faf);
        }

        let prio = [0.5f32];
        let mut gp = vk::DeviceQueueGlobalPriorityCreateInfoKHR::default().global_priority(vk::QueueGlobalPriorityKHR::LOW);
        let mut make_device = |with_prio: bool| -> Result<ash::Device, vk::Result> {
            let mut e = dev_exts.clone();
            let mut qci = vk::DeviceQueueCreateInfo::default().queue_family_index(queue_family).queue_priorities(&prio);
            if with_prio {
                if let Some(pe) = prio_ext {
                    e.push(pe.as_ptr());
                    qci = qci.push_next(&mut gp);
                }
            }
            let qcis = [qci];
            let dci = vk::DeviceCreateInfo::default().queue_create_infos(&qcis).enabled_extension_names(&e).push_next(&mut f2);
            unsafe { instance.create_device(physical, &dci, None) }
        };
        let (device, low_prio) = match make_device(prio_ext.is_some()) {
            Ok(d) => (d, prio_ext.is_some()),
            Err(e) if prio_ext.is_some() => {
                krbr_core::debug!("device creation with low global priority failed ({:?}), retrying", e);
                (make_device(false).map_err(|e| format!("vkCreateDevice failed ({e:?})"))?, false)
            }
            Err(e) => return Err(format!("vkCreateDevice failed ({e:?})")),
        };
        let queue = unsafe { device.get_device_queue(queue_family, 0) };

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
        let fence = unsafe { device.create_fence(&vk::FenceCreateInfo::default(), None) }.map_err(|e| format!("{e:?}"))?;
        let as_device = khr::acceleration_structure::Device::new(&instance, &device);
        let rt_device = use_rt_pipeline.then(|| khr::ray_tracing_pipeline::Device::new(&instance, &device));
        let swapchain_loader = surface_loader.as_ref().map(|_| khr::swapchain::Device::new(&instance, &device));

        krbr_core::info!(
            "GPU: {} (Vulkan {}.{}.{}), queue family {}{}{}, {}",
            info.name,
            vk::api_version_major(info.api_version),
            vk::api_version_minor(info.api_version),
            vk::api_version_patch(info.api_version),
            queue_family,
            if compute_only.is_some() { " (compute-only)" } else { "" },
            if low_prio { ", low global priority" } else { "" },
            if use_rt_pipeline { "ray tracing pipeline (no ray_query)" } else { "ray query" }
        );
        Ok(Self { entry, instance, physical, device, queue, queue_family, allocator: Arc::new(Mutex::new(allocator)), pool, fence, info, as_props, rt_props, as_device, rt_device, use_rt_pipeline, surface, surface_loader, swapchain_loader, debug })
    }

    /// レイを飛ばすシェーダの段階（バリア用）
    pub fn trace_stages(&self) -> vk::PipelineStageFlags2 {
        if self.use_rt_pipeline {
            vk::PipelineStageFlags2::COMPUTE_SHADER | vk::PipelineStageFlags2::RAY_TRACING_SHADER_KHR
        } else {
            vk::PipelineStageFlags2::COMPUTE_SHADER
        }
    }

    pub fn create_buffer(&self, size: u64, usage: vk::BufferUsageFlags, host_visible: bool, host_readback: bool) -> Buffer {
        let ci = vk::BufferCreateInfo::default().size(size.max(16)).usage(usage | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS);
        let buffer = unsafe { self.device.create_buffer(&ci, None) }.expect("vkCreateBuffer");
        let req = unsafe { self.device.get_buffer_memory_requirements(buffer) };
        let location = if host_readback {
            MemoryLocation::GpuToCpu
        } else if host_visible {
            MemoryLocation::CpuToGpu
        } else {
            MemoryLocation::GpuOnly
        };
        let allocation = self
            .allocator
            .lock()
            .unwrap()
            .allocate(&AllocationCreateDesc { name: "buffer", requirements: req, location, linear: true, allocation_scheme: AllocationScheme::GpuAllocatorManaged })
            .expect("allocate");
        unsafe { self.device.bind_buffer_memory(buffer, allocation.memory(), allocation.offset()) }.expect("bind");
        let address = unsafe { self.device.get_buffer_device_address(&vk::BufferDeviceAddressInfo::default().buffer(buffer)) };
        Buffer { buffer, allocation: Some(allocation), size, address }
    }

    pub fn destroy_buffer(&self, b: &mut Buffer) {
        if let Some(a) = b.allocation.take() {
            let _ = self.allocator.lock().unwrap().free(a);
        }
        if b.buffer != vk::Buffer::null() {
            unsafe { self.device.destroy_buffer(b.buffer, None) };
        }
        *b = Buffer::default();
    }

    /// device-local バッファを作ってステージング経由で転送
    pub fn create_device_buffer(&self, data: &[u8], usage: vk::BufferUsageFlags) -> Buffer {
        let mut b = self.create_buffer(data.len() as u64, usage | vk::BufferUsageFlags::TRANSFER_DST, false, false);
        if !data.is_empty() {
            self.upload(&mut b, data, 0);
        }
        b
    }

    pub fn upload(&self, dst: &mut Buffer, data: &[u8], dst_offset: u64) {
        if data.is_empty() {
            return;
        }
        let mut staging = self.create_buffer(data.len() as u64, vk::BufferUsageFlags::TRANSFER_SRC, true, false);
        staging.mapped().unwrap()[..data.len()].copy_from_slice(data);
        let (src, dstb, n) = (staging.buffer, dst.buffer, data.len() as u64);
        self.submit_and_wait(|dev, cmd| unsafe {
            dev.cmd_copy_buffer(cmd, src, dstb, &[vk::BufferCopy { src_offset: 0, dst_offset, size: n }]);
        });
        self.destroy_buffer(&mut staging);
    }

    pub fn create_image_2d(&self, w: u32, h: u32, layers: u32, format: vk::Format, usage: vk::ImageUsageFlags) -> Image {
        let ici = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width: w, height: h, depth: 1 })
            .mip_levels(1)
            .array_layers(layers)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = unsafe { self.device.create_image(&ici, None) }.expect("vkCreateImage");
        let req = unsafe { self.device.get_image_memory_requirements(image) };
        let allocation = self
            .allocator
            .lock()
            .unwrap()
            .allocate(&AllocationCreateDesc { name: "image", requirements: req, location: MemoryLocation::GpuOnly, linear: false, allocation_scheme: AllocationScheme::GpuAllocatorManaged })
            .expect("allocate image");
        unsafe { self.device.bind_image_memory(image, allocation.memory(), allocation.offset()) }.expect("bind image");
        let vci = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(if layers > 1 { vk::ImageViewType::TYPE_2D_ARRAY } else { vk::ImageViewType::TYPE_2D })
            .format(format)
            .subresource_range(vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: layers });
        let view = unsafe { self.device.create_image_view(&vci, None) }.expect("vkCreateImageView");
        Image { image, allocation: Some(allocation), view, format, width: w, height: h, layers }
    }

    pub fn destroy_image(&self, img: &mut Image) {
        if img.view != vk::ImageView::null() {
            unsafe { self.device.destroy_image_view(img.view, None) };
        }
        if let Some(a) = img.allocation.take() {
            let _ = self.allocator.lock().unwrap().free(a);
        }
        if img.image != vk::Image::null() {
            unsafe { self.device.destroy_image(img.image, None) };
        }
        *img = Image::default();
    }

    /// 画像全体にデータを転送して最終レイアウトへ遷移
    pub fn upload_image(&self, img: &Image, data: &[u8], final_layout: vk::ImageLayout) {
        let mut staging = self.create_buffer(data.len() as u64, vk::BufferUsageFlags::TRANSFER_SRC, true, false);
        staging.mapped().unwrap()[..data.len()].copy_from_slice(data);
        let (sb, image, w, h, layers) = (staging.buffer, img.image, img.width, img.height, img.layers);
        let trace = self.trace_stages();
        self.submit_and_wait(|dev, cmd| {
            image_barrier(dev, cmd, image, vk::ImageLayout::UNDEFINED, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::PipelineStageFlags2::NONE, vk::AccessFlags2::NONE, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE, layers);
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: 0, layer_count: layers })
                .image_extent(vk::Extent3D { width: w, height: h, depth: 1 });
            unsafe { dev.cmd_copy_buffer_to_image(cmd, sb, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[region]) };
            image_barrier(dev, cmd, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, final_layout, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE, trace, vk::AccessFlags2::SHADER_SAMPLED_READ | vk::AccessFlags2::SHADER_STORAGE_READ, layers);
        });
        self.destroy_buffer(&mut staging);
    }

    /// 1 回きりのコマンドを記録して完了まで待つ
    pub fn submit_and_wait(&self, record: impl FnOnce(&ash::Device, vk::CommandBuffer)) {
        let dev = &self.device;
        unsafe {
            let cmd = dev.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(self.pool).level(vk::CommandBufferLevel::PRIMARY).command_buffer_count(1)).expect("alloc cmd")[0];
            dev.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)).expect("begin");
            record(dev, cmd);
            dev.end_command_buffer(cmd).expect("end");
            let cmds = [cmd];
            let si = vk::SubmitInfo::default().command_buffers(&cmds);
            dev.reset_fences(&[self.fence]).expect("reset fence");
            dev.queue_submit(self.queue, &[si], self.fence).expect("vkQueueSubmit");
            dev.wait_for_fences(&[self.fence], true, u64::MAX).expect("wait fence");
            dev.free_command_buffers(self.pool, &[cmd]);
        }
    }

    pub fn wait_idle(&self) {
        let _ = unsafe { self.device.device_wait_idle() };
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.pool, None);
            // アロケータはデバイスより先に解放する
            if let Ok(mut a) = self.allocator.lock() {
                let empty = Allocator::new(&AllocatorCreateDesc {
                    instance: self.instance.clone(),
                    device: self.device.clone(),
                    physical_device: self.physical,
                    debug_settings: Default::default(),
                    buffer_device_address: false,
                    allocation_sizes: Default::default(),
                });
                if let Ok(e) = empty {
                    let old = std::mem::replace(&mut *a, e);
                    drop(old);
                }
            }
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

/// POD の並びをバイト列として見る
pub fn as_bytes<T: Copy>(v: &[T]) -> &[u8] {
    // 安全性：T は repr(C) の POD として使う前提。読み取りのみ
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}
