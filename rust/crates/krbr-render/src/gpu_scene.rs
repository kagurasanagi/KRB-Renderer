//! シーンを GPU へ転送し、加速構造（BLAS/TLAS）を作る。積分器に依存しない共通の資源

use ash::vk;
use glam::{Mat3, Vec3, Vec4};
use krbr_scene::scene::{BoardMaterialIds, Scene};
use krbr_scene::spectrum;

use crate::env_map::EnvDistribution;
use crate::gpu_types::*;
use crate::image_io::ImageF;
use crate::vk::{as_bytes, image_barrier, memory_barrier, Buffer, Context, Image};

fn align_up(v: u64, a: u64) -> u64 {
    if a == 0 {
        v
    } else {
        v.div_ceil(a) * a
    }
}

const GEOM_USAGE: vk::BufferUsageFlags = vk::BufferUsageFlags::from_raw(vk::BufferUsageFlags::STORAGE_BUFFER.as_raw() | vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR.as_raw());

fn make_sampler(dev: &ash::Device, u: vk::SamplerAddressMode, v: vk::SamplerAddressMode) -> vk::Sampler {
    let sci = vk::SamplerCreateInfo::default().mag_filter(vk::Filter::LINEAR).min_filter(vk::Filter::LINEAR).mipmap_mode(vk::SamplerMipmapMode::NEAREST).address_mode_u(u).address_mode_v(v).address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE).max_lod(0.0);
    unsafe { dev.create_sampler(&sci, None) }.expect("vkCreateSampler")
}

fn to_gpu_materials(materials: &[krbr_scene::Material], bump_intensity: f32) -> Vec<GpuMaterial> {
    let spec = spectrum::loaded_rgb2spec_table();
    materials
        .iter()
        .map(|m| {
            let mut m = m.clone();
            m.bump_strength *= bump_intensity;
            to_gpu(&m, spec)
        })
        .collect()
}

pub struct GpuScene<'a> {
    ctx: &'a Context,
    pub vertices: Buffer,
    pub indices: Buffer,
    pub tri_mats: Buffer,
    pub mesh_infos: Buffer,
    pub materials: Buffer,
    pub instance_infos: Buffer,
    pub env_marginal: Buffer,
    pub env_conditional: Buffer,
    pub env_pdf: Buffer,
    pub rgb2spec: Buffer,
    pub rgb2spec_scale: Buffer,
    pub emitters: Buffer,
    pub emitter_cdf: Buffer,
    pub emitter_count: u32,
    pub emitter_power: f32,
    pub env_average_luminance: f32,
    pub scene_center: Vec3,
    pub scene_radius: f32,
    pub env_image: Image,
    pub board_image: Image,
    pub env_sampler: vk::Sampler,
    pub board_sampler: vk::Sampler,
    pub tlas: vk::AccelerationStructureKHR,
    pub env_dist_w: u32,
    pub env_dist_h: u32,
    pub env_width: u32,
    pub env_height: u32,
    /// 基板テクスチャの対応（ワールド X, Z の原点と 1/大きさ）
    pub board_map: Vec4,
    pub board_mats: BoardMaterialIds,
    pub mask_params: Vec4,
    pub mask_params2: Vec4,
    pub triangle_count: usize,
    blas_buffers: Vec<Buffer>,
    blas: Vec<vk::AccelerationStructureKHR>,
    tlas_buffer: Buffer,
    instance_buffer: Buffer,
    board_image_is_sdf: bool,
}

impl<'a> GpuScene<'a> {
    pub fn new(ctx: &'a Context) -> Self {
        Self {
            ctx,
            vertices: Buffer::default(),
            indices: Buffer::default(),
            tri_mats: Buffer::default(),
            mesh_infos: Buffer::default(),
            materials: Buffer::default(),
            instance_infos: Buffer::default(),
            env_marginal: Buffer::default(),
            env_conditional: Buffer::default(),
            env_pdf: Buffer::default(),
            rgb2spec: Buffer::default(),
            rgb2spec_scale: Buffer::default(),
            emitters: Buffer::default(),
            emitter_cdf: Buffer::default(),
            emitter_count: 0,
            emitter_power: 0.0,
            env_average_luminance: 0.0,
            scene_center: Vec3::ZERO,
            scene_radius: 1.0,
            env_image: Image::default(),
            board_image: Image::default(),
            env_sampler: vk::Sampler::null(),
            board_sampler: vk::Sampler::null(),
            tlas: vk::AccelerationStructureKHR::null(),
            env_dist_w: 0,
            env_dist_h: 0,
            env_width: 0,
            env_height: 0,
            board_map: Vec4::ZERO,
            board_mats: BoardMaterialIds::default(),
            mask_params: Vec4::ZERO,
            mask_params2: Vec4::ZERO,
            triangle_count: 0,
            blas_buffers: Vec::new(),
            blas: Vec::new(),
            tlas_buffer: Buffer::default(),
            instance_buffer: Buffer::default(),
            board_image_is_sdf: false,
        }
    }

    pub fn context(&self) -> &'a Context {
        self.ctx
    }

    pub fn build(&mut self, sc: &Scene, env: &ImageF, dist: &EnvDistribution, bump_intensity: f32) {
        self.build_spectral_table(); // 材質の分光の係数に使うので、形状より先に
        self.build_geometry(sc, bump_intensity);
        self.set_environment(env, dist);
        self.build_board_texture(sc);
        krbr_core::info!(
            "GPU scene: {} triangles, {} BLAS, {} instances, env {}x{} (distribution {}x{})",
            self.triangle_count,
            self.blas.len(),
            sc.instances.len(),
            self.env_width,
            self.env_height,
            self.env_dist_w,
            self.env_dist_h
        );
    }

    /// シーンの形状・配置・材質を作り直す（環境マップはそのまま。積分器は作り直しが必要）
    pub fn rebuild_geometry(&mut self, sc: &Scene, bump_intensity: f32) {
        self.ctx.wait_idle();
        self.destroy_geometry();
        self.build_geometry(sc, bump_intensity);
        let g = &sc.sdf_grid;
        if self.board_image_is_sdf && self.board_image.width == g.width as u32 && self.board_image.height == g.height as u32 {
            self.update_sdf_tiles(sc);
            return;
        }
        let mut old = std::mem::take(&mut self.board_image);
        self.ctx.destroy_image(&mut old);
        self.build_board_texture(sc);
    }

    fn update_sdf_tiles(&mut self, sc: &Scene) {
        if sc.sdf_dirty.is_empty() {
            return;
        }
        let g = &sc.sdf_grid;
        // 作り直したタイルの画素を詰めて 1 回で送る
        let mut staging: Vec<u8> = Vec::new();
        let mut regions: Vec<vk::BufferImageCopy> = Vec::new();
        for &(side, [x0, y0, x1, y1]) in &sc.sdf_dirty {
            if x1 <= x0 || y1 <= y0 {
                continue;
            }
            regions.push(
                vk::BufferImageCopy::default()
                    .buffer_offset(staging.len() as u64)
                    .buffer_row_length((x1 - x0) as u32)
                    .image_subresource(vk::ImageSubresourceLayers { aspect_mask: vk::ImageAspectFlags::COLOR, mip_level: 0, base_array_layer: side as u32, layer_count: 1 })
                    .image_offset(vk::Offset3D { x: x0 as i32, y: y0 as i32, z: 0 })
                    .image_extent(vk::Extent3D { width: (x1 - x0) as u32, height: (y1 - y0) as u32, depth: 1 }),
            );
            for y in y0..y1 {
                let row = (y * g.width + x0) * 2;
                staging.extend_from_slice(&sc.sdf_image[side][row..row + (x1 - x0) * 2]);
            }
        }
        if regions.is_empty() {
            return;
        }
        let mut buf = self.ctx.create_buffer(staging.len() as u64, vk::BufferUsageFlags::TRANSFER_SRC, true, false);
        buf.mapped().unwrap()[..staging.len()].copy_from_slice(&staging);
        let (image, layers, trace, bb) = (self.board_image.image, self.board_image.layers, self.ctx.trace_stages(), buf.buffer);
        self.ctx.submit_and_wait(|dev, cmd| {
            image_barrier(dev, cmd, image, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, vk::ImageLayout::TRANSFER_DST_OPTIMAL, trace, vk::AccessFlags2::SHADER_SAMPLED_READ, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE, layers);
            unsafe { dev.cmd_copy_buffer_to_image(cmd, bb, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &regions) };
            image_barrier(dev, cmd, image, vk::ImageLayout::TRANSFER_DST_OPTIMAL, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL, vk::PipelineStageFlags2::COPY, vk::AccessFlags2::TRANSFER_WRITE, trace, vk::AccessFlags2::SHADER_SAMPLED_READ, layers);
        });
        self.ctx.destroy_buffer(&mut buf);
    }

    fn build_geometry(&mut self, sc: &Scene, bump_intensity: f32) {
        let ctx = self.ctx;
        let dev = &ctx.device;

        // ---- メッシュを 1 本のバッファにまとめる
        let mut verts: Vec<GpuVertex> = Vec::new();
        let mut idx: Vec<u32> = Vec::new();
        let mut tmat: Vec<u32> = Vec::new();
        let mut infos: Vec<GpuMeshInfo> = Vec::new();
        for m in &sc.meshes {
            infos.push(GpuMeshInfo { vertex_offset: verts.len() as u32, index_offset: idx.len() as u32, tri_offset: tmat.len() as u32, pad: 0 });
            for (p, n) in m.mesh.positions.iter().zip(&m.mesh.normals) {
                verts.push(GpuVertex { pos: *p, pad0: 0.0, nrm: *n, pad1: 0.0 });
            }
            idx.extend_from_slice(&m.mesh.indices);
            tmat.extend_from_slice(&m.mesh.tri_material);
        }
        self.triangle_count = tmat.len();
        let mats = to_gpu_materials(&sc.materials, bump_intensity);

        self.vertices = ctx.create_device_buffer(as_bytes(&verts), GEOM_USAGE);
        self.indices = ctx.create_device_buffer(as_bytes(&idx), GEOM_USAGE);
        self.tri_mats = ctx.create_device_buffer(as_bytes(&tmat), vk::BufferUsageFlags::STORAGE_BUFFER);
        self.mesh_infos = ctx.create_device_buffer(as_bytes(&infos), vk::BufferUsageFlags::STORAGE_BUFFER);
        self.materials = ctx.create_device_buffer(as_bytes(&mats), vk::BufferUsageFlags::STORAGE_BUFFER);

        // ---- BLAS（メッシュごと）
        let scratch_align = ctx.as_props.min_acceleration_structure_scratch_offset_alignment as u64;
        let n_mesh = sc.meshes.len();
        let mut geoms: Vec<vk::AccelerationStructureGeometryKHR> = Vec::with_capacity(n_mesh);
        let mut ranges: Vec<vk::AccelerationStructureBuildRangeInfoKHR> = Vec::with_capacity(n_mesh);
        let mut sizes_list: Vec<vk::AccelerationStructureBuildSizesInfoKHR> = Vec::with_capacity(n_mesh);
        let mut scratch_offsets: Vec<u64> = Vec::with_capacity(n_mesh);
        let mut scratch_total = 0u64;
        self.blas = vec![vk::AccelerationStructureKHR::null(); n_mesh];
        self.blas_buffers = Vec::with_capacity(n_mesh);
        for (i, m) in sc.meshes.iter().enumerate() {
            let tri = vk::AccelerationStructureGeometryTrianglesDataKHR::default()
                .vertex_format(vk::Format::R32G32B32_SFLOAT)
                .vertex_data(vk::DeviceOrHostAddressConstKHR { device_address: self.vertices.address + infos[i].vertex_offset as u64 * 32 })
                .vertex_stride(32)
                .max_vertex(m.mesh.positions.len().max(1) as u32 - 1)
                .index_type(vk::IndexType::UINT32)
                .index_data(vk::DeviceOrHostAddressConstKHR { device_address: self.indices.address + infos[i].index_offset as u64 * 4 });
            let geom = vk::AccelerationStructureGeometryKHR::default().geometry_type(vk::GeometryTypeKHR::TRIANGLES).geometry(vk::AccelerationStructureGeometryDataKHR { triangles: tri }).flags(vk::GeometryFlagsKHR::OPAQUE);
            geoms.push(geom);
            let prims = m.mesh.triangle_count() as u32;
            ranges.push(vk::AccelerationStructureBuildRangeInfoKHR::default().primitive_count(prims));
        }
        for i in 0..n_mesh {
            let geom_slice = std::slice::from_ref(&geoms[i]);
            let info = vk::AccelerationStructureBuildGeometryInfoKHR::default()
                .ty(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL)
                .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)
                .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
                .geometries(geom_slice);
            let mut sizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
            unsafe { ctx.as_device.get_acceleration_structure_build_sizes(vk::AccelerationStructureBuildTypeKHR::DEVICE, &info, &[ranges[i].primitive_count], &mut sizes) };
            let buf = ctx.create_buffer(sizes.acceleration_structure_size, vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR, false, false);
            let aci = vk::AccelerationStructureCreateInfoKHR::default().buffer(buf.buffer).size(sizes.acceleration_structure_size).ty(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL);
            self.blas[i] = unsafe { ctx.as_device.create_acceleration_structure(&aci, None) }.expect("vkCreateAccelerationStructureKHR");
            self.blas_buffers.push(buf);
            scratch_offsets.push(scratch_total);
            scratch_total = align_up(scratch_total + sizes.build_scratch_size, scratch_align);
            sizes_list.push(sizes);
        }

        // ---- TLAS（配置ごと）
        let mut inst: Vec<vk::AccelerationStructureInstanceKHR> = Vec::new();
        let mut inst_info: Vec<GpuInstanceInfo> = Vec::new();
        for i in &sc.instances {
            let w2o = Mat3::from_mat4(i.xform).inverse();
            let mut ii = GpuInstanceInfo { mesh: i.mesh, material_base: i.material_base, ..Default::default() };
            for c in 0..3 {
                ii.w2o[c] = w2o.col(c).extend(0.0);
            }
            inst_info.push(ii);
            let mut matrix = [0.0f32; 12];
            for r in 0..3 {
                for c in 0..4 {
                    matrix[r * 4 + c] = i.xform.col(c)[r];
                }
            }
            let addr = unsafe { ctx.as_device.get_acceleration_structure_device_address(&vk::AccelerationStructureDeviceAddressInfoKHR::default().acceleration_structure(self.blas[i.mesh as usize])) };
            inst.push(vk::AccelerationStructureInstanceKHR {
                transform: vk::TransformMatrixKHR { matrix },
                instance_custom_index_and_mask: vk::Packed24_8::new((inst_info.len() - 1) as u32, 0xFF),
                instance_shader_binding_table_record_offset_and_flags: vk::Packed24_8::new(0, vk::GeometryInstanceFlagsKHR::TRIANGLE_FACING_CULL_DISABLE.as_raw() as u8),
                acceleration_structure_reference: vk::AccelerationStructureReferenceKHR { device_handle: addr },
            });
        }
        self.instance_infos = ctx.create_device_buffer(as_bytes(&inst_info), vk::BufferUsageFlags::STORAGE_BUFFER);
        self.instance_buffer = ctx.create_device_buffer(as_bytes(&inst), vk::BufferUsageFlags::ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_KHR);
        let tinst = vk::AccelerationStructureGeometryInstancesDataKHR::default().data(vk::DeviceOrHostAddressConstKHR { device_address: self.instance_buffer.address });
        let tgeom = vk::AccelerationStructureGeometryKHR::default().geometry_type(vk::GeometryTypeKHR::INSTANCES).geometry(vk::AccelerationStructureGeometryDataKHR { instances: tinst });
        let tgeoms = [tgeom];
        let mut tinfo = vk::AccelerationStructureBuildGeometryInfoKHR::default()
            .ty(vk::AccelerationStructureTypeKHR::TOP_LEVEL)
            .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)
            .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
            .geometries(&tgeoms);
        let ninst = inst.len() as u32;
        let mut tsizes = vk::AccelerationStructureBuildSizesInfoKHR::default();
        unsafe { ctx.as_device.get_acceleration_structure_build_sizes(vk::AccelerationStructureBuildTypeKHR::DEVICE, &tinfo, &[ninst], &mut tsizes) };
        self.tlas_buffer = ctx.create_buffer(tsizes.acceleration_structure_size, vk::BufferUsageFlags::ACCELERATION_STRUCTURE_STORAGE_KHR, false, false);
        let taci = vk::AccelerationStructureCreateInfoKHR::default().buffer(self.tlas_buffer.buffer).size(tsizes.acceleration_structure_size).ty(vk::AccelerationStructureTypeKHR::TOP_LEVEL);
        self.tlas = unsafe { ctx.as_device.create_acceleration_structure(&taci, None) }.expect("vkCreateAccelerationStructureKHR(TLAS)");
        tinfo = tinfo.dst_acceleration_structure(self.tlas);
        let tlas_scratch_offset = scratch_total;
        scratch_total = align_up(scratch_total + tsizes.build_scratch_size, scratch_align);

        let mut scratch = ctx.create_buffer(scratch_total + scratch_align, vk::BufferUsageFlags::STORAGE_BUFFER, false, false);
        let scratch_base = align_up(scratch.address, scratch_align);
        let blas = self.blas.clone();
        let as_device = &ctx.as_device;
        let trace = ctx.trace_stages();
        ctx.submit_and_wait(|dev, cmd| {
            let mut infos_b: Vec<vk::AccelerationStructureBuildGeometryInfoKHR> = Vec::with_capacity(n_mesh);
            let mut range_refs: Vec<&[vk::AccelerationStructureBuildRangeInfoKHR]> = Vec::with_capacity(n_mesh);
            for i in 0..n_mesh {
                infos_b.push(
                    vk::AccelerationStructureBuildGeometryInfoKHR::default()
                        .ty(vk::AccelerationStructureTypeKHR::BOTTOM_LEVEL)
                        .flags(vk::BuildAccelerationStructureFlagsKHR::PREFER_FAST_TRACE)
                        .mode(vk::BuildAccelerationStructureModeKHR::BUILD)
                        .geometries(std::slice::from_ref(&geoms[i]))
                        .dst_acceleration_structure(blas[i])
                        .scratch_data(vk::DeviceOrHostAddressKHR { device_address: scratch_base + scratch_offsets[i] }),
                );
                range_refs.push(std::slice::from_ref(&ranges[i]));
            }
            if !infos_b.is_empty() {
                unsafe { as_device.cmd_build_acceleration_structures(cmd, &infos_b, &range_refs) };
            }
            memory_barrier(dev, cmd, vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR, vk::AccessFlags2::ACCELERATION_STRUCTURE_WRITE_KHR, vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR, vk::AccessFlags2::ACCELERATION_STRUCTURE_READ_KHR);
            let tinfo2 = tinfo.scratch_data(vk::DeviceOrHostAddressKHR { device_address: scratch_base + tlas_scratch_offset });
            let trange = [vk::AccelerationStructureBuildRangeInfoKHR::default().primitive_count(ninst)];
            unsafe { as_device.cmd_build_acceleration_structures(cmd, &[tinfo2], &[&trange]) };
            memory_barrier(dev, cmd, vk::PipelineStageFlags2::ACCELERATION_STRUCTURE_BUILD_KHR, vk::AccessFlags2::ACCELERATION_STRUCTURE_WRITE_KHR, trace, vk::AccessFlags2::ACCELERATION_STRUCTURE_READ_KHR);
        });
        ctx.destroy_buffer(&mut scratch);
        let _ = dev;
        self.build_emitters(sc);
    }

    fn build_emitters(&mut self, sc: &Scene) {
        let (mut lo, mut hi) = (Vec3::splat(1e30), Vec3::splat(-1e30));
        let mut em: Vec<GpuEmitter> = Vec::new();
        let mut cdf: Vec<f32> = vec![0.0];
        let mut power = 0.0f64;
        for i in &sc.instances {
            let m = &sc.meshes[i.mesh as usize].mesh;
            for p in &m.positions {
                let w = i.xform.transform_point3(*p);
                lo = lo.min(w);
                hi = hi.max(w);
            }
            for t in 0..m.tri_material.len() {
                let mat = i.material_base + m.tri_material[t];
                let Some(mm) = sc.materials.get(mat as usize) else { continue };
                let e = mm.emission_color * (mm.emission_luminance * EMISSION_SCALE);
                let y = 0.2126 * e.x + 0.7152 * e.y + 0.0722 * e.z;
                if y <= 0.0 {
                    continue;
                }
                let v: Vec<Vec3> = (0..3).map(|k| i.xform.transform_point3(m.positions[m.indices[t * 3 + k] as usize])).collect();
                let area = 0.5 * (v[1] - v[0]).cross(v[2] - v[0]).length();
                if area <= 0.0 {
                    continue;
                }
                em.push(GpuEmitter { p0: v[0].extend(f32::from_bits(mat)), p1: v[1].extend(area), p2: v[2].extend(0.0) });
                power += y as f64 * area as f64;
                cdf.push(power as f32);
            }
        }
        if lo.x > hi.x {
            lo = Vec3::ZERO;
            hi = Vec3::ZERO;
        }
        self.scene_center = 0.5 * (lo + hi);
        self.scene_radius = (0.5 * (hi - lo).length()).max(1.0);
        self.emitter_count = em.len() as u32;
        self.emitter_power = power as f32;
        for c in &mut cdf {
            *c = if power > 0.0 { (*c as f64 / power) as f32 } else { 0.0 };
        }
        if em.is_empty() {
            em.push(GpuEmitter::default());
            cdf.push(1.0);
        }
        self.emitters = self.ctx.create_device_buffer(as_bytes(&em), vk::BufferUsageFlags::STORAGE_BUFFER);
        self.emitter_cdf = self.ctx.create_device_buffer(as_bytes(&cdf), vk::BufferUsageFlags::STORAGE_BUFFER);
        if self.emitter_count > 0 {
            krbr_core::info!("GPU scene: {} emissive triangles", self.emitter_count);
        }
    }

    fn build_board_texture(&mut self, sc: &Scene) {
        let ctx = self.ctx;
        let g = &sc.sdf_grid;
        if g.width > 0 && !sc.sdf_image[0].is_empty() {
            // 距離場（RG8、表・裏の 2 層）
            self.board_image = ctx.create_image_2d(g.width as u32, g.height as u32, 2, vk::Format::R8G8_UNORM, vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST);
            let mut both = sc.sdf_image[0].clone();
            both.extend_from_slice(&sc.sdf_image[1]);
            ctx.upload_image(&self.board_image, &both, vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
            if self.board_sampler == vk::Sampler::null() {
                self.board_sampler = make_sampler(&ctx.device, vk::SamplerAddressMode::CLAMP_TO_EDGE, vk::SamplerAddressMode::CLAMP_TO_EDGE);
            }
            let size_x = g.width as f64 / g.px_per_mm;
            let size_z = g.height as f64 / g.px_per_mm;
            self.board_map = Vec4::new((g.origin.x - sc.board.center.x) as f32, (g.origin.y - sc.board.center.y) as f32, (1.0 / size_x) as f32, (1.0 / size_z) as f32);
            self.board_image_is_sdf = true;
            self.board_mats = sc.board_mats;
            self.mask_params = Vec4::new(sc.mask.on_copper, sc.mask.on_substrate, sc.mask.at_edge, sc.mask.falloff);
            self.mask_params2 = Vec4::new(sc.mask.pool, sc.mask.pool_falloff, sc.mask.copper, 1.0);
            return;
        }
        // 層テクスチャ方式は未対応：1x1 のダミー
        self.board_image_is_sdf = false;
        self.board_image = ctx.create_image_2d(1, 1, 2, vk::Format::R8G8B8A8_UNORM, vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST);
        ctx.upload_image(&self.board_image, &[0, 0, 0, 255, 0, 0, 0, 255], vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        if self.board_sampler == vk::Sampler::null() {
            self.board_sampler = make_sampler(&ctx.device, vk::SamplerAddressMode::CLAMP_TO_EDGE, vk::SamplerAddressMode::CLAMP_TO_EDGE);
        }
        self.board_map = Vec4::new(-sc.board.center.x as f32, -sc.board.center.y as f32, 1.0, 1.0);
        self.board_mats = sc.board_mats;
        self.mask_params = Vec4::new(sc.mask.on_copper, sc.mask.on_substrate, sc.mask.at_edge, sc.mask.falloff);
        self.mask_params2 = Vec4::new(sc.mask.pool, sc.mask.pool_falloff, sc.mask.copper, 0.0);
    }

    /// 環境マップを差し替える（GPU が待機中に呼ぶこと。積分器は作り直しが必要）
    pub fn set_environment(&mut self, env: &ImageF, dist: &EnvDistribution) {
        let ctx = self.ctx;
        ctx.destroy_image(&mut self.env_image);
        ctx.destroy_buffer(&mut self.env_marginal);
        ctx.destroy_buffer(&mut self.env_conditional);
        ctx.destroy_buffer(&mut self.env_pdf);
        self.env_image = ctx.create_image_2d(env.width, env.height, 1, vk::Format::R32G32B32A32_SFLOAT, vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST);
        ctx.upload_image(&self.env_image, as_bytes(&env.rgba), vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL);
        if self.env_sampler == vk::Sampler::null() {
            self.env_sampler = make_sampler(&ctx.device, vk::SamplerAddressMode::REPEAT, vk::SamplerAddressMode::CLAMP_TO_EDGE);
        }
        self.env_marginal = ctx.create_device_buffer(as_bytes(&dist.marginal_cdf), vk::BufferUsageFlags::STORAGE_BUFFER);
        self.env_conditional = ctx.create_device_buffer(as_bytes(&dist.conditional_cdf), vk::BufferUsageFlags::STORAGE_BUFFER);
        self.env_pdf = ctx.create_device_buffer(as_bytes(&dist.pdf), vk::BufferUsageFlags::STORAGE_BUFFER);
        self.env_dist_w = dist.width;
        self.env_dist_h = dist.height;
        self.env_width = env.width;
        self.env_height = env.height;
        // 平均輝度（立体角で重みづけ）
        let (mut sum, mut wsum) = (0.0f64, 0.0f64);
        let mut y = 0;
        while y < env.height {
            let s = ((y as f64 + 0.5) / env.height as f64 * std::f64::consts::PI).sin();
            let mut x = 0;
            while x < env.width {
                let p = &env.rgba[((y * env.width + x) * 4) as usize..];
                sum += s * (0.2126 * p[0] as f64 + 0.7152 * p[1] as f64 + 0.0722 * p[2] as f64);
                wsum += s;
                x += 2;
            }
            y += 2;
        }
        self.env_average_luminance = if wsum > 0.0 { (sum / wsum) as f32 } else { 0.0 };
    }

    fn build_spectral_table(&mut self) {
        // RGB → スペクトルの表。シェーダは vec4（xyz: 係数）の並びとして読む
        let t = spectrum::rgb2spec_table();
        let n = t.data.len() / 3;
        let data: Vec<Vec4> = (0..n).map(|i| Vec4::new(t.data[i * 3], t.data[i * 3 + 1], t.data[i * 3 + 2], 0.0)).collect();
        self.rgb2spec = self.ctx.create_device_buffer(as_bytes(&data), vk::BufferUsageFlags::STORAGE_BUFFER);
        self.rgb2spec_scale = self.ctx.create_device_buffer(as_bytes(&t.scale), vk::BufferUsageFlags::STORAGE_BUFFER);
    }

    /// 材質を送り直す（GPU が待機中に呼ぶこと）
    pub fn update_materials(&mut self, materials: &[krbr_scene::Material], bump_intensity: f32) {
        let mats = to_gpu_materials(materials, bump_intensity);
        let mut buf = std::mem::take(&mut self.materials);
        self.ctx.upload(&mut buf, as_bytes(&mats), 0);
        self.materials = buf;
    }

    fn destroy_geometry(&mut self) {
        let ctx = self.ctx;
        unsafe {
            if self.tlas != vk::AccelerationStructureKHR::null() {
                ctx.as_device.destroy_acceleration_structure(self.tlas, None);
            }
            for a in self.blas.drain(..) {
                if a != vk::AccelerationStructureKHR::null() {
                    ctx.as_device.destroy_acceleration_structure(a, None);
                }
            }
        }
        self.tlas = vk::AccelerationStructureKHR::null();
        for mut b in self.blas_buffers.drain(..) {
            ctx.destroy_buffer(&mut b);
        }
        for b in [&mut self.vertices, &mut self.indices, &mut self.tri_mats, &mut self.mesh_infos, &mut self.materials, &mut self.instance_infos, &mut self.tlas_buffer, &mut self.instance_buffer, &mut self.emitters, &mut self.emitter_cdf] {
            ctx.destroy_buffer(b);
        }
    }
}

impl Drop for GpuScene<'_> {
    fn drop(&mut self) {
        let ctx = self.ctx;
        ctx.wait_idle();
        self.destroy_geometry();
        for b in [&mut self.env_marginal, &mut self.env_conditional, &mut self.env_pdf, &mut self.rgb2spec, &mut self.rgb2spec_scale] {
            ctx.destroy_buffer(b);
        }
        ctx.destroy_image(&mut self.env_image);
        ctx.destroy_image(&mut self.board_image);
        unsafe {
            if self.env_sampler != vk::Sampler::null() {
                ctx.device.destroy_sampler(self.env_sampler, None);
            }
            if self.board_sampler != vk::Sampler::null() {
                ctx.device.destroy_sampler(self.board_sampler, None);
            }
        }
    }
}


