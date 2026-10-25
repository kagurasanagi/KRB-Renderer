"""krbr のアイコンを作る（元絵 rust/krbr/assets/krbr_icon.png のチップ部品。角は透明）。
出力：rust/krbr/assets/krbr.ico（exe に埋め込む。16〜256 px）、krbr_64.rgba（ビューアのウィンドウの 64x64 RGBA）、
      krbr_256.png（About の絵）、kicad_plugin/krbr_preview/krbr_preview.png（KiCad のツールバーのボタン。24 px と 48 px）
使い方：python scripts/make_icon.py   （Pillow が要る）
"""
from pathlib import Path

from PIL import Image


def main() -> None:
    out = Path(__file__).resolve().parent.parent / "rust" / "krbr" / "assets"
    im = Image.open(out / "krbr_icon.png").convert("RGBA")
    sizes = [16, 24, 32, 48, 64, 128, 256]
    big = im.resize((256, 256), Image.LANCZOS)
    big.save(out / "krbr.ico", sizes=[(s, s) for s in sizes])
    (out / "krbr_64.rgba").write_bytes(im.resize((64, 64), Image.LANCZOS).tobytes())
    big.save(out / "krbr_256.png")
    # KiCad のツールバーのボタン（kicad_plugin/krbr_preview）
    plugin = out.parent.parent.parent / "kicad_plugin" / "krbr_preview"
    plugin.mkdir(parents=True, exist_ok=True)
    im.resize((24, 24), Image.LANCZOS).save(plugin / "krbr_preview.png")
    im.resize((48, 48), Image.LANCZOS).save(plugin / "krbr_preview_48.png")
    print("wrote", out, plugin)


if __name__ == "__main__":
    main()
