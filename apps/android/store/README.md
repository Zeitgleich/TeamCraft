# Google Play listing

The store listing of the Android app (`de.zeitgleich.teamcraft`) in the layout of fastlane
`supply`'s metadata directory, so it can be uploaded with
`fastlane supply --metadata_path apps/android/store` (or by hand in the Play Console).

| Path                                      | What                                               | Made by                                   |
|-------------------------------------------|----------------------------------------------------|-------------------------------------------|
| `<locale>/title.txt`                      | App name (≤ 30 characters)                         | by hand                                   |
| `<locale>/short_description.txt`          | Short description (≤ 80 characters)                | by hand                                   |
| `<locale>/full_description.txt`           | Full description (≤ 4000 characters)               | by hand                                   |
| `<locale>/images/featureGraphic.png`      | Feature graphic, 1024 × 500, no alpha (SVG beside) | `scripts/screenshots/feature_graphic.py`  |
| `<locale>/images/icon.png`                | Hi-res icon, 512 × 512                             | `scripts/brand/icons.py`                  |

Locales: `en-US` (default) and `de-DE`.

## Screenshots

The phone screenshots are not kept in the repository. The listing's pictures are the App Store
screenshots' title card and scenes on a Pixel (1080 × 2400), made with
`scripts/screenshots/ios-store.sh <dir>` into `<dir>/<locale>/play-phone/` and uploaded by hand
in the Play Console (docs/release.md, "App Store screenshots").

Pictures of the real Android screens: `StoreScreenshots`
(`app/src/androidTest/…/ui/screens/StoreScreenshots.kt`) renders them with placeholder people and
texts; it is a test, so the sample data never reaches a shipped build.
`scripts/screenshots/android-store.sh` builds the app, boots its own headless Pixel 6 emulator in
the Android build image (AVD and system image in `TEAMCRAFT_ANDROID_CACHE`), puts the status bar
into SystemUI's demo mode (9:41, full battery and Wi-Fi), takes the pictures in English and German
and writes them to the given directory; it also renders the feature graphic and the icon here:

```sh
TEAMCRAFT_ANDROID_CACHE=/big/disk/android scripts/screenshots/android-store.sh <dir>
```
