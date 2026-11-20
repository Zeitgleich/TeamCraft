# App Store screenshots

The App Store listing's screenshots of the iOS app (`de.zeitgleich.teamcraft`), the iOS sibling of
the Google Play listing in [apps/android/store](../../android/store/README.md).

| Path                                          | What                                       | Made by                             |
|-----------------------------------------------|--------------------------------------------|-------------------------------------|
| `<locale>/screenshots/iphone-6.9/*.png`       | iPhone 6.9", 1320 × 2868, RGB (6 pictures) | `scripts/screenshots/ios-store.sh`  |
| `<locale>/screenshots/ipad-13/*.png`          | iPad 13", 2064 × 2752, RGB (5 pictures)    | `scripts/screenshots/ios-store.sh`  |

Locales: `en-US` and `de-DE`. App Store Connect scales the 6.9" and 13" pictures down for the
smaller devices, so these two sizes are all a submission needs.

The pictures show the Play screenshots' scenes and placeholder people and texts (chats, a
channel, a thread, the spaces, joining a call, the settings) in the app's light theme, each with a
caption above the device. They are drawn as HTML in headless Chrome
(`scripts/screenshots/ios-store/scenes.js`), not taken from a simulator: the screenshots are made
on Linux, where there is no iOS simulator. The scenes follow the iOS app's layout and theme
(`apps/ios/TeamCraft/Design/Theme.swift`) and use the app's own line icons; when a screen changes
a lot, update its scene. On iPad the spaces show in the split view's sidebar, so it has no
picture of its own.

```sh
scripts/screenshots/ios-store.sh
```

Upload by hand (App Store Connect → the app → the version → drag the folder's pictures onto the
iPhone and iPad boxes per language), or with the API key of the TestFlight upload:

```sh
ASC_KEY_ID=… ASC_ISSUER_ID=… ASC_KEY_PATH=AuthKey_….p8 apps/ios/scripts/upload-screenshots.py
```

It replaces the 6.9" and 13" screenshots of every language the version in preparation has;
`--dry-run` only checks the pictures.
