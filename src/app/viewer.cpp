// リアルタイム表示（ウィンドウ）。オフライン描画と同じシーン・積分器で、フレームごとにサンプルを積み増して表示する
#include <volk.h>
#define GLFW_INCLUDE_VULKAN
#include <GLFW/glfw3.h>

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstring>

#include "app/commands.hpp"
#include "app/prepare.hpp"
#include "app/report.hpp"
#include "core/log.hpp"
#include "core/platform.hpp"
#include "render/gpu_scene.hpp"
#include "render/progressive.hpp"
#include "render/tonemapper.hpp"

namespace krbr::app {

namespace fs = std::filesystem;
using Clock = std::chrono::steady_clock;

namespace {

struct Swapchain {
    vk::Context* ctx = nullptr;
    VkSwapchainKHR handle = VK_NULL_HANDLE;
    VkFormat format = VK_FORMAT_UNDEFINED;
    VkExtent2D extent{};
    std::vector<VkImage> images;
    std::vector<VkSemaphore> renderDone;  // 画像ごと（表示が終わるまで再利用しない）

    void create(vk::Context& c, uint32_t w, uint32_t h) {
        ctx = &c;
        VkPhysicalDevice pd = c.physical();
        VkSurfaceKHR surf = c.surface();
        VkSurfaceCapabilitiesKHR caps;
        vk::check(vkGetPhysicalDeviceSurfaceCapabilitiesKHR(pd, surf, &caps), "vkGetPhysicalDeviceSurfaceCapabilitiesKHR");
        uint32_t n = 0;
        vkGetPhysicalDeviceSurfaceFormatsKHR(pd, surf, &n, nullptr);
        std::vector<VkSurfaceFormatKHR> fmts(n);
        vkGetPhysicalDeviceSurfaceFormatsKHR(pd, surf, &n, fmts.data());
        // sRGB への変換はトーンマッピングのシェーダで済ませるので、UNORM の形式を選ぶ
        VkSurfaceFormatKHR chosen = fmts.empty() ? VkSurfaceFormatKHR{} : fmts[0];
        for (const auto& f : fmts)
            if ((f.format == VK_FORMAT_B8G8R8A8_UNORM || f.format == VK_FORMAT_R8G8B8A8_UNORM) &&
                f.colorSpace == VK_COLOR_SPACE_SRGB_NONLINEAR_KHR) {
                chosen = f;
                break;
            }
        format = chosen.format;
        extent = caps.currentExtent.width != UINT32_MAX ? caps.currentExtent : VkExtent2D{w, h};
        extent.width = std::clamp(extent.width, caps.minImageExtent.width, caps.maxImageExtent.width);
        extent.height = std::clamp(extent.height, caps.minImageExtent.height, caps.maxImageExtent.height);
        if (!(caps.supportedUsageFlags & VK_IMAGE_USAGE_TRANSFER_DST_BIT))
            throw std::runtime_error("swapchain images cannot be a transfer destination");
        uint32_t count = caps.minImageCount + 1;
        if (caps.maxImageCount) count = std::min(count, caps.maxImageCount);

        VkSwapchainKHR old = handle;
        VkSwapchainCreateInfoKHR ci{VK_STRUCTURE_TYPE_SWAPCHAIN_CREATE_INFO_KHR};
        ci.surface = surf;
        ci.minImageCount = count;
        ci.imageFormat = chosen.format;
        ci.imageColorSpace = chosen.colorSpace;
        ci.imageExtent = extent;
        ci.imageArrayLayers = 1;
        ci.imageUsage = VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT;
        ci.imageSharingMode = VK_SHARING_MODE_EXCLUSIVE;
        ci.preTransform = caps.currentTransform;
        ci.compositeAlpha = VK_COMPOSITE_ALPHA_OPAQUE_BIT_KHR;
        ci.presentMode = VK_PRESENT_MODE_FIFO_KHR;  // 垂直同期（必要以上に回さない）
        ci.clipped = VK_TRUE;
        ci.oldSwapchain = old;
        vk::check(vkCreateSwapchainKHR(c.device(), &ci, nullptr, &handle), "vkCreateSwapchainKHR");
        if (old) vkDestroySwapchainKHR(c.device(), old, nullptr);
        vkGetSwapchainImagesKHR(c.device(), handle, &n, nullptr);
        images.resize(n);
        vkGetSwapchainImagesKHR(c.device(), handle, &n, images.data());
        for (auto s : renderDone) vkDestroySemaphore(c.device(), s, nullptr);
        renderDone.resize(n);
        VkSemaphoreCreateInfo sci{VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
        for (auto& s : renderDone) vk::check(vkCreateSemaphore(c.device(), &sci, nullptr, &s), "vkCreateSemaphore");
    }

    void destroy() {
        if (!ctx) return;
        for (auto s : renderDone) vkDestroySemaphore(ctx->device(), s, nullptr);
        renderDone.clear();
        if (handle) vkDestroySwapchainKHR(ctx->device(), handle, nullptr);
        handle = VK_NULL_HANDLE;
        ctx = nullptr;
    }
};

// 入力で変わる表示の状態
struct ViewState {
    glm::vec3 target{0.0f};
    float distance = 100.0f;
    double elevation = 35.0, azimuth = 45.0, fov = 30.0;
    float exposure = 0.0f;
    float envRotationDeg = 0.0f;
    render::DisplayMode mode = render::DisplayMode::Beauty;
    bool dirty = true;  // カメラ・照明が変わった（累積をやり直す）
    bool saveRequested = false;
    bool resetRequested = false;

    // マウス
    bool rotating = false, panning = false;
    double lastX = 0, lastY = 0;
    int fbHeight = 1;
    float tanHalfFovY = 0.27f;
};

void onMouseButton(GLFWwindow* w, int button, int action, int) {
    auto* s = static_cast<ViewState*>(glfwGetWindowUserPointer(w));
    const bool down = action == GLFW_PRESS;
    if (button == GLFW_MOUSE_BUTTON_LEFT) s->rotating = down;
    if (button == GLFW_MOUSE_BUTTON_RIGHT || button == GLFW_MOUSE_BUTTON_MIDDLE) s->panning = down;
    glfwGetCursorPos(w, &s->lastX, &s->lastY);
}

void onCursor(GLFWwindow* w, double x, double y) {
    auto* s = static_cast<ViewState*>(glfwGetWindowUserPointer(w));
    const double dx = x - s->lastX, dy = y - s->lastY;
    s->lastX = x;
    s->lastY = y;
    if (s->rotating) {
        s->azimuth -= dx * 0.3;
        s->elevation = std::clamp(s->elevation + dy * 0.3, -89.0, 89.0);
        s->dirty = true;
    } else if (s->panning) {
        const scene::Camera c =
            scene::orbitCamera(s->target, s->distance, s->elevation, s->azimuth, s->fov, 1.0f);
        const float scale = 2.0f * s->distance * s->tanHalfFovY / static_cast<float>(std::max(1, s->fbHeight));
        s->target += (-c.right * static_cast<float>(dx) + c.up * static_cast<float>(dy)) * scale;
        s->dirty = true;
    }
}

void onScroll(GLFWwindow* w, double, double yoff) {
    auto* s = static_cast<ViewState*>(glfwGetWindowUserPointer(w));
    s->distance = std::max(1.0f, s->distance * static_cast<float>(std::pow(0.9, yoff)));
    s->dirty = true;
}

void onKey(GLFWwindow* w, int key, int, int action, int) {
    if (action != GLFW_PRESS && action != GLFW_REPEAT) return;
    auto* s = static_cast<ViewState*>(glfwGetWindowUserPointer(w));
    switch (key) {
        case GLFW_KEY_ESCAPE: glfwSetWindowShouldClose(w, GLFW_TRUE); break;
        case GLFW_KEY_R:
        case GLFW_KEY_F: s->resetRequested = true; break;
        case GLFW_KEY_S: s->saveRequested = true; break;
        case GLFW_KEY_EQUAL:
        case GLFW_KEY_KP_ADD: s->exposure += 0.25f; break;  // 露出は表示だけなので累積はやり直さない
        case GLFW_KEY_MINUS:
        case GLFW_KEY_KP_SUBTRACT: s->exposure -= 0.25f; break;
        case GLFW_KEY_LEFT_BRACKET:
            s->envRotationDeg -= 15.0f;
            s->dirty = true;
            break;
        case GLFW_KEY_RIGHT_BRACKET:
            s->envRotationDeg += 15.0f;
            s->dirty = true;
            break;
        case GLFW_KEY_1: s->mode = render::DisplayMode::Beauty; break;
        case GLFW_KEY_2: s->mode = render::DisplayMode::Albedo; break;
        case GLFW_KEY_3: s->mode = render::DisplayMode::Normal; break;
        default: break;
    }
}

// 表示中の RGBA8 画像を読み戻して PNG に保存する
bool saveDisplayImage(vk::Context& ctx, const vk::Image& img, const fs::path& p) {
    const VkDeviceSize bytes = static_cast<VkDeviceSize>(img.width) * img.height * 4;
    vk::Buffer rb = ctx.createBuffer(bytes, VK_BUFFER_USAGE_TRANSFER_DST_BIT, true, true);
    ctx.submitAndWait([&](VkCommandBuffer cmd) {
        VkBufferImageCopy region{};
        region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
        region.imageExtent = {img.width, img.height, 1};
        vkCmdCopyImageToBuffer(cmd, img.image, VK_IMAGE_LAYOUT_GENERAL, rb.buffer, 1, &region);
    });
    vmaInvalidateAllocation(ctx.allocator(), rb.alloc, 0, VK_WHOLE_SIZE);
    std::string err;
    const bool ok = io::savePngRgba8(p, static_cast<int>(img.width), static_cast<int>(img.height),
                                     static_cast<const unsigned char*>(rb.mapped), err);
    ctx.destroyBuffer(rb);
    if (!ok) log::error("{}: {}", platform::pathToUtf8(p), err);
    return ok;
}

}  // namespace

int cmdView(const ViewOptions& vo) {
    const RenderOptions& o = vo.common;
    if (!o.normalPriority) platform::setLowPriority();
    const bool hidden = vo.hidden || vo.testFrames > 0;

    auto prep = prepareScene(o);
    if (!prep) return 1;
    ProjectConfig& cfg = prep->cfg;
    // ビューアでは --spp を「ここまで積んだら止める」上限として使う（既定 1024）
    const uint32_t maxSpp = static_cast<uint32_t>(o.spp ? *o.spp : 1024);
    const int winW = o.width ? *o.width : 1600, winH = o.height ? *o.height : 900;

    glfwSetErrorCallback([](int code, const char* msg) { log::error("GLFW {}: {}", code, msg); });
    if (!glfwInit()) return 1;
    if (!glfwVulkanSupported()) {
        log::error("GLFW: Vulkan not supported");
        glfwTerminate();
        return 1;
    }
    glfwWindowHint(GLFW_CLIENT_API, GLFW_NO_API);
    if (hidden) {
        // 開発用のテスト：画面に出さず、フォーカスも奪わない
        glfwWindowHint(GLFW_VISIBLE, GLFW_FALSE);
        glfwWindowHint(GLFW_FOCUS_ON_SHOW, GLFW_FALSE);
        glfwWindowHint(GLFW_FOCUSED, GLFW_FALSE);
    }
    const std::string baseTitle = "krbr - " + prep->board.projectName;
    GLFWwindow* window = glfwCreateWindow(winW, winH, baseTitle.c_str(), nullptr, nullptr);
    if (!window) {
        glfwTerminate();
        return 1;
    }

    ViewState st;
    glfwSetWindowUserPointer(window, &st);
    glfwSetMouseButtonCallback(window, onMouseButton);
    glfwSetCursorPosCallback(window, onCursor);
    glfwSetScrollCallback(window, onScroll);
    glfwSetKeyCallback(window, onKey);

    int rc = 0;
    try {
        vk::ContextOptions co;
        co.validation = o.validate;
        uint32_t nExt = 0;
        const char** ext = glfwGetRequiredInstanceExtensions(&nExt);
        co.instanceExtensions.assign(ext, ext + nExt);
        co.createSurface = [window](VkInstance inst) {
            VkSurfaceKHR s = VK_NULL_HANDLE;
            return glfwCreateWindowSurface(inst, window, nullptr, &s) == VK_SUCCESS ? s : VK_NULL_HANDLE;
        };
        vk::Context ctx;
        ctx.init(co);
        render::GpuScene gpu;
        gpu.build(ctx, prep->scene, prep->env, prep->dist);
        prep->env.rgba.clear();
        prep->env.rgba.shrink_to_fit();
        writeMissingReport(prep->scene.missing, prep->board.file, {});  // ビューアではログだけ

        int fbw = 0, fbh = 0;
        glfwGetFramebufferSize(window, &fbw, &fbh);
        fbw = std::max(fbw, 1);
        fbh = std::max(fbh, 1);

        // 初期のカメラ（オフライン描画と同じ自動調整）
        auto resetCamera = [&](int w, int h) {
            const scene::Camera c = initialCamera(*prep, o, w, h);
            st.target = c.target;
            st.distance = c.distance;
            st.elevation = cfg.elevationDeg;
            st.azimuth = cfg.azimuthDeg;
            st.fov = cfg.fovDeg;
            st.tanHalfFovY = c.tanHalfFovY;
            st.dirty = true;
        };
        st.exposure = cfg.exposureEv;
        st.envRotationDeg = cfg.envRotationDeg;
        resetCamera(fbw, fbh);

        auto currentFrame = [&](int w, int h) {
            ProjectConfig c = cfg;
            c.envRotationDeg = st.envRotationDeg;
            const scene::Camera cam = scene::orbitCamera(st.target, st.distance, st.elevation, st.azimuth, st.fov,
                                                         static_cast<float>(w) / static_cast<float>(h));
            return frameParams(c, cam, o.seed);
        };

        Swapchain swap;
        swap.create(ctx, static_cast<uint32_t>(fbw), static_cast<uint32_t>(fbh));
        render::Progressive prog;
        prog.init(ctx, gpu, cfg.integrator, swap.extent.width, swap.extent.height,
                  currentFrame(static_cast<int>(swap.extent.width), static_cast<int>(swap.extent.height)));
        render::Tonemapper tone;
        tone.init(ctx, prog.targets());

        VkDevice dev = ctx.device();
        VkCommandBufferAllocateInfo cai{VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO};
        cai.commandPool = ctx.commandPool();
        cai.level = VK_COMMAND_BUFFER_LEVEL_PRIMARY;
        cai.commandBufferCount = 1;
        VkCommandBuffer cmd;
        vk::check(vkAllocateCommandBuffers(dev, &cai, &cmd), "vkAllocateCommandBuffers");
        VkSemaphore acquired;
        VkSemaphoreCreateInfo sci{VK_STRUCTURE_TYPE_SEMAPHORE_CREATE_INFO};
        vk::check(vkCreateSemaphore(dev, &sci, nullptr, &acquired), "vkCreateSemaphore");
        VkFence fence;
        VkFenceCreateInfo fci{VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
        vk::check(vkCreateFence(dev, &fci, nullptr, &fence), "vkCreateFence");
        // サンプルを積む処理だけの GPU 時間（垂直同期の待ちを含めない）を測るタイムスタンプ
        VkQueryPool timestamps;
        VkQueryPoolCreateInfo qci{VK_STRUCTURE_TYPE_QUERY_POOL_CREATE_INFO};
        qci.queryType = VK_QUERY_TYPE_TIMESTAMP;
        qci.queryCount = 2;
        vk::check(vkCreateQueryPool(dev, &qci, nullptr, &timestamps), "vkCreateQueryPool");
        VkPhysicalDeviceProperties devProps;
        vkGetPhysicalDeviceProperties(ctx.physical(), &devProps);
        const double tsPeriodMs = devProps.limits.timestampPeriod * 1e-6;

        log::info("viewer: {}x{}, up to {} spp. mouse: L=rotate, R/M=pan, wheel=zoom; keys: R=reset, +/-=exposure, "
                  "[ ]=rotate environment, 1/2/3=image/albedo/normal, S=save, Esc=quit",
                  swap.extent.width, swap.extent.height, maxSpp);

        uint32_t samplesPerFrame = 1;
        const double targetFrameMs = 10.0;  // 60fps（16.7 ms）に収まるよう、表示処理の分を残す
        bool needRecreate = false;
        int frames = 0, saveIndex = 0;
        auto lastTitle = Clock::now();
        double lastFrameMs = 0.0, lastGpuMs = 0.0;

        const fs::path outPng = platform::u8path(o.out);
        const fs::path outDir = outPng.has_parent_path() ? outPng.parent_path() : fs::path(".");
        const std::string stem = platform::pathToUtf8(outPng.stem());

        while (!glfwWindowShouldClose(window)) {
            const bool converged = prog.samples() >= maxSpp && !st.dirty;
            if (converged && vo.testFrames == 0)
                glfwWaitEventsTimeout(0.25);  // 収束したら GPU を休ませ、入力を待つ
            else
                glfwPollEvents();

            glfwGetFramebufferSize(window, &fbw, &fbh);
            if (fbw == 0 || fbh == 0) {  // 最小化中
                glfwWaitEvents();
                continue;
            }
            st.fbHeight = fbh;
            if (vo.testOrbitDeg != 0.0) {
                st.azimuth += vo.testOrbitDeg;
                st.dirty = true;
            }
            if (st.resetRequested) {
                resetCamera(fbw, fbh);
                st.resetRequested = false;
            }
            if (needRecreate || static_cast<uint32_t>(fbw) != swap.extent.width ||
                static_cast<uint32_t>(fbh) != swap.extent.height) {
                vkDeviceWaitIdle(dev);
                swap.create(ctx, static_cast<uint32_t>(fbw), static_cast<uint32_t>(fbh));
                prog.resize(swap.extent.width, swap.extent.height);
                tone.init(ctx, prog.targets());
                needRecreate = false;
                st.dirty = true;
            }
            if (st.dirty) {
                prog.setFrame(currentFrame(static_cast<int>(swap.extent.width), static_cast<int>(swap.extent.height)));
                st.dirty = false;
            }

            uint32_t imageIndex = 0;
            VkResult ar = vkAcquireNextImageKHR(dev, swap.handle, UINT64_MAX, acquired, VK_NULL_HANDLE, &imageIndex);
            if (ar == VK_ERROR_OUT_OF_DATE_KHR) {
                needRecreate = true;
                continue;
            }
            if (ar != VK_SUCCESS && ar != VK_SUBOPTIMAL_KHR) vk::check(ar, "vkAcquireNextImageKHR");

            const uint32_t add = prog.samples() >= maxSpp ? 0 : std::min(samplesPerFrame, maxSpp - prog.samples());
            vkResetCommandBuffer(cmd, 0);
            VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
            bi.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
            vkBeginCommandBuffer(cmd, &bi);
            vkCmdResetQueryPool(cmd, timestamps, 0, 2);
            vkCmdWriteTimestamp2(cmd, VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT, timestamps, 0);
            prog.record(cmd, add);
            vkCmdWriteTimestamp2(cmd, VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT, timestamps, 1);
            tone.record(cmd, st.exposure, st.mode);
            const VkImage dispImg = tone.output().image, swapImg = swap.images[imageIndex];
            vk::imageBarrier(cmd, dispImg, VK_IMAGE_LAYOUT_GENERAL, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                             VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT, VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT,
                             VK_PIPELINE_STAGE_2_BLIT_BIT, VK_ACCESS_2_TRANSFER_READ_BIT);
            vk::imageBarrier(cmd, swapImg, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                             VK_PIPELINE_STAGE_2_BLIT_BIT, 0, VK_PIPELINE_STAGE_2_BLIT_BIT,
                             VK_ACCESS_2_TRANSFER_WRITE_BIT);
            VkImageBlit blit{};
            blit.srcSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
            blit.srcOffsets[1] = {static_cast<int32_t>(tone.output().width), static_cast<int32_t>(tone.output().height), 1};
            blit.dstSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
            blit.dstOffsets[1] = {static_cast<int32_t>(swap.extent.width), static_cast<int32_t>(swap.extent.height), 1};
            vkCmdBlitImage(cmd, dispImg, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, swapImg,
                           VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &blit, VK_FILTER_NEAREST);
            vk::imageBarrier(cmd, swapImg, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
                             VK_PIPELINE_STAGE_2_BLIT_BIT, VK_ACCESS_2_TRANSFER_WRITE_BIT,
                             VK_PIPELINE_STAGE_2_BOTTOM_OF_PIPE_BIT, 0);
            vk::imageBarrier(cmd, dispImg, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_IMAGE_LAYOUT_GENERAL,
                             VK_PIPELINE_STAGE_2_BLIT_BIT, VK_ACCESS_2_TRANSFER_READ_BIT,
                             VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT | VK_PIPELINE_STAGE_2_COPY_BIT,
                             VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT | VK_ACCESS_2_TRANSFER_READ_BIT);
            vk::check(vkEndCommandBuffer(cmd), "vkEndCommandBuffer");

            const VkPipelineStageFlags waitStage = VK_PIPELINE_STAGE_TRANSFER_BIT;
            VkSubmitInfo si{VK_STRUCTURE_TYPE_SUBMIT_INFO};
            si.waitSemaphoreCount = 1;
            si.pWaitSemaphores = &acquired;
            si.pWaitDstStageMask = &waitStage;
            si.commandBufferCount = 1;
            si.pCommandBuffers = &cmd;
            si.signalSemaphoreCount = 1;
            si.pSignalSemaphores = &swap.renderDone[imageIndex];
            vkResetFences(dev, 1, &fence);
            const auto t0 = Clock::now();
            vk::check(vkQueueSubmit(ctx.queue(), 1, &si, fence), "vkQueueSubmit");
            VkPresentInfoKHR pi{VK_STRUCTURE_TYPE_PRESENT_INFO_KHR};
            pi.waitSemaphoreCount = 1;
            pi.pWaitSemaphores = &swap.renderDone[imageIndex];
            pi.swapchainCount = 1;
            pi.pSwapchains = &swap.handle;
            pi.pImageIndices = &imageIndex;
            const VkResult pr = vkQueuePresentKHR(ctx.queue(), &pi);
            if (pr == VK_ERROR_OUT_OF_DATE_KHR || pr == VK_SUBOPTIMAL_KHR) needRecreate = true;
            else if (pr != VK_SUCCESS) vk::check(pr, "vkQueuePresentKHR");
            vk::check(vkWaitForFences(dev, 1, &fence, VK_TRUE, UINT64_MAX), "vkWaitForFences");
            lastFrameMs = std::chrono::duration<double, std::milli>(Clock::now() - t0).count();
            ++frames;

            // サンプルを積む GPU 時間が目標に収まるように、1 フレームに積む数を調整する
            uint64_t ts[2] = {0, 0};
            const bool tsOk = vkGetQueryPoolResults(dev, timestamps, 0, 2, sizeof(ts), ts, sizeof(uint64_t),
                                                    VK_QUERY_RESULT_64_BIT) == VK_SUCCESS;
            lastGpuMs = tsOk ? static_cast<double>(ts[1] - ts[0]) * tsPeriodMs : lastFrameMs;
            if (add > 0) {
                const double perSample = lastGpuMs / add;
                samplesPerFrame = static_cast<uint32_t>(std::clamp(targetFrameMs / std::max(perSample, 0.05), 1.0, 64.0));
            }

            const auto now = Clock::now();
            if (std::chrono::duration<double>(now - lastTitle).count() > 0.3) {
                const std::string title = std::format("{} - {} / {} spp - {:.1f} ms/frame (GPU {:.1f} ms) - EV {:+.2f}",
                                                      baseTitle, prog.samples(), maxSpp, lastFrameMs, lastGpuMs,
                                                      st.exposure);
                glfwSetWindowTitle(window, title.c_str());
                lastTitle = now;
            }

            if (st.saveRequested) {
                st.saveRequested = false;
                const render::AovImages aov = prog.readback();
                std::string err;
                const fs::path png = outDir / std::format("{}_{:03}.png", stem, saveIndex);
                const fs::path exr = outDir / std::format("{}_{:03}.exr", stem, saveIndex);
                ++saveIndex;
                if (io::savePngTonemapped(png, aov.color, {st.exposure}, err) && io::saveExr(exr, aov.color, err))
                    log::info("saved {} / {} ({} spp)", platform::pathToUtf8(png), platform::pathToUtf8(exr),
                              prog.samples());
                else
                    log::error("save failed: {}", err);
            }

            if (vo.testFrames > 0 && frames >= vo.testFrames) {
                log::info("test: {} frames, {} spp, last frame {:.1f} ms (GPU {:.1f} ms, {} spp/frame), swapchain {}x{} "
                          "format {}",
                          frames, prog.samples(), lastFrameMs, lastGpuMs, samplesPerFrame, swap.extent.width,
                          swap.extent.height, static_cast<int>(swap.format));
                if (!vo.screenshot.empty()) {
                    const fs::path p = platform::u8path(vo.screenshot);
                    if (saveDisplayImage(ctx, tone.output(), p))
                        log::info("wrote {}", platform::pathToUtf8(p));
                    else
                        rc = 1;
                }
                break;
            }
        }

        vkDeviceWaitIdle(dev);
        vkDestroyQueryPool(dev, timestamps, nullptr);
        vkDestroyFence(dev, fence, nullptr);
        vkDestroySemaphore(dev, acquired, nullptr);
        vkFreeCommandBuffers(dev, ctx.commandPool(), 1, &cmd);
        tone.destroy();
        prog.destroy();
        swap.destroy();
        gpu.destroy();
        ctx.destroy();
    } catch (const std::exception& e) {
        log::error("viewer failed: {}", e.what());
        rc = 1;
    }
    glfwDestroyWindow(window);
    glfwTerminate();
    return rc;
}

}  // namespace krbr::app
