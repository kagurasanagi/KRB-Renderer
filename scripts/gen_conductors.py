"""金属の複素屈折率 n, k（測定値）の表を作る。
出典：refractiveindex.info のデータベース（CC0、https://github.com/polyanskiy/refractiveindex.info-database）。
コミットを固定して取り出し、360〜830 nm を 5 nm 刻みに線形補間する（krbr の分光の範囲と同じ）。
出力：rust/crates/krbr-scene/src/conductor_data.rs（手で書き換えないこと）
使い方：python scripts/gen_conductors.py
"""
import cmath
import urllib.parse
import urllib.request
from pathlib import Path

COMMIT = "c5c2f188e848453def5970e347399d653df2ffc2"
BASE = f"https://raw.githubusercontent.com/polyanskiy/refractiveindex.info-database/{COMMIT}/database/data/"
LAMBDA_MIN, LAMBDA_MAX, STEP = 360, 830, 5

# (ID, データのパス（2 つなら一軸性の結晶の常光・異常光）, 出典の説明)
CONDUCTORS = [
    ("Au", ["main/Au/nk/Johnson.yml"], "Au: Johnson & Christy 1972"),
    ("Ag", ["main/Ag/nk/Johnson.yml"], "Ag: Johnson & Christy 1972"),
    ("Cu", ["main/Cu/nk/Johnson.yml"], "Cu: Johnson & Christy 1972"),
    ("Al", ["main/Al/nk/Rakic.yml"], "Al: Rakić 1995"),
    ("Cr", ["main/Cr/nk/Johnson.yml"], "Cr: Johnson & Christy 1974"),
    ("Ni", ["main/Ni/nk/Johnson.yml"], "Ni: Johnson & Christy 1974"),
    ("Pt", ["main/Pt/nk/Rakic-BB.yml"], "Pt: Rakić et al. 1998 (Brendel-Bormann model)"),
    ("Pd", ["main/Pd/nk/Johnson.yml"], "Pd: Johnson & Christy 1974"),
    ("Rh", ["main/Rh/nk/Weaver.yml"], "Rh: Weaver et al. 1977"),
    ("Os", ["main/Os/nk/Nemoshkalenko-o.yml", "main/Os/nk/Nemoshkalenko-e.yml"], "Os: Nemoshkalenko et al. 1968 (polycrystalline average of o and e)"),
    ("Fe", ["main/Fe/nk/Johnson.yml"], "Fe: Johnson & Christy 1974"),
    ("Co", ["main/Co/nk/Johnson.yml"], "Co: Johnson & Christy 1974"),
    ("Ti", ["main/Ti/nk/Johnson.yml"], "Ti: Johnson & Christy 1974"),
    ("W", ["main/W/nk/Weaver.yml"], "W: Weaver et al. 1975"),
    ("Zn", ["main/Zn/nk/Werner.yml"], "Zn: Werner et al. 2009"),
    ("CuZn30", ["other/alloys/Cu-Zn/nk/Querry-Cu70Zn30.yml"], "Brass Cu70Zn30: Querry 1985"),
    ("CuZn15", ["other/alloys/Cu-Zn/nk/Querry-Cu85Zn15.yml"], "Red brass Cu85Zn15: Querry 1985"),
    ("SS_austenitic", ["other/alloys/stainless steel/nk/Karlsson-austenitic.yml"], "Austenitic stainless steel: Karlsson & Ribbing 1982"),
]


def fetch(path: str) -> list[tuple[float, float, float]]:
    url = BASE + urllib.parse.quote(path)
    text = urllib.request.urlopen(url).read().decode("utf-8")
    rows, in_data = [], False
    for line in text.splitlines():
        s = line.strip()
        if s.startswith("type:"):
            if "tabulated nk" not in s:
                raise SystemExit(f"{path}: not tabulated nk ({s})")
            in_data = False
        elif s.startswith("data: |"):
            in_data = True
        elif in_data and s:
            parts = s.split()
            if len(parts) != 3:
                in_data = False
                continue
            rows.append(tuple(float(v) for v in parts))
    rows.sort()
    return rows


def interp(rows, lam_um: float) -> complex:
    if lam_um < rows[0][0] or lam_um > rows[-1][0]:
        raise SystemExit(f"{lam_um} µm is outside the data ({rows[0][0]}..{rows[-1][0]})")
    for (l0, n0, k0), (l1, n1, k1) in zip(rows, rows[1:]):
        if l0 <= lam_um <= l1:
            t = (lam_um - l0) / (l1 - l0) if l1 > l0 else 0.0
            return complex(n0 + t * (n1 - n0), k0 + t * (k1 - k0))
    raise AssertionError


def table(paths: list[str]) -> list[complex]:
    data = [fetch(p) for p in paths]
    out = []
    for lam in range(LAMBDA_MIN, LAMBDA_MAX + 1, STEP):
        nks = [interp(d, lam * 1e-3) for d in data]
        if len(nks) == 1:
            out.append(nks[0])
        else:
            # 一軸性の結晶の多結晶：誘電率を 2:1（常光:異常光）で平均する
            eps = (2 * nks[0] ** 2 + nks[1] ** 2) / 3
            out.append(cmath.sqrt(eps))
    return out


def main() -> None:
    root = Path(__file__).resolve().parent.parent
    dst = root / "rust/crates/krbr-scene/src/conductor_data.rs"
    count = (LAMBDA_MAX - LAMBDA_MIN) // STEP + 1
    lines = [
        "// scripts/gen_conductors.py が作るファイル。手で書き換えないこと。",
        f"// 出典：refractiveindex.info のデータベース（CC0）、コミット {COMMIT}",
        "// 測定値の中に 3.142 のような値がある（円周率ではない）",
        "#![allow(clippy::approx_constant)]",
        "",
        f"pub const LAMBDA_MIN: f32 = {LAMBDA_MIN}.0;",
        f"pub const LAMBDA_STEP: f32 = {STEP}.0;",
        f"pub const SAMPLES: usize = {count};",
        "",
        "/// (ID, 出典, n, k)。波長は LAMBDA_MIN から LAMBDA_STEP 刻み",
        "pub const TABLES: &[(&str, &str, [f32; SAMPLES], [f32; SAMPLES])] = &[",
    ]
    for cid, paths, source in CONDUCTORS:
        nk = table(paths)
        fmt = lambda vs: "[" + ", ".join(f"{v:.4}" for v in vs) + "]"
        lines.append(f'    ("{cid}", "{source}",')
        lines.append(f"        {fmt([c.real for c in nk])},")
        lines.append(f"        {fmt([c.imag for c in nk])}),")
        print(f"{cid}: n(550) = {nk[38].real:.3f}, k(550) = {nk[38].imag:.3f}")
    lines.append("];")
    dst.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    print(f"wrote {dst}")


if __name__ == "__main__":
    main()
