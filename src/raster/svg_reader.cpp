#include "raster/svg_reader.hpp"

#include <cctype>
#include <charconv>
#include <cmath>
#include <map>
#include <numbers>
#include <optional>
#include <vector>

namespace krbr::raster {

using geom::Vec2;

namespace {

struct Style {
    bool fill = true;  // SVG の既定は fill:black
    bool stroke = false;
    double strokeWidth = 1.0;
    bool evenOdd = false;
};

struct Transform {
    double sx = 1, sy = 1, tx = 0, ty = 0;
    Vec2 apply(Vec2 p) const { return {p.x * sx + tx, p.y * sy + ty}; }
    Transform then(const Transform& inner) const {
        // this * inner
        return {sx * inner.sx, sy * inner.sy, sx * inner.tx + tx, sy * inner.ty + ty};
    }
};

std::string trim(std::string_view s) {
    size_t a = 0, b = s.size();
    while (a < b && std::isspace(static_cast<unsigned char>(s[a]))) ++a;
    while (b > a && std::isspace(static_cast<unsigned char>(s[b - 1]))) --b;
    return std::string(s.substr(a, b - a));
}

double toNum(std::string_view s, double def = 0) {
    std::string t = trim(s);
    // "0.12mm" などの単位は落とす
    while (!t.empty() && std::isalpha(static_cast<unsigned char>(t.back()))) t.pop_back();
    double v = def;
    std::from_chars(t.data(), t.data() + t.size(), v);
    return v;
}

void applyStyleProp(Style& st, const std::string& key, const std::string& val) {
    if (key == "fill") st.fill = val != "none";
    else if (key == "stroke") st.stroke = val != "none";
    else if (key == "stroke-width") st.strokeWidth = toNum(val, st.strokeWidth);
    else if (key == "fill-rule") st.evenOdd = val == "evenodd";
}

void applyStyleString(Style& st, std::string_view css) {
    size_t pos = 0;
    while (pos < css.size()) {
        size_t semi = css.find(';', pos);
        if (semi == std::string_view::npos) semi = css.size();
        std::string_view decl = css.substr(pos, semi - pos);
        const size_t colon = decl.find(':');
        if (colon != std::string_view::npos) applyStyleProp(st, trim(decl.substr(0, colon)), trim(decl.substr(colon + 1)));
        pos = semi + 1;
    }
}

Transform parseTransform(std::string_view s) {
    Transform t;
    size_t pos = 0;
    while (pos < s.size()) {
        const size_t open = s.find('(', pos);
        if (open == std::string_view::npos) break;
        const size_t close = s.find(')', open);
        if (close == std::string_view::npos) break;
        const std::string name = trim(s.substr(pos, open - pos));
        std::vector<double> args;
        std::string_view a = s.substr(open + 1, close - open - 1);
        size_t i = 0;
        while (i < a.size()) {
            while (i < a.size() && (a[i] == ' ' || a[i] == ',')) ++i;
            size_t j = i;
            while (j < a.size() && a[j] != ' ' && a[j] != ',') ++j;
            if (j > i) args.push_back(toNum(a.substr(i, j - i)));
            i = j;
        }
        Transform m;
        if (name == "translate" && !args.empty()) {
            m.tx = args[0];
            m.ty = args.size() > 1 ? args[1] : 0;
        } else if (name == "scale" && !args.empty()) {
            m.sx = args[0];
            m.sy = args.size() > 1 ? args[1] : args[0];
        }
        t = t.then(m);
        pos = close + 1;
    }
    return t;
}

struct Tag {
    std::string name;
    std::map<std::string, std::string> attrs;
    bool closing = false;
    bool selfClosing = false;
};

// 次のタグを読む。コメント・宣言・テキストは飛ばす
bool nextTag(const std::string& s, size_t& pos, Tag& tag) {
    for (;;) {
        pos = s.find('<', pos);
        if (pos == std::string::npos) return false;
        if (s.compare(pos, 4, "<!--") == 0) {
            pos = s.find("-->", pos);
            if (pos == std::string::npos) return false;
            pos += 3;
            continue;
        }
        if (s[pos + 1] == '?' || s[pos + 1] == '!') {
            pos = s.find('>', pos);
            if (pos == std::string::npos) return false;
            ++pos;
            continue;
        }
        break;
    }
    tag = Tag{};
    size_t i = pos + 1;
    if (s[i] == '/') {
        tag.closing = true;
        ++i;
    }
    size_t j = i;
    while (j < s.size() && !std::isspace(static_cast<unsigned char>(s[j])) && s[j] != '>' && s[j] != '/') ++j;
    tag.name = s.substr(i, j - i);
    i = j;
    while (i < s.size()) {
        while (i < s.size() && std::isspace(static_cast<unsigned char>(s[i]))) ++i;
        if (i >= s.size()) return false;
        if (s[i] == '>') {
            pos = i + 1;
            return true;
        }
        if (s[i] == '/' && i + 1 < s.size() && s[i + 1] == '>') {
            tag.selfClosing = true;
            pos = i + 2;
            return true;
        }
        size_t k = i;
        while (k < s.size() && s[k] != '=' && s[k] != '>' && !std::isspace(static_cast<unsigned char>(s[k]))) ++k;
        std::string key = s.substr(i, k - i);
        while (k < s.size() && std::isspace(static_cast<unsigned char>(s[k]))) ++k;
        if (k < s.size() && s[k] == '=') {
            ++k;
            while (k < s.size() && std::isspace(static_cast<unsigned char>(s[k]))) ++k;
            const char q = s[k];
            if (q == '"' || q == '\'') {
                const size_t e = s.find(q, k + 1);
                if (e == std::string::npos) return false;
                tag.attrs[key] = s.substr(k + 1, e - k - 1);
                i = e + 1;
            } else {
                size_t e = k;
                while (e < s.size() && !std::isspace(static_cast<unsigned char>(s[e])) && s[e] != '>') ++e;
                tag.attrs[key] = s.substr(k, e - k);
                i = e;
            }
        } else {
            tag.attrs[key] = "";
            i = k;
        }
    }
    return false;
}

struct SubPath {
    std::vector<Vec2> pts;
    bool closed = false;
};

// SVG のパスデータ（M L H V C Q A Z とその相対版）
std::vector<SubPath> parsePathData(std::string_view d) {
    std::vector<SubPath> out;
    size_t i = 0;
    char cmd = 0;
    Vec2 cur{0, 0}, start{0, 0};
    auto skip = [&] {
        while (i < d.size() && (std::isspace(static_cast<unsigned char>(d[i])) || d[i] == ',')) ++i;
    };
    auto number = [&](double& v) -> bool {
        skip();
        if (i >= d.size()) return false;
        const char* b = d.data() + i;
        auto r = std::from_chars(b, d.data() + d.size(), v);
        if (r.ec != std::errc()) return false;
        i = r.ptr - d.data();
        return true;
    };
    auto flag = [&](double& v) -> bool {
        skip();
        if (i >= d.size() || (d[i] != '0' && d[i] != '1')) return false;
        v = d[i] == '1' ? 1 : 0;
        ++i;
        return true;
    };
    auto cur_path = [&]() -> SubPath& {
        if (out.empty()) out.push_back({{cur}, false});
        return out.back();
    };
    for (;;) {
        skip();
        if (i >= d.size()) break;
        if (std::isalpha(static_cast<unsigned char>(d[i])) && d[i] != 'e' && d[i] != 'E') {
            cmd = d[i++];
            if (cmd == 'Z' || cmd == 'z') {
                if (!out.empty()) out.back().closed = true;
                cur = start;
                continue;
            }
        }
        if (!cmd) break;
        const bool rel = std::islower(static_cast<unsigned char>(cmd));
        const char c = static_cast<char>(std::toupper(static_cast<unsigned char>(cmd)));
        double a[7];
        if (c == 'M') {
            if (!number(a[0]) || !number(a[1])) break;
            cur = rel ? cur + Vec2{a[0], a[1]} : Vec2{a[0], a[1]};
            start = cur;
            out.push_back({{cur}, false});
            cmd = rel ? 'l' : 'L';  // 続く座標は lineto
        } else if (c == 'L') {
            if (!number(a[0]) || !number(a[1])) break;
            cur = rel ? cur + Vec2{a[0], a[1]} : Vec2{a[0], a[1]};
            cur_path().pts.push_back(cur);
        } else if (c == 'H') {
            if (!number(a[0])) break;
            cur.x = rel ? cur.x + a[0] : a[0];
            cur_path().pts.push_back(cur);
        } else if (c == 'V') {
            if (!number(a[0])) break;
            cur.y = rel ? cur.y + a[0] : a[0];
            cur_path().pts.push_back(cur);
        } else if (c == 'C' || c == 'Q') {
            const int n = c == 'C' ? 6 : 4;
            bool ok = true;
            for (int k = 0; k < n; ++k) ok = ok && number(a[k]);
            if (!ok) break;
            std::vector<Vec2> ctrl{cur};
            for (int k = 0; k < n; k += 2) ctrl.push_back(rel ? cur + Vec2{a[k], a[k + 1]} : Vec2{a[k], a[k + 1]});
            if (c == 'Q') {  // 2次を3次に
                const Vec2 p0 = ctrl[0], q = ctrl[1], p2 = ctrl[2];
                ctrl = {p0, p0 + 2.0 / 3.0 * (q - p0), p2 + 2.0 / 3.0 * (q - p2), p2};
            }
            auto pts = geom::bezierPoints(ctrl);
            auto& sp = cur_path();
            sp.pts.insert(sp.pts.end(), pts.begin() + 1, pts.end());
            cur = ctrl.back();
        } else if (c == 'A') {
            if (!number(a[0]) || !number(a[1]) || !number(a[2]) || !flag(a[3]) || !flag(a[4]) || !number(a[5]) ||
                !number(a[6]))
                break;
            const Vec2 p1 = rel ? cur + Vec2{a[5], a[6]} : Vec2{a[5], a[6]};
            double rx = std::abs(a[0]), ry = std::abs(a[1]);
            const double phi = a[2] * std::numbers::pi / 180.0;
            auto& sp = cur_path();
            if (rx == 0 || ry == 0) {
                sp.pts.push_back(p1);
            } else {
                // SVG 実装ノート F.6.5 の端点表現 -> 中心表現
                const double cp = std::cos(phi), sn = std::sin(phi);
                const Vec2 dp = (cur - p1) / 2.0;
                const Vec2 q{cp * dp.x + sn * dp.y, -sn * dp.x + cp * dp.y};
                double lam = (q.x * q.x) / (rx * rx) + (q.y * q.y) / (ry * ry);
                if (lam > 1) {
                    rx *= std::sqrt(lam);
                    ry *= std::sqrt(lam);
                }
                const double num = rx * rx * ry * ry - rx * rx * q.y * q.y - ry * ry * q.x * q.x;
                const double den = rx * rx * q.y * q.y + ry * ry * q.x * q.x;
                double coef = den > 0 ? std::sqrt(std::max(0.0, num / den)) : 0;
                if (a[3] == a[4]) coef = -coef;
                const Vec2 cq{coef * rx * q.y / ry, -coef * ry * q.x / rx};
                const Vec2 c0 = Vec2{cp * cq.x - sn * cq.y, sn * cq.x + cp * cq.y} + (cur + p1) / 2.0;
                auto ang = [](Vec2 u, Vec2 v) {
                    const double s = u.x * v.y - u.y * v.x;
                    return std::atan2(s, glm::dot(u, v));
                };
                const Vec2 v1{(q.x - cq.x) / rx, (q.y - cq.y) / ry};
                const Vec2 v2{(-q.x - cq.x) / rx, (-q.y - cq.y) / ry};
                const double t1 = ang({1, 0}, v1);
                double dt = ang(v1, v2);
                if (!a[4] && dt > 0) dt -= 2 * std::numbers::pi;
                if (a[4] && dt < 0) dt += 2 * std::numbers::pi;
                const int n = std::clamp(static_cast<int>(std::ceil(std::abs(dt) / (std::numbers::pi / 64))), 2, 512);
                for (int k = 1; k <= n; ++k) {
                    const double t = t1 + dt * k / n;
                    const Vec2 e{rx * std::cos(t), ry * std::sin(t)};
                    sp.pts.push_back(c0 + Vec2{cp * e.x - sn * e.y, sn * e.x + cp * e.y});
                }
            }
            cur = p1;
        } else {
            break;  // 未対応のコマンド
        }
    }
    return out;
}

void emit(const std::vector<SubPath>& subs, const Style& st, const Transform& xf, geom::Paths& out) {
    std::vector<std::vector<Vec2>> rings;
    for (const auto& sp : subs) {
        std::vector<Vec2> pts;
        pts.reserve(sp.pts.size());
        for (Vec2 p : sp.pts) pts.push_back(xf.apply(p));
        rings.push_back(std::move(pts));
    }
    const double w = st.strokeWidth * std::abs(xf.sx);
    if (st.fill) {
        geom::Paths fill;
        for (const auto& r : rings)
            if (r.size() >= 3) fill.push_back(geom::toPath(r));
        if (!fill.empty())
            geom::append(out, geom::unite(fill, st.evenOdd ? Clipper2Lib::FillRule::EvenOdd
                                                           : Clipper2Lib::FillRule::NonZero));
    }
    if (st.stroke && w > 0) {
        for (size_t k = 0; k < rings.size(); ++k) geom::append(out, geom::strokePolyline(rings[k], w, subs[k].closed));
    }
}

}  // namespace

SvgResult readKicadSvg(const std::string& text) {
    SvgResult res;
    std::vector<std::pair<Style, Transform>> stack{{Style{}, Transform{}}};
    size_t pos = 0;
    Tag tag;
    while (nextTag(text, pos, tag)) {
        if (tag.closing) {
            if ((tag.name == "g" || tag.name == "svg") && stack.size() > 1) stack.pop_back();
            continue;
        }
        Style st = stack.back().first;
        Transform xf = stack.back().second;
        for (const char* k : {"fill", "stroke", "stroke-width", "fill-rule"}) {
            auto it = tag.attrs.find(k);
            if (it != tag.attrs.end()) applyStyleProp(st, k, it->second);
        }
        if (auto it = tag.attrs.find("style"); it != tag.attrs.end()) applyStyleString(st, it->second);
        if (auto it = tag.attrs.find("transform"); it != tag.attrs.end()) xf = xf.then(parseTransform(it->second));

        if (tag.name == "svg") {
            // width="297mm" と viewBox の比から mm への換算を決める
            auto w = tag.attrs.find("width");
            auto vb = tag.attrs.find("viewBox");
            if (w != tag.attrs.end() && vb != tag.attrs.end()) {
                double vals[4] = {0, 0, 0, 0};
                size_t i = 0, k = 0;
                const std::string& v = vb->second;
                while (k < 4 && i < v.size()) {
                    while (i < v.size() && (v[i] == ' ' || v[i] == ',')) ++i;
                    size_t j = i;
                    while (j < v.size() && v[j] != ' ' && v[j] != ',') ++j;
                    if (j > i) vals[k++] = toNum(std::string_view(v).substr(i, j - i));
                    i = j;
                }
                const double mm = toNum(w->second);
                if (vals[2] > 0 && w->second.find("mm") != std::string::npos) {
                    const double s = mm / vals[2];
                    xf = xf.then(Transform{s, s, -vals[0] * s, -vals[1] * s});
                }
            }
            if (!tag.selfClosing) stack.push_back({st, xf});
            continue;
        }
        if (tag.name == "g") {
            if (!tag.selfClosing) stack.push_back({st, xf});
            continue;
        }
        if (tag.name == "path") {
            auto d = tag.attrs.find("d");
            if (d != tag.attrs.end()) {
                emit(parsePathData(d->second), st, xf, res.paths);
                ++res.elements;
            }
        } else if (tag.name == "circle") {
            const Vec2 c = xf.apply({toNum(tag.attrs["cx"]), toNum(tag.attrs["cy"])});
            const double r = toNum(tag.attrs["r"]) * std::abs(xf.sx);
            const double w = st.strokeWidth * std::abs(xf.sx);
            if (st.fill) geom::append(res.paths, geom::disc(c, r + (st.stroke ? w / 2 : 0)));
            else if (st.stroke && w > 0) geom::append(res.paths, geom::strokePolyline(geom::circlePoints(c, r), w, true));
            ++res.elements;
        } else if (tag.name == "polyline" || tag.name == "polygon") {
            std::string d = "M " + tag.attrs["points"];
            auto subs = parsePathData(d);
            if (tag.name == "polygon" && !subs.empty()) subs.back().closed = true;
            emit(subs, st, xf, res.paths);
            ++res.elements;
        } else if (tag.name == "rect") {
            const double x = toNum(tag.attrs["x"]), y = toNum(tag.attrs["y"]);
            const double w = toNum(tag.attrs["width"]), h = toNum(tag.attrs["height"]);
            emit({SubPath{{{x, y}, {x + w, y}, {x + w, y + h}, {x, y + h}}, true}}, st, xf, res.paths);
            ++res.elements;
        }
        // <text> は KiCad が補助情報として出すだけなので無視する
    }
    res.ok = true;
    return res;
}

}  // namespace krbr::raster
