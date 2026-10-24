"""krbr.exe に入る Rust のクレート（Windows 向け、通常の依存のみ）の名前・版・ライセンス・URL を
rust/crates/krbr-app/web/third_party.json に書く（「KRB Renderer について」の使用ライブラリの一覧）。
依存を変えたら実行し直す:  python scripts/gen_third_party.py
"""
import json
import subprocess
from pathlib import Path

root = Path(__file__).resolve().parent.parent
rust = root / "rust"
out = rust / "crates" / "krbr-app" / "web" / "third_party.json"

meta = json.loads(subprocess.run(
    ["cargo", "metadata", "--format-version", "1", "--offline", "--filter-platform", "x86_64-pc-windows-msvc"],
    cwd=rust, capture_output=True, check=True).stdout)
packages = {p["id"]: p for p in meta["packages"]}
nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
start = next(p["id"] for p in meta["packages"] if p["name"] == "krbr" and p["source"] is None)

seen, stack = set(), [start]
while stack:
    i = stack.pop()
    if i in seen:
        continue
    seen.add(i)
    for d in nodes[i]["deps"]:
        # 通常の依存だけ（ビルド時・テスト時だけのものは exe に入らない）
        if any(k["kind"] is None for k in d["dep_kinds"]):
            stack.append(d["pkg"])

crates = []
for i in seen:
    p = packages[i]
    if p["source"] is None:
        continue  # このリポジトリのクレート
    crates.append({
        "name": p["name"],
        "version": p["version"],
        "license": p.get("license") or (("file: " + p["license_file"]) if p.get("license_file") else "?"),
        "url": p.get("repository") or p.get("homepage") or f"https://crates.io/crates/{p['name']}",
    })
crates.sort(key=lambda c: (c["name"].lower(), c["version"]))
out.write_text(json.dumps(crates, ensure_ascii=False, indent=0) + "\n", encoding="utf-8", newline="\n")
print(f"{len(crates)} crates -> {out}")
