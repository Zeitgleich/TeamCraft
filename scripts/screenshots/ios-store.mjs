// Takes the App Store screenshots in headless Chrome: every scene of
// scripts/screenshots/ios-store/scenes.js, per device and language, at the size App Store Connect
// takes for that device (iPhone 6.9": 1320 x 2868, iPad 13": 2064 x 2752), and the product
// page's header (5244 x 2950) and search result picture (3840 x 2560).
//
//   node scripts/screenshots/ios-store.mjs <output dir> [scene ...]
//
// Writes <output dir>/<language>/<device>/<n>-<scene>.png; scenes limit the run. ios-store.sh
// runs it and converts the pictures to the JPEGs that are uploaded.

import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { launch } from "./cdp.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..", "..");
/** The device's screen in points and its scale: the canvas is captured at the store's pixels. */
const DEVICES = {
  iphone: { folder: "iphone-6.9", width: 440, height: 956, scale: 3 },
  ipad: { folder: "ipad-13", width: 1032, height: 1376, scale: 2 },
  // Google Play phone screenshots (9:20).
  android: { folder: "play-phone", width: 432, height: 960, scale: 2.5 },
  // The product page's creative assets (5244 x 2950 and 3840 x 2560).
  header: { folder: "header", width: 2622, height: 1475, scale: 2 },
  "search-result": { folder: "search", width: 1920, height: 1280, scale: 2 },
};
const LANGUAGES = ["en", "de"];

const [out, ...only] = process.argv.slice(2);
if (!out) {
  console.error("usage: ios-store.mjs <output dir> [scene ...]");
  process.exit(2);
}

const icons = iconPaths(readFileSync(join(ROOT, "crates/teamcraft-ui/src/components/icons.rs"), "utf8"));
const brand = {
  mark: readFileSync(join(ROOT, "assets/brand/mark.svg"), "utf8"),
  wordmark: readFileSync(join(ROOT, "assets/brand/wordmark-dark.svg"), "utf8"),
};
const page = await launch();
try {
  await page.goto(pathToFileURL(join(HERE, "ios-store", "page.html")).href);
  await page.waitFor("window.renderScene");
  const scenes = await page.evaluate("window.SCENES");
  for (const [device, size] of Object.entries(DEVICES)) {
    await page.emulate({ width: size.width, height: size.height, scale: size.scale });
    for (const lang of LANGUAGES) {
      const folder = join(out, lang, size.folder);
      mkdirSync(folder, { recursive: true });
      for (const [index, scene] of scenes[device].entries()) {
        if (only.length > 0 && !only.includes(scene)) continue;
        await page.evaluate(`window.renderScene(${JSON.stringify({ device, lang, scene })}, ${JSON.stringify(icons)}, ${JSON.stringify(brand)})`);
        const file = join(folder, `${index + 1}-${scene}.png`);
        writeFileSync(file, await page.screenshot());
        console.log(file);
      }
    }
  }
} finally {
  await page.close();
}
process.exit(0);

/** The app's line icons by name, read from the `IconName::path` match in icons.rs. */
function iconPaths(source) {
  const paths = {};
  const entry = /Self::(\w+) => \{?\s*"((?:[^"\\]|\\\s*)*)"/g;
  for (const [, name, path] of source.matchAll(entry)) {
    paths[name] = path.replace(/\\\s*/g, " ");
  }
  return paths;
}
