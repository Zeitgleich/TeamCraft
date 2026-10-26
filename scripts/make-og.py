#!/usr/bin/env python3
"""Render the TeamCraft 1200x630 social preview (og:image) from brand assets.

Composes the brand mark (assets/brand/mark.svg) on a dark navy backdrop with
the wordmark and tagline, matching the package-site design system. Output is
written to scripts/site/og-image.png (the path the og:image meta tag uses).

Why a script instead of a hand-drawn static asset: the card is generated from
the same brand mark SVG and brand tokens the rest of the site uses, so it stays
pixel-consistent with the design system and can be regenerated in one command
if the mark or palette ever changes. The rendered PNG is committed so the site
never needs the toolchain at build time.
"""
import io
import pathlib

import cairosvg
from PIL import Image, ImageDraw, ImageFilter, ImageFont

ROOT = pathlib.Path(__file__).resolve().parent.parent
MARK = ROOT / "assets/brand/mark.svg"
OUT = pathlib.Path(__file__).resolve().parent / "site" / "og-image.png"

W, H = 1200, 630
NAVY = (11, 15, 22)          # #0b0f16
NAVY2 = (17, 25, 38)         # #111926
BLUE = (0, 93, 252)          # #005dfc
LIGHT_BLUE = (92, 157, 255)  # #5c9dff
WHITE = (255, 255, 255)
MUTED = (135, 147, 168)      # #8793a8

FONT_DIR = pathlib.Path("/home/paperclip/.local/lib/node_modules/sourcey/dist/og/fonts")
FONT_BOLD = FONT_DIR / "Inter-Bold.ttf"
FONT_REG = FONT_DIR / "Inter-Regular.ttf"

URL = "zeitgleich.github.io/TeamCraft"


def render_mark(size: int) -> Image.Image:
    """Render the brand mark SVG to a square RGBA image."""
    png = cairosvg.svg2png(url=str(MARK), output_width=size, output_height=size)
    return Image.open(io.BytesIO(png)).convert("RGBA")


def vertical_gradient(w: int, h: int, top, bottom) -> Image.Image:
    base = Image.new("RGB", (w, h))
    d = ImageDraw.Draw(base)
    for y in range(h):
        t = y / (h - 1)
        color = tuple(int(top[i] + (bottom[i] - top[i]) * t) for i in range(3))
        d.line([(0, y), (w, y)], fill=color)
    return base


def main() -> None:
    # Backdrop: dark navy vertical gradient.
    bg = vertical_gradient(W, H, NAVY, NAVY2)

    # Subtle radial glow behind the mark.
    glow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    gd = ImageDraw.Draw(glow)
    gx, gy, gr = 300, 315, 420
    for r in range(gr, 0, -4):
        a = int(26 * (1 - r / gr))
        gd.ellipse([gx - r, gy - r, gx + r, gy + r], fill=(0, 93, 252, a))
    glow = glow.filter(ImageFilter.GaussianBlur(40))
    bg = Image.alpha_composite(bg.convert("RGBA"), glow).convert("RGB")

    # Mark on the left.
    mark = render_mark(430)
    bg.paste(mark, (70, (H - 430) // 2), mark)

    # Text on the right.
    d = ImageDraw.Draw(bg)
    f_big = ImageFont.truetype(str(FONT_BOLD), 84)
    f_tag = ImageFont.truetype(str(FONT_REG), 40)
    f_url = ImageFont.truetype(str(FONT_BOLD), 30)

    tx = 560
    ty = 200
    d.text((tx, ty), "TeamCraft", font=f_big, fill=WHITE)
    # Tagline with a blue accent on "can't read".
    d.text((tx, ty + 120), "Team chat your server", font=f_tag, fill=MUTED)
    d.text((tx, ty + 170), "can't read.", font=f_tag, fill=LIGHT_BLUE)

    # URL pill sized to the text so it never overflows.
    url_w = f_url.getlength(URL)
    pad_x = 28
    pad_y = 18
    pill_w = int(url_w) + pad_x * 2
    pill_h = 60
    pill_x = tx
    pill_y = ty + 250
    d.rounded_rectangle(
        [pill_x, pill_y, pill_x + pill_w, pill_y + pill_h],
        radius=pill_h // 2,
        fill=BLUE,
    )
    d.text(
        (pill_x + pad_x, pill_y + (pill_h - 30) // 2),
        URL,
        font=f_url,
        fill=WHITE,
    )

    bg.save(OUT, "PNG")
    print(f"wrote {OUT} ({bg.size}) pill_w={pill_w} url_w={url_w:.0f}")


if __name__ == "__main__":
    main()
