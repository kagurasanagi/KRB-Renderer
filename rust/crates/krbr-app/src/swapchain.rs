//! スワップチェーン（垂直同期、UNORM 形式。sRGB への変換はトーンマッピングのシェーダで済ませる）

use ash::vk;
use krbr_render::Context;

pub struct Swapchain<'a> {
    ctx: &'a Context,
    pub handle: vk::SwapchainKHR,
    pub format: vk::Format,
    pub extent: vk::Extent2D,
    /// 画像を読み戻せる（テストで表示内容を保存する）
    pub can_read_back: bool,
    pub images: Vec<vk::Image>,
    pub views: Vec<vk::ImageView>,
    /// 画像ごと（表示が終わるまで再利用しない）
    pub render_done: Vec<vk::Semaphore>,
}

impl<'a> Swapchain<'a> {
    pub fn new(ctx: &'a Context, w: u32, h: u32) -> Result<Self, String> {
        let mut s = Self { ctx, handle: vk::SwapchainKHR::null(), format: vk::Format::UNDEFINED, extent: vk::Extent2D::default(), can_read_back: false, images: Vec::new(), views: Vec::new(), render_done: Vec::new() };
        s.recreate(w, h)?;
        Ok(s)
    }

    pub fn recreate(&mut self, w: u32, h: u32) -> Result<(), String> {
        let ctx = self.ctx;
        let dev = &ctx.device;
        let sl = ctx.surface_loader.as_ref().ok_or("no surface")?;
        let loader = ctx.swapchain_loader.as_ref().ok_or("no swapchain loader")?;
        let caps = unsafe { sl.get_physical_device_surface_capabilities(ctx.physical, ctx.surface) }.map_err(|e| format!("{e:?}"))?;
        let fmts = unsafe { sl.get_physical_device_surface_formats(ctx.physical, ctx.surface) }.map_err(|e| format!("{e:?}"))?;
        let chosen = fmts
            .iter()
            .find(|f| (f.format == vk::Format::B8G8R8A8_UNORM || f.format == vk::Format::R8G8B8A8_UNORM) && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
            .or(fmts.first())
            .copied()
            .ok_or("no surface format")?;
        self.format = chosen.format;
        let mut extent = if caps.current_extent.width != u32::MAX { caps.current_extent } else { vk::Extent2D { width: w, height: h } };
        extent.width = extent.width.clamp(caps.min_image_extent.width, caps.max_image_extent.width.max(1));
        extent.height = extent.height.clamp(caps.min_image_extent.height, caps.max_image_extent.height.max(1));
        self.extent = extent;
        if !caps.supported_usage_flags.contains(vk::ImageUsageFlags::TRANSFER_DST) {
            return Err("swapchain images cannot be a transfer destination".into());
        }
        self.can_read_back = caps.supported_usage_flags.contains(vk::ImageUsageFlags::TRANSFER_SRC);
        let mut count = caps.min_image_count + 1;
        if caps.max_image_count > 0 {
            count = count.min(caps.max_image_count);
        }
        let old = self.handle;
        let mut usage = vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::COLOR_ATTACHMENT;
        if self.can_read_back {
            usage |= vk::ImageUsageFlags::TRANSFER_SRC;
        }
        let ci = vk::SwapchainCreateInfoKHR::default()
            .surface(ctx.surface)
            .min_image_count(count)
            .image_format(chosen.format)
            .image_color_space(chosen.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(usage)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(vk::PresentModeKHR::FIFO) // 垂直同期（必要以上に回さない）
            .clipped(true)
            .old_swapchain(old);
        self.handle = unsafe { loader.create_swapchain(&ci, None) }.map_err(|e| format!("vkCreateSwapchainKHR: {e:?}"))?;
        self.destroy_views();
        if old != vk::SwapchainKHR::null() {
            unsafe { loader.destroy_swapchain(old, None) };
        }
        self.images = unsafe { loader.get_swapchain_images(self.handle) }.map_err(|e| format!("{e:?}"))?;
        for img in &self.images {
            let vci = vk::ImageViewCreateInfo::default().image(*img).view_type(vk::ImageViewType::TYPE_2D).format(self.format).subresource_range(vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 });
            self.views.push(unsafe { dev.create_image_view(&vci, None) }.map_err(|e| format!("{e:?}"))?);
            self.render_done.push(unsafe { dev.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }.map_err(|e| format!("{e:?}"))?);
        }
        Ok(())
    }

    fn destroy_views(&mut self) {
        let dev = &self.ctx.device;
        unsafe {
            for v in self.views.drain(..) {
                dev.destroy_image_view(v, None);
            }
            for s in self.render_done.drain(..) {
                dev.destroy_semaphore(s, None);
            }
        }
    }
}

impl Drop for Swapchain<'_> {
    fn drop(&mut self) {
        self.ctx.wait_idle();
        self.destroy_views();
        if self.handle != vk::SwapchainKHR::null() {
            if let Some(loader) = &self.ctx.swapchain_loader {
                unsafe { loader.destroy_swapchain(self.handle, None) };
            }
        }
    }
}
