# Google Play listing

The store listing of the Android app (`de.zeitgleich.teamcraft`) in the layout of fastlane
`supply`'s metadata directory, so it can be uploaded with
`fastlane supply --metadata_path apps/android/store` (or by hand in the Play Console).

| Path                                      | What                                               | Made by                                   |
|-------------------------------------------|----------------------------------------------------|-------------------------------------------|
| `<locale>/title.txt`                      | App name (≤ 30 characters)                         | by hand                                   |
| `<locale>/short_description.txt`          | Short description (≤ 80 characters)                | by hand                                   |
| `<locale>/full_description.txt`           | Full description (≤ 4000 characters)               | by hand                                   |
| `<locale>/images/phoneScreenshots/*.png`  | Phone screenshots, 1080 × 2400, no alpha           | `scripts/screenshots/android-store.sh`    |
| `<locale>/images/featureGraphic.png`      | Feature graphic, 1024 × 500, no alpha (SVG beside) | `scripts/screenshots/feature_graphic.py`  |
| `<locale>/images/icon.png`                | Hi-res icon, 512 × 512                             | `scripts/brand/icons.py`                  |

Locales: `en-US` (default) and `de-DE`.

## Screenshots

`StoreScreenshots` (`app/src/androidTest/…/ui/screens/StoreScreenshots.kt`) renders the app's real
screens with placeholder people and texts; it is a test, so the sample data never reaches a shipped
build. `scripts/screenshots/android-store.sh` builds the app, boots its own headless Pixel 6
emulator in the Android build image (AVD and system image in `TEAMCRAFT_ANDROID_CACHE`), puts the
status bar into SystemUI's demo mode (9:41, full battery and Wi-Fi), takes the pictures in English
and German and writes them here; it also renders the feature graphic and the icon:

```sh
TEAMCRAFT_ANDROID_CACHE=/big/disk/android scripts/screenshots/android-store.sh
```

All pictures are dark except the settings, which show the light theme.
The call screen is being redesigned (#497); its picture is left out until then. `TEAMCRAFT_SCREENSHOT_CALL=true`
takes it too (the test's `-e call true`).
