#pragma once
#include <filesystem>
#include <optional>
#include <string>
#include <vector>

namespace krbr::platform {

namespace fs = std::filesystem;

fs::path exeDir();
// 全体用キャッシュ（Windows: %LOCALAPPDATA%\krbr\cache、他: $XDG_CACHE_HOME/krbr）
fs::path cacheDir();
// 全体設定（Windows: %APPDATA%\krbr、他: $XDG_CONFIG_HOME/krbr）
fs::path configDir();
// 一時作業用（プロセスごとに分ける）
fs::path tempDir();

std::optional<std::string> getEnv(const std::string& name);

// 自プロセス（と子プロセス）を低優先度にする
void setLowPriority();

// ウィンドウを出さずに子プロセスを実行して終了コードを返す。標準出力・エラーは logFile へ
int runHidden(const fs::path& exe, const std::vector<std::string>& args, const fs::path& workDir,
              const fs::path& logFile);

std::optional<std::string> readTextFile(const fs::path& p);
std::optional<std::vector<unsigned char>> readBinaryFile(const fs::path& p);
bool writeTextFile(const fs::path& p, const std::string& text);

// UTF-8 文字列 <-> path（Windows のワイド文字パス対策）
fs::path u8path(const std::string& s);
std::string pathToUtf8(const fs::path& p);

}  // namespace krbr::platform
