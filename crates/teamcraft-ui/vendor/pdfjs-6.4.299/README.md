# PDF.js (vendored)

Unmodified files of the `pdfjs-dist` npm package, version **6.4.299** (Apache-2.0, see `LICENSE`), used by the
in-app PDF preview (`../../pdf-viewer.mjs`, docs/e2ee.md §4.5.0).

| File                    | sha256                                                           |
|-------------------------|------------------------------------------------------------------|
| `pdf.min.mjs`           | 57456c8e0c81e46be31174b499ef77f2b9f5ee46d04412ba627320a36755d4c2 |
| `pdf.worker.min.mjs`    | 9536359f1b8367850d485731ca1d5e45c159a7b7a0912325e539937aa21ceb18 |

`wasm/` holds the decoders (JBIG2, JPEG 2000, colour management) with their licenses. Not shipped on purpose:
the scripting sandbox (`pdf.sandbox`, QuickJS), the no-WebAssembly fallbacks (scripts), CMaps and fonts.

Upgrade: `npm pack pdfjs-dist@<version>`, copy the same files into a new `vendor/pdfjs-<version>` directory,
update the hashes here, `index.html` and `PDF_VIEWER_MODULE` in `src/components/file_preview.rs` (the version in
the path is the cache key).
