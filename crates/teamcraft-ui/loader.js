// Trunk initializer of the web client: drives the loading screen of index.html while the wasm loads
// and starts, and removes it once the app is mounted.
//
// The screen itself is plain HTML and CSS, so it is painted before this runs. This adds what needs
// script: the cached brand (name and icon of the last visit), the saved theme, download progress, and
// an error with a retry button. Its texts are the `loader-*` messages of i18n/*.ftl, which
// scripts/finish-loader.sh puts into init.js as `globalThis.teamcraftLoaderTexts` (this file is pinned
// by its integrity value, init.js is not).

// The same keys the app uses (settings.rs, auth/config.rs, teamcraft-client-api brand.rs).
const KEY_THEME = 'teamcraft.theme';
const KEY_LOCALE = 'teamcraft.locale';
const KEY_SERVER_URL = 'teamcraft.server_url';
const BRAND_CACHE_PREFIX = 'teamcraft.brand:';
const BRAND_PATH = '/api/v1/brand';
const BRAND_ASSET_PREFIX = '/api/v1/brand/assets/';
// The id of the server's built-in brand when no brand is configured (teamcraft-server brand.rs).
const BUILTIN_BRAND_ID = 'teamcraft';
const MAX_NAME_CHARS = 64;
const FADE_MS = 180;

export default function loader() {
  const root = document.getElementById('tc-loader');
  return {
    onStart: () => {
      if (!root) return;
      // Script runs, so progress and errors come from here; the script-free reload is not needed.
      root.querySelector('.tc-loader-reload')?.remove();
      applyTheme();
      applyBrand(root);
      const label = text('loader-loading');
      if (label) root.setAttribute('aria-label', label);
    },
    // `total` is the size of the wasm as built (trunk puts it in), not the Content-Length, which is
    // the compressed size when the server or a proxy compresses.
    onProgress: ({ current, total }) => {
      if (!root || !total || current > total) return;
      root.classList.add('has-progress');
      root.style.setProperty('--tc-loader-progress', String(current / total));
      if (current === total) root.classList.add('is-starting');
    },
    onComplete: () => {},
    onSuccess: () => {
      if (!root) return;
      // The app mounted during init; let it lay out once before the screen fades over it.
      requestAnimationFrame(() => {
        root.classList.add('is-done');
        setTimeout(() => root.remove(), FADE_MS + 50);
      });
    },
    onFailure: (error) => {
      console.error('the app could not be loaded', error);
      if (root) showFailure(root);
    },
  };
}

function showFailure(root) {
  root.classList.add('has-failed');
  root.removeAttribute('aria-label');
  const panel = root.querySelector('.tc-loader-error');
  if (!panel) return;
  panel.querySelector('.tc-loader-error-title').textContent = text('loader-failed-title');
  panel.querySelector('.tc-loader-error-body').textContent = text('loader-failed-body');
  const retry = panel.querySelector('.tc-loader-retry');
  retry.textContent = text('loader-retry');
  retry.addEventListener('click', () => location.reload());
  panel.hidden = false;
  retry.focus();
}

// The theme the user picked in the app, so a forced light or dark theme does not flash.
function applyTheme() {
  const theme = stored(KEY_THEME);
  if (theme === 'light' || theme === 'dark') document.documentElement.setAttribute('data-theme', theme);
}

// Name and icon of the brand: the copy the app cached on the last visit, else (first visit to this
// server) the server's answer, which the app asks for again right after from the HTTP cache.
function applyBrand(root) {
  const serverUrl = (stored(KEY_SERVER_URL) || '').replace(/\/+$/, '');
  let cached = null;
  try {
    cached = JSON.parse(stored(BRAND_CACHE_PREFIX + serverUrl) || 'null');
  } catch (error) {
    // An unreadable cache is the app's to replace; the screen just asks the server.
  }
  if (cached) {
    showBrand(root, cached, serverUrl === '');
  } else if (serverUrl === '') {
    fetch(BRAND_PATH, { credentials: 'omit' })
      .then((response) => (response.ok ? response.json() : null))
      .then((brand) => brand && root.isConnected && showBrand(root, brand, true))
      .catch(() => {});
  }
}

// Only what the app itself accepts: a plain name of bounded length, and the brand endpoint's own
// asset paths (and only from this origin).
function showBrand(root, brand, isSameOrigin) {
  if (!brand || typeof brand !== 'object') return;
  // No server brand: the product's own mark (style/loader.css), else neutral dots in its colours.
  root.classList.toggle('is-product', brand.id === BUILTIN_BRAND_ID);
  const name = typeof brand.display_name === 'string' ? brand.display_name.trim() : '';
  if (name && [...name].length <= MAX_NAME_CHARS) {
    root.querySelector('.tc-loader-name').textContent = name;
    // As the app does (BrandView::page_title): the product name stays in the tab.
    document.title = name === 'TeamCraft' ? name : `${name} – TeamCraft`;
  }
  const assets = isSameOrigin && brand.assets && typeof brand.assets === 'object' ? brand.assets : {};
  const slot = ['icon_dark', 'icon'].find(
    (candidate) => (candidate !== 'icon_dark' || isDark()) && assets[candidate] === BRAND_ASSET_PREFIX + candidate,
  );
  const logo = root.querySelector('.tc-loader-logo');
  if (!slot || !logo) return;
  logo.addEventListener('load', () => {
    logo.hidden = false;
    root.classList.add('has-logo');
  });
  logo.src = assets[slot];
}

function isDark() {
  const forced = document.documentElement.getAttribute('data-theme');
  if (forced) return forced === 'dark';
  return window.matchMedia('(prefers-color-scheme: dark)').matches;
}

// The app's language setting if it has one, else the browser's languages, else English.
function text(key) {
  const texts = globalThis.teamcraftLoaderTexts || {};
  const languages = [stored(KEY_LOCALE), ...(navigator.languages || [navigator.language])];
  for (const tag of languages) {
    if (!tag) continue;
    const language = texts[tag] || texts[String(tag).split(/[-_]/)[0].toLowerCase()];
    if (language && language[key]) return language[key];
  }
  return (texts.en && texts.en[key]) || '';
}

function stored(key) {
  try {
    return localStorage.getItem(key);
  } catch (error) {
    // Storage blocked (privacy settings): the screen just shows its defaults.
    return null;
  }
}
