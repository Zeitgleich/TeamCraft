#!/usr/bin/env python3
"""Writes the social preview card of the package site (scripts/site/og-card.png, 1200x630).

Open Graph and Twitter card crawlers fetch this one image when the page is shared. It is drawn like the
hero of the page: the dark theme of scripts/site/style.css, the brand mark, the headline set in Inter
(the font file of the site, converted to outlines so no font has to be installed) and a small desktop
window and phone in the app's colours. No emoji, no text in the mock-ups.

Needs fontTools and brotli (pip install fonttools brotli) and rsvg-convert; ImageMagick (magick)
reduces the PNG to a small palette. Run from the repository root:
python3 scripts/brand/og_card.py
"""

import re
import subprocess
import tempfile
from pathlib import Path

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

ROOT = Path(__file__).resolve().parents[2]
FONT = ROOT / "scripts/site/fonts/inter-latin-variable.woff2"
MARK = ROOT / "assets/brand/mark.svg"
OUT = ROOT / "scripts/site/og-card.png"
WIDTH, HEIGHT = 1200, 630

# The dark theme of scripts/site/style.css and the app (crates/teamcraft-ui/style/main/000-tokens.css).
BG = "#0b0f16"
TEXT = "#eef2f8"
MUTED = "#97a3b6"
ACCENT = "#5c9dff"
CHROME = "#0a0e15"
SIDEBAR = "#0f1520"
SURFACE = "#131a25"
SURFACE_2 = "#19212e"
BORDER = "#2e394c"
CURRENT = "#0064fc"


def instance(weight):
    return instancer.instantiateVariableFont(TTFont(FONT), {"wght": weight})


def text_path(font, text, size, x, y, fill, tracking=0.0):
    """The text as one outlined <path>, baseline at (x, y); tracking in em."""
    scale = size / font["head"].unitsPerEm
    glyphs = font.getGlyphSet()
    cmap = font.getBestCmap()
    pen = SVGPathPen(glyphs)
    cursor = 0.0
    for char in text:
        name = cmap[ord(char)]
        glyphs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, x + cursor, y)))
        cursor += glyphs[name].width * scale + tracking * size
    return f'<path fill="{fill}" d="{pen.getCommands()}"/>'


def mark_group(x, y, size):
    body = MARK.read_text(encoding="utf-8")
    inner = re.search(r"<svg[^>]*>(.*)</svg>", body, re.S).group(1)
    inner = re.sub(r"<!--.*?-->", "", inner, flags=re.S)
    # Ids are scoped to the whole card; the mark is the only user of them.
    scale = size / 1130
    return f'<g transform="translate({x} {y}) scale({scale}) translate(-62 -77)">{inner}</g>'


def message(x, y, width, avatar):
    return (f'<circle cx="{x + 10}" cy="{y + 10}" r="10" fill="{avatar}"/>'
            f'<rect x="{x + 30}" y="{y + 1}" width="64" height="7" rx="3.5" fill="{TEXT}" opacity=".85"/>'
            f'<rect x="{x + 30}" y="{y + 15}" width="{width}" height="7" rx="3.5" fill="{MUTED}" opacity=".55"/>')


def window():
    x, y, w, h = 690, 150, 450, 330
    parts = [
        f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="16" fill="{SURFACE}" stroke="{BORDER}" stroke-width="1.5"/>',
        f'<path d="M{x} {y + 40}V{y + 16}a16 16 0 0 1 16 -16H{x + w - 16}a16 16 0 0 1 16 16V{y + 40}Z" fill="{CHROME}"/>',
        f'<circle cx="{x + 22}" cy="{y + 20}" r="5" fill="{BORDER}"/><circle cx="{x + 40}" cy="{y + 20}" r="5" fill="{BORDER}"/>'
        f'<circle cx="{x + 58}" cy="{y + 20}" r="5" fill="{BORDER}"/>',
        f'<rect x="{x + 150}" y="{y + 11}" width="190" height="18" rx="9" fill="{SIDEBAR}"/>',
        f'<path d="M{x + 56} {y + 40}H{x + 200}V{y + h - 16}a16 16 0 0 1 -16 16H{x + 56}Z" fill="{SIDEBAR}"/>',
        f'<path d="M{x} {y + 40}H{x + 56}V{y + h}H{x + 16}a16 16 0 0 1 -16 -16Z" fill="{CHROME}"/>',
        f'<rect x="{x + 11}" y="{y + 56}" width="34" height="34" rx="10" fill="{CURRENT}"/>',
        f'<rect x="{x + 11}" y="{y + 100}" width="34" height="34" rx="10" fill="{SURFACE_2}"/>',
        f'<rect x="{x + 72}" y="{y + 58}" width="90" height="9" rx="4.5" fill="{TEXT}" opacity=".8"/>',
    ]
    for i, width in enumerate((110, 84, 120, 96, 72)):
        top = y + 86 + i * 30
        fill = CURRENT if i == 1 else "none"
        parts.append(f'<rect x="{x + 62}" y="{top - 6}" width="128" height="24" rx="8" fill="{fill}" opacity=".9"/>')
        parts.append(f'<rect x="{x + 74}" y="{top + 2}" width="{width - 20}" height="8" rx="4" fill="{MUTED}" opacity=".6"/>')
    for i, (width, avatar) in enumerate(((170, ACCENT), (130, "#1de7d5"), (190, "#2cb7fd"), (100, ACCENT))):
        parts.append(message(x + 222, y + 66 + i * 54, width, avatar))
    parts.append(f'<rect x="{x + 222}" y="{y + h - 50}" width="{w - 242}" height="32" rx="10" fill="{SIDEBAR}" stroke="{BORDER}"/>')
    return "\n".join(parts)


def phone():
    x, y, w, h = 1028, 280, 122, 252
    parts = [
        f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="24" fill="{CHROME}" stroke="{BORDER}" stroke-width="3"/>',
        f'<rect x="{x + 12}" y="{y + 14}" width="{w - 24}" height="{h - 28}" rx="14" fill="{SURFACE}"/>',
        f'<rect x="{x + 22}" y="{y + 26}" width="56" height="7" rx="3.5" fill="{TEXT}" opacity=".8"/>',
    ]
    for i in range(3):
        top = y + 52 + i * 46
        side = i % 2
        bubble_x = x + 16 + (side * 20)
        fill = CURRENT if side else SURFACE_2
        parts.append(f'<rect x="{bubble_x}" y="{top}" width="70" height="32" rx="12" fill="{fill}"/>')
        parts.append(f'<rect x="{bubble_x + 10}" y="{top + 12}" width="48" height="7" rx="3.5" fill="{TEXT}" opacity=".7"/>')
    parts.append(f'<rect x="{x + 22}" y="{y + h - 52}" width="{w - 44}" height="22" rx="11" fill="{SIDEBAR}" stroke="{BORDER}"/>')
    return "\n".join(parts)


def card():
    bold = instance(700)
    medium = instance(500)
    headline = [("Team chat your", 0), ("server can’t read.", 1)]
    lines = [text_path(bold, text, 66, 72, 300 + i * 84, TEXT, tracking=-0.02) for text, i in headline]
    sub = [text_path(medium, "Self-hosted and end-to-end encrypted.", 28, 72, 456, MUTED),
           text_path(medium, "Chat, calls and meetings.", 28, 72, 496, MUTED)]
    name = text_path(bold, "TeamCraft", 34, 164, 118, TEXT, tracking=-0.01)
    grid = "".join(f'<path d="M{x} 0V{HEIGHT}" />' for x in range(0, WIDTH + 1, 56)) + \
           "".join(f'<path d="M0 {y}H{WIDTH}" />' for y in range(0, HEIGHT + 1, 56))
    return f"""<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" viewBox="0 0 {WIDTH} {HEIGHT}">
<defs>
<radialGradient id="glow-a" cx="0.5" cy="0" r="0.7"><stop offset="0" stop-color="{ACCENT}" stop-opacity=".22"/><stop offset="1" stop-color="{ACCENT}" stop-opacity="0"/></radialGradient>
<radialGradient id="glow-b" cx="0.85" cy="0.6" r="0.5"><stop offset="0" stop-color="{ACCENT}" stop-opacity=".16"/><stop offset="1" stop-color="{ACCENT}" stop-opacity="0"/></radialGradient>
</defs>
<rect width="{WIDTH}" height="{HEIGHT}" fill="{BG}"/>
<rect width="{WIDTH}" height="{HEIGHT}" fill="url(#glow-a)"/>
<rect width="{WIDTH}" height="{HEIGHT}" fill="url(#glow-b)"/>
<g stroke="#eef2f8" stroke-opacity=".05" stroke-width="1" fill="none">{grid}</g>
{mark_group(72, 60, 72)}
{name}
{chr(10).join(lines)}
{chr(10).join(sub)}
{window()}
{phone()}
</svg>
"""


def main():
    with tempfile.TemporaryDirectory() as temp:
        svg = Path(temp) / "og-card.svg"
        raw = Path(temp) / "og-card.png"
        svg.write_text(card(), encoding="utf-8")
        subprocess.run(["rsvg-convert", "-w", str(WIDTH), "-h", str(HEIGHT), "-o", str(raw), str(svg)], check=True)
        subprocess.run(["magick", str(raw), "-colors", "256", "-strip", "-define", "png:compression-level=9",
                        f"PNG8:{OUT}"], check=True)
    print(f"{OUT.relative_to(ROOT)}: {OUT.stat().st_size} bytes")


if __name__ == "__main__":
    main()
