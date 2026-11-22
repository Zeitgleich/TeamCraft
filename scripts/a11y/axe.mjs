// axe-core over the web client's demo build (issue #2287): a fixed list of views in light and dark,
// WCAG 2.0 to 2.2 A and AA rules. Fails on any violation that is not in axe-known.json, so new
// regressions stop the build while the findings still open stay listed (and are listed in the
// issue), never switched off. A known entry that no longer fires is reported: remove it.
//
//   node scripts/a11y/axe.mjs <site URL> <axe.min.js> [--report <file>] [--write-known] [view ...]
//
// <site URL> serves scripts/screenshots/web-demo-build.sh's output; scripts/a11y/fetch-axe.sh
// downloads the pinned axe-core. Node 22+, Chrome (CHROME=/path/to/chrome); no npm packages.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { delay, launch } from "../screenshots/cdp.mjs";

const KNOWN_FILE = join(dirname(fileURLToPath(import.meta.url)), "axe-known.json");
const TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const DESKTOP = { width: 1440, height: 900 };
const PHONE = { width: 390, height: 844, scale: 2, isMobile: true };
/** Lets entrance transitions, avatars and fonts settle before axe reads the page. */
const SETTLE_MS = 800;

// Each view: the viewport and the steps from the demo's start screen. Calls are dark in either scheme.
const VIEWS = {
  home: { viewport: DESKTOP, steps: async () => {} },
  channel: { viewport: DESKTOP, steps: openChannel },
  thread: { viewport: DESKTOP, steps: openThread },
  switcher: { viewport: DESKTOP, steps: openSwitcher },
  settings: { viewport: DESKTOP, steps: openSettings },
  call: { viewport: DESKTOP, steps: openCall, schemes: ["dark"] },
  phone: { viewport: PHONE, steps: openChannel },
};

const args = process.argv.slice(2);
const writeKnown = take(args, "--write-known");
const report = takeValue(args, "--report");
const [site, axePath, ...only] = args;
if (!site || !axePath) {
  console.error("usage: axe.mjs <site URL> <axe.min.js> [--report <file>] [--write-known] [view ...]");
  process.exit(2);
}
const axeSource = readFileSync(axePath, "utf8");
const known = new Set(JSON.parse(readFileSync(KNOWN_FILE, "utf8")).map((entry) => entry.key));

const found = new Map();
for (const [name, view] of Object.entries(VIEWS)) {
  if (only.length > 0 && !only.includes(name)) continue;
  for (const scheme of view.schemes ?? ["light", "dark"]) {
    const page = await launch();
    try {
      await page.send("Emulation.setLocaleOverride", { locale: "en-US" });
      await page.emulate({ ...view.viewport, scheme });
      await page.goto(`${site}/?backend=mock`);
      await page.waitFor(`document.querySelector('[aria-label^="Project A"]')`, 120_000);
      await view.steps(page);
      await delay(SETTLE_MS);
      await page.evaluate(axeSource);
      const violations = await page.evaluate(
        `axe.run(document, { runOnly: { type: "tag", values: ${JSON.stringify(TAGS)} }, resultTypes: ["violations"] })`
          + `.then((result) => result.violations.map((v) => ({ id: v.id, impact: v.impact, help: v.help,`
          + ` targets: v.nodes.map((n) => n.target.join(" ")).slice(0, 5), count: v.nodes.length })))`,
      );
      for (const violation of violations) {
        found.set(`${name}/${scheme}/${violation.id}`, { view: name, scheme, ...violation });
      }
      console.log(`${name} (${scheme}): ${violations.length} rule(s) failing`);
    } finally {
      await page.close();
    }
  }
}

const fresh = [...found].filter(([key]) => !known.has(key));
const gone = only.length > 0 ? [] : [...known].filter((key) => !found.has(key));
if (report) writeFileSync(report, JSON.stringify([...found.values()], null, 2));
if (writeKnown) {
  const entries = [...found].map(([key, v]) => ({ key, impact: v.impact, help: v.help, targets: v.targets }));
  writeFileSync(KNOWN_FILE, `${JSON.stringify(entries, null, 2)}\n`);
  console.log(`wrote ${entries.length} known findings to ${KNOWN_FILE}`);
  process.exit(0);
}
for (const key of gone) console.log(`::notice::${key} no longer fires: remove it from axe-known.json`);
for (const [key, v] of fresh) {
  console.log(`::error::${key} (${v.impact}): ${v.help}; ${v.count} element(s), e.g. ${v.targets.join(" | ")}`);
}
console.log(`${found.size} failing rule(s), ${found.size - fresh.length} known, ${fresh.length} new`);
process.exit(fresh.length > 0 ? 1 : 0);

async function openChannel(page) {
  await page.click("button", "Project A");
  await page.click("button", "design");
  await delay(SETTLE_MS);
}

async function openThread(page) {
  await openChannel(page);
  await page.click("button", "3 replies");
}

async function openSwitcher(page) {
  await page.click("button.search-btn");
}

async function openSettings(page) {
  await page.click("button", "Account menu for");
  await page.click('[role="menuitem"]', "Settings");
  await page.waitFor(`document.getElementById("settings-title")`);
}

async function openCall(page) {
  await openChannel(page);
  await page.click("button", "Start video call");
  await page.click("button", "Join");
}

function take(list, flag) {
  const index = list.indexOf(flag);
  if (index < 0) return false;
  list.splice(index, 1);
  return true;
}

function takeValue(list, flag) {
  const index = list.indexOf(flag);
  if (index < 0) return null;
  return list.splice(index, 2)[1];
}
