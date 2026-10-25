// Takes the README screenshots (issue #479) from the web client's demo build in headless Chrome:
// the channel view with a thread and the phone layout in light and dark, and a call.
//
//   node scripts/screenshots/readme.mjs <site URL> <output dir> [name ...]
//
// <site URL> serves scripts/screenshots/web-demo-build.sh's output (see readme.sh, which does
// everything). Writes <name>-<light|dark>.png; names limit the run to those pictures.

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { delay, launch } from "./cdp.mjs";

const DESKTOP = { width: 1440, height: 900 };
const PHONE = { width: 390, height: 844, scale: 2, isMobile: true };
/** Lets entrance transitions, avatars and fonts settle (not a check, just the picture). */
const SETTLE_MS = 1500;

const SHOTS = {
  channel: { viewport: DESKTOP, steps: channelWithThread },
  // Calls are dark in either scheme: one picture.
  call: { viewport: DESKTOP, steps: call, schemes: ["dark"] },
  phone: { viewport: PHONE, steps: phoneChannel },
};

const [site, out, ...only] = process.argv.slice(2);
if (!site || !out) {
  console.error("usage: readme.mjs <site URL> <output dir> [name ...]");
  process.exit(2);
}
mkdirSync(out, { recursive: true });

for (const [name, shot] of Object.entries(SHOTS)) {
  if (only.length > 0 && !only.includes(name)) continue;
  for (const scheme of shot.schemes ?? ["light", "dark"]) {
    const page = await launch();
    try {
      await page.send("Emulation.setLocaleOverride", { locale: "en-US" });
      await page.emulate({ ...shot.viewport, scheme });
      await page.goto(`${site}/?backend=mock`);
      await page.waitFor(`document.querySelector('[aria-label^="Project A"]')`, 120_000);
      await shot.steps(page);
      await delay(SETTLE_MS);
      const file = join(out, `${name}-${scheme}.png`);
      writeFileSync(file, await page.screenshot());
      console.log(file);
    } finally {
      await page.close();
    }
  }
}
process.exit(0);

/** The #design channel of Project A with a thread open beside it. */
async function channelWithThread(page) {
  await page.click("button", "Project A");
  await page.click("button", "design");
  await delay(SETTLE_MS);
  await page.click("button", "3 replies");
}

/** A video call in #design after the pre-join screen. */
async function call(page) {
  await page.click("button", "Project A");
  await page.click("button", "design");
  await page.click("button", "Start video call");
  await page.click("button", "Join");
}

/** The phone layout: the channel list of Project A, then #design. */
async function phoneChannel(page) {
  await page.click("button", "Project A");
  await page.click("button", "design");
}
