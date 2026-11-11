"""Composes the Lumbre and Lumbre Código icons.

    python3 compose.py <out_dir>

Same recipe as the rest of the Nodalia family (nodalia-brand/retratos/compose2.py): the portrait
is cut out with `mask.png` (made by `mask.swift`, Apple Vision), turned to black and white, and
set on the application's colour. Lumbre's colour is charcoal, so two things differ: the dark
suit is lifted to stay readable against a dark field, and an ember glow behind the figure replaces
the drop shadow, which would be invisible.

Inputs, next to this file: `src.jpg` (see SOURCE.md) and `mask.png`.
"""

import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageOps

HERE = Path(__file__).parent
OUT = Path(sys.argv[1]) if len(sys.argv) > 1 else HERE

# Square crop of the source: x0, y0 and side as fractions of its width.
CROP = (0.17, 0.035, 0.66)
# Field colours, top to bottom, and the accent used for the Lumbre Código mark.
CHARCOAL_TOP, CHARCOAL_BOTTOM = "#453B35", "#1B1512"
EMBER = "#FF6A1F"
# Strength of the ember glow behind the figure, 0..1.
GLOW = 0.55
# Darkest grey the figure may have, so the suit never sinks into the field.
FLOOR = 34
N = 824  # the icon body inside the 1024 canvas, as in the rest of the family


def rgb(value):
    return tuple(int(value[i : i + 2], 16) for i in (1, 3, 5))


def portrait():
    image = Image.open(HERE / "src.jpg").convert("L")
    mask = Image.open(HERE / "mask.png").convert("L").resize(image.size, Image.LANCZOS)
    width = image.size[0]
    x, y, side = (int(value * width) for value in CROP)
    box = (x, y, x + side, y + side)
    grey = image.crop(box).resize((N, N), Image.LANCZOS)
    mask = mask.crop(box).resize((N, N), Image.LANCZOS)

    # The source is an autochrome: calm its grain before sharpening.
    grey = grey.filter(ImageFilter.MedianFilter(5)).filter(ImageFilter.GaussianBlur(0.8))
    hard = mask.point(lambda v: 255 if v > 128 else 0)
    grey = ImageOps.autocontrast(grey, cutoff=(1, 1), mask=hard)
    grey = grey.point(lambda v: int(255 * ((v / 255) ** 0.72)))
    grey = grey.filter(ImageFilter.UnsharpMask(radius=24, percent=55, threshold=0))
    grey = grey.filter(ImageFilter.UnsharpMask(radius=3, percent=90, threshold=2))
    grey = grey.point(lambda v: FLOOR + v * (255 - FLOOR) // 255)

    # Clean contour: close small holes, smooth, pull in slightly so no background rides along.
    mask = mask.point(lambda v: 255 if v > 140 else 0)
    mask = mask.filter(ImageFilter.MaxFilter(13)).filter(ImageFilter.MinFilter(13))
    mask = mask.filter(ImageFilter.GaussianBlur(4)).point(lambda v: 255 if v > 127 else 0)
    mask = mask.filter(ImageFilter.MinFilter(5)).filter(ImageFilter.GaussianBlur(1.4))
    return grey, mask


def field():
    ramp = Image.linear_gradient("L").resize((N, N))
    return Image.composite(Image.new("RGB", (N, N), rgb(CHARCOAL_BOTTOM)), Image.new("RGB", (N, N), rgb(CHARCOAL_TOP)), ramp)


def body():
    grey, mask = portrait()
    base = field()
    # An ember glow behind the figure instead of the family's drop shadow, which a dark field
    # would swallow: it separates the dark suit from the charcoal and is the "lumbre" itself.
    glow = mask.filter(ImageFilter.MaxFilter(31)).filter(ImageFilter.GaussianBlur(38)).point(lambda v: int(v * GLOW))
    base = Image.composite(Image.new("RGB", (N, N), rgb(EMBER)), base, glow)
    return Image.composite(Image.merge("RGB", [grey] * 3), base, mask)


def code_mark(image):
    """Lumbre Código: a prompt, `›_`, on an ember tile in the lower right corner."""
    scale = 4
    size = 268
    tile = Image.new("RGBA", (size * scale, size * scale), (0, 0, 0, 0))
    draw = ImageDraw.Draw(tile)
    draw.rounded_rectangle((0, 0, size * scale - 1, size * scale - 1), radius=62 * scale, fill=rgb(EMBER) + (255,))
    ink = rgb(CHARCOAL_BOTTOM) + (255,)
    stroke = 30 * scale
    chevron = [(70, 78), (128, 134), (70, 190)]
    draw.line([(px * scale, py * scale) for px, py in chevron], fill=ink, width=stroke, joint="curve")
    for px, py in chevron:
        r = stroke // 2
        draw.ellipse((px * scale - r, py * scale - r, px * scale + r, py * scale + r), fill=ink)
    draw.rounded_rectangle((144 * scale, 176 * scale, 206 * scale, 205 * scale), radius=14 * scale, fill=ink)
    tile = tile.resize((size, size), Image.LANCZOS)
    out = image.copy()
    out.paste(tile, (N - size - 46, N - size - 46), tile)
    return out


def export(image, name):
    image.resize((1024, 1024), Image.LANCZOS).save(OUT / f"{name}-fullbleed.png")
    canvas = Image.new("RGBA", (1024, 1024), (0, 0, 0, 0))
    shape = Image.new("L", (N * 4, N * 4), 0)
    ImageDraw.Draw(shape).rounded_rectangle((0, 0, N * 4 - 1, N * 4 - 1), radius=190 * 4, fill=255)
    canvas.paste(image, (100, 100), shape.resize((N, N), Image.LANCZOS))
    canvas.save(OUT / f"{name}.png")
    return canvas


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    lumbre = body()
    icons = [export(lumbre, "lumbre"), export(code_mark(lumbre), "lumbre-code")]
    # A review sheet on light and dark, with the small sizes that matter in a launcher.
    sheet = Image.new("RGB", (1180, 1180), "#F3F4F6")
    ImageDraw.Draw(sheet).rectangle((0, 590, 1180, 1180), fill="#0E1116")
    for row in (0, 1):
        for column, icon in enumerate(icons):
            big = icon.resize((400, 400), Image.LANCZOS)
            sheet.paste(big, (40 + column * 560, 40 + row * 590), big)
            for index, small in enumerate((128, 64, 32, 16)):
                thumb = icon.resize((small, small), Image.LANCZOS)
                sheet.paste(thumb, (460 + column * 560, 60 + row * 590 + index * 20 + sum((128, 64, 32, 16)[:index])), thumb)
    sheet.save(OUT / "preview.png")
