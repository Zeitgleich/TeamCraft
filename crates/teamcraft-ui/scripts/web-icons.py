#!/usr/bin/env python3
"""Writes the web app's install icons (crates/teamcraft-ui/icons) from the desktop app's icon.

`any` icons keep the rounded square; the maskable icon and the Apple touch icon fill the whole
square with the background (the platform cuts its own shape) and keep the mark inside the safe
zone. Needs Pillow. Run from the repository root: python3 crates/teamcraft-ui/scripts/web-icons.py
"""

from pathlib import Path

from PIL import Image

ROOT = Path(__file__).resolve().parents[3]
SOURCE = ROOT / "apps/desktop/icons/icon-1024.png"
OUT = ROOT / "crates/teamcraft-ui/icons"


def full_bleed(icon: Image.Image) -> Image.Image:
    """The background gradient over the whole square, with the white mark of `icon` on top."""
    width, height = icon.size
    top = icon.getpixel((width // 2, height // 10))
    bottom = icon.getpixel((width // 2, height - height // 10))
    background = Image.new("RGBA", icon.size)
    for y in range(height):
        share = y / (height - 1)
        color = tuple(round(top[i] + (bottom[i] - top[i]) * share) for i in range(3)) + (255,)
        background.paste(color, (0, y, width, y + 1))
    mark = icon.convert("L").point(lambda value: 255 if value > 235 else 0)
    background.paste(Image.new("RGBA", icon.size, (255, 255, 255, 255)), mask=mark)
    return background


def main() -> None:
    icon = Image.open(SOURCE).convert("RGBA")
    OUT.mkdir(exist_ok=True)
    for size in (192, 512):
        icon.resize((size, size), Image.LANCZOS).save(OUT / f"icon-{size}.png", optimize=True)
    bleed = full_bleed(icon)
    bleed.resize((512, 512), Image.LANCZOS).save(OUT / "icon-maskable-512.png", optimize=True)
    bleed.resize((180, 180), Image.LANCZOS).convert("RGB").save(OUT / "apple-touch-icon.png", optimize=True)


if __name__ == "__main__":
    main()
