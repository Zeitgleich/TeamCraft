#!/usr/bin/env python3
"""Downloads the interface fonts' upstream files (issue #1494) at pinned versions and checks them
against their SHA-256, into one directory for `scripts/build-fonts.py`:

        scripts/fetch-fonts.py <empty-dir> && scripts/build-fonts.py <dir>

The sources, versions and licences are listed in `crates/teamcraft-ui/fonts/NOTICE.md`.
"""
import argparse
import hashlib
import io
import pathlib
import sys
import urllib.parse
import urllib.request
import zipfile

GOOGLE_FONTS = "https://raw.githubusercontent.com/google/fonts/5bb28733cec313b63fbd415b574b5ae35c8aa8b8/ofl/"
OPENDYSLEXIC = "https://raw.githubusercontent.com/antijingoist/opendyslexic/1824da5c0e41dc3e13ffc7f3a636dcaf695d61b7/"
PLEX_ZIP = ("https://github.com/IBM/plex/releases/download/%40ibm%2Fplex-sans-variable%400.2.0/"
            "plex-sans-variable.zip")
SOURCE_SANS = "https://github.com/adobe-fonts/source-sans/releases/download/3.052R/"

# Saved name -> URL.
FILES = {
    "Inter[opsz,wght].ttf": GOOGLE_FONTS + "inter/Inter[opsz,wght].ttf",
    "OFL-inter.txt": GOOGLE_FONTS + "inter/OFL.txt",
    "GoogleSansFlex[ROND,opsz,slnt,wdth,wght].ttf":
        GOOGLE_FONTS + "googlesansflex/GoogleSansFlex[ROND,opsz,slnt,wdth,wght].ttf",
    "OFL-google-sans-flex.txt": GOOGLE_FONTS + "googlesansflex/OFL.txt",
    "AtkinsonHyperlegibleNext[wght].ttf": GOOGLE_FONTS + "atkinsonhyperlegiblenext/AtkinsonHyperlegibleNext[wght].ttf",
    "OFL-atkinson-hyperlegible-next.txt": GOOGLE_FONTS + "atkinsonhyperlegiblenext/OFL.txt",
    "Lexend[wght].ttf": GOOGLE_FONTS + "lexend/Lexend[wght].ttf",
    "OFL-lexend.txt": GOOGLE_FONTS + "lexend/OFL.txt",
    "NotoSans[wdth,wght].ttf": GOOGLE_FONTS + "notosans/NotoSans[wdth,wght].ttf",
    "OFL-noto-sans.txt": GOOGLE_FONTS + "notosans/OFL.txt",
    "JetBrainsMono[wght].ttf": GOOGLE_FONTS + "jetbrainsmono/JetBrainsMono[wght].ttf",
    "OFL-jetbrains-mono.txt": GOOGLE_FONTS + "jetbrainsmono/OFL.txt",
    "OpenDyslexic-Regular.woff2": OPENDYSLEXIC + "compiled/OpenDyslexic-Regular.woff2",
    "OpenDyslexic-Bold.woff2": OPENDYSLEXIC + "compiled/OpenDyslexic-Bold.woff2",
    "OpenDyslexic-Regular.otf": OPENDYSLEXIC + "compiled/OpenDyslexic-Regular.otf",
    "OpenDyslexic-Bold.otf": OPENDYSLEXIC + "compiled/OpenDyslexic-Bold.otf",
    "OFL-opendyslexic.txt": OPENDYSLEXIC + "OFL.txt",
    "OFL-source-sans-3.txt": "https://raw.githubusercontent.com/adobe-fonts/source-sans/3.052R/LICENSE.md",
}
# Saved name -> (archive URL, member).
ARCHIVE_MEMBERS = {
    "IBM Plex Sans Var-Roman-Latin1.woff2": (PLEX_ZIP, "fonts/split/woff2/IBM Plex Sans Var-Roman-Latin1.woff2"),
    "IBM Plex Sans Var-Roman-Latin2.woff2": (PLEX_ZIP, "fonts/split/woff2/IBM Plex Sans Var-Roman-Latin2.woff2"),
    "IBM Plex Sans Var-Roman-Latin3.woff2": (PLEX_ZIP, "fonts/split/woff2/IBM Plex Sans Var-Roman-Latin3.woff2"),
    "IBM Plex Sans Var-Roman-Pi.woff2": (PLEX_ZIP, "fonts/split/woff2/IBM Plex Sans Var-Roman-Pi.woff2"),
    "IBM Plex Sans Var-Roman-Greek.woff2": (PLEX_ZIP, "fonts/split/woff2/IBM Plex Sans Var-Roman-Greek.woff2"),
    "IBM Plex Sans Var-Roman-Cyrillic.woff2": (PLEX_ZIP, "fonts/split/woff2/IBM Plex Sans Var-Roman-Cyrillic.woff2"),
    "IBM Plex Sans Var-Roman.ttf": (PLEX_ZIP, "fonts/complete/ttf/IBM Plex Sans Var-Roman.ttf"),
    "OFL-ibm-plex-sans.txt": (PLEX_ZIP, "fonts/complete/ttf/license.txt"),
    "SourceSans3VF-Upright.ttf.woff2":
        (SOURCE_SANS + "WOFF2-source-sans-3.052R.zip", "WOFF2/VF/SourceSans3VF-Upright.ttf.woff2"),
    "SourceSans3VF-Upright.ttf": (SOURCE_SANS + "VF-source-sans-3.052R.zip", "VF/SourceSans3VF-Upright.ttf"),
}
# SHA-256 of every saved file (scripts/fetch-fonts.sha256).
CHECKSUMS = pathlib.Path(__file__).with_name("fetch-fonts.sha256")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("target", type=pathlib.Path)
    parser.add_argument("--write-checksums", action="store_true",
                        help="record the downloaded files' hashes instead of checking them")
    args = parser.parse_args(argv)
    args.target.mkdir(parents=True, exist_ok=True)
    for name, url in FILES.items():
        (args.target / name).write_bytes(download(url))
    archives = {}
    for name, (url, member) in ARCHIVE_MEMBERS.items():
        if url not in archives:
            archives[url] = zipfile.ZipFile(io.BytesIO(download(url)))
        (args.target / name).write_bytes(archives[url].read(member))
    hashes = {name: hashlib.sha256((args.target / name).read_bytes()).hexdigest()
              for name in sorted(list(FILES) + list(ARCHIVE_MEMBERS))}
    if args.write_checksums:
        CHECKSUMS.write_text("".join(f"{digest}  {name}\n" for name, digest in hashes.items()))
        return 0
    expected = dict(reversed(line.split("  ", 1)) for line in CHECKSUMS.read_text().splitlines())
    wrong = [name for name, digest in hashes.items() if expected.get(name) != digest]
    if wrong:
        print("unexpected content: " + ", ".join(wrong), file=sys.stderr)
        return 1
    print(f"fetched {len(hashes)} files into {args.target}")
    return 0


def download(url):
    quoted = urllib.parse.quote(url, safe=":/%@")
    with urllib.request.urlopen(quoted) as response:
        return response.read()


if __name__ == "__main__":
    sys.exit(main())
