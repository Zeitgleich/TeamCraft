// Package site landing page (scripts/package-site-page.py): picks the download for the visitor's
// system, turns the Linux repository channels into tabs, adds copy buttons and reveals sections as
// they scroll into view. The page works without it; nothing leaves the browser.
"use strict";

const OS_NAMES = { windows: "Windows", macos: "macOS", linux: "Linux", android: "Android" };

(async () => {
  watchHeader();
  revealOnScroll();
  enableTabs();
  enableCopyButtons();
  const system = await detectSystem();
  offerDownload(system);
})();

async function detectSystem() {
  const agent = navigator.userAgent || "";
  const platform = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || "";
  let os = "";
  if (/android/i.test(agent)) os = "android";
  else if (/iphone|ipad|ipod/i.test(agent) || (/mac/i.test(platform) && navigator.maxTouchPoints > 1)) os = "ios";
  else if (/win/i.test(platform) || /windows/i.test(agent)) os = "windows";
  else if (/mac/i.test(platform) || /mac os x/i.test(agent)) os = "macos";
  else if (/linux|x11|cros/i.test(platform + " " + agent)) os = "linux";
  if (os === "macos") return { os, arch: await macArch() };
  let arch = /aarch64|arm64|armv8/i.test(agent + " " + platform) ? "arm64" : "x64";
  try {
    if (navigator.userAgentData && navigator.userAgentData.getHighEntropyValues) {
      const values = await navigator.userAgentData.getHighEntropyValues(["architecture", "bitness"]);
      if (values.architecture === "arm" && values.bitness === "64") arch = "arm64";
      else if (values.architecture === "x86") arch = "x64";
    }
  } catch (_) {
    // Not offered by every browser; the guess from the user agent stays.
  }
  return { os, arch };
}

// Apple silicon or Intel, or "" when the browser doesn't tell: Safari reports an Intel Mac in its user
// agent on every Mac, so only explicit signals count. Unknown keeps the page's order (Apple silicon
// first), and the other button always stays.
async function macArch() {
  try {
    if (navigator.userAgentData && navigator.userAgentData.getHighEntropyValues) {
      const { architecture } = await navigator.userAgentData.getHighEntropyValues(["architecture"]);
      if (architecture === "arm") return "arm64";
      if (architecture === "x86") return "x64";
    }
  } catch (_) {
    // Not offered by every browser; the graphics renderer may still tell.
  }
  try {
    const gl = document.createElement("canvas").getContext("webgl");
    const info = gl && gl.getExtension("WEBGL_debug_renderer_info");
    const renderer = info ? String(gl.getParameter(info.UNMASKED_RENDERER_WEBGL)) : "";
    if (/Apple M\d/.test(renderer)) return "arm64";
    if (/Intel|AMD|Radeon|NVIDIA/i.test(renderer)) return "x64";
  } catch (_) {
    // No WebGL: unknown.
  }
  return "";
}

function offerDownload({ os, arch }) {
  const button = document.getElementById("cta-button");
  const label = document.getElementById("cta-label");
  const meta = document.getElementById("cta-meta");
  const cta = document.querySelector(".cta");
  if (!button || !label || !meta || !cta || !os) return;
  for (const card of document.querySelectorAll(`.platform[data-os="${os}"]`)) {
    card.classList.add("is-detected");
    const badge = card.querySelector(".detected");
    if (badge) badge.hidden = false;
    preferArch(card, arch);
  }
  if (os === "ios" && cta.dataset.appStore) {
    label.textContent = "Get it on the App Store";
    button.href = cta.dataset.appStore;
    meta.textContent = "For iPhone and iPad";
    return;
  }
  if (os === "ios") {
    label.textContent = "Not yet available for iPhone and iPad";
    button.classList.add("is-disabled");
    button.removeAttribute("href");
    button.setAttribute("aria-disabled", "true");
    meta.textContent = "Download TeamCraft on a computer, or use your organization's web app.";
    return;
  }
  const block = document.querySelector(`[data-channel-block="${cta.dataset.channel}"]`);
  if (!block) return;
  const candidates = [...block.querySelectorAll(`a.download[data-primary][data-os="${os}"]`)];
  // Linux: the package repository (it updates with the system) before a single package.
  const choice = candidates.find((link) => link.dataset.kind === "repository")
    || candidates.find((link) => link.dataset.arch === arch) || candidates[0];
  if (!choice) return;
  label.textContent = `Download for ${OS_NAMES[os]}`;
  button.href = choice.href;
  meta.textContent = `${choice.dataset.label} · ${choice.dataset.meta} · ${choice.dataset.tag} `;
  if (cta.dataset.channelTitle === "Beta") {
    const badge = document.createElement("span");
    badge.className = "badge badge-beta";
    badge.textContent = "Beta";
    meta.append(badge);
  }
}

// On the visitor's own platform card the main button gets their architecture: it trades places
// with the "Also for" link of that architecture, or, on a card with a button per architecture, takes
// the highlight and the first place.
function preferArch(card, arch) {
  if (!arch) return;
  for (const item of card.querySelectorAll(".pkg-item")) preferPackageArch(item, arch);
  const choices = card.querySelector(".choices");
  if (choices) {
    preferChoice(choices, arch);
    return;
  }
  const main = card.querySelector(".primary a.download");
  if (!main || main.dataset.arch === arch) return;
  const other = card.querySelector(`.primary-alt a.download[data-arch="${arch}"]`);
  if (!other) return;
  const mainMeta = card.querySelector(".primary-meta");
  const swap = (name) => {
    const value = main.getAttribute(name);
    main.setAttribute(name, other.getAttribute(name));
    other.setAttribute(name, value);
  };
  const mainLabel = main.getAttribute("aria-label");
  const otherLabel = other.getAttribute("aria-label");
  for (const name of ["href", "data-arch", "data-meta"]) swap(name);
  main.setAttribute("aria-label", `Download ${otherLabel}`);
  other.setAttribute("aria-label", mainLabel.replace(/^Download /, ""));
  other.textContent = other.dataset.meta;
  if (mainMeta) mainMeta.textContent = main.dataset.meta;
}

// A package button (Linux .deb / .rpm) of the visitor's architecture trades places with its "Also for" link.
function preferPackageArch(item, arch) {
  const main = item.querySelector("a.choice");
  const other = item.querySelector(`.primary-alt a.download[data-arch="${arch}"]`);
  if (!main || !other || main.dataset.arch === arch) return;
  const size = main.querySelector(".choice-size");
  const mainLabel = main.getAttribute("aria-label");
  const otherLabel = other.getAttribute("aria-label");
  for (const name of ["href", "data-arch", "data-meta"]) {
    const value = main.getAttribute(name);
    main.setAttribute(name, other.getAttribute(name));
    other.setAttribute(name, value);
  }
  main.setAttribute("aria-label", `Download ${otherLabel}`);
  other.setAttribute("aria-label", mainLabel.replace(/^Download /, ""));
  if (size) size.textContent = main.dataset.meta;
  other.textContent = other.dataset.meta;
}

function preferChoice(choices, arch) {
  const pick = choices.querySelector(`a.choice[data-arch="${arch}"]`);
  if (!pick || pick.classList.contains("button-primary")) return;
  for (const link of choices.querySelectorAll("a.choice")) {
    link.classList.toggle("button-primary", link === pick);
    link.classList.toggle("button-secondary", link !== pick);
  }
  choices.prepend(pick);
}

function watchHeader() {
  const header = document.querySelector(".site-header");
  if (!header) return;
  const update = () => header.classList.toggle("is-scrolled", window.scrollY > 8);
  update();
  window.addEventListener("scroll", update, { passive: true });
}

// Only sections that start below the fold fade in, so nothing visible on load ever flickers.
function revealOnScroll() {
  if (!("IntersectionObserver" in window)) return;
  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  const observer = new IntersectionObserver((entries) => {
    for (const entry of entries) {
      if (!entry.isIntersecting) continue;
      entry.target.classList.remove("is-pending");
      observer.unobserve(entry.target);
    }
  }, { rootMargin: "0px 0px -8% 0px" });
  for (const element of document.querySelectorAll(".reveal")) {
    if (element.getBoundingClientRect().top < window.innerHeight) continue;
    element.classList.add("is-pending");
    observer.observe(element);
  }
}

// Linux repository channels: stacked panels with headings become a tab list (WAI-ARIA tabs pattern).
function enableTabs() {
  for (const tabs of document.querySelectorAll("[data-tabs]")) {
    const panels = [...tabs.querySelectorAll(":scope > .tab-panel")];
    if (panels.length < 2) continue;
    const list = document.createElement("div");
    list.className = "tablist";
    list.setAttribute("role", "tablist");
    list.setAttribute("aria-label", "Release channel");
    const buttons = panels.map((panel) => {
      const button = document.createElement("button");
      button.type = "button";
      button.id = `${panel.id}-tab`;
      button.setAttribute("role", "tab");
      button.setAttribute("aria-controls", panel.id);
      button.textContent = panel.dataset.tabTitle;
      panel.setAttribute("role", "tabpanel");
      panel.setAttribute("aria-labelledby", button.id);
      panel.tabIndex = 0;
      list.append(button);
      return button;
    });
    const select = (index, focus) => {
      buttons.forEach((button, i) => {
        const selected = i === index;
        button.setAttribute("aria-selected", String(selected));
        button.tabIndex = selected ? 0 : -1;
        panels[i].hidden = !selected;
      });
      if (focus) buttons[index].focus();
    };
    buttons.forEach((button, i) => {
      button.addEventListener("click", () => select(i, false));
      button.addEventListener("keydown", (event) => {
        const last = buttons.length - 1;
        const next = { ArrowRight: i === last ? 0 : i + 1, ArrowLeft: i === 0 ? last : i - 1, Home: 0, End: last }[event.key];
        if (next === undefined) return;
        event.preventDefault();
        select(next, true);
      });
    });
    tabs.prepend(list);
    tabs.classList.add("is-enhanced");
    const initial = panels.findIndex((panel) => panel.hasAttribute("data-selected"));
    select(Math.max(initial, 0), false);
  }
}

function enableCopyButtons() {
  for (const button of document.querySelectorAll("button.copy[data-copy]")) {
    const label = button.querySelector("span") || button;
    button.hidden = false;
    button.setAttribute("aria-live", "polite");
    button.addEventListener("click", async () => {
      const source = document.getElementById(button.dataset.copy);
      if (!source) return;
      if (await copyText(source)) {
        label.textContent = "Copied";
        button.classList.add("is-done");
      } else {
        label.textContent = "Selected, press Ctrl+C";
      }
      setTimeout(() => {
        label.textContent = "Copy";
        button.classList.remove("is-done");
      }, 1800);
    });
  }
}

// The clipboard API needs permission in some browsers; then the snippet is selected for copying by
// hand.
async function copyText(source) {
  try {
    await navigator.clipboard.writeText(source.textContent);
    return true;
  } catch (_) {
    const range = document.createRange();
    range.selectNodeContents(source);
    const selection = window.getSelection();
    selection.removeAllRanges();
    selection.addRange(range);
    return false;
  }
}
