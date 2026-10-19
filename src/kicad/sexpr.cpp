#include "kicad/sexpr.hpp"

#include <charconv>
#include <cstdlib>

#include "core/hash.hpp"

namespace krbr::sexpr {

std::string_view Node::tag() const {
    if (!isList || items.empty() || items[0].isList) return {};
    return items[0].text;
}

const Node* Node::child(std::string_view name) const {
    for (const auto& c : items)
        if (c.isList && c.tag() == name) return &c;
    return nullptr;
}

std::vector<const Node*> Node::children(std::string_view name) const {
    std::vector<const Node*> out;
    for (const auto& c : items)
        if (c.isList && c.tag() == name) out.push_back(&c);
    return out;
}

std::string Node::str(size_t i, std::string def) const {
    if (i >= items.size() || items[i].isList) return def;
    return items[i].text;
}

double Node::num(size_t i, double def) const {
    if (i >= items.size() || items[i].isList) return def;
    const std::string& s = items[i].text;
    double v = def;
    auto r = std::from_chars(s.data(), s.data() + s.size(), v);
    if (r.ec != std::errc()) return def;
    return v;
}

std::optional<std::string> Node::childStr(std::string_view name, size_t i) const {
    const Node* c = child(name);
    if (!c || i >= c->items.size() || c->items[i].isList) return std::nullopt;
    return c->items[i].text;
}

std::optional<double> Node::childNum(std::string_view name, size_t i) const {
    const Node* c = child(name);
    if (!c || i >= c->items.size() || c->items[i].isList) return std::nullopt;
    return c->num(i);
}

std::optional<bool> Node::childBool(std::string_view name) const {
    const Node* c = child(name);
    if (!c) return std::nullopt;
    if (c->items.size() < 2) return true;
    const std::string v = c->str(1);
    return !(v == "no" || v == "false" || v == "none");
}

bool Node::hasAtom(std::string_view atom) const {
    for (size_t i = 1; i < items.size(); ++i)
        if (!items[i].isList && !items[i].quoted && items[i].text == atom) return true;
    return false;
}

namespace {

struct Parser {
    std::string_view s;
    size_t pos = 0;
    ParseError* err;

    void skipWs() {
        while (pos < s.size()) {
            const char c = s[pos];
            if (c == ' ' || c == '\t' || c == '\r' || c == '\n') {
                ++pos;
            } else if (c == '#' && (pos == 0 || s[pos - 1] == '\n')) {
                while (pos < s.size() && s[pos] != '\n') ++pos;  // 行頭コメント（念のため）
            } else {
                break;
            }
        }
    }

    bool fail(const char* msg) {
        if (err) {
            err->message = msg;
            err->offset = pos;
        }
        return false;
    }

    bool parseQuoted(Node& out) {
        ++pos;  // "
        out.quoted = true;
        std::string& t = out.text;
        while (pos < s.size()) {
            const char c = s[pos++];
            if (c == '"') return true;
            if (c == '\\' && pos < s.size()) {
                const char e = s[pos++];
                switch (e) {
                    case 'n': t.push_back('\n'); break;
                    case 't': t.push_back('\t'); break;
                    case 'r': t.push_back('\r'); break;
                    default: t.push_back(e); break;
                }
            } else {
                t.push_back(c);
            }
        }
        return fail("unterminated string");
    }

    bool parseNode(Node& out) {
        skipWs();
        if (pos >= s.size()) return fail("unexpected end of input");
        const char c = s[pos];
        if (c == '(') {
            ++pos;
            out.isList = true;
            for (;;) {
                skipWs();
                if (pos >= s.size()) return fail("unterminated list");
                if (s[pos] == ')') {
                    ++pos;
                    return true;
                }
                out.items.emplace_back();
                if (!parseNode(out.items.back())) return false;
            }
        }
        if (c == ')') return fail("unexpected ')'");
        if (c == '"') return parseQuoted(out);
        const size_t start = pos;
        while (pos < s.size()) {
            const char d = s[pos];
            if (d == ' ' || d == '\t' || d == '\r' || d == '\n' || d == '(' || d == ')') break;
            ++pos;
        }
        out.text.assign(s.substr(start, pos - start));
        return true;
    }
};

}  // namespace

static void serialize(const Node& n, std::string& out) {
    if (n.isList) {
        out.push_back('(');
        for (const auto& c : n.items) {
            serialize(c, out);
            out.push_back(' ');
        }
        out.push_back(')');
    } else {
        if (n.quoted) out.push_back('"');
        out += n.text;
        if (n.quoted) out.push_back('"');
    }
}

uint64_t hashNode(const Node& n) {
    std::string s;
    serialize(n, s);
    return xxh64(s.data(), s.size());
}

std::optional<Node> parse(std::string_view text, ParseError* err) {
    Parser p{text, 0, err};
    Node root;
    if (!p.parseNode(root)) return std::nullopt;
    return root;
}

}  // namespace krbr::sexpr
