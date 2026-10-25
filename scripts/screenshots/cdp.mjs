// A minimal Chrome DevTools Protocol client for the screenshot scripts: starts headless Chrome,
// opens one page and drives it. Node 22+ (built-in WebSocket and fetch); no npm packages.

import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const CHROME = process.env.CHROME || "google-chrome";
const DEBUG_PORT = 9333;

/** Starts Chrome and returns a page session; await `close()` at the end. */
export async function launch() {
  const profile = mkdtempSync(join(tmpdir(), "teamcraft-shots-"));
  const chrome = spawn(CHROME, [
    "--headless=new",
    `--remote-debugging-port=${DEBUG_PORT}`,
    `--user-data-dir=${profile}`,
    "--no-first-run",
    "--hide-scrollbars",
    "--force-color-profile=srgb",
    "--lang=en-US",
    "--font-render-hinting=none",
    "about:blank",
  ], { stdio: "ignore" });
  const target = await pageTarget();
  const page = new Page(new WebSocket(target.webSocketDebuggerUrl));
  await page.opened;
  await page.send("Page.enable");
  await page.send("Runtime.enable");
  const exited = new Promise((resolve) => chrome.once("exit", resolve));
  page.close = async () => {
    page.socket.close();
    chrome.kill();
    await exited;
    rmSync(profile, { recursive: true, force: true });
  };
  return page;
}

async function pageTarget() {
  for (let attempt = 0; attempt < 100; attempt++) {
    try {
      const targets = await (await fetch(`http://127.0.0.1:${DEBUG_PORT}/json`)).json();
      const page = targets.find((target) => target.type === "page");
      if (page) return page;
    } catch {
      // Chrome is still starting.
    }
    await delay(100);
  }
  throw new Error("Chrome did not start (set CHROME=/path/to/chrome)");
}

class Page {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map();
    this.opened = new Promise((resolve) => socket.addEventListener("open", resolve, { once: true }));
    socket.addEventListener("message", (event) => {
      const message = JSON.parse(event.data);
      const call = this.pending.get(message.id);
      if (!call) return;
      this.pending.delete(message.id);
      if (message.error) call.reject(new Error(message.error.message));
      else call.resolve(message.result);
    });
  }

  send(method, params = {}) {
    const id = this.nextId++;
    this.socket.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }

  /** Window size in CSS px, device scale and colour scheme; `isMobile` emulates a touch phone. */
  async emulate({ width, height, scale = 1, scheme = "light", isMobile = false }) {
    await this.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: scale, mobile: isMobile });
    await this.send("Emulation.setTouchEmulationEnabled", { enabled: isMobile });
    await this.send("Emulation.setEmulatedMedia", {
      features: [
        { name: "prefers-color-scheme", value: scheme },
        { name: "prefers-reduced-motion", value: "reduce" },
      ],
    });
  }

  async goto(url) {
    await this.send("Page.navigate", { url });
  }

  /** The value of `expression` in the page (awaited if it is a promise). */
  async evaluate(expression) {
    const { result, exceptionDetails } = await this.send("Runtime.evaluate", {
      expression,
      awaitPromise: true,
      returnByValue: true,
    });
    if (exceptionDetails) throw new Error(`${expression}: ${exceptionDetails.exception?.description ?? exceptionDetails.text}`);
    return result.value;
  }

  /** Waits until `expression` is truthy in the page. */
  async waitFor(expression, timeoutMs = 20_000) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      if (await this.evaluate(`Boolean(${expression})`).catch(() => false)) return;
      await delay(100);
    }
    throw new Error(`timed out waiting for ${expression}`);
  }

  /** Clicks the first element matching `selector` whose text or label contains `text` (if given). */
  async click(selector, text = null) {
    await this.waitFor(findExpression(selector, text));
    await this.evaluate(`${findExpression(selector, text)}.click()`);
  }

  async screenshot() {
    const { data } = await this.send("Page.captureScreenshot", { format: "png" });
    return Buffer.from(data, "base64");
  }
}

function findExpression(selector, text) {
  const all = `[...document.querySelectorAll(${JSON.stringify(selector)})]`;
  if (text === null) return `${all}[0]`;
  const wanted = JSON.stringify(text);
  return `${all}.find((element) => [element.textContent, element.getAttribute("aria-label"), element.title]`
    + `.some((value) => value?.includes(${wanted})))`;
}

export function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
