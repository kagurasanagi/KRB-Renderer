// リアルタイム表示（ウィンドウ）。オフライン描画と同じシーン・積分器で、フレームごとにサンプルを積み増して表示する。
// 操作パネル（Dear ImGui）で、環境マップ・露出・表面の凹凸などをその場で変えられる。
#include <volk.h>
#define GLFW_INCLUDE_VULKAN
#include <GLFW/glfw3.h>
#ifdef _WIN32
#include <windows.h>
#include <commdlg.h>
#define GLFW_EXPOSE_NATIVE_WIN32
#include <GLFW/glfw3native.h>
#endif
#include <imgui.h>
#include <imgui_impl_glfw.h>
#include <imgui_impl_vulkan.h>

#include <algorithm>
#include <chrono>
#include <cmath>
#include <cstring>
#include <future>

#include "app/commands.hpp"
#include "app/prepare.hpp"
#include "app/report.hpp"
#include "core/file_watcher.hpp"
#include "core/log.hpp"
#include "core/platform.hpp"
#include "render/denoiser.hpp"
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
    bool canReadBack = false;  // 画像を読み戻せる（テストで表示内容を保存する）
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
        canReadBack = (caps.supportedUsageFlags & VK_IMAGE_USAGE_TRANSFER_SRC_BIT) != 0;
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
        ci.imageUsage = VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_COLOR_ATTACHMENT_BIT |
                        (canReadBack ? VK_IMAGE_USAGE_TRANSFER_SRC_BIT : 0);
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

// 入力・操作パネルで変わる表示の状態
struct ViewState {
    glm::vec3 target{0.0f};
    float distance = 100.0f;
    double elevation = 35.0, azimuth = 45.0, fov = 30.0;
    float exposure = 0.0f;
    float envRotationDeg = 0.0f;
    float envIntensity = 1.0f;
    float bumpIntensity = 1.0f;
    int maxSpp = 1024;
    render::DisplayMode mode = render::DisplayMode::Beauty;
    bool showPanel = true;
    bool dirty = true;  // カメラ・照明が変わった（累積をやり直す）
    bool materialsDirty = false;
    bool saveRequested = false;
    bool resetRequested = false;
    bool autoExposureRequested = false;
    bool denoise = true;  // ノイズ除去した画像を表示する
    bool spectral = false;  // 分光モード（代表波長方式）
    bool integratorChanged = false;

    // マウス
    bool rotating = false, panning = false;
    double lastX = 0, lastY = 0;
    int fbHeight = 1;
    float tanHalfFovY = 0.27f;
};

bool uiWantsMouse() { return ImGui::GetCurrentContext() && ImGui::GetIO().WantCaptureMouse; }
bool uiWantsKeyboard() { return ImGui::GetCurrentContext() && ImGui::GetIO().WantCaptureKeyboard; }

void onMouseButton(GLFWwindow* w, int button, int action, int) {
    auto* s = static_cast<ViewState*>(glfwGetWindowUserPointer(w));
    const bool down = action == GLFW_PRESS;
    if (down && uiWantsMouse()) return;  // パネルの上での操作はカメラに渡さない
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
    if (uiWantsMouse()) return;
    auto* s = static_cast<ViewState*>(glfwGetWindowUserPointer(w));
    s->distance = std::max(1.0f, s->distance * static_cast<float>(std::pow(0.9, yoff)));
    s->dirty = true;
}

void onKey(GLFWwindow* w, int key, int, int action, int) {
    if (action != GLFW_PRESS && action != GLFW_REPEAT) return;
    if (uiWantsKeyboard()) return;
    auto* s = static_cast<ViewState*>(glfwGetWindowUserPointer(w));
    switch (key) {
        case GLFW_KEY_ESCAPE: glfwSetWindowShouldClose(w, GLFW_TRUE); break;
        case GLFW_KEY_F1:
        case GLFW_KEY_TAB: s->showPanel = !s->showPanel; break;
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

// 画像（RGBA8 / BGRA8）を読み戻して PNG に保存する
bool saveImage(vk::Context& ctx, VkImage image, VkImageLayout layout, uint32_t w, uint32_t h, bool bgra,
               const fs::path& p) {
    const VkDeviceSize bytes = static_cast<VkDeviceSize>(w) * h * 4;
    vk::Buffer rb = ctx.createBuffer(bytes, VK_BUFFER_USAGE_TRANSFER_DST_BIT, true, true);
    ctx.submitAndWait([&](VkCommandBuffer cmd) {
        VkBufferImageCopy region{};
        region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
        region.imageExtent = {w, h, 1};
        vkCmdCopyImageToBuffer(cmd, image, layout, rb.buffer, 1, &region);
    });
    vmaInvalidateAllocation(ctx.allocator(), rb.alloc, 0, VK_WHOLE_SIZE);
    std::vector<unsigned char> px(static_cast<const unsigned char*>(rb.mapped),
                                  static_cast<const unsigned char*>(rb.mapped) + bytes);
    ctx.destroyBuffer(rb);
    if (bgra)
        for (size_t i = 0; i < px.size(); i += 4) std::swap(px[i], px[i + 2]);
    for (size_t i = 3; i < px.size(); i += 4) px[i] = 255;
    std::string err;
    const bool ok = io::savePngRgba8(p, static_cast<int>(w), static_cast<int>(h), px.data(), err);
    if (!ok) log::error("{}: {}", platform::pathToUtf8(p), err);
    return ok;
}

// 環境マップの読み込み（別スレッドで行い、終わったら GPU へ差し替える）
struct EnvData {
    std::string path;
    io::ImageF image;
    render::EnvDistribution dist;
    std::string error;
};

std::unique_ptr<EnvData> loadEnvironment(std::string path) {
    auto d = std::make_unique<EnvData>();
    d->path = std::move(path);
    if (!io::loadHdrImage(platform::u8path(d->path), d->image, d->error)) return d;
    d->dist = render::buildEnvDistribution(d->image);
    return d;
}

// 同じフォルダの .hdr / .exr の一覧
std::vector<fs::path> listEnvironments(const fs::path& current) {
    std::vector<fs::path> out;
    std::error_code ec;
    const fs::path dir = current.has_parent_path() ? current.parent_path() : fs::path(".");
    for (const auto& e : fs::directory_iterator(dir, ec)) {
        if (!e.is_regular_file()) continue;
        std::string ext = platform::pathToUtf8(e.path().extension());
        std::transform(ext.begin(), ext.end(), ext.begin(), [](unsigned char c) { return static_cast<char>(std::tolower(c)); });
        if (ext == ".hdr" || ext == ".exr") out.push_back(e.path());
    }
    std::sort(out.begin(), out.end());
    return out;
}

#ifdef _WIN32
// ファイルを選ぶ（Windows 標準のダイアログ。ユーザーがボタンを押したときだけ開く）
std::optional<std::string> chooseEnvironmentFile(GLFWwindow* window, const fs::path& initialDir) {
    std::wstring buf(4096, L'\0');
    const std::wstring dir = initialDir.wstring();
    OPENFILENAMEW ofn{};
    ofn.lStructSize = sizeof(ofn);
    ofn.hwndOwner = glfwGetWin32Window(window);
    ofn.lpstrFilter = L"HDR / OpenEXR (*.hdr;*.exr)\0*.hdr;*.exr\0All files\0*.*\0";
    ofn.lpstrFile = buf.data();
    ofn.nMaxFile = static_cast<DWORD>(buf.size());
    ofn.lpstrInitialDir = dir.empty() ? nullptr : dir.c_str();
    ofn.lpstrTitle = L"環境マップを選ぶ";
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
    if (!GetOpenFileNameW(&ofn)) return std::nullopt;
    buf.resize(std::wcslen(buf.c_str()));
    return platform::pathToUtf8(fs::path(buf));
}
#else
std::optional<std::string> chooseEnvironmentFile(GLFWwindow*, const fs::path&) { return std::nullopt; }
#endif

// 画像全体の対数平均輝度から露出を決める（中間の灰色 0.18 に合わせる）
float autoExposure(const io::ImageF& img) {
    double sum = 0.0;
    size_t n = 0;
    for (size_t i = 0; i + 3 < img.rgba.size(); i += 4) {
        const double l = 0.2126 * img.rgba[i] + 0.7152 * img.rgba[i + 1] + 0.0722 * img.rgba[i + 2];
        sum += std::log(std::max(l, 1e-4));
        ++n;
    }
    if (!n) return 0.0f;
    const double avg = std::exp(sum / static_cast<double>(n));
    return static_cast<float>(std::clamp(std::log2(0.18 / avg), -6.0, 6.0));
}

void setupImGuiStyle(GLFWwindow* window) {
    ImGuiIO& io = ImGui::GetIO();
    io.IniFilename = nullptr;  // パネルの配置をファイルに書かない
    float xs = 1.0f, ys = 1.0f;
    glfwGetWindowContentScale(window, &xs, &ys);
    const float scale = std::max(1.0f, xs);
    ImGui::StyleColorsDark();
    ImGuiStyle& style = ImGui::GetStyle();
    style.WindowRounding = 6.0f;
    style.FrameRounding = 4.0f;
    style.Colors[ImGuiCol_WindowBg].w = 0.88f;
    style.ScaleAllSizes(scale);
    // 日本語を表示できる Windows のフォント（無ければ既定のフォント）
    const char* fonts[] = {"C:/Windows/Fonts/YuGothM.ttc", "C:/Windows/Fonts/meiryo.ttc", "C:/Windows/Fonts/msgothic.ttc"};
    for (const char* f : fonts) {
        std::error_code ec;
        if (fs::exists(f, ec) && io.Fonts->AddFontFromFileTTF(f, 16.0f * scale)) return;
    }
    io.Fonts->AddFontDefault();
}

}  // namespace

int cmdView(const ViewOptions& vo) {
    const RenderOptions& o = vo.common;
    if (!o.normalPriority) platform::setLowPriority();
    const bool hidden = vo.hidden || vo.testFrames > 0;

    auto prep = prepareScene(o);
    if (!prep) return 1;
    ProjectConfig& cfg = prep->cfg;
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
    st.maxSpp = o.spp ? *o.spp : 1024;
    st.exposure = cfg.exposureEv;
    st.spectral = cfg.spectral;
    st.envRotationDeg = cfg.envRotationDeg;
    st.envIntensity = cfg.envIntensity;
    st.bumpIntensity = cfg.scene.bumpIntensity;
    glfwSetWindowUserPointer(window, &st);
    // 先に自前のコールバックを登録し、ImGui はそれを呼び出す形で上に重ねる
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
        gpu.build(ctx, prep->scene, prep->env, prep->dist, st.bumpIntensity);
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
        resetCamera(fbw, fbh);

        auto currentFrame = [&](int w, int h) {
            ProjectConfig c = cfg;
            c.envRotationDeg = st.envRotationDeg;
            c.envIntensity = st.envIntensity;
            c.spectral = st.spectral;
            const scene::Camera cam = scene::orbitCamera(st.target, st.distance, st.elevation, st.azimuth, st.fov,
                                                         static_cast<float>(w) / static_cast<float>(h));
            st.tanHalfFovY = cam.tanHalfFovY;
            return frameParams(c, cam, o.seed);
        };

        Swapchain swap;
        swap.create(ctx, static_cast<uint32_t>(fbw), static_cast<uint32_t>(fbh));
        render::Progressive prog;
        prog.init(ctx, gpu, cfg.integrator, swap.extent.width, swap.extent.height,
                  currentFrame(static_cast<int>(swap.extent.width), static_cast<int>(swap.extent.height)));
        render::Tonemapper tone;
        tone.init(ctx, prog.targets());
        // ノイズ除去：操作中は品質 fast で毎フレーム、止まったら high で間隔をあけて
        render::Denoiser denoiser;
        render::Tonemapper toneDenoised;
        st.denoise = o.denoise.value_or(true);
        auto initDenoiser = [&] {
            if (!st.denoise && !denoiser.ready() && !o.denoise.value_or(true)) return;
            if (denoiser.init(ctx, prog.targets(), render::DenoiseQuality::Fast)) {
                render::RenderTargets t = prog.targets();
                t.color = denoiser.output();
                toneDenoised.init(ctx, t);
            }
        };
        initDenoiser();
        uint64_t accumEpoch = 0, denoisedEpoch = UINT64_MAX;  // 累積をやり直すたびに増やす
        uint32_t denoisedSamples = 0;
        double lastDenoiseMs = 0.0;
        auto lastDenoiseAt = Clock::now();
        auto lastChangeAt = Clock::now();

        // ---- 操作パネル（ImGui）
        IMGUI_CHECKVERSION();
        ImGui::CreateContext();
        setupImGuiStyle(window);
        ImGui_ImplGlfw_InitForVulkan(window, true);
        VkFormat swapFormat = swap.format;
        ImGui_ImplVulkan_InitInfo ii{};
        ii.ApiVersion = VK_API_VERSION_1_3;
        ii.Instance = ctx.instance();
        ii.PhysicalDevice = ctx.physical();
        ii.Device = ctx.device();
        ii.QueueFamily = ctx.queueFamily();
        ii.Queue = ctx.queue();
        ii.DescriptorPoolSize = IMGUI_IMPL_VULKAN_MINIMUM_SAMPLED_IMAGE_POOL_SIZE;
        ii.MinImageCount = 2;
        ii.ImageCount = static_cast<uint32_t>(swap.images.size());
        ii.UseDynamicRendering = true;
        ii.PipelineInfoMain.PipelineRenderingCreateInfo = {VK_STRUCTURE_TYPE_PIPELINE_RENDERING_CREATE_INFO};
        ii.PipelineInfoMain.PipelineRenderingCreateInfo.colorAttachmentCount = 1;
        ii.PipelineInfoMain.PipelineRenderingCreateInfo.pColorAttachmentFormats = &swapFormat;
        if (!ImGui_ImplVulkan_Init(&ii)) throw std::runtime_error("ImGui Vulkan backend init failed");

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

        log::info("viewer: {}x{}, up to {} spp. mouse: L=rotate, R/M=pan, wheel=zoom; keys: F1/Tab=panel, R=reset, "
                  "+/-=exposure, [ ]=rotate environment, 1/2/3=image/albedo/normal, S=save, Esc=quit",
                  swap.extent.width, swap.extent.height, st.maxSpp);

        // 環境マップ
        std::string envPath = cfg.hdr;
        std::vector<fs::path> envList = listEnvironments(platform::u8path(envPath));
        std::future<std::unique_ptr<EnvData>> envJob;
        std::string envJobPath, envError;
        auto startEnvLoad = [&](const std::string& path) {
            if (envJob.valid() || path == envPath) return;
            envJobPath = path;
            envError.clear();
            envJob = std::async(std::launch::async, loadEnvironment, path);
            log::info("loading environment {}", path);
        };

        // 保存の監視：読み直しとシーンの作り直しは裏のスレッドで行い、できたら GPU の形状だけ差し替える
        struct ReloadResult {
            std::unique_ptr<scene::Scene> scene;
            ReloadStats stats;
            bool saved = false;
        };
        std::unique_ptr<FileWatcher> watcher;
        if (vo.watch) watcher = std::make_unique<FileWatcher>(prep->board.file);
        std::future<ReloadResult> reloadJob;
        std::chrono::system_clock::time_point reloadSavedAt{};
        size_t modelsCompleted = prep->library->completed();
        std::string lastUpdate;  // パネルに出す、直近の更新の情報
        bool testReloaded = false;

        uint32_t samplesPerFrame = 1;
        const double targetFrameMs = 10.0;  // 60fps（16.7 ms）に収まるよう、表示処理の分を残す
        bool needRecreate = false;
        int frames = 0, saveIndex = 0;
        auto lastTitle = Clock::now();
        double lastFrameMs = 0.0, lastGpuMs = 0.0;
        bool testEnvRequested = false;

        const fs::path outPng = platform::u8path(o.out);
        const fs::path outDir = outPng.has_parent_path() ? outPng.parent_path() : fs::path(".");
        const std::string stem = platform::pathToUtf8(outPng.stem());

        while (!glfwWindowShouldClose(window)) {
            const bool idle = prog.samples() >= static_cast<uint32_t>(st.maxSpp) && !st.dirty && !envJob.valid() &&
                              !reloadJob.valid();
            if (idle && vo.testFrames == 0)
                glfwWaitEventsTimeout(watcher ? 0.1 : 0.25);  // 収束したら GPU を休ませ、入力と保存を待つ
            else
                glfwPollEvents();

            glfwGetFramebufferSize(window, &fbw, &fbh);
            if (fbw == 0 || fbh == 0) {  // 最小化中
                glfwWaitEvents();
                continue;
            }
            st.fbHeight = fbh;

            // ---- 開発用テストの操作
            if (vo.testOrbitDeg != 0.0) {
                st.azimuth += vo.testOrbitDeg;
                st.dirty = true;
            }
            if (!vo.testEnv.empty() && !testEnvRequested && frames == 5) {
                testEnvRequested = true;
                startEnvLoad(vo.testEnv);
            }

            // ---- 保存の監視（裏で読み直す）
            if (!reloadJob.valid()) {
                const bool fromWatcher = watcher && watcher->poll();
                bool saved = fromWatcher;
                if (vo.testReload && !testReloaded && frames == 5) {
                    saved = testReloaded = true;  // 開発用：保存されたものとして読み直す
                }
                const bool modelsDone = prep->library->completed() != modelsCompleted;
                const bool silkDone = prep->takeFinishedSilk();
                if (saved || modelsDone || silkDone) {
                    modelsCompleted = prep->library->completed();
                    reloadSavedAt = fromWatcher ? watcher->savedAt() : std::chrono::system_clock::now();
                    PreparedScene* p = prep.get();
                    reloadJob = std::async(std::launch::async, [p, saved] {
                        ReloadResult r;
                        r.saved = saved;
                        r.scene = saved ? p->reload(r.stats) : p->buildScene(true);
                        return r;
                    });
                }
            }

            // ---- 操作パネル
            ImGui_ImplVulkan_NewFrame();
            ImGui_ImplGlfw_NewFrame();
            ImGui::NewFrame();
            if (st.showPanel) {
                ImGui::SetNextWindowPos(ImVec2(12, 12), ImGuiCond_FirstUseEver);
                ImGui::SetNextWindowSize(ImVec2(0, 0), ImGuiCond_FirstUseEver);
                ImGui::Begin("krbr", &st.showPanel, ImGuiWindowFlags_AlwaysAutoResize);
                ImGui::Text("%u / %d spp   %.1f ms/frame (GPU %.1f ms)", prog.samples(), st.maxSpp, lastFrameMs,
                            lastGpuMs);
                ImGui::ProgressBar(std::min(1.0f, static_cast<float>(prog.samples()) / std::max(1, st.maxSpp)),
                                   ImVec2(-1, 0), "");

                if (reloadJob.valid())
                    ImGui::TextColored(ImVec4(1, 0.8f, 0.3f, 1), "基板を読み直し中...");
                else if (!lastUpdate.empty())
                    ImGui::TextDisabled("%s", lastUpdate.c_str());

                ImGui::SeparatorText("環境マップ");
                const std::string curName = platform::pathToUtf8(platform::u8path(envPath).filename());
                ImGui::SetNextItemWidth(260);
                if (ImGui::BeginCombo("##hdr", curName.c_str())) {
                    for (const auto& p : envList) {
                        const std::string name = platform::pathToUtf8(p.filename());
                        const std::string full = platform::pathToUtf8(p);
                        if (ImGui::Selectable(name.c_str(), full == envPath || name == curName)) startEnvLoad(full);
                    }
                    ImGui::EndCombo();
                }
                ImGui::SameLine();
                if (ImGui::Button("開く...")) {
                    if (auto f = chooseEnvironmentFile(window, platform::u8path(envPath).parent_path())) startEnvLoad(*f);
                }
                if (envJob.valid()) ImGui::TextColored(ImVec4(1, 0.8f, 0.3f, 1), "読み込み中: %s", envJobPath.c_str());
                if (!envError.empty()) ImGui::TextColored(ImVec4(1, 0.4f, 0.4f, 1), "%s", envError.c_str());
                if (ImGui::SliderFloat("強さ", &st.envIntensity, 0.05f, 8.0f, "%.2f", ImGuiSliderFlags_Logarithmic))
                    st.dirty = true;
                if (ImGui::SliderFloat("回転", &st.envRotationDeg, -180.0f, 180.0f, "%.0f°")) st.dirty = true;

                ImGui::SeparatorText("露出");
                ImGui::SetNextItemWidth(200);
                ImGui::SliderFloat("EV", &st.exposure, -6.0f, 6.0f, "%+.2f");
                ImGui::SameLine();
                if (ImGui::Button("自動")) st.autoExposureRequested = true;
                ImGui::SameLine();
                if (ImGui::Button("0")) st.exposure = 0.0f;

                if (denoiser.ready() || st.denoise) {
                    ImGui::Checkbox("ノイズ除去 (OIDN)", &st.denoise);
                    if (denoiser.ready()) {
                        ImGui::SameLine();
                        ImGui::TextDisabled("%s, %.0f ms", denoiser.deviceName().c_str(), lastDenoiseMs);
                    } else {
                        ImGui::SameLine();
                        ImGui::TextDisabled("使用できません");
                    }
                }

                ImGui::SeparatorText("積分器");
                {
                    static const char* kIntegrators[] = {"pt", "bdpt", "vcm"};
                    int cur = 0;
                    for (int i = 0; i < 3; ++i)
                        if (cfg.integrator == kIntegrators[i]) cur = i;
                    int sel = cur;
                    ImGui::RadioButton("PT", &sel, 0);
                    ImGui::SameLine();
                    ImGui::RadioButton("BDPT", &sel, 1);
                    ImGui::SameLine();
                    ImGui::RadioButton("VCM", &sel, 2);
                    if (sel != cur) {
                        cfg.integrator = kIntegrators[sel];
                        st.integratorChanged = true;
                    }
                    ImGui::TextDisabled("%s", sel == 0   ? "単方向（速い）"
                                              : sel == 1 ? "双方向（光源側の経路とつなぐ）"
                                                         : "双方向＋近傍のまとめ（透明な物体越しの集光模様も収束）");
                }

                ImGui::SeparatorText("色の計算");
                int colorMode = st.spectral ? 1 : 0;
                ImGui::RadioButton("RGB", &colorMode, 0);
                ImGui::SameLine();
                ImGui::RadioButton("分光（4 波長）", &colorMode, 1);
                if ((colorMode == 1) != st.spectral) {
                    st.spectral = colorMode == 1;
                    st.dirty = true;
                }

                ImGui::SeparatorText("表面");
                if (ImGui::SliderFloat("凹凸の強さ", &st.bumpIntensity, 0.0f, 2.0f, "%.2f")) st.materialsDirty = true;

                ImGui::SeparatorText("表示");
                int mode = static_cast<int>(st.mode);
                ImGui::RadioButton("画像", &mode, 0);
                ImGui::SameLine();
                ImGui::RadioButton("アルベド", &mode, 1);
                ImGui::SameLine();
                ImGui::RadioButton("法線", &mode, 2);
                st.mode = static_cast<render::DisplayMode>(mode);
                ImGui::SliderInt("最大 spp", &st.maxSpp, 16, 16384, "%d", ImGuiSliderFlags_Logarithmic);

                ImGui::SeparatorText("カメラ");
                float fov = static_cast<float>(st.fov);
                if (ImGui::SliderFloat("画角", &fov, 5.0f, 90.0f, "%.0f°")) {
                    st.fov = fov;
                    st.dirty = true;
                }
                ImGui::Text("仰角 %.0f°  方位 %.0f°", st.elevation, std::fmod(st.azimuth, 360.0));
                if (ImGui::Button("視点を戻す (R)")) st.resetRequested = true;
                ImGui::SameLine();
                if (ImGui::Button("保存 (S)")) st.saveRequested = true;

                ImGui::Separator();
                ImGui::TextDisabled("左ドラッグ: 回転  右ドラッグ: 移動  ホイール: 拡大");
                ImGui::TextDisabled("F1 / Tab: このパネルの表示切り替え");
                ImGui::End();
            }
            ImGui::Render();

            // ---- 状態の反映（GPU は前のフレームの完了を待っているので、ここで資源を触ってよい）
            if (reloadJob.valid() && reloadJob.wait_for(std::chrono::seconds(0)) == std::future_status::ready) {
                ReloadResult r = reloadJob.get();
                if (r.scene) {
                    vkDeviceWaitIdle(dev);
                    prep->scene = std::move(*r.scene);
                    gpu.rebuildGeometry(prep->scene, st.bumpIntensity);
                    prog.rebuildIntegrator();
                    st.dirty = true;
                    const double ms = std::chrono::duration<double, std::milli>(std::chrono::system_clock::now() -
                                                                                reloadSavedAt).count();
                    if (r.saved) {
                        lastUpdate = std::format("保存から {:.2f} 秒で更新（タイル {} 枚{}）", ms / 1000.0,
                                                 r.stats.tilesRebuilt,
                                                 prep->scene.pendingModels
                                                     ? std::format("、{} 個のモデルを分割中", prep->scene.pendingModels)
                                                     : "");
                        log::info("viewer: reloaded {:.0f} ms after save (parse {:.0f}, build {:.0f}, {} tiles); "
                                  "{} added, {} changed, {} removed",
                                  ms, r.stats.parseMs, r.stats.buildMs, r.stats.tilesRebuilt, r.stats.diff.added,
                                  r.stats.diff.changed, r.stats.diff.removed);
                    } else {
                        lastUpdate = "裏での分割・シルクの描き直しを反映";
                    }
                } else if (r.saved) {
                    log::warn("viewer: cannot read the board ({}); waiting for the next save", r.stats.error);
                }
            }
            if (envJob.valid() && envJob.wait_for(std::chrono::seconds(0)) == std::future_status::ready) {
                std::unique_ptr<EnvData> d = envJob.get();
                if (d->error.empty()) {
                    vkDeviceWaitIdle(dev);
                    gpu.setEnvironment(d->image, d->dist);
                    prog.rebuildIntegrator();
                    envPath = d->path;
                    envList = listEnvironments(platform::u8path(envPath));
                    st.dirty = true;
                    log::info("environment: {} ({}x{})", envPath, d->image.width, d->image.height);
                } else {
                    envError = d->error;
                    log::error("environment {}: {}", d->path, d->error);
                }
            }
            if (st.integratorChanged) {
                st.integratorChanged = false;
                vkDeviceWaitIdle(dev);
                try {
                    prog.setIntegrator(cfg.integrator);
                    log::info("integrator: {}", cfg.integrator);
                } catch (const std::exception& e) {
                    log::error("integrator {}: {}", cfg.integrator, e.what());
                    cfg.integrator = "pt";
                    prog.setIntegrator("pt");
                }
                st.dirty = true;
            }
            if (st.materialsDirty) {
                gpu.updateMaterials(prep->scene.materials, st.bumpIntensity);
                st.materialsDirty = false;
                st.dirty = true;
            }
            if (st.resetRequested) {
                resetCamera(fbw, fbh);
                st.resetRequested = false;
            }
            if (needRecreate || static_cast<uint32_t>(fbw) != swap.extent.width ||
                static_cast<uint32_t>(fbh) != swap.extent.height) {
                vkDeviceWaitIdle(dev);
                const size_t oldCount = swap.images.size();
                swap.create(ctx, static_cast<uint32_t>(fbw), static_cast<uint32_t>(fbh));
                if (swap.images.size() != oldCount) ImGui_ImplVulkan_SetMinImageCount(2);
                prog.resize(swap.extent.width, swap.extent.height);
                tone.init(ctx, prog.targets());
                if (denoiser.ready()) initDenoiser();
                needRecreate = false;
                st.dirty = true;
            }
            if (st.dirty) {
                prog.setFrame(currentFrame(static_cast<int>(swap.extent.width), static_cast<int>(swap.extent.height)));
                st.dirty = false;
                ++accumEpoch;
                lastChangeAt = Clock::now();
            }
            if (st.autoExposureRequested) {
                st.autoExposureRequested = false;
                if (prog.samples() > 0) {
                    st.exposure = autoExposure(prog.readback().color);
                    log::info("auto exposure: {:+.2f} EV", st.exposure);
                }
            }

            uint32_t imageIndex = 0;
            VkResult ar = vkAcquireNextImageKHR(dev, swap.handle, UINT64_MAX, acquired, VK_NULL_HANDLE, &imageIndex);
            if (ar == VK_ERROR_OUT_OF_DATE_KHR) {
                needRecreate = true;
                continue;
            }
            if (ar != VK_SUCCESS && ar != VK_SUBOPTIMAL_KHR) vk::check(ar, "vkAcquireNextImageKHR");

            const uint32_t maxSpp = static_cast<uint32_t>(st.maxSpp);
            const uint32_t add = prog.samples() >= maxSpp ? 0 : std::min(samplesPerFrame, maxSpp - prog.samples());
            // このフレームでノイズ除去するか（画像の表示のときだけ）
            const bool moving = Clock::now() - lastChangeAt < std::chrono::milliseconds(300);
            const uint32_t samplesAfter = prog.samples() + add;
            const bool wantDenoise = st.denoise && denoiser.ready() && st.mode == render::DisplayMode::Beauty &&
                                     samplesAfter > 0 &&
                                     (denoisedEpoch != accumEpoch || denoisedSamples != samplesAfter) &&
                                     (moving || samplesAfter >= maxSpp ||
                                      Clock::now() - lastDenoiseAt > std::chrono::milliseconds(150));
            if (wantDenoise) {
                // サンプルの追加と入力の準備を先に流し、OIDN（GPU）で除去してから表示のコマンドを作る
                denoiser.setQuality(moving ? render::DenoiseQuality::Fast : render::DenoiseQuality::High);
                ctx.submitAndWait([&](VkCommandBuffer c) {
                    vkCmdResetQueryPool(c, timestamps, 0, 2);
                    vkCmdWriteTimestamp2(c, VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT, timestamps, 0);
                    prog.record(c, add);
                    vkCmdWriteTimestamp2(c, VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT, timestamps, 1);
                    denoiser.recordPrepare(c);
                });
                if (denoiser.execute(&lastDenoiseMs)) {
                    denoisedEpoch = accumEpoch;
                    denoisedSamples = prog.samples();
                }
                lastDenoiseAt = Clock::now();
            }
            const bool showDenoised = st.denoise && denoiser.ready() && st.mode == render::DisplayMode::Beauty &&
                                      denoisedEpoch == accumEpoch;
            const bool lastTestFrame = vo.testFrames > 0 && frames + 1 >= vo.testFrames && !envJob.valid() &&
                                       !reloadJob.valid() && (!vo.testReload || testReloaded);
            vkResetCommandBuffer(cmd, 0);
            VkCommandBufferBeginInfo bi{VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO};
            bi.flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT;
            vkBeginCommandBuffer(cmd, &bi);
            if (!wantDenoise) {
                vkCmdResetQueryPool(cmd, timestamps, 0, 2);
                vkCmdWriteTimestamp2(cmd, VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT, timestamps, 0);
                prog.record(cmd, add);
                vkCmdWriteTimestamp2(cmd, VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT, timestamps, 1);
            }
            if (wantDenoise && denoisedEpoch == accumEpoch) denoiser.recordOutput(cmd);
            render::Tonemapper& tm = showDenoised ? toneDenoised : tone;
            tm.record(cmd, st.exposure, st.mode, prog.xyzOutput());
            const VkImage dispImg = tm.output().image, swapImg = swap.images[imageIndex];
            vk::imageBarrier(cmd, dispImg, VK_IMAGE_LAYOUT_GENERAL, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL,
                             VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT, VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT,
                             VK_PIPELINE_STAGE_2_BLIT_BIT, VK_ACCESS_2_TRANSFER_READ_BIT);
            vk::imageBarrier(cmd, swapImg, VK_IMAGE_LAYOUT_UNDEFINED, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                             VK_PIPELINE_STAGE_2_BLIT_BIT, 0, VK_PIPELINE_STAGE_2_BLIT_BIT,
                             VK_ACCESS_2_TRANSFER_WRITE_BIT);
            VkImageBlit blit{};
            blit.srcSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
            blit.srcOffsets[1] = {static_cast<int32_t>(tm.output().width), static_cast<int32_t>(tm.output().height), 1};
            blit.dstSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
            blit.dstOffsets[1] = {static_cast<int32_t>(swap.extent.width), static_cast<int32_t>(swap.extent.height), 1};
            vkCmdBlitImage(cmd, dispImg, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, swapImg,
                           VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, 1, &blit, VK_FILTER_NEAREST);
            vk::imageBarrier(cmd, dispImg, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_IMAGE_LAYOUT_GENERAL,
                             VK_PIPELINE_STAGE_2_BLIT_BIT, VK_ACCESS_2_TRANSFER_READ_BIT,
                             VK_PIPELINE_STAGE_2_COMPUTE_SHADER_BIT | VK_PIPELINE_STAGE_2_COPY_BIT,
                             VK_ACCESS_2_SHADER_STORAGE_WRITE_BIT | VK_ACCESS_2_TRANSFER_READ_BIT);

            // 操作パネルを画像の上に重ねて描く（動的レンダリング）
            vk::imageBarrier(cmd, swapImg, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                             VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL, VK_PIPELINE_STAGE_2_BLIT_BIT,
                             VK_ACCESS_2_TRANSFER_WRITE_BIT, VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                             VK_ACCESS_2_COLOR_ATTACHMENT_READ_BIT | VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT);
            VkImageViewCreateInfo vci{VK_STRUCTURE_TYPE_IMAGE_VIEW_CREATE_INFO};
            vci.image = swapImg;
            vci.viewType = VK_IMAGE_VIEW_TYPE_2D;
            vci.format = swap.format;
            vci.subresourceRange = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
            VkImageView swapView;
            vk::check(vkCreateImageView(dev, &vci, nullptr, &swapView), "vkCreateImageView");
            VkRenderingAttachmentInfo att{VK_STRUCTURE_TYPE_RENDERING_ATTACHMENT_INFO};
            att.imageView = swapView;
            att.imageLayout = VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL;
            att.loadOp = VK_ATTACHMENT_LOAD_OP_LOAD;
            att.storeOp = VK_ATTACHMENT_STORE_OP_STORE;
            VkRenderingInfo ri{VK_STRUCTURE_TYPE_RENDERING_INFO};
            ri.renderArea = {{0, 0}, swap.extent};
            ri.layerCount = 1;
            ri.colorAttachmentCount = 1;
            ri.pColorAttachments = &att;
            vkCmdBeginRendering(cmd, &ri);
            ImGui_ImplVulkan_RenderDrawData(ImGui::GetDrawData(), cmd);
            vkCmdEndRendering(cmd);

            vk::Buffer shot;
            if (lastTestFrame && swap.canReadBack && !vo.screenshot.empty()) {
                // 表示内容（パネル込み）をテスト用に読み戻す
                vk::imageBarrier(cmd, swapImg, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                                 VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                                 VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT, VK_PIPELINE_STAGE_2_COPY_BIT,
                                 VK_ACCESS_2_TRANSFER_READ_BIT);
                shot = ctx.createBuffer(static_cast<VkDeviceSize>(swap.extent.width) * swap.extent.height * 4,
                                        VK_BUFFER_USAGE_TRANSFER_DST_BIT, true, true);
                VkBufferImageCopy region{};
                region.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1};
                region.imageExtent = {swap.extent.width, swap.extent.height, 1};
                vkCmdCopyImageToBuffer(cmd, swapImg, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, shot.buffer, 1, &region);
                vk::imageBarrier(cmd, swapImg, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, VK_IMAGE_LAYOUT_PRESENT_SRC_KHR,
                                 VK_PIPELINE_STAGE_2_COPY_BIT, VK_ACCESS_2_TRANSFER_READ_BIT,
                                 VK_PIPELINE_STAGE_2_BOTTOM_OF_PIPE_BIT, 0);
            } else {
                vk::imageBarrier(cmd, swapImg, VK_IMAGE_LAYOUT_COLOR_ATTACHMENT_OPTIMAL,
                                 VK_IMAGE_LAYOUT_PRESENT_SRC_KHR, VK_PIPELINE_STAGE_2_COLOR_ATTACHMENT_OUTPUT_BIT,
                                 VK_ACCESS_2_COLOR_ATTACHMENT_WRITE_BIT, VK_PIPELINE_STAGE_2_BOTTOM_OF_PIPE_BIT, 0);
            }
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
            vkDestroyImageView(dev, swapView, nullptr);
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

            if (lastTestFrame) {
                log::info("test: {} frames, {} spp, last frame {:.1f} ms (GPU {:.1f} ms, {} spp/frame), swapchain {}x{} "
                          "format {}, environment {}",
                          frames, prog.samples(), lastFrameMs, lastGpuMs, samplesPerFrame, swap.extent.width,
                          swap.extent.height, static_cast<int>(swap.format), envPath);
                if (!vo.screenshot.empty()) {
                    const fs::path p = platform::u8path(vo.screenshot);
                    bool ok = false;
                    if (shot.buffer) {
                        vmaInvalidateAllocation(ctx.allocator(), shot.alloc, 0, VK_WHOLE_SIZE);
                        std::vector<unsigned char> px(static_cast<const unsigned char*>(shot.mapped),
                                                      static_cast<const unsigned char*>(shot.mapped) + shot.size);
                        if (swap.format == VK_FORMAT_B8G8R8A8_UNORM)
                            for (size_t i = 0; i < px.size(); i += 4) std::swap(px[i], px[i + 2]);
                        for (size_t i = 3; i < px.size(); i += 4) px[i] = 255;
                        std::string err;
                        ok = io::savePngRgba8(p, static_cast<int>(swap.extent.width),
                                              static_cast<int>(swap.extent.height), px.data(), err);
                    } else {
                        ok = saveImage(ctx, tone.output().image, VK_IMAGE_LAYOUT_GENERAL, tone.output().width,
                                       tone.output().height, false, p);  // 読み戻せない場合はノイズ除去前
                    }
                    if (ok)
                        log::info("wrote {}", platform::pathToUtf8(p));
                    else
                        rc = 1;
                }
                ctx.destroyBuffer(shot);
                break;
            }
            ctx.destroyBuffer(shot);
        }

        vkDeviceWaitIdle(dev);
        if (envJob.valid()) envJob.wait();
        if (reloadJob.valid()) reloadJob.wait();
        toneDenoised.destroy();
        denoiser.destroy();
        ImGui_ImplVulkan_Shutdown();
        ImGui_ImplGlfw_Shutdown();
        ImGui::DestroyContext();
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
