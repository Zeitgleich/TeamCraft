# Interface fonts

The fonts of the font setting (issue #1494). They load only when someone picks one (`src/fonts.rs`):
the web client fetches the file from its own origin and the service worker keeps it; the desktop app
embeds the web build; Android bundles the same set in `apps/android/app/src/main/res/font/`, and
the iOS app bundles those files too (`apps/ios/project.yml`, `UIAppFonts`).

Every font is under the SIL Open Font License 1.1; the licence texts are in `licenses/`, and each file
keeps its name table with the copyright and licence. Fonts with a Reserved Font Name are shipped
unmodified, as their upstream project publishes them; the others are subset by
`scripts/build-fonts.py` (Latin and Latin Extended, plus Greek and Cyrillic where noted; weights 400
to 700; other axes pinned to their default; no hinting). `scripts/fetch-fonts.py` downloads the
inputs at the versions below and checks them against `scripts/fetch-fonts.sha256`.

| Font                       | Version | Source                                                                                                   | Copyright                                      | Reserved Font Name | Shipped as                                |
|----------------------------|---------|----------------------------------------------------------------------------------------------------------|------------------------------------------------|--------------------|-------------------------------------------|
| Inter                      | 4.001   | github.com/google/fonts `ofl/inter` (upstream github.com/rsms/inter)                                     | The Inter Project Authors                      | –                  | subset: Latin, Greek, Cyrillic            |
| Google Sans Flex           | 5.000   | github.com/google/fonts `ofl/googlesansflex`                                                             | The Google Sans Flex Authors                   | –                  | subset: Latin                             |
| IBM Plex Sans              | 3.000   | github.com/IBM/plex release `@ibm/plex-sans-variable@0.2.0`                                              | IBM Corp.                                      | Plex               | unmodified (IBM's split WOFF2, by script) |
| Source Sans 3              | 3.052   | github.com/adobe-fonts/source-sans release `3.052R`                                                      | Adobe                                          | Source             | unmodified (Adobe's variable WOFF2)       |
| Atkinson Hyperlegible Next | 2.001   | github.com/google/fonts `ofl/atkinsonhyperlegiblenext` (upstream googlefonts/atkinson-hyperlegible-next) | The Atkinson Hyperlegible Next Project Authors | –                  | subset: Latin                             |
| Lexend                     | 1.007   | github.com/google/fonts `ofl/lexend` (upstream googlefonts/lexend)                                       | The Lexend Project Authors                     | RevReading Lexend  | subset: Latin                             |
| OpenDyslexic               | 0.990   | github.com/antijingoist/opendyslexic `compiled/` at `1824da5c`                                           | Abbie Gonzalez                                 | OpenDyslexic       | unmodified (Regular and Bold)             |
| Noto Sans                  | 2.015   | github.com/google/fonts `ofl/notosans` (upstream notofonts/latin-greek-cyrillic)                         | The Noto Project Authors                       | –                  | subset: Latin, Greek, Cyrillic            |
| JetBrains Mono             | 2.211   | github.com/google/fonts `ofl/jetbrainsmono` (upstream JetBrains/JetBrainsMono)                           | The JetBrains Mono Project Authors             | –                  | subset: Latin, Greek, Cyrillic            |

The google/fonts files are taken at commit `5bb28733cec313b63fbd415b574b5ae35c8aa8b8`. Lexend's
Reserved Font Name is "RevReading Lexend", which the subset doesn't use. Lexend has no tabular
figures upstream; the build adds a `tnum` feature (each digit centred on the widest digit's
advance), so counts and times keep their width.

Arial, Georgia and Verdana are offered too, but never shipped: the setting names them, and only a
device that has them installed shows them; others fall back to the system font.
