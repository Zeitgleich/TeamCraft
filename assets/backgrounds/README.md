# Photo backgrounds

Built-in pictures for call background replacement. Only public domain and CC0 material is used
(see [SOURCES.md](SOURCES.md) for title, author, source and licence of each picture; the licence
was checked on the source page). No people, faces, logos or readable text.

| File                | Content                                                           |
|---------------------|-------------------------------------------------------------------|
| `<slug>.jpg`        | 1920x1080, JPEG of at most 400 KiB, compiled into the desktop app |
| `thumbs/<slug>.jpg` | 320x180, served by the web UI for the picker and loaded lazily    |
| `manifest.json`     | Source of truth: origin, licence, SHA-256, crop (`focus`, `zoom`) |

Rebuild everything from the originals (they are downloaded into `.background-originals/`, which
is git-ignored; keep it off `/tmp`):

```sh
scripts/process-backgrounds.py          # download, verify checksum, crop, scale, write JPEGs + SOURCES.md
scripts/process-backgrounds.py --check  # fail if the committed files differ (needs Pillow, same version)
```

## Adding a picture

1. Pick a file that is CC0 or public domain on its Wikimedia Commons or Flickr page (not Unsplash,
   Pexels or Pixabay licences) and shows nothing identifiable: no people, brands or readable text.
2. Add an entry to `manifest.json` (`sha256` of the download, optional `focus` and `zoom`).
3. Run the script, look at the result, and add the preset to `BackgroundPreset`
   (`teamcraft-client-api`), `PhotoBackground` (`teamcraft-media`) and `i18n/*.ftl`.
