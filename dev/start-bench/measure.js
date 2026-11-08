// Times the web app's start in Playwright's WebKit with the app's own start timing (#1465,
// docs/performance.md "Web start on phones"), against serve.py:
//   node measure.js <origin> signin <runs>                       the sign-in page
//   node measure.js <origin> cached <runs> [channels] [messages] [history]
//                                                                a signed-in start from the local data
// One start per line as JSON: when the first view was there, the phases, the first paint, the wasm
// requests, the requests to the API before it.
const { webkit } = require('playwright');

const [origin, scenario, runsText, channels = '40', messages = '25', history = '0'] = process.argv.slice(2);
const runs = Number(runsText || 5);

async function timing(page) {
  await page.waitForFunction(() => globalThis.teamcraftStartTiming, null, { timeout: 120000 });
  return page.evaluate(() => {
    const summary = globalThis.teamcraftStartTiming;
    const parts = Object.fromEntries(summary.parts.map((part) => [part.part, [part.ms, part.endMs]]));
    const wasm = performance
      .getEntriesByType('resource')
      .filter((entry) => entry.name.endsWith('.wasm'))
      .map((entry) => ({
        name: entry.name.split('/').pop(),
        start: Math.round(entry.startTime),
        end: Math.round(entry.responseEnd),
        bytes: entry.encodedBodySize,
      }));
    const paint = Object.fromEntries(performance.getEntriesByType('paint').map((entry) => [entry.name, Math.round(entry.startTime)]));
    return { firstView: summary.firstViewMs, paint, parts, wasm, view: document.querySelector('main, .auth-card, .app-shell')?.className || '' };
  });
}

(async () => {
  const browser = await webkit.launch();
  const context = await browser.newContext({ viewport: { width: 390, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true });
  const page = await context.newPage();
  const requests = [];
  page.on('request', (request) => requests.push(request.url().replace(origin, '')));
  page.on('console', (message) => {
    const text = message.text();
    if (/start timing|error|panic/i.test(text)) console.error('console:', text.slice(0, 400));
  });
  await page.addInitScript(() => localStorage.setItem('teamcraft.startTiming', '1'));
  if (scenario === 'cached') {
    await page.goto(`${origin}/__bench/index.html?channels=${channels}&messages=${messages}&history=${history}`);
    await page.waitForURL(`${origin}/`, { timeout: 600000 });
  } else {
    await page.goto(`${origin}/`);
  }
  const results = [];
  for (let run = 0; run < runs; run += 1) {
    if (run > 0) {
      requests.length = 0;
      // Time for what the last start keeps in the store (the sidebar's counts) to reach the disk.
      await page.waitForTimeout(1500);
      await page.reload();
    }
    const result = await timing(page);
    result.api = requests.filter((url) => url.startsWith('/api/'));
    results.push(result);
    console.log(JSON.stringify(result));
  }
  await browser.close();
})().catch((error) => {
  console.error(error);
  process.exit(1);
});
