#include "core/platform.hpp"

#include <cstdlib>
#include <fstream>
#include <sstream>

#include "core/log.hpp"

#ifdef _WIN32
#include <windows.h>
#else
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>
#endif

namespace krbr::platform {

fs::path u8path(const std::string& s) {
    return fs::path(std::u8string(reinterpret_cast<const char8_t*>(s.data()), s.size()));
}

std::string pathToUtf8(const fs::path& p) {
    const std::u8string s = p.generic_u8string();
    return std::string(reinterpret_cast<const char*>(s.data()), s.size());
}

std::optional<std::string> getEnv(const std::string& name) {
#ifdef _WIN32
    const std::wstring wname = u8path(name).wstring();
    DWORD n = GetEnvironmentVariableW(wname.c_str(), nullptr, 0);
    if (n == 0) return std::nullopt;
    std::wstring buf(n, L'\0');
    n = GetEnvironmentVariableW(wname.c_str(), buf.data(), n);
    buf.resize(n);
    return pathToUtf8(fs::path(buf));
#else
    const char* v = std::getenv(name.c_str());
    if (!v) return std::nullopt;
    return std::string(v);
#endif
}

fs::path exeDir() {
#ifdef _WIN32
    std::wstring buf(MAX_PATH, L'\0');
    for (;;) {
        DWORD n = GetModuleFileNameW(nullptr, buf.data(), static_cast<DWORD>(buf.size()));
        if (n < buf.size()) {
            buf.resize(n);
            break;
        }
        buf.resize(buf.size() * 2);
    }
    return fs::path(buf).parent_path();
#else
    return fs::canonical("/proc/self/exe").parent_path();
#endif
}

static fs::path homeBased(const char* xdgVar, const char* fallback) {
    if (auto x = getEnv(xdgVar)) return u8path(*x);
    if (auto h = getEnv("HOME")) return u8path(*h) / fallback;
    return fs::temp_directory_path();
}

fs::path cacheDir() {
#ifdef _WIN32
    if (auto v = getEnv("LOCALAPPDATA")) return u8path(*v) / "krbr" / "cache";
    return fs::temp_directory_path() / "krbr" / "cache";
#else
    return homeBased("XDG_CACHE_HOME", ".cache") / "krbr";
#endif
}

fs::path configDir() {
#ifdef _WIN32
    if (auto v = getEnv("APPDATA")) return u8path(*v) / "krbr";
    return fs::temp_directory_path() / "krbr" / "config";
#else
    return homeBased("XDG_CONFIG_HOME", ".config") / "krbr";
#endif
}

fs::path tempDir() {
#ifdef _WIN32
    const auto pid = static_cast<unsigned long>(GetCurrentProcessId());
#else
    const auto pid = static_cast<unsigned long>(getpid());
#endif
    fs::path p = fs::temp_directory_path() / "krbr" / std::to_string(pid);
    std::error_code ec;
    fs::create_directories(p, ec);
    return p;
}

void setLowPriority() {
#ifdef _WIN32
    if (!SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS))
        log::warn("SetPriorityClass failed ({})", GetLastError());
#else
    setpriority(PRIO_PROCESS, 0, 10);
#endif
}

#ifdef _WIN32
static std::wstring quoteArg(const std::wstring& a) {
    if (!a.empty() && a.find_first_of(L" \t\"") == std::wstring::npos) return a;
    std::wstring r = L"\"";
    size_t backslashes = 0;
    for (wchar_t c : a) {
        if (c == L'\\') {
            ++backslashes;
        } else if (c == L'"') {
            r.append(backslashes * 2 + 1, L'\\');
            r.push_back(c);
            backslashes = 0;
        } else {
            r.append(backslashes, L'\\');
            r.push_back(c);
            backslashes = 0;
        }
    }
    r.append(backslashes * 2, L'\\');
    r.push_back(L'"');
    return r;
}
#endif

int runHidden(const fs::path& exe, const std::vector<std::string>& args, const fs::path& workDir,
              const fs::path& logFile) {
#ifdef _WIN32
    std::wstring cmd = quoteArg(exe.wstring());
    for (const auto& a : args) cmd += L" " + quoteArg(u8path(a).wstring());

    SECURITY_ATTRIBUTES sa{sizeof(sa), nullptr, TRUE};
    HANDLE hLog = CreateFileW(logFile.wstring().c_str(), GENERIC_WRITE, FILE_SHARE_READ, &sa, CREATE_ALWAYS,
                              FILE_ATTRIBUTE_NORMAL, nullptr);
    HANDLE hNul = CreateFileW(L"NUL", GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, &sa, OPEN_EXISTING, 0,
                              nullptr);
    STARTUPINFOW si{};
    si.cb = sizeof(si);
    si.dwFlags = STARTF_USESTDHANDLES | STARTF_USESHOWWINDOW;
    si.wShowWindow = SW_HIDE;
    si.hStdInput = hNul;
    si.hStdOutput = hLog;
    si.hStdError = hLog;
    PROCESS_INFORMATION pi{};
    // CREATE_NO_WINDOW: コンソールも作らない。優先度は低優先度を明示する
    BOOL ok = CreateProcessW(nullptr, cmd.data(), nullptr, nullptr, TRUE,
                             CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS, nullptr, workDir.wstring().c_str(),
                             &si, &pi);
    if (hLog != INVALID_HANDLE_VALUE) CloseHandle(hLog);
    if (hNul != INVALID_HANDLE_VALUE) CloseHandle(hNul);
    if (!ok) {
        log::warn("CreateProcess failed ({}): {}", GetLastError(), pathToUtf8(exe));
        return -1;
    }
    WaitForSingleObject(pi.hProcess, INFINITE);
    DWORD code = 0;
    GetExitCodeProcess(pi.hProcess, &code);
    CloseHandle(pi.hThread);
    CloseHandle(pi.hProcess);
    return static_cast<int>(code);
#else
    pid_t pid = fork();
    if (pid == 0) {
        if (chdir(workDir.c_str()) != 0) _exit(127);
        FILE* f = std::freopen(logFile.c_str(), "w", stdout);
        (void)f;
        dup2(fileno(stdout), fileno(stderr));
        std::vector<char*> argv;
        std::string e = exe.string();
        argv.push_back(e.data());
        std::vector<std::string> copy = args;
        for (auto& a : copy) argv.push_back(a.data());
        argv.push_back(nullptr);
        execv(e.c_str(), argv.data());
        _exit(127);
    }
    int status = 0;
    waitpid(pid, &status, 0);
    return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
#endif
}

std::optional<std::string> readTextFile(const fs::path& p) {
    std::ifstream f(p, std::ios::binary);
    if (!f) return std::nullopt;
    std::ostringstream ss;
    ss << f.rdbuf();
    return ss.str();
}

std::optional<std::vector<unsigned char>> readBinaryFile(const fs::path& p) {
    std::ifstream f(p, std::ios::binary | std::ios::ate);
    if (!f) return std::nullopt;
    const auto size = static_cast<size_t>(f.tellg());
    f.seekg(0);
    std::vector<unsigned char> data(size);
    if (size && !f.read(reinterpret_cast<char*>(data.data()), static_cast<std::streamsize>(size))) return std::nullopt;
    return data;
}

bool writeTextFile(const fs::path& p, const std::string& text) {
    std::error_code ec;
    if (p.has_parent_path()) fs::create_directories(p.parent_path(), ec);
    std::ofstream f(p, std::ios::binary);
    if (!f) return false;
    f << text;
    return static_cast<bool>(f);
}

}  // namespace krbr::platform
