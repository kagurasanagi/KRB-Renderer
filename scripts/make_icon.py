"""krbr のアイコンを作る（基板・IC・金の配線とパッド）。
出力：rust/krbr/assets/krbr.ico（exe に埋め込む。16〜256 px）、krbr_64.rgba（ビューアのウィンドウの 64x64 RGBA）、
      kicad_plugin/krbr_preview/krbr_preview.png（KiCad のツールバーのボタン。24 px と 48 px）
使い方：python scripts/make_icon.py   （Pillow が要る）
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

S = 1024  # 大きく描いて縮める（縁をなめらかにする）


def draw() -> Image.Image:
    im = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    gold = (226, 182, 92, 255)
    # 影
    shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle((70, 100, S - 50, S - 30), radius=150, fill=(0, 0, 0, 120))
    im.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(30)))
    # 基板（緑のレジスト、縁は FR4 の色）
    d.rounded_rectangle((48, 48, S - 48, S - 48), radius=150, fill=(158, 148, 108, 255))
    d.rounded_rectangle((70, 70, S - 70, S - 70), radius=132, fill=(18, 112, 48, 255))
    # 配線（金）
    w = 34
    for y in (300, 420, 604, 724):
        d.line((70, y, 300, y), fill=gold, width=w)
        d.line((S - 300, y, S - 70, y), fill=gold, width=w)
    d.line((512, 70, 512, 290), fill=gold, width=w)
    d.line((512, S - 290, 512, S - 70), fill=gold, width=w)
    # 取り付け穴のパッド
    for cx, cy in ((180, 180), (S - 180, 180), (180, S - 180), (S - 180, S - 180)):
        d.ellipse((cx - 62, cy - 62, cx + 62, cy + 62), fill=gold)
        d.ellipse((cx - 28, cy - 28, cx + 28, cy + 28), fill=(30, 30, 30, 255))
    # IC（黒いモールドと錫の足）
    for i in range(5):
        y = 330 + i * 92
        d.rounded_rectangle((262, y, 330, y + 46), radius=8, fill=(205, 205, 200, 255))
        d.rounded_rectangle((S - 330, y, S - 262, y + 46), radius=8, fill=(205, 205, 200, 255))
    d.rounded_rectangle((300, 290, S - 300, S - 290), radius=36, fill=(26, 26, 28, 255))
    # 1 番ピンの印と、上面の光沢
    d.ellipse((350, 340, 400, 390), fill=(70, 70, 74, 255))
    gloss = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(gloss).polygon([(300, 290), (S - 300, 290), (S - 300, 420), (300, 600)], fill=(255, 255, 255, 34))
    im.alpha_composite(gloss)
    return im


def main() -> None:
    out = Path(__file__).resolve().parent.parent / "rust" / "krbr" / "assets"
    out.mkdir(parents=True, exist_ok=True)
    im = draw()
    sizes = [16, 24, 32, 48, 64, 128, 256]
    im.resize((256, 256), Image.LANCZOS).save(out / "krbr.ico", sizes=[(s, s) for s in sizes])
    (out / "krbr_64.rgba").write_bytes(im.resize((64, 64), Image.LANCZOS).tobytes())
    im.resize((256, 256), Image.LANCZOS).save(out / "krbr_256.png")
    # KiCad のツールバーのボタン（kicad_plugin/krbr_preview）
    plugin = out.parent.parent.parent / "kicad_plugin" / "krbr_preview"
    plugin.mkdir(parents=True, exist_ok=True)
    im.resize((24, 24), Image.LANCZOS).save(plugin / "krbr_preview.png")
    im.resize((48, 48), Image.LANCZOS).save(plugin / "krbr_preview_48.png")
    print("wrote", out, plugin)


if __name__ == "__main__":
    main()
