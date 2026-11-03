// The PDF viewer of file previews (#557): pages drawn by the vendored, pinned PDF.js next to this
// file. Loaded on first use (web_pdf.rs), never part of the first visit.
//
// The PDF is untrusted. PDF.js runs with document scripting off (`enableScripting` is never set and
// the scripting sandbox, pdf.sandbox / quickjs, is not shipped), font and glyph code built with
// `eval` off (`isEvalSupported: false`), and no annotation, form or link layer: pages are canvases
// plus a text layer for selecting text. Nothing here loads a remote URL; the document is the bytes
// the page decrypted. Styles are set through CSSOM only (the page's CSP is `style-src 'self'`).

import * as pdfjs from "./pdf.min.mjs";

pdfjs.GlobalWorkerOptions.workerSrc = new URL("./pdf.worker.min.mjs", import.meta.url).href;

const WASM_URL = new URL("./wasm/", import.meta.url).href;
/// Pages this far (in CSS pixels) outside the scroll area are drawn already.
const DRAW_MARGIN_PX = 600;
/// Drawn pages kept besides the ones near the viewport are released beyond this distance.
const RELEASE_MARGIN_PX = 3000;
const MIN_SCALE = 0.25;
const MAX_SCALE = 4;

/// Opens `bytes` (a Uint8Array, which PDF.js takes over) in `container`, a scrollable element.
/// `onpage(n)` is told which page is in view, `onfailure()` once when a page can't be drawn (#835).
/// Rejects when the file is not a readable PDF.
///
/// The app calls into the viewer from Rust: an exception thrown back through the app's wasm leaves
/// it in a broken state, so `destroy` never throws and the callbacks into the app are guarded.
export async function openPdf(container, bytes, onpage, onfailure) {
  const loading = pdfjs.getDocument({
    data: bytes,
    isEvalSupported: false,
    enableScripting: false,
    wasmUrl: WASM_URL,
    // A PDF names no further files to fetch (no range requests, no external streams).
    disableAutoFetch: true,
    disableStream: true,
  });
  try {
    const doc = await loading.promise;
    return await new Viewer(loading, doc, container, onpage, onfailure);
  } catch (error) {
    // Frees the worker; a failure here changes nothing for the caller, who gets `error`.
    loading.destroy().catch(() => {});
    throw error;
  }
}

/// Calls `callback` of the app, which may be gone (dropped with its view) by now.
function notify(callback, ...args) {
  try {
    callback?.(...args);
  } catch (error) {
    console.warn("PDF viewer: the app's callback failed", error);
  }
}

class Viewer {
  #onScroll;
  #isScanQueued = false;

  constructor(loading, doc, container, onpage, onfailure) {
    this.loading = loading;
    this.doc = doc;
    this.container = container;
    this.onpage = onpage;
    this.onfailure = onfailure;
    this.hasFailed = false;
    this.scale = 1;
    this.pages = [];
    this.current = 1;
    this.texts = new Map();
    this.isDestroyed = false;
    this.pageCount = doc.numPages;
    this.#onScroll = () => this.#scanSoon();
    container.addEventListener("scroll", this.#onScroll, { passive: true });
    return this.#build().then(() => this);
  }

  async #build() {
    const first = await this.doc.getPage(1);
    this.base = first.getViewport({ scale: 1 });
    this.container.replaceChildren();
    for (let number = 1; number <= this.pageCount; number++) {
      const element = document.createElement("div");
      element.className = "pdf-page";
      element.dataset.page = String(number);
      this.container.append(element);
      this.pages.push({ number, element, state: "empty", size: this.base, token: 0 });
    }
    this.#layout();
    this.#scan();
  }

  setScale(scale) {
    this.scale = Math.min(MAX_SCALE, Math.max(MIN_SCALE, scale));
    for (const page of this.pages) this.#release(page);
    this.#layout();
    this.goTo(this.current);
    this.#scan();
    return this.scale;
  }

  /// Scale at which the first page is as wide as the container.
  fitWidth() {
    const width = this.container.clientWidth - 24;
    return this.setScale(width / this.base.width);
  }

  goTo(number) {
    const page = this.pages[Math.min(this.pageCount, Math.max(1, number)) - 1];
    if (page) this.container.scrollTop = page.element.offsetTop - this.container.offsetTop;
  }

  /// The number of the next page (after the current one, wrapping round) whose text contains `query`;
  /// 0 when none does. Page texts are read once, as the user searches.
  async find(query) {
    const needle = query.trim().toLowerCase();
    if (!needle) return 0;
    for (let step = 1; step <= this.pageCount; step++) {
      const number = ((this.current - 1 + step) % this.pageCount) + 1;
      if ((await this.#text(number)).includes(needle)) {
        this.goTo(number);
        return number;
      }
      if (this.isDestroyed) return 0;
    }
    return 0;
  }

  /// Frees the pages, the document and its worker. Never throws (see `openPdf`): whatever fails
  /// here, the app's view is closing anyway.
  destroy() {
    if (this.isDestroyed) return;
    this.isDestroyed = true;
    this.onpage = null;
    this.onfailure = null;
    try {
      this.container.removeEventListener("scroll", this.#onScroll);
      for (const page of this.pages) this.#release(page);
      this.container.replaceChildren();
    } catch (error) {
      console.warn("PDF viewer: releasing the pages failed", error);
    }
    // The loading task owns the document and its worker. PDF.js 5 dropped
    // `PDFDocumentProxy.destroy`, whose call threw here and broke closing the viewer (#835).
    this.loading.destroy().catch((error) => console.warn("PDF viewer: closing the document failed", error));
  }

  async #text(number) {
    if (!this.texts.has(number)) {
      const page = await this.doc.getPage(number);
      const content = await page.getTextContent();
      this.texts.set(number, content.items.map((item) => item.str ?? "").join(" ").toLowerCase());
      page.cleanup();
    }
    return this.texts.get(number);
  }

  #layout() {
    for (const page of this.pages) {
      page.element.style.width = `${Math.floor(page.size.width * this.scale)}px`;
      page.element.style.height = `${Math.floor(page.size.height * this.scale)}px`;
    }
  }

  /// Once per frame at most, however fast the scroll events come.
  #scanSoon() {
    if (this.#isScanQueued) return;
    this.#isScanQueued = true;
    requestAnimationFrame(() => {
      this.#isScanQueued = false;
      if (!this.isDestroyed) this.#scan();
    });
  }

  /// Draws the pages near the visible area and releases the far ones.
  #scan() {
    const top = this.container.scrollTop;
    const bottom = top + this.container.clientHeight;
    const offset = this.container.offsetTop;
    for (const page of this.pages) {
      const start = page.element.offsetTop - offset;
      const end = start + page.element.offsetHeight;
      if (end >= top - DRAW_MARGIN_PX && start <= bottom + DRAW_MARGIN_PX) {
        this.#draw(page);
      } else if (end < top - RELEASE_MARGIN_PX || start > bottom + RELEASE_MARGIN_PX) {
        this.#release(page);
      }
    }
    this.#updateCurrent();
  }

  #updateCurrent() {
    const middle = this.container.scrollTop + this.container.clientHeight / 3;
    let current = 1;
    for (const page of this.pages) {
      if (page.element.offsetTop - this.container.offsetTop <= middle) current = page.number;
      else break;
    }
    if (current !== this.current) {
      this.current = current;
      notify(this.onpage, current);
    }
  }

  async #draw(page) {
    if (page.state !== "empty") return;
    page.state = "drawing";
    // Releasing the page (or a new zoom) while it loads makes this draw stale: it then stops without
    // touching the page, which another draw may own by now.
    const token = ++page.token;
    const scale = this.scale;
    try {
      const source = await this.doc.getPage(page.number);
      if (this.isDestroyed || page.token !== token) return;
      const viewport = source.getViewport({ scale });
      page.size = source.getViewport({ scale: 1 });
      const ratio = window.devicePixelRatio || 1;
      const canvas = document.createElement("canvas");
      canvas.width = Math.floor(viewport.width * ratio);
      canvas.height = Math.floor(viewport.height * ratio);
      canvas.style.width = `${Math.floor(viewport.width)}px`;
      canvas.style.height = `${Math.floor(viewport.height)}px`;
      const text = document.createElement("div");
      text.className = "pdf-text";
      text.style.setProperty("--total-scale-factor", String(scale));
      text.style.setProperty("--scale-round-x", "1px");
      text.style.setProperty("--scale-round-y", "1px");
      page.element.style.width = `${Math.floor(viewport.width)}px`;
      page.element.style.height = `${Math.floor(viewport.height)}px`;
      page.element.replaceChildren(canvas, text);
      page.state = "drawn";
      page.source = source;
      const task = source.render({
        canvasContext: canvas.getContext("2d"),
        viewport,
        transform: ratio === 1 ? null : [ratio, 0, 0, ratio, 0, 0],
      });
      page.task = task;
      await task.promise;
      if (page.token !== token) return;
      await new pdfjs.TextLayer({
        textContentSource: source.streamTextContent(),
        container: text,
        viewport,
      }).render();
      // Sizes of pages differ from the first one's: what is near the viewport may have moved.
      this.#scanSoon();
    } catch (error) {
      // A cancelled render (the page was released while drawing) is not an error.
      if (page.token === token && error?.name !== "RenderingCancelledException") {
        page.element.classList.add("is-broken");
        page.state = "broken";
        this.#fail(error);
      }
    }
  }

  /// Tells the app once that pages can't be drawn: it shows a message and the download instead.
  #fail(error) {
    console.warn("PDF viewer: a page could not be drawn", error);
    if (this.hasFailed || this.isDestroyed) return;
    this.hasFailed = true;
    notify(this.onfailure);
  }

  #release(page) {
    page.token++;
    if (page.state === "empty") return;
    if (page.state === "broken") {
      page.element.classList.remove("is-broken");
      page.state = "empty";
      return;
    }
    page.task?.cancel();
    page.task = null;
    page.source?.cleanup();
    page.source = null;
    for (const canvas of page.element.querySelectorAll("canvas")) {
      // Frees the bitmap now, not when the garbage collector gets to it.
      canvas.width = 0;
      canvas.height = 0;
    }
    page.element.replaceChildren();
    page.state = "empty";
  }
}
