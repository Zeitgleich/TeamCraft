// Package site landing page (scripts/package-site-page.py): picks the download for the visitor's
// system and adds copy buttons. The page works without it; nothing leaves the browser.
"use strict";

(async () => {
  const system = await detectSystem();
  offerDownload(system);
  enableCopyButtons();
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

function offerDownload({ os, arch }) {
  const button = document.getElementById("cta-button");
  const meta = document.getElementById("cta-meta");
  const cta = document.querySelector(".cta");
  if (!button || !meta || !cta || !os) return;
  for (const card of document.querySelectorAll(`.platform[data-os="${os}"]`)) {
    card.classList.add("is-detected");
    const label = card.querySelector(".detected");
    if (label) label.hidden = false;
  }
  if (os === "ios" && cta.dataset.appStore) {
    button.textContent = "Get it on the App Store";
    button.href = cta.dataset.appStore;
    return;
  }
  if (os === "ios") {
    button.textContent = "Not yet available for iPhone and iPad";
    button.classList.add("is-disabled");
    button.removeAttribute("href");
    meta.textContent = "Download TeamCraft on a computer, or use your organization's web app.";
    return;
  }
  const block = document.querySelector(`[data-channel-block="${cta.dataset.channel}"]`);
  if (!block) return;
  const candidates = [...block.querySelectorAll(`a.download[data-primary][data-os="${os}"]`)];
  const choice = candidates.find((link) => link.dataset.arch === arch || link.dataset.arch === "universal")
    || candidates[0];
  if (!choice) return;
  const names = { windows: "Windows", macos: "macOS", linux: "Linux", android: "Android" };
  button.textContent = `Download for ${names[os]}`;
  button.href = choice.href;
  const badge = meta.querySelector(".badge");
  meta.textContent = `${choice.dataset.label} · ${choice.dataset.meta} `;
  if (badge) meta.append(badge);
}

function enableCopyButtons() {
  for (const button of document.querySelectorAll("button.copy[data-copy]")) {
    button.hidden = false;
    button.addEventListener("click", async () => {
      const source = document.getElementById(button.dataset.copy);
      if (!source) return;
      if (await copyText(source)) {
        button.textContent = "Copied";
        button.classList.add("is-done");
      } else {
        button.textContent = "Selected, press Ctrl+C";
      }
      setTimeout(() => {
        button.textContent = "Copy";
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
