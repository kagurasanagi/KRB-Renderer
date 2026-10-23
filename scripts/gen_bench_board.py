"""krbr bench の同梱の基板（bench/krbr_bench.kicad_pcb）と、それが使う 3D モデル（bench/3dmodels/）を作る。
KiCad 10 の標準のフットプリントと STEP だけを使い、決まった配置で並べる（乱数を使わないので、何度作っても同じ）。
モデルのパスは ${KIPRJMOD}/3dmodels/... に書き換え、ファイルを bench/3dmodels/ に写すので、基板はこのフォルダだけで閉じる。
シルクには線と図形だけを置く（線フォントの文字は kicad-cli が要り、環境で結果が変わるので置かない）。
使い方：python scripts/gen_bench_board.py [KiCad の share/kicad のフォルダ]（既定 C:/Program Files/KiCad/10.0/share/kicad）
"""
import math
import shutil
import sys
import uuid
from pathlib import Path

KICAD = Path(sys.argv[1] if len(sys.argv) > 1 else "C:/Program Files/KiCad/10.0/share/kicad")
OUT = Path(__file__).resolve().parent.parent / "bench"
BOARD = OUT / "krbr_bench.kicad_pcb"
UUID_NS = uuid.uuid5(uuid.NAMESPACE_URL, "krbr-bench")

# 基板の範囲（KiCad の座標、mm）と角の丸み
X0, Y0, X1, Y1 = 100.0, 100.0, 200.0, 170.0
CORNER = 3.0


# ---- S 式 -------------------------------------------------------------------------------------------

class Q(str):
    """引用符で囲んで書く文字列"""


def parse(text):
    pos = 0
    n = len(text)

    def skip():
        nonlocal pos
        while pos < n and text[pos] in " \t\r\n":
            pos += 1

    def node():
        nonlocal pos
        skip()
        if text[pos] == "(":
            pos += 1
            items = []
            while True:
                skip()
                if text[pos] == ")":
                    pos += 1
                    return items
                items.append(node())
        if text[pos] == '"':
            pos += 1
            out = []
            while text[pos] != '"':
                if text[pos] == "\\":
                    pos += 1
                    out.append({"n": "\n", "t": "\t"}.get(text[pos], text[pos]))
                else:
                    out.append(text[pos])
                pos += 1
            pos += 1
            return Q("".join(out))
        start = pos
        while text[pos] not in " \t\r\n()":
            pos += 1
        return text[start:pos]

    return node()


def fmt_atom(a):
    if isinstance(a, Q):
        return '"' + a.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n") + '"'
    if isinstance(a, float):
        s = f"{a:.6f}".rstrip("0").rstrip(".")
        return "0" if s in ("-0", "") else s
    return str(a)


def write(node, indent=0, out=None):
    """KiCad と同じく、子がすべて原子なら 1 行、そうでなければタブで字下げする"""
    out = [] if out is None else out
    pad = "\t" * indent
    if all(not isinstance(c, list) for c in node):
        out.append(pad + "(" + " ".join(fmt_atom(c) for c in node) + ")")
        return out
    if node and node[0] == "pts":
        out.append(pad + "(pts")
        for i in range(1, len(node), 4):
            out.append(pad + "\t" + " ".join("(" + " ".join(fmt_atom(c) for c in xy) + ")" for xy in node[i:i + 4]))
        out.append(pad + ")")
        return out
    head = [c for c in node if not isinstance(c, list)]
    out.append(pad + "(" + " ".join(fmt_atom(c) for c in head))
    for c in node:
        if isinstance(c, list):
            write(c, indent + 1, out)
    out.append(pad + ")")
    return out


def child(node, tag):
    return next((c for c in node if isinstance(c, list) and c and c[0] == tag), None)


# ---- 共通 -------------------------------------------------------------------------------------------

_uuid_count = 0


def new_uuid():
    global _uuid_count
    _uuid_count += 1
    return ["uuid", Q(str(uuid.uuid5(UUID_NS, str(_uuid_count))))]


def rot(x, y, deg):
    """KiCad の回転（y が下向きの座標で、正の角度は画面で反時計回り）。krbr-kicad の rotate_point と同じ"""
    a = math.radians(deg)
    s, c = math.sin(a), math.cos(a)
    return x * c + y * s, -x * s + y * c


def stroke(width):
    return ["stroke", ["width", float(width)], ["type", "solid"]]


# ---- フットプリント -----------------------------------------------------------------------------

_lib_cache = {}
models_used = set()


def load_footprint(lib_id):
    if lib_id not in _lib_cache:
        lib, name = lib_id.split(":")
        _lib_cache[lib_id] = parse((KICAD / "footprints" / f"{lib}.pretty" / f"{name}.kicad_mod").read_text(encoding="utf-8"))
    return _lib_cache[lib_id]


def courtyard(fp):
    """フットプリントの座標での F.CrtYd の範囲 (x0, y0, x1, y1)"""
    xs, ys = [], []
    for c in fp:
        if not isinstance(c, list) or child(c, "layer") is None or child(c, "layer")[1] != "F.CrtYd":
            continue
        for tag in ("start", "end", "mid", "center"):
            p = child(c, tag)
            if p:
                xs.append(float(p[1]))
                ys.append(float(p[2]))
        pts = child(c, "pts")
        if pts:
            for xy in pts[1:]:
                xs.append(float(xy[1]))
                ys.append(float(xy[2]))
        if c[0] == "fp_circle":
            cx, cy = float(child(c, "center")[1]), float(child(c, "center")[2])
            r = math.hypot(float(child(c, "end")[1]) - cx, float(child(c, "end")[2]) - cy)
            xs += [cx - r, cx + r]
            ys += [cy - r, cy + r]
    return min(xs), min(ys), max(xs), max(ys)


class Placed:
    def __init__(self, lib_id, ref, value, x, y, angle):
        self.lib_id, self.ref, self.value, self.x, self.y, self.angle = lib_id, ref, value, x, y, angle
        self.src = load_footprint(lib_id)
        cx0, cy0, cx1, cy1 = courtyard(self.src)
        pts = [self.to_board(px, py) for px, py in ((cx0, cy0), (cx1, cy0), (cx0, cy1), (cx1, cy1))]
        self.box = (min(p[0] for p in pts), min(p[1] for p in pts), max(p[0] for p in pts), max(p[1] for p in pts))

    def to_board(self, x, y):
        dx, dy = rot(x, y, self.angle)
        return self.x + dx, self.y + dy

    def pads(self):
        """パッドの番号と基板の座標"""
        out = []
        for c in self.src:
            if isinstance(c, list) and c and c[0] == "pad":
                at = child(c, "at")
                out.append((str(c[1]), self.to_board(float(at[1]), float(at[2]))))
        return out

    def pad(self, number):
        return next(p for n, p in self.pads() if n == number)

    def node(self):
        lib, _ = self.lib_id.split(":")
        out = ["footprint", Q(self.lib_id), ["layer", Q("F.Cu")], new_uuid(), ["at", self.x, self.y] + ([float(self.angle)] if self.angle else [])]
        for c in self.src[2:]:
            if not isinstance(c, list):
                continue
            tag = c[0]
            if tag in ("version", "generator", "generator_version", "layer", "fp_text", "fp_text_box"):
                continue
            if tag == "property":
                # 文字は部品図の層にだけ置く（シルクの線フォントの文字は kicad-cli が要る）
                if c[1] not in ("Reference", "Value"):
                    continue
                at = child(c, "at")
                angle = (float(at[3]) if len(at) > 3 else 0.0) + self.angle
                c = ["property", c[1], Q(self.ref if c[1] == "Reference" else self.value), ["at", at[1], at[2], angle % 360.0],
                     ["layer", Q("F.Fab")], new_uuid(), child(c, "effects")]
            elif tag == "pad":
                c = [x for x in c if not (isinstance(x, list) and x[0] == "uuid")]
                at = child(c, "at")
                angle = ((float(at[3]) if len(at) > 3 else 0.0) + self.angle) % 360.0
                at[:] = ["at", at[1], at[2]] + ([angle] if angle else [])
                c.append(new_uuid())
            elif tag == "model":
                spec = str(c[1])
                rel = spec.split("}/", 1)[1] if spec.startswith("${") else spec
                if not (KICAD / "3dmodels" / rel).is_file():
                    raise SystemExit(f"{self.lib_id}: model not found: {spec}")
                models_used.add(rel)
                c = ["model", Q("${KIPRJMOD}/3dmodels/" + rel)] + c[2:]
            elif tag.startswith("fp_") or tag == "zone":
                c = [x for x in c if not (isinstance(x, list) and x[0] == "uuid")] + [new_uuid()]
            out.append(c)
        return out


placed = []


def place(lib_id, ref, value, x, y, angle=0.0):
    p = Placed(lib_id, ref, value, x, y, angle)
    placed.append(p)
    return p


_ref_count = {}


def next_ref(prefix):
    _ref_count[prefix] = _ref_count.get(prefix, 0) + 1
    return f"{prefix}{_ref_count[prefix]}"


# ---- 基板の項目 -------------------------------------------------------------------------------------

items = []


def segment(a, b, width, layer="F.Cu", net="SIG"):
    items.append(["segment", ["start", a[0], a[1]], ["end", b[0], b[1]], ["width", width], ["layer", Q(layer)], ["net", Q(net)], new_uuid()])


def track(points, width, layer="F.Cu", net="SIG"):
    for a, b in zip(points, points[1:]):
        segment(a, b, width, layer, net)


def arc_track(start, mid, end, width, layer="F.Cu", net="SIG"):
    items.append(["arc", ["start", start[0], start[1]], ["mid", mid[0], mid[1]], ["end", end[0], end[1]], ["width", width], ["layer", Q(layer)], ["net", Q(net)], new_uuid()])


vias = []


def via(p, size=0.6, drill=0.3, net="GND"):
    vias.append(p)
    items.append(["via", ["at", p[0], p[1]], ["size", size], ["drill", drill], ["layers", Q("F.Cu"), Q("B.Cu")], ["net", Q(net)], new_uuid()])


def gr(kind, layer, width, **pts):
    n = ["gr_" + kind]
    for k, v in pts.items():
        n.append([k, v[0], v[1]])
    n += [stroke(width)]
    if kind in ("rect", "circle"):
        n.append(["fill", "no"])
    n += [["layer", Q(layer)], new_uuid()]
    items.append(n)


def zone(layer, polys, net="GND"):
    """塗りのデータごとのゾーン（塗りは KiCad で塗り直すまでこのまま。輪郭は塗りと同じ）"""
    for i, pts in enumerate(polys):
        p = ["pts"] + [["xy", x, y] for x, y in pts]
        items.append(["zone", ["net", Q(net)], ["layer", Q(layer)], new_uuid(), ["hatch", "edge", 0.5], ["connect_pads", ["clearance", 0.2]],
                      ["min_thickness", 0.2], ["fill", "yes", ["thermal_gap", 0.3], ["thermal_bridge_width", 0.3]],
                      ["polygon", p], ["filled_polygon", ["layer", Q(layer)], p]])


def rounded_rect_pts(x0, y0, x1, y1, r, steps=8):
    pts = []
    for cx, cy, a0 in ((x1 - r, y0 + r, -90), (x1 - r, y1 - r, 0), (x0 + r, y1 - r, 90), (x0 + r, y0 + r, 180)):
        for i in range(steps + 1):
            a = math.radians(a0 + 90 * i / steps)
            pts.append((round(cx + r * math.cos(a), 4), round(cy + r * math.sin(a), 4)))
    return pts


def overlaps(a, b, gap=0.0):
    return a[0] < b[2] + gap and b[0] < a[2] + gap and a[1] < b[3] + gap and b[1] < a[3] + gap


# ---- 配置 -------------------------------------------------------------------------------------------

def layout():
    # BGA と、まわりの 0402 のパスコン（左・上・下。右は配線に空ける）
    bga = place("Package_BGA:BGA-256_17.0x17.0mm_Layout16x16_P1.0mm_Ball0.5mm_Pad0.4mm_NSMD", "U1", "SoC", 122.0, 124.0)
    for i in range(8):
        t = -7.0 + 2.0 * i
        place("Capacitor_SMD:C_0402_1005Metric", next_ref("C"), "100n", 122.0 - 11.0, 124.0 + t, 90.0)
        place("Capacitor_SMD:C_0402_1005Metric", next_ref("C"), "100n", 122.0 + t, 124.0 - 11.0)
        place("Capacitor_SMD:C_0402_1005Metric", next_ref("C"), "100n", 122.0 + t, 124.0 + 11.0)

    # BGA の内側の玉：玉の間のビアへ短い線（ドッグボーン）。いちばん右の列は外への配線に使う
    for n, (x, y) in bga.pads():
        col = int("".join(ch for ch in n if ch.isdigit()))
        if col == 16 or col == 1 or n[0] in "AT":
            continue
        v = (x + 0.5, y + 0.5)
        segment((x, y), v, 0.15)
        via(v, 0.45, 0.2, "SIG")

    # LQFP-64 と水晶、パスコン
    qfp = place("Package_QFP:LQFP-64_10x10mm_P0.5mm", "U2", "MCU", 154.0, 120.0)
    place("Crystal:Crystal_SMD_3225-4Pin_3.2x2.5mm", "Y1", "24MHz", 154.0, 131.5)
    place("Capacitor_SMD:C_0402_1005Metric", next_ref("C"), "12p", 149.5, 131.5, 90.0)
    place("Capacitor_SMD:C_0402_1005Metric", next_ref("C"), "12p", 158.5, 131.5, 90.0)
    for dx, dy in ((-8.0, -8.0), (8.0, -8.0), (-8.0, 8.0), (8.0, 8.0)):
        place("Capacitor_SMD:C_0402_1005Metric", next_ref("C"), "100n", 154.0 + dx, 120.0 + dy, 45.0)

    # BGA の右の列（A16..T16）から LQFP の左の辺（1..16）へ 45° の曲がりで
    rows = "ABCDEFGHJKLMNPRT"
    for i, r in enumerate(rows):
        a = bga.pad(f"{r}16")
        b = qfp.pad(str(i + 1))
        k = abs(b[1] - a[1])
        x1 = a[0] + 3.0 + 0.25 * i
        track([a, (x1, a[1]), (x1 + k, b[1]), b], 0.15)

    # QFN と SOIC・TSSOP、LQFP の右の辺から QFN の左の辺へ（円弧の配線も混ぜる）
    qfn = place("Package_DFN_QFN:QFN-32-1EP_5x5mm_P0.5mm_EP3.45x3.45mm", "U3", "PHY", 176.0, 117.0)
    place("Package_SO:SOIC-8_3.9x4.9mm_P1.27mm", "U4", "FLASH", 190.0, 119.0, 90.0)
    place("Package_SO:TSSOP-20_4.4x6.5mm_P0.65mm", "U5", "BUF", 176.0, 130.0, 90.0)
    for i in range(8):
        a = qfp.pad(str(49 - 1 - i - 8))  # 右の辺（33..48）の中ほど
        b = qfn.pad(str(1 + i))
        xm = (a[0] + b[0]) / 2
        if i % 2 == 0:
            track([a, (xm - 1.0, a[1]), (xm - 1.0 + abs(b[1] - a[1]), b[1]), b], 0.15)
        else:
            # 弦の中点から、弦に垂直に弦の長さの 0.2 倍だけ膨らませた円弧
            h = abs(b[1] - a[1]) / 2
            start = (xm - h, a[1])
            end = (xm + h, b[1])
            dx, dy = end[0] - start[0], end[1] - start[1]
            mid = ((start[0] + end[0]) / 2 + 0.2 * dy, (start[1] + end[1]) / 2 - 0.2 * dx)
            track([a, start], 0.15)
            arc_track(start, mid, end, 0.15)
            track([end, b], 0.15)

    # USB-C（上の辺。口が外を向くよう 180° 回す）
    place("Connector_USB:USB_C_Receptacle_GCT_USB4105-xx-A_16P_TopMnt_Horizontal", "J1", "USB-C", 188.0, Y0 + 3.675, 180.0)

    # 電源：インダクタ、電解コンデンサ、レギュレータ、ダイオード、トランジスタ
    place("Inductor_SMD:L_Bourns_SRR1260", "L1", "10u", 189.0, 155.0)
    place("Capacitor_SMD:CP_Elec_6.3x7.7", "C100", "100u", 174.0, 157.0)
    place("Package_TO_SOT_SMD:SOT-223-3_TabPin2", "U6", "3V3", 174.0, 143.5, 90.0)
    place("Diode_SMD:D_SOD-123", "D1", "SS14", 189.0, 143.0)
    place("Diode_SMD:D_SOD-123", "D2", "SS14", 189.0, 146.5)
    for i in range(4):
        place("Package_TO_SOT_SMD:SOT-23", next_ref("Q"), "MOSFET", 182.0 + 4.0 * i, 137.5)

    # 受動部品の格子（左下）：0402 と 0603 の R・C、0805 の列。隣どうしを線でつなぎ、列の端にビア
    gx0, gy0 = 104.5, 141.0
    for row in range(8):
        y = gy0 + row * 2.6
        kinds = ("R_0402", "C_0402", "R_0603", "C_0603")
        prev = None
        for col in range(16):
            kind = kinds[(row + col) % 4]
            lib = "Resistor_SMD" if kind[0] == "R" else "Capacitor_SMD"
            size = "1005Metric" if kind.endswith("0402") else "1608Metric"
            x = gx0 + col * 3.0
            p = place(f"{lib}:{kind}_{size}", next_ref(kind[0]), "10k" if kind[0] == "R" else "1u", x, y)
            if prev is not None:
                segment(prev.pad("2"), p.pad("1"), 0.2)
            else:
                via((p.pad("1")[0] - 1.2, y))
                segment(p.pad("1"), (p.pad("1")[0] - 1.2, y), 0.2)
            prev = p
        end = prev.pad("2")
        via((end[0] + 1.2, y))
        segment(end, (end[0] + 1.2, y), 0.2)
    for col in range(10):
        place("Capacitor_SMD:C_0805_2012Metric", next_ref("C"), "10u", 106.0 + col * 4.4, 162.5)

    # LED の列と電流制限の抵抗（下の辺に沿って）
    for i in range(10):
        x = 106.0 + i * 4.4
        place("LED_SMD:LED_0603_1608Metric", next_ref("D"), "LED", x, 166.0)

    # タクトスイッチ
    place("Button_Switch_SMD:SW_SPST_TL3342", "SW1", "RESET", 156.0, 152.0)
    place("Button_Switch_SMD:SW_SPST_TL3342", "SW2", "BOOT", 156.0, 162.0)

    # ピンヘッダ（上の辺に沿って横向き）
    place("Connector_PinHeader_2.54mm:PinHeader_2x10_P2.54mm_Vertical", "J2", "GPIO", 110.0, 106.5, 90.0)

    # 取り付け穴（四隅）
    for x, y in ((X0 + 4, Y0 + 4), (X1 - 4, Y0 + 4), (X0 + 4, Y1 - 4), (X1 - 4, Y1 - 4)):
        place("MountingHole:MountingHole_3.2mm_M3_Pad_Via", next_ref("H"), "M3", x, y)


def check_layout():
    errors = []
    for i, a in enumerate(placed):
        b0 = a.box
        if b0[0] < X0 - 1e-6 or b0[2] > X1 + 1e-6 or b0[1] < Y0 - 2.0 or b0[3] > Y1 + 1e-6:
            errors.append(f"{a.ref} ({a.lib_id}) is outside the board: {b0}")
        for b in placed[i + 1:]:
            if overlaps(b0, b.box) and not (a.ref.startswith("H") or b.ref.startswith("H")):
                errors.append(f"{a.ref} and {b.ref} overlap: {b0} / {b.box}")
    if errors:
        raise SystemExit("\n".join(errors))


def free_strips():
    """外形に沿った幅 2.5 mm の帯のうち、部品の範囲にかからない部分（表面のベタ）"""
    inset0, inset1 = 0.6, 3.0
    strips = []

    def cut(lo, hi, horizontal, c0, c1):
        boxes = [p.box for p in placed if not p.ref.startswith("H")]
        spans = []
        for b in boxes:
            r = (b[0], b[1], b[2], b[3])
            band = (lo, c0, hi, c1) if horizontal else (c0, lo, c1, hi)
            if overlaps(r, band, 0.3):
                spans.append((r[0] - 0.4, r[2] + 0.4) if horizontal else (r[1] - 0.4, r[3] + 0.4))
        spans.sort()
        t = lo
        out = []
        for s0, s1 in spans:
            if s0 > t:
                out.append((t, s0))
            t = max(t, s1)
        if t < hi:
            out.append((t, hi))
        return [(a, b) for a, b in out if b - a > 2.0]

    # 角の丸みと取り付け穴を避けて、四辺の帯
    m = CORNER + 5.0
    for a, b in cut(X0 + m, X1 - m, True, Y0 + inset0, Y0 + inset1):
        strips.append([(a, Y0 + inset0), (b, Y0 + inset0), (b, Y0 + inset1), (a, Y0 + inset1)])
    for a, b in cut(X0 + m, X1 - m, True, Y1 - inset1, Y1 - inset0):
        strips.append([(a, Y1 - inset1), (b, Y1 - inset1), (b, Y1 - inset0), (a, Y1 - inset0)])
    for a, b in cut(Y0 + m, Y1 - m, False, X0 + inset0, X0 + inset1):
        strips.append([(X0 + inset0, a), (X0 + inset1, a), (X0 + inset1, b), (X0 + inset0, b)])
    for a, b in cut(Y0 + m, Y1 - m, False, X1 - inset1, X1 - inset0):
        strips.append([(X1 - inset1, a), (X1 - inset0, a), (X1 - inset0, b), (X1 - inset1, b)])
    return strips


def stitching(strips):
    """表面のベタの帯の中ほどに、3 mm ごとのビア"""
    for s in strips:
        xs = [p[0] for p in s]
        ys = [p[1] for p in s]
        cx, cy = (min(xs) + max(xs)) / 2, (min(ys) + max(ys)) / 2
        if max(xs) - min(xs) > max(ys) - min(ys):
            n = int((max(xs) - min(xs) - 1.0) / 3.0)
            for i in range(n + 1):
                via((round(min(xs) + 0.5 + i * 3.0, 3), round(cy, 3)))
        else:
            n = int((max(ys) - min(ys) - 1.0) / 3.0)
            for i in range(n + 1):
                via((round(cx, 3), round(min(ys) + 0.5 + i * 3.0, 3)))


def board_graphics():
    # 外形（角の丸い四角）
    r = CORNER
    w = 0.05
    gr("line", "Edge.Cuts", w, start=(X0 + r, Y0), end=(X1 - r, Y0))
    gr("line", "Edge.Cuts", w, start=(X1, Y0 + r), end=(X1, Y1 - r))
    gr("line", "Edge.Cuts", w, start=(X1 - r, Y1), end=(X0 + r, Y1))
    gr("line", "Edge.Cuts", w, start=(X0, Y1 - r), end=(X0, Y0 + r))
    k = r * (1 - math.sqrt(0.5))
    gr("arc", "Edge.Cuts", w, start=(X0, Y0 + r), mid=(X0 + k, Y0 + k), end=(X0 + r, Y0))
    gr("arc", "Edge.Cuts", w, start=(X1 - r, Y0), mid=(X1 - k, Y0 + k), end=(X1, Y0 + r))
    gr("arc", "Edge.Cuts", w, start=(X1, Y1 - r), mid=(X1 - k, Y1 - k), end=(X1 - r, Y1))
    gr("arc", "Edge.Cuts", w, start=(X0 + r, Y1), mid=(X0 + k, Y1 - k), end=(X0, Y1 - r))
    # シルク：区画の枠と目印
    gr("rect", "F.SilkS", 0.15, start=(102.5, 138.8), end=(153.2, 168.3))
    gr("rect", "F.SilkS", 0.15, start=(168.5, 134.0), end=(197.5, 163.5))
    for i in range(5):
        gr("circle", "F.SilkS", 0.15, center=(110.0 + i * 2.0, 135.5), end=(110.6 + i * 2.0, 135.5))
    gr("line", "F.SilkS", 0.2, start=(140.0, 135.5), end=(163.0, 135.5))
    # 裏のシルク
    gr("rect", "B.SilkS", 0.15, start=(110.0, 112.0), end=(190.0, 158.0))


def header():
    layers = [(0, "F.Cu", "signal"), (4, "In1.Cu", "signal"), (6, "In2.Cu", "signal"), (2, "B.Cu", "signal"), (9, "F.Adhes", "user", "F.Adhesive"), (11, "B.Adhes", "user", "B.Adhesive"),
              (13, "F.Paste", "user"), (15, "B.Paste", "user"), (5, "F.SilkS", "user", "F.Silkscreen"), (7, "B.SilkS", "user", "B.Silkscreen"), (1, "F.Mask", "user"), (3, "B.Mask", "user"),
              (17, "Dwgs.User", "user", "User.Drawings"), (19, "Cmts.User", "user", "User.Comments"), (25, "Edge.Cuts", "user"), (27, "Margin", "user"),
              (31, "F.CrtYd", "user", "F.Courtyard"), (29, "B.CrtYd", "user", "B.Courtyard"), (35, "F.Fab", "user"), (33, "B.Fab", "user")]
    stack = [["layer", Q("F.SilkS"), ["type", Q("Top Silk Screen")]], ["layer", Q("F.Paste"), ["type", Q("Top Solder Paste")]],
             ["layer", Q("F.Mask"), ["type", Q("Top Solder Mask")], ["thickness", 0.01]], ["layer", Q("F.Cu"), ["type", Q("copper")], ["thickness", 0.035]],
             ["layer", Q("dielectric 1"), ["type", Q("prepreg")], ["thickness", 0.1], ["material", Q("FR4")], ["epsilon_r", 4.5], ["loss_tangent", 0.02]],
             ["layer", Q("In1.Cu"), ["type", Q("copper")], ["thickness", 0.035]],
             ["layer", Q("dielectric 2"), ["type", Q("core")], ["thickness", 1.24], ["material", Q("FR4")], ["epsilon_r", 4.5], ["loss_tangent", 0.02]],
             ["layer", Q("In2.Cu"), ["type", Q("copper")], ["thickness", 0.035]],
             ["layer", Q("dielectric 3"), ["type", Q("prepreg")], ["thickness", 0.1], ["material", Q("FR4")], ["epsilon_r", 4.5], ["loss_tangent", 0.02]],
             ["layer", Q("B.Cu"), ["type", Q("copper")], ["thickness", 0.035]], ["layer", Q("B.Mask"), ["type", Q("Bottom Solder Mask")], ["thickness", 0.01]],
             ["layer", Q("B.Paste"), ["type", Q("Bottom Solder Paste")]], ["layer", Q("B.SilkS"), ["type", Q("Bottom Silk Screen")]],
             ["copper_finish", Q("ENIG")], ["dielectric_constraints", "no"]]
    return [["version", "20260206"], ["generator", Q("krbr_gen_bench_board")], ["generator_version", Q("10.0")],
            ["general", ["thickness", 1.6], ["legacy_teardrops", "no"]], ["paper", Q("A4")],
            ["layers"] + [[str(l[0]), Q(l[1]), l[2]] + ([Q(l[3])] if len(l) > 3 else []) for l in layers],
            ["setup", ["stackup"] + stack, ["pad_to_mask_clearance", 0], ["allow_soldermask_bridges_in_footprints", "no"],
             ["tenting", ["front", "yes"], ["back", "yes"]], ["aux_axis_origin", X0, Y0], ["grid_origin", X0, Y0]]]


def main():
    if not (KICAD / "footprints").is_dir():
        raise SystemExit(f"KiCad library not found: {KICAD}")
    layout()
    check_layout()
    strips = free_strips()
    stitching(strips)
    board_graphics()
    # 内層と裏面は全面の GND、表面は外形に沿った帯
    full = rounded_rect_pts(X0 + 0.5, Y0 + 0.5, X1 - 0.5, Y1 - 0.5, CORNER - 0.5)
    for layer in ("In1.Cu", "In2.Cu", "B.Cu"):
        zone(layer, [full])
    zone("F.Cu", strips)

    root = ["kicad_pcb"] + header() + [p.node() for p in placed] + items + [["embedded_fonts", "no"]]
    OUT.mkdir(exist_ok=True)
    models = OUT / "3dmodels"
    if models.exists():
        shutil.rmtree(models)
    for rel in sorted(models_used):
        dst = models / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(KICAD / "3dmodels" / rel, dst)
    BOARD.write_text("\n".join(write(root)) + "\n", encoding="utf-8", newline="\n")
    size = sum(f.stat().st_size for f in models.rglob("*") if f.is_file())
    print(f"{BOARD}: {len(placed)} footprints, {len(vias)} vias, {len(models_used)} models ({size / 1e6:.1f} MB)")


if __name__ == "__main__":
    main()
