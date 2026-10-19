#pragma once
// .kicad_pcb から読み取った基板の内容。座標はすべて基板座標（mm、x 右・y 下）に変換済み。
#include <glm/glm.hpp>

#include <cstdint>
#include <filesystem>
#include <optional>
#include <string>
#include <vector>

namespace krbr::kicad {

using Vec2 = glm::dvec2;
using Vec3 = glm::dvec3;

enum class GrKind { Line, Arc, Circle, Poly, Curve };

// 基板座標の矩形
struct Box2 {
    Vec2 lo{1e30}, hi{-1e30};
    void add(Vec2 p) {
        lo = glm::min(lo, p);
        hi = glm::max(hi, p);
    }
    void add(Vec2 p, double r) {
        add(p - Vec2{r, r});
        add(p + Vec2{r, r});
    }
    void add(const Box2& b) {
        if (b.valid()) {
            add(b.lo);
            add(b.hi);
        }
    }
    bool valid() const { return lo.x <= hi.x && lo.y <= hi.y; }
    bool intersects(const Box2& b) const {
        return valid() && b.valid() && lo.x <= b.hi.x && b.lo.x <= hi.x && lo.y <= b.hi.y && b.lo.y <= hi.y;
    }
    Box2 expanded(double m) const { return valid() ? Box2{lo - Vec2{m, m}, hi + Vec2{m, m}} : *this; }
};

// 差分の検出用：ファイルの最上位の項目（部品・配線・ビア・ベタ・図形）ごとのキーと内容のハッシュと範囲
struct BoardItem {
    std::string key;     // UUID（無ければ内容のハッシュ）
    uint64_t hash = 0;   // 項目の S 式全体のハッシュ
    Box2 bbox;           // 銅箔・レジスト・シルクなどの範囲（基板座標）
    bool edgeCuts = false;     // 基板外形を含む
    bool holes = false;        // 穴を含む
    bool strokeSilk = false;   // 線フォントのシルクの文字を含む（kicad-cli で描き直しが要る）
};

// 図形（基板直下の gr_* と、部品内の fp_*、文字の render_cache）
struct Graphic {
    GrKind kind = GrKind::Line;
    std::string layer;
    double width = 0.0;  // 線幅（0 なら輪郭なし）
    bool filled = false;
    Vec2 start{}, mid{}, end{};       // Line: start-end / Arc: start-mid-end / Circle: start=中心, end=円周上
    std::vector<Vec2> pts;            // Poly: 頂点 / Curve: ベジェ制御点4つ
    int footprint = -1;               // 所属部品（-1 は基板）
    bool fromText = false;            // 文字（render_cache）由来
};

enum class PadType { Smd, ThruHole, NpThruHole, Connect };
enum class PadShape { Circle, Rect, Oval, Trapezoid, RoundRect, Custom };

struct Drill {
    bool present = false;
    bool oval = false;
    Vec2 size{};    // 円なら x=y=直径
    Vec2 offset{};  // パッド座標系でのずれ
};

struct Pad {
    std::string number;
    PadType type = PadType::Smd;
    PadShape shape = PadShape::Rect;
    Vec2 pos{};             // 基板座標
    double angleDeg = 0.0;  // パッドの絶対角度（KiCad のファイルは部品回転込み）
    Vec2 size{};
    Vec2 rectDelta{};
    double roundRatio = 0.0;
    double chamferRatio = 0.0;
    unsigned chamferCorners = 0;  // bit0: top_left, 1: top_right, 2: bottom_left, 3: bottom_right
    Drill drill;
    std::vector<std::string> layers;
    std::optional<double> maskMargin;
    PadShape customAnchor = PadShape::Circle;
    std::vector<Graphic> primitives;  // カスタムパッド（パッド座標系、未回転）
    int footprint = -1;

    bool onLayer(const std::string& layer) const;
};

struct Track {
    Vec2 start{}, end{};
    std::optional<Vec2> mid;  // 円弧配線
    double width = 0.0;
    std::string layer;
};

struct Via {
    Vec2 pos{};
    double size = 0.0;
    double drill = 0.0;
    std::string layerFrom = "F.Cu", layerTo = "B.Cu";
    std::optional<bool> tentFront, tentBack;
};

struct ZoneFill {
    std::string layer;
    std::vector<Vec2> pts;  // KiCad の fractured polygon（穴は切れ目でつながった単一輪郭）
};

// 線フォントの文字（render_cache が無いので自前では形を作れない）
struct StrokeText {
    std::string text;
    std::string layer;
    Vec2 pos{};
    int footprint = -1;
};

struct Model3D {
    std::string path;  // ファイルに書かれたままの指定
    Vec3 offset{0.0};
    Vec3 scale{1.0};
    Vec3 rotate{0.0};
    bool hidden = false;
    double opacity = 1.0;
};

struct Footprint {
    std::string libId;
    std::string reference;
    std::string value;
    Vec2 pos{};
    double angleDeg = 0.0;
    bool back = false;  // 裏面実装
    bool smd = false, throughHole = false, dnp = false, boardOnly = false;
    std::vector<Model3D> models;
};

struct Stackup {
    double thickness = 1.6;
    std::string copperFinish;  // "ENIG" / "HAL SnPb" / "None" など
    std::string maskColorFront, maskColorBack, silkColorFront, silkColorBack;
    double copperThickness = 0.035;
};

struct BoardSetup {
    double padToMaskClearance = 0.0;
    bool tentFront = true, tentBack = true;
};

struct Board {
    std::filesystem::path file;
    std::filesystem::path projectDir;
    std::string projectName;
    int version = 0;
    Stackup stackup;
    BoardSetup setup;
    std::vector<Graphic> graphics;  // 基板直下・部品内の図形すべて
    std::vector<Pad> pads;
    std::vector<Track> tracks;
    std::vector<Via> vias;
    std::vector<ZoneFill> zones;
    std::vector<StrokeText> strokeTexts;
    std::vector<Footprint> footprints;
    int unfilledZones = 0;
    std::vector<BoardItem> items;  // 差分の検出用
    uint64_t setupHash = 0;        // general / setup / layers のハッシュ（変われば全体を作り直す）
};

// 前回の読み込みとの差分
struct BoardDiff {
    bool full = false;          // 外形・層構成などが変わった（全体を作り直す）
    bool holesChanged = false;  // 穴が変わった（基材の側面・穴の内壁を作り直す）
    bool strokeSilkChanged = false;
    std::vector<Box2> dirty;    // 作り直しが要る範囲
    size_t added = 0, removed = 0, changed = 0;
};
BoardDiff diffBoards(const Board& before, const Board& after);

struct ParseResult {
    std::optional<Board> board;
    std::string error;
};

ParseResult loadBoard(const std::filesystem::path& file);

// 基板座標系での回転（KiCad 準拠：y 下向き座標で正の角度は画面上で反時計回り）
Vec2 rotatePoint(Vec2 p, double angleDeg);

}  // namespace krbr::kicad
