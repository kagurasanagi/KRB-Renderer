#include "render/gpu_scene.hpp"

#include <algorithm>
#include <cstring>

#include "core/log.hpp"
#include "render/gpu_types.hpp"

namespace krbr::render {

namespace {

VkDeviceSize alignUp(VkDeviceSize v, VkDeviceSize a) { return a ? (v + a - 1) / a * a : v; }

constexpr VkBufferUsageFlags kGeomUsage = VK_BUFFER_USAGE_STORAGE_BUFFER_BIT |
                                          VK_BUFFER_USAGE_ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_BIT_KHR;

VkSampler makeSampler(VkDevice dev, VkSamplerAddressMode u, VkSamplerAddressMode v) {
    VkSamplerCreateInfo sci{VK_STRUCTURE_TYPE_SAMPLER_CREATE_INFO};
    sci.magFilter = VK_FILTER_LINEAR;
    sci.minFilter = VK_FILTER_LINEAR;
    sci.mipmapMode = VK_SAMPLER_MIPMAP_MODE_NEAREST;
    sci.addressModeU = u;
    sci.addressModeV = v;
    sci.addressModeW = VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE;
    sci.maxLod = 0.0f;
    VkSampler s;
    vk::check(vkCreateSampler(dev, &sci, nullptr, &s), "vkCreateSampler");
    return s;
}

}  // namespace

GpuScene::~GpuScene() { destroy(); }

namespace {
std::vector<GpuMaterial> toGpuMaterials(const std::vector<scene::Material>& materials, float bumpIntensity) {
    std::vector<GpuMaterial> mats;
    for (auto m : materials) {
        m.bump_strength *= bumpIntensity;
        mats.push_back(toGpu(m));
    }
    return mats;
}
}  // namespace

void GpuScene::build(vk::Context& ctx, const scene::Scene& sc, const io::ImageF& env, const EnvDistribution& dist,
                     float bumpIntensity) {
    ctx_ = &ctx;
    buildGeometry(sc, bumpIntensity);
    setEnvironment(env, dist);
    buildBoardTexture(sc);
    log::info("GPU scene: {} triangles, {} BLAS, {} instances, env {}x{} (distribution {}x{})", triangleCount,
              blas_.size(), sc.instances.size(), envWidth, envHeight, envDistW, envDistH);
}

void GpuScene::rebuildGeometry(const scene::Scene& sc, float bumpIntensity) {
    vkDeviceWaitIdle(ctx_->device());
    destroyGeometry();
    buildGeometry(sc, bumpIntensity);
    const auto& G = sc.layers.grid;
    if (sc.layersAreSdf && sc.sdf && boardImageIsSdf_ && boardImage.width == static_cast<uint32_t>(G.width) &&
        boardImage.height == static_cast<uint32_t>(G.height)) {
        updateSdfTiles(sc);
        return;
    }
    ctx_->destroyImage(boardImage);
    buildBoardTexture(sc);
}

void GpuScene::updateSdfTiles(const scene::Scene& sc) {
    if (sc.sdfDirty.empty()) return;
    const auto& c = *sc.sdf;
    // 作り直したタイルの画素を詰めて 1 回で送る
    std::vector<uint8_t> staging;
    std::vector<VkBufferImageCopy> regions;
    for (const auto& [side, t] : sc.sdfDirty) {
        int x0, y0, x1, y1;
        c.tilePixels(t, x0, y0, x1, y1);
        if (x1 <= x0 || y1 <= y0) continue;
        VkBufferImageCopy r{};
        r.bufferOffset = staging.size();
        r.bufferRowLength = static_cast<uint32_t>(x1 - x0);
        r.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, static_cast<uint32_t>(side), 1};
        r.imageOffset = {x0, y0, 0};
        r.imageExtent = {static_cast<uint32_t>(x1 - x0), static_cast<uint32_t>(y1 - y0), 1};
        regions.push_back(r);
        for (int y = y0; y < y1; ++y) {
            const uint8_t* row = c.sdfImage[side].data() + (static_cast<size_t>(y) * c.sdfGrid.width + x0) * 2;
            staging.insert(staging.end(), row, row + static_cast<size_t>(x1 - x0) * 2);
        }
    }
    if (regions.empty()) return;
    vk::Buffer buf = ctx_->createBuffer(staging.size(), VK_BUFFER_USAGE_TRANSFER_SRC_BIT, true);
    std::memcpy(buf.mapped, staging.data(), staging.size());
    vmaFlushAllocation(ctx_->allocator(), buf.alloc, 0, VK_WHOLE_SIZE);
    ctx_->submitAndWait([&](VkCommandBuffer cmd) {
        vk::imageBarrier(cmd, boardImage.image, VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
                         VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT,
                         VK_ACCESS_2_SHADER_SAMPLED_READ_BIT, VK_PIPELINE_STAGE_2_COPY_BIT,
                         VK_ACCESS_2_TRANSFER_WRITE_BIT, boardImage.layers);
        vkCmdCopyBufferToImage(cmd, buf.buffer, boardImage.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                               static_cast<uint32_t>(regions.size()), regions.data());
        vk::imageBarrier(cmd, boardImage.image, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                         VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL, VK_PIPELINE_STAGE_2_COPY_BIT,
                         VK_ACCESS_2_TRANSFER_WRITE_BIT, VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT,
                         VK_ACCESS_2_SHADER_SAMPLED_READ_BIT, boardImage.layers);
    });
    ctx_->destroyBuffer(buf);
}

void GpuScene::buildGeometry(const scene::Scene& sc, float bumpIntensity) {
    vk::Context& ctx = *ctx_;
    VkDevice dev = ctx.device();

    // ---- メッシュを1本のバッファにまとめる
    std::vector<GpuVertex> verts;
    std::vector<uint32_t> idx, tmat;
    std::vector<GpuMeshInfo> infos;
    for (const auto& m : sc.meshes) {
        GpuMeshInfo info{static_cast<uint32_t>(verts.size()), static_cast<uint32_t>(idx.size()),
                         static_cast<uint32_t>(tmat.size()), 0};
        infos.push_back(info);
        for (size_t i = 0; i < m.mesh.positions.size(); ++i)
            verts.push_back({m.mesh.positions[i], 0.0f, m.mesh.normals[i], 0.0f});
        idx.insert(idx.end(), m.mesh.indices.begin(), m.mesh.indices.end());
        tmat.insert(tmat.end(), m.mesh.triMaterial.begin(), m.mesh.triMaterial.end());
    }
    triangleCount = tmat.size();
    const std::vector<GpuMaterial> mats = toGpuMaterials(sc.materials, bumpIntensity);

    vertices = ctx.createDeviceBuffer(verts.data(), verts.size() * sizeof(GpuVertex), kGeomUsage);
    indices = ctx.createDeviceBuffer(idx.data(), idx.size() * sizeof(uint32_t), kGeomUsage);
    triMats = ctx.createDeviceBuffer(tmat.data(), tmat.size() * sizeof(uint32_t), VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);
    meshInfos =
        ctx.createDeviceBuffer(infos.data(), infos.size() * sizeof(GpuMeshInfo), VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);
    materials = ctx.createDeviceBuffer(mats.data(), mats.size() * sizeof(GpuMaterial), VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);

    // ---- BLAS（メッシュごと）
    const VkDeviceSize scratchAlign = ctx.asProps().minAccelerationStructureScratchOffsetAlignment;
    struct BlasBuild {
        VkAccelerationStructureGeometryKHR geom{VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_GEOMETRY_KHR};
        VkAccelerationStructureBuildGeometryInfoKHR info{VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_BUILD_GEOMETRY_INFO_KHR};
        VkAccelerationStructureBuildRangeInfoKHR range{};
        VkDeviceSize scratchOffset = 0;
    };
    std::vector<BlasBuild> builds(sc.meshes.size());
    VkDeviceSize scratchTotal = 0;
    blas_.resize(sc.meshes.size(), VK_NULL_HANDLE);
    blasBuffers_.resize(sc.meshes.size());
    for (size_t i = 0; i < sc.meshes.size(); ++i) {
        const auto& m = sc.meshes[i].mesh;
        auto& b = builds[i];
        auto& tri = b.geom.geometry.triangles;
        tri.sType = VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_GEOMETRY_TRIANGLES_DATA_KHR;
        tri.vertexFormat = VK_FORMAT_R32G32B32_SFLOAT;
        tri.vertexData.deviceAddress = vertices.address + infos[i].vertexOffset * sizeof(GpuVertex);
        tri.vertexStride = sizeof(GpuVertex);
        tri.maxVertex = static_cast<uint32_t>(std::max<size_t>(m.positions.size(), 1) - 1);
        tri.indexType = VK_INDEX_TYPE_UINT32;
        tri.indexData.deviceAddress = indices.address + infos[i].indexOffset * sizeof(uint32_t);
        b.geom.geometryType = VK_GEOMETRY_TYPE_TRIANGLES_KHR;
        b.geom.flags = VK_GEOMETRY_OPAQUE_BIT_KHR;
        b.info.type = VK_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL_KHR;
        b.info.flags = VK_BUILD_ACCELERATION_STRUCTURE_PREFER_FAST_TRACE_BIT_KHR;
        b.info.mode = VK_BUILD_ACCELERATION_STRUCTURE_MODE_BUILD_KHR;
        b.info.geometryCount = 1;
        b.info.pGeometries = &b.geom;
        const uint32_t prims = static_cast<uint32_t>(m.triangleCount());
        b.range.primitiveCount = prims;
        VkAccelerationStructureBuildSizesInfoKHR sizes{VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_BUILD_SIZES_INFO_KHR};
        vkGetAccelerationStructureBuildSizesKHR(dev, VK_ACCELERATION_STRUCTURE_BUILD_TYPE_DEVICE_KHR, &b.info, &prims,
                                                &sizes);
        blasBuffers_[i] = ctx.createBuffer(sizes.accelerationStructureSize,
                                           VK_BUFFER_USAGE_ACCELERATION_STRUCTURE_STORAGE_BIT_KHR, false);
        VkAccelerationStructureCreateInfoKHR aci{VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_CREATE_INFO_KHR};
        aci.buffer = blasBuffers_[i].buffer;
        aci.size = sizes.accelerationStructureSize;
        aci.type = VK_ACCELERATION_STRUCTURE_TYPE_BOTTOM_LEVEL_KHR;
        vk::check(vkCreateAccelerationStructureKHR(dev, &aci, nullptr, &blas_[i]), "vkCreateAccelerationStructureKHR");
        b.info.dstAccelerationStructure = blas_[i];
        b.scratchOffset = scratchTotal;
        scratchTotal = alignUp(scratchTotal + sizes.buildScratchSize, scratchAlign);
    }

    // ---- TLAS（配置ごと）
    std::vector<VkAccelerationStructureInstanceKHR> inst;
    std::vector<GpuInstanceInfo> instInfo;
    for (const auto& in : sc.instances) {
        instInfo.push_back({in.mesh, in.materialBase, 0, 0});
        VkAccelerationStructureInstanceKHR v{};
        for (int r = 0; r < 3; ++r)
            for (int c = 0; c < 4; ++c) v.transform.matrix[r][c] = in.xform[c][r];
        v.instanceCustomIndex = static_cast<uint32_t>(instInfo.size() - 1);  // GpuInstanceInfo の番号
        v.mask = 0xFF;
        v.instanceShaderBindingTableRecordOffset = 0;
        v.flags = VK_GEOMETRY_INSTANCE_TRIANGLE_FACING_CULL_DISABLE_BIT_KHR;
        VkAccelerationStructureDeviceAddressInfoKHR ai{VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_DEVICE_ADDRESS_INFO_KHR};
        ai.accelerationStructure = blas_[in.mesh];
        v.accelerationStructureReference = vkGetAccelerationStructureDeviceAddressKHR(dev, &ai);
        inst.push_back(v);
    }
    instanceInfos = ctx.createDeviceBuffer(instInfo.data(), instInfo.size() * sizeof(GpuInstanceInfo),
                                           VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);
    instanceBuffer_ = ctx.createDeviceBuffer(inst.data(), inst.size() * sizeof(VkAccelerationStructureInstanceKHR),
                                             VK_BUFFER_USAGE_ACCELERATION_STRUCTURE_BUILD_INPUT_READ_ONLY_BIT_KHR);
    VkAccelerationStructureGeometryKHR tgeom{VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_GEOMETRY_KHR};
    tgeom.geometryType = VK_GEOMETRY_TYPE_INSTANCES_KHR;
    tgeom.geometry.instances.sType = VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_GEOMETRY_INSTANCES_DATA_KHR;
    tgeom.geometry.instances.data.deviceAddress = instanceBuffer_.address;
    VkAccelerationStructureBuildGeometryInfoKHR tinfo{VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_BUILD_GEOMETRY_INFO_KHR};
    tinfo.type = VK_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL_KHR;
    tinfo.flags = VK_BUILD_ACCELERATION_STRUCTURE_PREFER_FAST_TRACE_BIT_KHR;
    tinfo.mode = VK_BUILD_ACCELERATION_STRUCTURE_MODE_BUILD_KHR;
    tinfo.geometryCount = 1;
    tinfo.pGeometries = &tgeom;
    const uint32_t ninst = static_cast<uint32_t>(inst.size());
    VkAccelerationStructureBuildSizesInfoKHR tsizes{VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_BUILD_SIZES_INFO_KHR};
    vkGetAccelerationStructureBuildSizesKHR(dev, VK_ACCELERATION_STRUCTURE_BUILD_TYPE_DEVICE_KHR, &tinfo, &ninst, &tsizes);
    tlasBuffer_ =
        ctx.createBuffer(tsizes.accelerationStructureSize, VK_BUFFER_USAGE_ACCELERATION_STRUCTURE_STORAGE_BIT_KHR, false);
    VkAccelerationStructureCreateInfoKHR taci{VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_CREATE_INFO_KHR};
    taci.buffer = tlasBuffer_.buffer;
    taci.size = tsizes.accelerationStructureSize;
    taci.type = VK_ACCELERATION_STRUCTURE_TYPE_TOP_LEVEL_KHR;
    vk::check(vkCreateAccelerationStructureKHR(dev, &taci, nullptr, &tlas), "vkCreateAccelerationStructureKHR(TLAS)");
    tinfo.dstAccelerationStructure = tlas;
    const VkDeviceSize tlasScratchOffset = scratchTotal;
    scratchTotal = alignUp(scratchTotal + tsizes.buildScratchSize, scratchAlign);

    vk::Buffer scratch = ctx.createBuffer(scratchTotal + scratchAlign, VK_BUFFER_USAGE_STORAGE_BUFFER_BIT, false);
    const VkDeviceAddress scratchBase = alignUp(scratch.address, scratchAlign);
    ctx.submitAndWait([&](VkCommandBuffer cmd) {
        std::vector<VkAccelerationStructureBuildGeometryInfoKHR> infosB;
        std::vector<const VkAccelerationStructureBuildRangeInfoKHR*> ranges;
        for (auto& b : builds) {
            b.info.pGeometries = &b.geom;
            b.info.scratchData.deviceAddress = scratchBase + b.scratchOffset;
            infosB.push_back(b.info);
            ranges.push_back(&b.range);
        }
        if (!infosB.empty())
            vkCmdBuildAccelerationStructuresKHR(cmd, static_cast<uint32_t>(infosB.size()), infosB.data(), ranges.data());
        vk::memoryBarrier(cmd, VK_PIPELINE_STAGE_2_ACCELERATION_STRUCTURE_BUILD_BIT_KHR,
                          VK_ACCESS_2_ACCELERATION_STRUCTURE_WRITE_BIT_KHR,
                          VK_PIPELINE_STAGE_2_ACCELERATION_STRUCTURE_BUILD_BIT_KHR,
                          VK_ACCESS_2_ACCELERATION_STRUCTURE_READ_BIT_KHR);
        tinfo.scratchData.deviceAddress = scratchBase + tlasScratchOffset;
        VkAccelerationStructureBuildRangeInfoKHR trange{};
        trange.primitiveCount = ninst;
        const VkAccelerationStructureBuildRangeInfoKHR* tr = &trange;
        vkCmdBuildAccelerationStructuresKHR(cmd, 1, &tinfo, &tr);
        vk::memoryBarrier(cmd, VK_PIPELINE_STAGE_2_ACCELERATION_STRUCTURE_BUILD_BIT_KHR,
                          VK_ACCESS_2_ACCELERATION_STRUCTURE_WRITE_BIT_KHR, VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT,
                          VK_ACCESS_2_ACCELERATION_STRUCTURE_READ_BIT_KHR);
    });
    ctx.destroyBuffer(scratch);
}

void GpuScene::buildBoardTexture(const scene::Scene& sc) {
    vk::Context& ctx = *ctx_;
    VkDevice dev = ctx.device();
    // ---- 基板の層テクスチャ（2 層の配列：表・裏）
    if (sc.layersAreSdf && sc.sdf) {
        // 距離場（RG8、表・裏の 2 層）
        const auto& G = sc.layers.grid;
        boardImage = ctx.createImage2D(static_cast<uint32_t>(G.width), static_cast<uint32_t>(G.height), 2,
                                       VK_FORMAT_R8G8_UNORM, VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT);
        std::vector<uint8_t> both(sc.sdf->sdfImage[0].size() + sc.sdf->sdfImage[1].size());
        std::memcpy(both.data(), sc.sdf->sdfImage[0].data(), sc.sdf->sdfImage[0].size());
        std::memcpy(both.data() + sc.sdf->sdfImage[0].size(), sc.sdf->sdfImage[1].data(), sc.sdf->sdfImage[1].size());
        ctx.uploadImage(boardImage, both.data(), both.size(), VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
        if (!boardSampler)
            boardSampler = makeSampler(dev, VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE);
        const double sizeX = G.width / G.pxPerMm, sizeZ = G.height / G.pxPerMm;
        boardMap = {static_cast<float>(G.origin.x - sc.board.center.x), static_cast<float>(G.origin.y - sc.board.center.y),
                    static_cast<float>(1.0 / sizeX), static_cast<float>(1.0 / sizeZ)};
        boardImageIsSdf_ = true;
        boardMats = sc.boardMats;
        maskParams = {sc.mask.onCopper, sc.mask.onSubstrate, sc.mask.atEdge, sc.mask.falloff};
        maskParams2 = {sc.mask.pool, sc.mask.poolFalloff, sc.mask.copper, 1.0f};
        return;
    }
    boardImageIsSdf_ = false;
    // 凹凸を形状で作る場合は層テクスチャを使わないので、1x1 のダミーにする
    raster::LayerTextures dummy;
    if (sc.layers.grid.width <= 0 || sc.layers.rgba[0].empty()) {
        dummy.grid.width = dummy.grid.height = 1;
        dummy.grid.pxPerMm = 1.0;
        dummy.rgba[0] = dummy.rgba[1] = {0, 0, 0, 255};
    }
    const auto& L = dummy.grid.width > 0 ? dummy : sc.layers;
    const uint32_t tw = static_cast<uint32_t>(L.grid.width), th = static_cast<uint32_t>(L.grid.height);
    boardImage = ctx.createImage2D(tw, th, 2, VK_FORMAT_R8G8B8A8_UNORM,
                                   VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT);
    std::vector<uint8_t> both(L.rgba[0].size() + L.rgba[1].size());
    std::memcpy(both.data(), L.rgba[0].data(), L.rgba[0].size());
    std::memcpy(both.data() + L.rgba[0].size(), L.rgba[1].data(), L.rgba[1].size());
    ctx.uploadImage(boardImage, both.data(), both.size(), VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    if (!boardSampler)
        boardSampler = makeSampler(dev, VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE);
    const double sizeX = L.grid.width / L.grid.pxPerMm, sizeZ = L.grid.height / L.grid.pxPerMm;
    boardMap = {static_cast<float>(L.grid.origin.x - sc.board.center.x),
                static_cast<float>(L.grid.origin.y - sc.board.center.y), static_cast<float>(1.0 / sizeX),
                static_cast<float>(1.0 / sizeZ)};
    boardMats = sc.boardMats;
    maskParams = {sc.mask.onCopper, sc.mask.onSubstrate, sc.mask.atEdge, sc.mask.falloff};
    maskParams2 = {sc.mask.pool, sc.mask.poolFalloff, sc.mask.copper, sc.layersAreSdf ? 1.0f : 0.0f};
}

void GpuScene::setEnvironment(const io::ImageF& env, const EnvDistribution& dist) {
    vk::Context& ctx = *ctx_;
    VkDevice dev = ctx.device();
    ctx.destroyImage(envImage);
    ctx.destroyBuffer(envMarginal);
    ctx.destroyBuffer(envConditional);
    ctx.destroyBuffer(envPdf);
    envImage = ctx.createImage2D(env.width, env.height, 1, VK_FORMAT_R32G32B32A32_SFLOAT,
                                 VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT);
    ctx.uploadImage(envImage, env.rgba.data(), env.rgba.size() * sizeof(float), VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    if (!envSampler) envSampler = makeSampler(dev, VK_SAMPLER_ADDRESS_MODE_REPEAT, VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE);
    envMarginal = ctx.createDeviceBuffer(dist.marginalCdf.data(), dist.marginalCdf.size() * sizeof(float),
                                         VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);
    envConditional = ctx.createDeviceBuffer(dist.conditionalCdf.data(), dist.conditionalCdf.size() * sizeof(float),
                                            VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);
    envPdf = ctx.createDeviceBuffer(dist.pdf.data(), dist.pdf.size() * sizeof(float), VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);
    envDistW = dist.width;
    envDistH = dist.height;
    envWidth = static_cast<uint32_t>(env.width);
    envHeight = static_cast<uint32_t>(env.height);
}

void GpuScene::updateMaterials(const std::vector<scene::Material>& materials, float bumpIntensity) {
    const std::vector<GpuMaterial> mats = toGpuMaterials(materials, bumpIntensity);
    ctx_->upload(this->materials, mats.data(), mats.size() * sizeof(GpuMaterial));
}

void GpuScene::destroyGeometry() {
    VkDevice dev = ctx_->device();
    if (tlas) vkDestroyAccelerationStructureKHR(dev, tlas, nullptr);
    tlas = VK_NULL_HANDLE;
    for (auto a : blas_)
        if (a) vkDestroyAccelerationStructureKHR(dev, a, nullptr);
    blas_.clear();
    for (auto& b : blasBuffers_) ctx_->destroyBuffer(b);
    blasBuffers_.clear();
    for (vk::Buffer* b : {&vertices, &indices, &triMats, &meshInfos, &materials, &instanceInfos, &tlasBuffer_,
                          &instanceBuffer_})
        ctx_->destroyBuffer(*b);
}

void GpuScene::destroy() {
    if (!ctx_) return;
    VkDevice dev = ctx_->device();
    vkDeviceWaitIdle(dev);
    destroyGeometry();
    for (vk::Buffer* b : {&envMarginal, &envConditional, &envPdf}) ctx_->destroyBuffer(*b);
    ctx_->destroyImage(envImage);
    ctx_->destroyImage(boardImage);
    if (envSampler) vkDestroySampler(dev, envSampler, nullptr);
    if (boardSampler) vkDestroySampler(dev, boardSampler, nullptr);
    envSampler = boardSampler = VK_NULL_HANDLE;
    ctx_ = nullptr;
}

}  // namespace krbr::render
