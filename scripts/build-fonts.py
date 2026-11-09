#!/usr/bin/env python3
"""Builds the interface fonts (issue #1494) from their upstream files.

Fonts without a Reserved Font Name are subset to Latin and Latin Extended, plus Greek and Cyrillic
where the font has them; they keep only the weight axis between 400 and 700 (the weights the UI
uses, 000-tokens.css) with every other axis pinned to its default, lose their hinting and keep the
layout features the UI needs (tabular figures among them). A font without tabular figures (Lexend)
gets them here: a `tnum` feature that swaps each digit for a copy centred on the widest digit's
advance, so counts and times don't jitter (the UI asks for them with `font-variant-numeric`).

Fonts with a Reserved Font Name (IBM Plex: "Plex", Source Sans 3: "Source", OpenDyslexic) are
shipped unmodified: the SIL OFL forbids that name on a modified version, and subsetting is a
modification. Their upstream projects publish web files of their own (IBM's split WOFF2 by script,
Adobe's variable WOFF2, OpenDyslexic's WOFF2), which are copied as they are.

The outputs:

- `crates/teamcraft-ui/fonts/*.woff2` for web and desktop (loaded only when someone picks the font,
  `fonts.rs`; desktop embeds the web build), with the licences in `crates/teamcraft-ui/fonts/licenses/`;
- `apps/android/app/src/main/res/font/font_*` for Android, which can't read WOFF2.

Where every input comes from is listed in `crates/teamcraft-ui/fonts/NOTICE.md`. Put the files into
one directory under the names below (`UPSTREAM_NAMES`) and run

        scripts/build-fonts.py <upstream-dir>

It needs fontTools with brotli (`pip install fonttools brotli`). The output is deterministic for
the same inputs and fontTools version.
"""
import argparse
import io
import pathlib
import shutil
import sys

from fontTools import subset
from fontTools.ttLib import TTFont
from fontTools.ttLib.tables import otTables
from fontTools.ttLib.tables._g_l_y_f import Glyph, GlyphComponent
from fontTools.varLib import instancer

ROOT = pathlib.Path(__file__).resolve().parent.parent
WEB_DIR = ROOT / "crates/teamcraft-ui/fonts"
ANDROID_DIR = ROOT / "apps/android/app/src/main/res/font"

# The weights the UI uses (000-tokens.css: --weight-regular to --weight-bold).
MIN_WEIGHT = 400
MAX_WEIGHT = 700

# Ranges per script, inclusive. They must match the `unicode-range`s in teamcraft-ui `fonts.rs`.
LATIN = [
    (0x0000, 0x024F), (0x0259, 0x0259), (0x02B0, 0x02FF), (0x0300, 0x036F), (0x1E00, 0x1EFF),
    (0x2000, 0x206F), (0x20A0, 0x20C0), (0x2100, 0x214F), (0x2190, 0x21FF), (0x2212, 0x2215),
    (0x2C60, 0x2C7F), (0xA720, 0xA7FF), (0xFEFF, 0xFEFF), (0xFFFD, 0xFFFD),
]
GREEK = [(0x0370, 0x03FF), (0x1F00, 0x1FFF)]
CYRILLIC = [(0x0400, 0x052F), (0x1C80, 0x1C8F), (0x2DE0, 0x2DFF), (0xA640, 0xA69F)]
SCRIPTS = {"latin": LATIN, "greek": GREEK, "cyrillic": CYRILLIC}

# What the UI uses on top of fontTools' defaults (kerning, ligatures, marks, fractions, ...).
EXTRA_FEATURES = ["tnum", "pnum", "lnum", "onum", "case", "zero", "ss01", "cv11"]

# Subset fonts: id -> (upstream file, scripts kept). The ids are the setting's wire values
# (teamcraft-client-api `font.rs`).
SUBSET_FONTS = {
    "inter": ("Inter[opsz,wght].ttf", ["latin", "greek", "cyrillic"]),
    "google-sans-flex": ("GoogleSansFlex[ROND,opsz,slnt,wdth,wght].ttf", ["latin"]),
    "atkinson-hyperlegible-next": ("AtkinsonHyperlegibleNext[wght].ttf", ["latin"]),
    "lexend": ("Lexend[wght].ttf", ["latin"]),
    "noto-sans": ("NotoSans[wdth,wght].ttf", ["latin", "greek", "cyrillic"]),
    "jetbrains-mono": ("JetBrainsMono[wght].ttf", ["latin", "greek", "cyrillic"]),
}
# Unmodified files: output name -> upstream file.
WEB_COPIES = {
    "ibm-plex-sans-latin1.woff2": "IBM Plex Sans Var-Roman-Latin1.woff2",
    "ibm-plex-sans-latin2.woff2": "IBM Plex Sans Var-Roman-Latin2.woff2",
    "ibm-plex-sans-latin3.woff2": "IBM Plex Sans Var-Roman-Latin3.woff2",
    "ibm-plex-sans-pi.woff2": "IBM Plex Sans Var-Roman-Pi.woff2",
    "ibm-plex-sans-greek.woff2": "IBM Plex Sans Var-Roman-Greek.woff2",
    "ibm-plex-sans-cyrillic.woff2": "IBM Plex Sans Var-Roman-Cyrillic.woff2",
    "source-sans-3.woff2": "SourceSans3VF-Upright.ttf.woff2",
    "opendyslexic.woff2": "OpenDyslexic-Regular.woff2",
    "opendyslexic-bold.woff2": "OpenDyslexic-Bold.woff2",
}
ANDROID_COPIES = {
    "font_ibm_plex_sans.ttf": "IBM Plex Sans Var-Roman.ttf",
    "font_source_sans_3.ttf": "SourceSans3VF-Upright.ttf",
    "font_opendyslexic.otf": "OpenDyslexic-Regular.otf",
    "font_opendyslexic_bold.otf": "OpenDyslexic-Bold.otf",
}
# Licence texts: output name -> upstream file.
LICENSES = {
    "OFL-inter.txt": "OFL-inter.txt",
    "OFL-google-sans-flex.txt": "OFL-google-sans-flex.txt",
    "OFL-ibm-plex-sans.txt": "OFL-ibm-plex-sans.txt",
    "OFL-source-sans-3.txt": "OFL-source-sans-3.txt",
    "OFL-atkinson-hyperlegible-next.txt": "OFL-atkinson-hyperlegible-next.txt",
    "OFL-lexend.txt": "OFL-lexend.txt",
    "OFL-opendyslexic.txt": "OFL-opendyslexic.txt",
    "OFL-noto-sans.txt": "OFL-noto-sans.txt",
    "OFL-jetbrains-mono.txt": "OFL-jetbrains-mono.txt",
}
UPSTREAM_NAMES = ([name for name, _ in SUBSET_FONTS.values()] + list(WEB_COPIES.values())
                  + list(ANDROID_COPIES.values()) + list(LICENSES.values()))


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("upstream", type=pathlib.Path, help="directory with the upstream files")
    args = parser.parse_args(argv)
    missing = [name for name in UPSTREAM_NAMES if not (args.upstream / name).is_file()]
    if missing:
        print("missing upstream files: " + ", ".join(missing), file=sys.stderr)
        return 2
    (WEB_DIR / "licenses").mkdir(parents=True, exist_ok=True)
    ANDROID_DIR.mkdir(parents=True, exist_ok=True)
    for font_id, (name, scripts) in SUBSET_FONTS.items():
        font = pin_axes(TTFont(args.upstream / name))
        add_tabular_figures(font)
        write_subset(font, font_id, scripts)
    for target, source in WEB_COPIES.items():
        shutil.copyfile(args.upstream / source, WEB_DIR / target)
    for target, source in ANDROID_COPIES.items():
        shutil.copyfile(args.upstream / source, ANDROID_DIR / target)
    for target, source in LICENSES.items():
        shutil.copyfile(args.upstream / source, WEB_DIR / "licenses" / target)
    report()
    return 0


def pin_axes(font):
    """Keeps the weight axis (limited to the UI's weights) and pins every other axis to its default."""
    limits = {}
    for axis in font["fvar"].axes:
        if axis.axisTag == "wght":
            default = axis.defaultValue if MIN_WEIGHT <= axis.defaultValue <= MAX_WEIGHT else MIN_WEIGHT
            limits["wght"] = (max(axis.minValue, MIN_WEIGHT), default, min(axis.maxValue, MAX_WEIGHT))
        else:
            limits[axis.axisTag] = axis.defaultValue
    pinned = instancer.instantiateVariableFont(font, limits, updateFontNames=False)
    # Reloaded, so the subsetter sees plain tables rather than the instancer's lazy ones.
    buffer = io.BytesIO()
    pinned.save(buffer)
    buffer.seek(0)
    return TTFont(buffer)


def add_tabular_figures(font):
    """Adds a `tnum` feature to a font whose digits differ in width and that has none."""
    cmap = font.getBestCmap()
    digits = [cmap[codepoint] for codepoint in range(0x30, 0x3A)]
    gsub = font["GSUB"].table
    if any(record.FeatureTag == "tnum" for record in gsub.FeatureList.FeatureRecord):
        return
    widths = {font["hmtx"][glyph][0] for glyph in digits}
    if len(widths) == 1:
        return
    # The widest digit at the boldest weight, so bold digits fit too.
    bold = instancer.instantiateVariableFont(TTFont(reloaded(font)), {"wght": MAX_WEIGHT})
    width = max(max(widths), *(bold["hmtx"][glyph][0] for glyph in digits))
    # Every table read before the glyph set changes (gvar checks the glyph count when it loads).
    font.ensureDecompiled()
    glyf, hmtx = font["glyf"], font["hmtx"]
    mapping = {}
    for glyph in digits:
        name = f"{glyph}.tnum"
        component = GlyphComponent()
        component.glyphName = glyph
        component.x = round((width - hmtx[glyph][0]) / 2)
        component.y = 0
        component.flags = 0x0004  # ROUND_XY_TO_GRID; fontTools sets the argument flags.
        tabular = Glyph()
        tabular.numberOfContours = -1
        tabular.components = [component]
        glyf[name] = tabular
        tabular.recalcBounds(glyf)
        hmtx[name] = (width, tabular.xMin)
        # No variations of its own: the component varies, the advance stays fixed.
        font["gvar"].variations[name] = []
        mapping[glyph] = name
    # The glyph table may already have listed them; one order for every table.
    order = list(dict.fromkeys(font.getGlyphOrder() + list(mapping.values())))
    font.setGlyphOrder(order)
    glyf.glyphOrder = order
    # The advances come from the glyphs' phantom points, which keep the new ones fixed.
    if "HVAR" in font:
        del font["HVAR"]
    if "GDEF" in font and font["GDEF"].table.GlyphClassDef:
        for name in mapping.values():
            font["GDEF"].table.GlyphClassDef.classDefs[name] = 1
    lookup = otTables.Lookup()
    lookup.LookupType = 1
    lookup.LookupFlag = 0
    substitution = otTables.SingleSubst()
    substitution.mapping = mapping
    lookup.SubTable = [substitution]
    lookup.SubTableCount = 1
    gsub.LookupList.Lookup.append(lookup)
    gsub.LookupList.LookupCount = len(gsub.LookupList.Lookup)
    feature = otTables.FeatureRecord()
    feature.FeatureTag = "tnum"
    feature.Feature = otTables.Feature()
    feature.Feature.FeatureParams = None
    feature.Feature.LookupListIndex = [len(gsub.LookupList.Lookup) - 1]
    feature.Feature.LookupCount = 1
    gsub.FeatureList.FeatureRecord.append(feature)
    gsub.FeatureList.FeatureCount = len(gsub.FeatureList.FeatureRecord)
    feature_index = gsub.FeatureList.FeatureCount - 1
    for script in gsub.ScriptList.ScriptRecord:
        systems = [script.Script.DefaultLangSys] + [record.LangSys for record in script.Script.LangSysRecord]
        for system in filter(None, systems):
            system.FeatureIndex.append(feature_index)
            system.FeatureCount = len(system.FeatureIndex)


def reloaded(font):
    buffer = io.BytesIO()
    font.save(buffer)
    buffer.seek(0)
    return buffer


def write_subset(font, font_id, scripts):
    """Subsets `font` to `scripts` and writes the web and Android files."""
    unicodes = [c for c in font.getBestCmap() if any(in_ranges(c, SCRIPTS[name]) for name in scripts)]
    options = subset.Options()
    options.layout_features = subset.Options().layout_features + EXTRA_FEATURES
    options.hinting = False
    options.name_IDs = ["*"]
    options.name_languages = ["*"]
    options.notdef_outline = True
    options.recalc_timestamp = False
    subsetter = subset.Subsetter(options)
    subsetter.populate(unicodes=unicodes)
    subsetter.subset(font)
    font.flavor = None
    # Android resource names may only hold [a-z0-9_].
    font.save(ANDROID_DIR / ("font_" + font_id.replace("-", "_") + ".ttf"))
    font.flavor = "woff2"
    font.save(WEB_DIR / f"{font_id}.woff2")


def in_ranges(codepoint, ranges):
    return any(start <= codepoint <= end for start, end in ranges)


def report():
    for directory in (WEB_DIR, ANDROID_DIR):
        files = sorted(path for path in directory.iterdir() if path.is_file() and path.name.startswith(("font_",))
                       or path.suffix == ".woff2")
        print(f"{directory.relative_to(ROOT)}:")
        for path in files:
            print(f"  {path.name:40} {path.stat().st_size:>9,}")
        print(f"  {'total':40} {sum(path.stat().st_size for path in files):>9,}")


if __name__ == "__main__":
    sys.exit(main())
