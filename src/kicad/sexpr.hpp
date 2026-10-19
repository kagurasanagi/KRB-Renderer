#pragma once
// KiCad の S式（.kicad_pcb など）の最小パーサ
#include <cstdint>
#include <optional>
#include <string>
#include <string_view>
#include <vector>

namespace krbr::sexpr {

struct Node {
    bool isList = false;
    bool quoted = false;  // アトムが "..." だったか
    std::string text;     // アトムの文字列（リストなら空）
    std::vector<Node> items;

    // リストの先頭アトム（タグ名）
    std::string_view tag() const;
    size_t size() const { return items.size(); }
    const Node& operator[](size_t i) const { return items[i]; }

    // 先頭が name の子リストを返す（最初の1つ）
    const Node* child(std::string_view name) const;
    std::vector<const Node*> children(std::string_view name) const;
    bool has(std::string_view name) const { return child(name) != nullptr; }

    // items[i] をアトムとして取得
    std::string str(size_t i, std::string def = {}) const;
    double num(size_t i, double def = 0.0) const;
    // (name v) 形式の子の値
    std::optional<std::string> childStr(std::string_view name, size_t i = 1) const;
    std::optional<double> childNum(std::string_view name, size_t i = 1) const;
    // (name yes) / (name) / (name no) 形式の真偽値
    std::optional<bool> childBool(std::string_view name) const;
    // 位置引数としての素のアトム（例: (pad "1" smd rect ...) の smd）が含まれるか
    bool hasAtom(std::string_view atom) const;
};

struct ParseError {
    std::string message;
    size_t offset = 0;
};

// ノードの内容（アトムと入れ子の構造）のハッシュ
uint64_t hashNode(const Node& n);

// 失敗時は nullopt と err を返す
std::optional<Node> parse(std::string_view text, ParseError* err = nullptr);

}  // namespace krbr::sexpr
