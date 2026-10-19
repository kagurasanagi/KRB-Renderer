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

void GpuScene::build(vk::Context& ctx, const scene::Scene& sc, const io::ImageF& env, const EnvDistribution& dist) {
    ctx_ = &ctx;
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
    std::vector<GpuMaterial> mats;
    for (const auto& m : sc.materials) mats.push_back(toGpu(m));

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
    for (const auto& in : sc.instances) {
        VkAccelerationStructureInstanceKHR v{};
        for (int r = 0; r < 3; ++r)
            for (int c = 0; c < 4; ++c) v.transform.matrix[r][c] = in.xform[c][r];
        v.instanceCustomIndex = in.mesh;
        v.mask = 0xFF;
        v.instanceShaderBindingTableRecordOffset = 0;
        v.flags = VK_GEOMETRY_INSTANCE_TRIANGLE_FACING_CULL_DISABLE_BIT_KHR;
        VkAccelerationStructureDeviceAddressInfoKHR ai{VK_STRUCTURE_TYPE_ACCELERATION_STRUCTURE_DEVICE_ADDRESS_INFO_KHR};
        ai.accelerationStructure = blas_[in.mesh];
        v.accelerationStructureReference = vkGetAccelerationStructureDeviceAddressKHR(dev, &ai);
        inst.push_back(v);
    }
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

    // ---- 環境マップ
    envImage = ctx.createImage2D(env.width, env.height, 1, VK_FORMAT_R32G32B32A32_SFLOAT,
                                 VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT);
    ctx.uploadImage(envImage, env.rgba.data(), env.rgba.size() * sizeof(float), VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    envSampler = makeSampler(dev, VK_SAMPLER_ADDRESS_MODE_REPEAT, VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE);
    envMarginal = ctx.createDeviceBuffer(dist.marginalCdf.data(), dist.marginalCdf.size() * sizeof(float),
                                         VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);
    envConditional = ctx.createDeviceBuffer(dist.conditionalCdf.data(), dist.conditionalCdf.size() * sizeof(float),
                                            VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);
    envPdf = ctx.createDeviceBuffer(dist.pdf.data(), dist.pdf.size() * sizeof(float), VK_BUFFER_USAGE_STORAGE_BUFFER_BIT);
    envDistW = dist.width;
    envDistH = dist.height;

    // ---- 基板の層テクスチャ（2 層の配列：表・裏）
    const auto& L = sc.layers;
    const uint32_t tw = static_cast<uint32_t>(L.grid.width), th = static_cast<uint32_t>(L.grid.height);
    boardImage = ctx.createImage2D(tw, th, 2, VK_FORMAT_R8G8B8A8_UNORM,
                                   VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT);
    std::vector<uint8_t> both(L.rgba[0].size() + L.rgba[1].size());
    std::memcpy(both.data(), L.rgba[0].data(), L.rgba[0].size());
    std::memcpy(both.data() + L.rgba[0].size(), L.rgba[1].data(), L.rgba[1].size());
    ctx.uploadImage(boardImage, both.data(), both.size(), VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL);
    boardSampler = makeSampler(dev, VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE, VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE);
    const double sizeX = L.grid.width / L.grid.pxPerMm, sizeZ = L.grid.height / L.grid.pxPerMm;
    boardMap = {static_cast<float>(L.grid.origin.x - sc.board.center.x),
                static_cast<float>(L.grid.origin.y - sc.board.center.y), static_cast<float>(1.0 / sizeX),
                static_cast<float>(1.0 / sizeZ)};
    boardMats = sc.boardMats;

    log::info("GPU scene: {} triangles, {} BLAS, {} instances, env {}x{} (distribution {}x{})", triangleCount,
              blas_.size(), inst.size(), env.width, env.height, envDistW, envDistH);
}

void GpuScene::destroy() {
    if (!ctx_) return;
    VkDevice dev = ctx_->device();
    vkDeviceWaitIdle(dev);
    if (tlas) vkDestroyAccelerationStructureKHR(dev, tlas, nullptr);
    tlas = VK_NULL_HANDLE;
    for (auto a : blas_)
        if (a) vkDestroyAccelerationStructureKHR(dev, a, nullptr);
    blas_.clear();
    for (auto& b : blasBuffers_) ctx_->destroyBuffer(b);
    blasBuffers_.clear();
    for (vk::Buffer* b : {&vertices, &indices, &triMats, &meshInfos, &materials, &envMarginal, &envConditional, &envPdf,
                          &tlasBuffer_, &instanceBuffer_})
        ctx_->destroyBuffer(*b);
    ctx_->destroyImage(envImage);
    ctx_->destroyImage(boardImage);
    if (envSampler) vkDestroySampler(dev, envSampler, nullptr);
    if (boardSampler) vkDestroySampler(dev, boardSampler, nullptr);
    envSampler = boardSampler = VK_NULL_HANDLE;
    ctx_ = nullptr;
}

}  // namespace krbr::render
