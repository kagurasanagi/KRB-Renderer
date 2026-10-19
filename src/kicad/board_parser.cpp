#include <cmath>
#include <numbers>

#include "core/log.hpp"
#include "core/platform.hpp"
#include "kicad/board.hpp"
#include "kicad/sexpr.hpp"

namespace krbr::kicad {

using sexpr::Node;

Vec2 rotatePoint(Vec2 p, double angleDeg) {
    if (angleDeg == 0.0) return p;
    const double a = angleDeg * std::numbers::pi / 180.0;
    const double c = std::cos(a), s = std::sin(a);
    // y 下向き座標系で、画面上の反時計回りが正
    return {p.x * c + p.y * s, -p.x * s + p.y * c};
}

bool Pad::onLayer(const std::string& layer) const {
    for (const auto& l : layers) {
        if (l == layer) return true;
        if (l.size() > 2 && l[0] == '*' && l[1] == '.') {
            // "*.Cu" -> 任意の ".Cu"
            const std::string suffix = l.substr(1);
            if (layer.size() > suffix.size() && layer.compare(layer.size() - suffix.size(), suffix.size(), suffix) == 0)
                return true;
        }
        if (l.rfind("F&B.", 0) == 0) {
            const std::string suffix = l.substr(3);
            if (layer == "F" + suffix || layer == "B" + suffix) return true;
        }
    }
    return false;
}

namespace {

Vec2 xy(const Node* n, size_t i = 1) {
    if (!n) return {};
    return {n->num(i), n->num(i + 1)};
}

Vec3 xyz(const Node* n) {
    if (!n) return {};
    const Node* x = n->child("xyz");
    if (!x) return {};
    return {x->num(1), x->num(2), x->num(3)};
}

double strokeWidth(const Node& n) {
    if (const Node* s = n.child("stroke"))
        if (auto w = s->childNum("width")) return *w;
    if (auto w = n.childNum("width")) return *w;
    return 0.0;
}

bool isFilled(const Node& n) {
    const Node* f = n.child("fill");
    if (!f) return false;
    const std::string v = f->str(1);
    return v == "yes" || v == "solid";
}

bool isHidden(const Node& n) {
    if (n.hasAtom("hide")) return true;
    if (auto h = n.childBool("hide")) return *h;
    if (const Node* e = n.child("effects")) {
        if (e->hasAtom("hide")) return true;
        if (auto h = e->childBool("hide")) return *h;
    }
    return false;
}

// 3点を通る円弧を折れ線にする（ゾーンの pts 内の arc 用）
void appendArc(std::vector<Vec2>& out, Vec2 a, Vec2 m, Vec2 b, double tol = 0.001) {
    const double d = 2.0 * (a.x * (m.y - b.y) + m.x * (b.y - a.y) + b.x * (a.y - m.y));
    if (std::abs(d) < 1e-12) {
        out.push_back(a);
        out.push_back(b);
        return;
    }
    const double a2 = glm::dot(a, a), m2 = glm::dot(m, m), b2 = glm::dot(b, b);
    const Vec2 c{(a2 * (m.y - b.y) + m2 * (b.y - a.y) + b2 * (a.y - m.y)) / d,
                 (a2 * (b.x - m.x) + m2 * (a.x - b.x) + b2 * (m.x - a.x)) / d};
    const double r = glm::length(a - c);
    double t0 = std::atan2(a.y - c.y, a.x - c.x);
    double tm = std::atan2(m.y - c.y, m.x - c.x);
    double t1 = std::atan2(b.y - c.y, b.x - c.x);
    auto norm = [](double t) {
        while (t < 0) t += 2 * std::numbers::pi;
        while (t >= 2 * std::numbers::pi) t -= 2 * std::numbers::pi;
        return t;
    };
    double sweep = norm(t1 - t0);
    if (norm(tm - t0) > sweep) sweep -= 2 * std::numbers::pi;  // 逆回り
    const double step = 2.0 * std::acos(std::max(-1.0, 1.0 - tol / std::max(r, 1e-9)));
    const int n = std::max(2, static_cast<int>(std::ceil(std::abs(sweep) / std::max(step, 1e-4))));
    for (int i = 0; i <= n; ++i) {
        const double t = t0 + sweep * i / n;
        out.push_back(c + r * Vec2{std::cos(t), std::sin(t)});
    }
}

std::vector<Vec2> readPts(const Node* pts) {
    std::vector<Vec2> out;
    if (!pts) return out;
    for (const auto& it : pts->items) {
        if (!it.isList) continue;
        if (it.tag() == "xy") {
            out.push_back(xy(&it));
        } else if (it.tag() == "arc") {
            appendArc(out, xy(it.child("start")), xy(it.child("mid")), xy(it.child("end")));
        }
    }
    return out;
}

struct Xform {
    Vec2 pos{};
    double angle = 0.0;
    Vec2 apply(Vec2 p) const { return pos + rotatePoint(p, angle); }
};

// gr_* / fp_* を読む。prefix 抜きのタグ名（line, arc, ...）で判定
bool readGraphic(const Node& n, std::string_view kind, const Xform& xf, int fp, Graphic& g) {
    g.layer = n.childStr("layer").value_or("");
    g.width = strokeWidth(n);
    g.filled = isFilled(n);
    g.footprint = fp;
    if (kind == "line") {
        g.kind = GrKind::Line;
        g.start = xf.apply(xy(n.child("start")));
        g.end = xf.apply(xy(n.child("end")));
    } else if (kind == "arc") {
        g.kind = GrKind::Arc;
        g.start = xf.apply(xy(n.child("start")));
        g.mid = xf.apply(xy(n.child("mid")));
        g.end = xf.apply(xy(n.child("end")));
        if (!n.child("mid")) return false;  // 旧形式（angle 指定）は非対応
    } else if (kind == "circle") {
        g.kind = GrKind::Circle;
        g.start = xf.apply(xy(n.child("center")));
        g.end = xf.apply(xy(n.child("end")));
    } else if (kind == "rect") {
        g.kind = GrKind::Poly;
        const Vec2 a = xy(n.child("start")), b = xy(n.child("end"));
        g.pts = {xf.apply(a), xf.apply({b.x, a.y}), xf.apply(b), xf.apply({a.x, b.y})};
    } else if (kind == "poly") {
        g.kind = GrKind::Poly;
        for (Vec2 p : readPts(n.child("pts"))) g.pts.push_back(xf.apply(p));
    } else if (kind == "curve" || kind == "bezier") {
        g.kind = GrKind::Curve;
        for (Vec2 p : readPts(n.child("pts"))) g.pts.push_back(xf.apply(p));
        if (g.pts.size() != 4) return false;
    } else {
        return false;
    }
    return true;
}

// 文字（property / fp_text / gr_text）。render_cache があれば多角形として取り込む
void readText(const Node& n, int fp, Board& board) {
    if (isHidden(n)) return;
    const std::string layer = n.childStr("layer").value_or("");
    if (layer.empty()) return;
    const Node* cache = n.child("render_cache");
    if (cache) {
        for (const Node* poly : cache->children("polygon")) {
            Graphic g;
            g.kind = GrKind::Poly;
            g.layer = layer;
            g.filled = true;
            g.footprint = fp;
            g.fromText = true;
            g.pts = readPts(poly->child("pts"));  // render_cache は基板座標
            if (g.pts.size() >= 3) board.graphics.push_back(std::move(g));
        }
        return;
    }
    // property の場合 items[1] が名前、items[2] が値。fp_text は items[1] が種別、items[2] が文字
    std::string text = n.tag() == "gr_text" ? n.str(1) : n.str(2);
    if (text.empty()) return;
    StrokeText st;
    st.text = text;
    st.layer = layer;
    st.pos = xy(n.child("at"));
    st.footprint = fp;
    board.strokeTexts.push_back(std::move(st));
}

PadShape parseShape(const std::string& s) {
    if (s == "circle") return PadShape::Circle;
    if (s == "rect") return PadShape::Rect;
    if (s == "oval") return PadShape::Oval;
    if (s == "trapezoid") return PadShape::Trapezoid;
    if (s == "roundrect") return PadShape::RoundRect;
    if (s == "custom") return PadShape::Custom;
    return PadShape::Rect;
}

void readPad(const Node& n, const Xform& xf, int fp, Board& board) {
    Pad p;
    p.number = n.str(1);
    const std::string type = n.str(2);
    if (type == "smd") p.type = PadType::Smd;
    else if (type == "thru_hole") p.type = PadType::ThruHole;
    else if (type == "np_thru_hole") p.type = PadType::NpThruHole;
    else p.type = PadType::Connect;
    p.shape = parseShape(n.str(3));
    p.footprint = fp;
    const Node* at = n.child("at");
    p.pos = xf.apply(xy(at));
    p.angleDeg = at ? at->num(3, 0.0) : 0.0;
    p.size = xy(n.child("size"));
    p.rectDelta = xy(n.child("rect_delta"));
    p.roundRatio = n.childNum("roundrect_rratio").value_or(0.25);
    p.chamferRatio = n.childNum("chamfer_ratio").value_or(0.0);
    if (const Node* ch = n.child("chamfer")) {
        for (size_t i = 1; i < ch->size(); ++i) {
            const std::string c = ch->str(i);
            if (c == "top_left") p.chamferCorners |= 1;
            if (c == "top_right") p.chamferCorners |= 2;
            if (c == "bottom_left") p.chamferCorners |= 4;
            if (c == "bottom_right") p.chamferCorners |= 8;
        }
    }
    if (const Node* d = n.child("drill")) {
        p.drill.present = true;
        size_t i = 1;
        if (d->str(1) == "oval") {
            p.drill.oval = true;
            i = 2;
        }
        const double w = d->num(i, 0.0);
        const double h = (i + 1 < d->size() && !(*d)[i + 1].isList) ? d->num(i + 1, w) : w;
        p.drill.size = {w, h};
        p.drill.offset = xy(d->child("offset"));
        if (w <= 0.0) p.drill.present = false;
    }
    if (const Node* l = n.child("layers"))
        for (size_t i = 1; i < l->size(); ++i) p.layers.push_back(l->str(i));
    if (auto m = n.childNum("solder_mask_margin")) p.maskMargin = *m;
    if (p.shape == PadShape::Custom) {
        if (const Node* opt = n.child("options"))
            if (auto a = opt->childStr("anchor")) p.customAnchor = parseShape(*a);
        if (const Node* prims = n.child("primitives")) {
            const Xform ident{};
            for (const auto& it : prims->items) {
                if (!it.isList) continue;
                std::string_view tag = it.tag();
                if (tag.rfind("gr_", 0) != 0) continue;
                Graphic g;
                if (readGraphic(it, tag.substr(3), ident, fp, g)) {
                    if (it.has("fill") == false && g.kind == GrKind::Poly) g.filled = true;  // 旧形式の既定
                    p.primitives.push_back(std::move(g));
                }
            }
        }
    }
    if (n.child("padstack")) log::debug("pad {} uses padstack (per-layer shapes); using default layer shape", p.number);
    board.pads.push_back(std::move(p));
}

void readModel(const Node& n, Footprint& fp) {
    Model3D m;
    m.path = n.str(1);
    m.offset = xyz(n.child("offset"));
    if (!n.child("offset") && n.child("at")) m.offset = xyz(n.child("at")) * 25.4;  // 旧形式（インチ）
    if (n.child("scale")) m.scale = xyz(n.child("scale"));
    m.rotate = xyz(n.child("rotate"));
    m.hidden = n.hasAtom("hide") || n.childBool("hide").value_or(false);
    m.opacity = n.childNum("opacity").value_or(1.0);
    fp.models.push_back(std::move(m));
}

void readFootprint(const Node& n, Board& board) {
    Footprint fp;
    fp.libId = n.str(1);
    const std::string layer = n.childStr("layer").value_or("F.Cu");
    fp.back = layer == "B.Cu";
    const Node* at = n.child("at");
    fp.pos = xy(at);
    fp.angleDeg = at ? at->num(3, 0.0) : 0.0;
    if (const Node* a = n.child("attr")) {
        fp.smd = a->hasAtom("smd");
        fp.throughHole = a->hasAtom("through_hole");
        fp.dnp = a->hasAtom("dnp");
        fp.boardOnly = a->hasAtom("board_only");
    }
    if (auto d = n.childBool("dnp")) fp.dnp = fp.dnp || *d;
    const int idx = static_cast<int>(board.footprints.size());
    const Xform xf{fp.pos, fp.angleDeg};

    for (const auto& it : n.items) {
        if (!it.isList) continue;
        const std::string_view tag = it.tag();
        if (tag == "property") {
            const std::string name = it.str(1);
            if (name == "Reference") fp.reference = it.str(2);
            if (name == "Value") fp.value = it.str(2);
            readText(it, idx, board);
        } else if (tag == "fp_text") {
            if (it.str(1) == "reference" && fp.reference.empty()) fp.reference = it.str(2);
            if (it.str(1) == "value" && fp.value.empty()) fp.value = it.str(2);
            readText(it, idx, board);
        } else if (tag.rfind("fp_", 0) == 0) {
            Graphic g;
            if (readGraphic(it, tag.substr(3), xf, idx, g)) board.graphics.push_back(std::move(g));
        } else if (tag == "pad") {
            readPad(it, xf, idx, board);
        } else if (tag == "model") {
            readModel(it, fp);
        } else if (tag == "zone") {
            for (const Node* fpoly : it.children("filled_polygon")) {
                ZoneFill z;
                z.layer = fpoly->childStr("layer").value_or("");
                z.pts = readPts(fpoly->child("pts"));
                if (z.pts.size() >= 3) board.zones.push_back(std::move(z));
            }
        }
    }
    board.footprints.push_back(std::move(fp));
}

void readTenting(const Node* t, std::optional<bool>& front, std::optional<bool>& back) {
    if (!t) return;
    // v10: (tenting (front yes) (back yes))  /  旧: (tenting front back) / (tenting none)
    if (t->child("front") || t->child("back")) {
        if (auto f = t->childBool("front")) front = *f;
        if (auto b = t->childBool("back")) back = *b;
        return;
    }
    front = t->hasAtom("front");
    back = t->hasAtom("back");
}

void readSetup(const Node& n, Board& board) {
    if (auto v = n.childNum("pad_to_mask_clearance")) board.setup.padToMaskClearance = *v;
    std::optional<bool> f, b;
    readTenting(n.child("tenting"), f, b);
    if (f) board.setup.tentFront = *f;
    if (b) board.setup.tentBack = *b;
    if (const Node* st = n.child("stackup")) {
        board.stackup.copperFinish = st->childStr("copper_finish").value_or("");
        for (const Node* l : st->children("layer")) {
            const std::string name = l->str(1);
            const std::string color = l->childStr("color").value_or("");
            if (name == "F.Mask") board.stackup.maskColorFront = color;
            if (name == "B.Mask") board.stackup.maskColorBack = color;
            if (name == "F.SilkS") board.stackup.silkColorFront = color;
            if (name == "B.SilkS") board.stackup.silkColorBack = color;
            if (name == "F.Cu")
                if (auto t = l->childNum("thickness")) board.stackup.copperThickness = *t;
        }
    }
}

}  // namespace

ParseResult loadBoard(const std::filesystem::path& file) {
    ParseResult res;
    auto text = platform::readTextFile(file);
    if (!text) {
        res.error = "cannot read " + platform::pathToUtf8(file);
        return res;
    }
    sexpr::ParseError perr;
    auto root = sexpr::parse(*text, &perr);
    if (!root) {
        res.error = std::format("S-expression parse error at offset {}: {}", perr.offset, perr.message);
        return res;
    }
    if (root->tag() != "kicad_pcb") {
        res.error = "not a kicad_pcb file";
        return res;
    }

    Board board;
    board.file = file;
    board.projectDir = std::filesystem::absolute(file).parent_path();
    board.projectName = platform::pathToUtf8(file.stem());
    board.version = static_cast<int>(root->childNum("version").value_or(0));
    if (const Node* g = root->child("general"))
        if (auto t = g->childNum("thickness")) board.stackup.thickness = *t;

    const Xform ident{};
    for (const auto& it : root->items) {
        if (!it.isList) continue;
        const std::string_view tag = it.tag();
        if (tag == "setup") {
            readSetup(it, board);
        } else if (tag == "footprint" || tag == "module") {
            readFootprint(it, board);
        } else if (tag == "segment") {
            Track t;
            t.start = xy(it.child("start"));
            t.end = xy(it.child("end"));
            t.width = it.childNum("width").value_or(0.0);
            t.layer = it.childStr("layer").value_or("");
            board.tracks.push_back(std::move(t));
        } else if (tag == "arc") {
            Track t;
            t.start = xy(it.child("start"));
            t.mid = xy(it.child("mid"));
            t.end = xy(it.child("end"));
            t.width = it.childNum("width").value_or(0.0);
            t.layer = it.childStr("layer").value_or("");
            board.tracks.push_back(std::move(t));
        } else if (tag == "via") {
            Via v;
            v.pos = xy(it.child("at"));
            v.size = it.childNum("size").value_or(0.0);
            v.drill = it.childNum("drill").value_or(0.0);
            if (const Node* l = it.child("layers")) {
                v.layerFrom = l->str(1, "F.Cu");
                v.layerTo = l->str(2, "B.Cu");
            }
            readTenting(it.child("tenting"), v.tentFront, v.tentBack);
            board.vias.push_back(std::move(v));
        } else if (tag == "zone") {
            const auto fills = it.children("filled_polygon");
            const bool wantsFill = it.child("fill") && it.child("fill")->hasAtom("yes");
            if (fills.empty() && wantsFill && !it.child("keepout")) ++board.unfilledZones;
            for (const Node* fpoly : fills) {
                ZoneFill z;
                z.layer = fpoly->childStr("layer").value_or("");
                z.pts = readPts(fpoly->child("pts"));
                if (z.pts.size() >= 3) board.zones.push_back(std::move(z));
            }
        } else if (tag == "gr_text") {
            readText(it, -1, board);
        } else if (tag.rfind("gr_", 0) == 0) {
            Graphic g;
            if (readGraphic(it, tag.substr(3), ident, -1, g)) board.graphics.push_back(std::move(g));
        }
    }
    res.board = std::move(board);
    return res;
}

}  // namespace krbr::kicad
