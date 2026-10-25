// Trunk initializer of the web client: drives the loading screen of index.html while the wasm loads
// and starts, and removes it once the app is mounted.
//
// The screen itself is plain HTML and CSS, so it is painted before this runs, with the TeamCraft mark
// assembling and typing. This adds what needs script: the saved theme, the brand's name as the page
// title, download progress, an error with a retry button, and the handover: once the app is mounted,
// the mark flies into the app's own brand (the server's logo, or the TeamCraft mark when no brand is
// configured) while the screen fades away (issue #463). Its texts are the `loader-*` messages of
// i18n/*.ftl, which scripts/finish-loader.sh puts into init.js as `globalThis.teamcraftLoaderTexts`
// (this file is pinned by its integrity value, init.js is not).

// The same keys the app uses (settings.rs, auth/config.rs, teamcraft-client-api brand.rs).
const KEY_THEME = 'teamcraft.theme';
const KEY_LOCALE = 'teamcraft.locale';
const KEY_SERVER_URL = 'teamcraft.server_url';
const BRAND_CACHE_PREFIX = 'teamcraft.brand:';
const BRAND_PATH = '/api/v1/brand';
const MAX_NAME_CHARS = 64;
// The plain fade of style/loader.css (`.is-done`), for reduced motion.
const FADE_MS = 180;
// The handover: the mark moves across the screen (a strong ease-in-out, as the app's moves) and
// lands where the app shows its brand. A target that is not the TeamCraft mark takes over earlier,
// through a short blur, so the two read as one shape changing.
const FLIGHT_MS = 560;
const FLIGHT_EASING = 'cubic-bezier(0.77, 0, 0.175, 1)';
const SAME_MARK_CROSSFADE_AT = 0.8;
const OTHER_BRAND_CROSSFADE_AT = 0.45;
const CROSSFADE_BLUR = 'blur(3px)';
// Nothing to fly to (a screen without a brand): the mark rises towards the viewer and fades.
const DEPART_MS = 320;
const EMPHASIZED_EASING = 'cubic-bezier(0.05, 0.7, 0.1, 1)';
// The screen goes after this even if the handover never finishes (a hidden tab).
const HANDOVER_TIMEOUT_MS = 2000;
// The handover waits for the mark to finish assembling, but never longer than this.
const MAX_ASSEMBLY_WAIT_MS = 900;
// Where the app shows the brand, most specific first: the sign-in card's mark, logo or name, then the
// top bar's logo or name (login.rs, top_bar.rs).
const BRAND_TARGETS = ['.auth-brand img', '.auth-brand .auth-brand-name', '.top-bar .brand-logo', '.top-bar .brand-name'];
const PRODUCT_MARK_CLASS = 'auth-product-icon';
// Beside the product's tile, the server brand's tile (login.rs): it slides out from behind the
// product's as the mark lands.
const BRAND_TILE = '.auth-tile-brand';
const BRAND_TILE_MS = 420;

export default function loader() {
  const root = document.getElementById('tc-loader');
  return {
    onStart: () => {
      if (!root) return;
      // Script runs, so progress and errors come from here; the script-free reload is not needed.
      root.querySelector('.tc-loader-reload')?.remove();
      applyTheme();
      applyBrandName();
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
      if (root) handOver(root);
    },
    onFailure: (error) => {
      console.error('the app could not be loaded', error);
      if (root) showFailure(root);
    },
  };
}

// The app mounted during init. Once the mark has assembled and the app has laid out, the mark flies
// into the app's brand, or the screen just fades with reduced motion.
async function handOver(root) {
  const mark = root.querySelector('.tc-loader-mark');
  if (!mark || !mark.animate || prefersReducedMotion()) {
    await nextFrame();
    fadeOut(root);
    return;
  }
  await assembled(mark);
  await nextFrame();
  const target = brandTarget();
  root.classList.add('is-handing-over');
  root.setAttribute('aria-busy', 'false');
  const animation = target ? flyInto(mark, target) : depart(mark);
  // A hidden tab may never finish the animation; the screen goes either way (it no longer takes
  // input meanwhile).
  const done = Promise.race([animation.finished, delay(HANDOVER_TIMEOUT_MS)]).catch(() => {});
  done.then(() => root.remove());
}

// The mark moves onto the target and scales to its height; the target, hidden meanwhile, takes over
// at the end: the same mark swaps in the last frames, any other brand through a blurred crossfade.
// Returns the movement, which ends last.
function flyInto(mark, target) {
  const from = mark.getBoundingClientRect();
  const to = target.getBoundingClientRect();
  const scale = Math.min(to.width, to.height) / from.width;
  const dx = to.left + to.width / 2 - (from.left + from.width / 2);
  const dy = to.top + to.height / 2 - (from.top + from.height / 2);
  const isSameMark = target.classList.contains(PRODUCT_MARK_CLASS);
  const at = isSameMark ? SAME_MARK_CROSSFADE_AT : OTHER_BRAND_CROSSFADE_AT;
  const blur = isSameMark ? 'blur(0px)' : CROSSFADE_BLUR;
  const timing = { duration: FLIGHT_MS, fill: 'forwards' };
  if (isSameMark) revealBrandTile(target);
  target.animate(
    [
      { opacity: 0, filter: blur },
      { opacity: 0, filter: blur, offset: at },
      { opacity: 1, filter: 'blur(0px)' },
    ],
    { duration: FLIGHT_MS },
  );
  mark.animate(
    [
      { opacity: 1, filter: 'blur(0px)' },
      { opacity: 1, filter: 'blur(0px)', offset: at },
      { opacity: 0, filter: blur },
    ],
    timing,
  );
  return mark.animate(
    [{ transform: 'none' }, { transform: `translate(${dx}px, ${dy}px) scale(${scale})` }],
    { ...timing, easing: FLIGHT_EASING },
  );
}

function revealBrandTile(target) {
  const tile = target.closest('.auth-brand')?.querySelector(BRAND_TILE);
  tile?.animate(
    [
      { transform: 'translateX(-70%) scale(0.85)', opacity: 0 },
      { transform: 'none', opacity: 1 },
    ],
    { duration: BRAND_TILE_MS, delay: FLIGHT_MS * SAME_MARK_CROSSFADE_AT, easing: EMPHASIZED_EASING, fill: 'backwards' },
  );
}

function depart(mark) {
  return mark.animate(
    [
      { transform: 'none', opacity: 1, filter: 'blur(0px)' },
      { transform: 'scale(1.15)', opacity: 0, filter: CROSSFADE_BLUR },
    ],
    { duration: DEPART_MS, easing: EMPHASIZED_EASING, fill: 'forwards' },
  );
}

function fadeOut(root) {
  root.classList.add('is-done');
  setTimeout(() => root.remove(), FADE_MS + 50);
}

// The mark's entrance (its finite animations) has played; the typing loops go on.
function assembled(mark) {
  const entrance = (mark.getAnimations ? mark.getAnimations({ subtree: true }) : [])
    .filter((animation) => animation.effect && animation.effect.getTiming().iterations !== Infinity)
    .map((animation) => animation.finished.catch(() => {}));
  return Promise.race([Promise.all(entrance), delay(MAX_ASSEMBLY_WAIT_MS)]);
}

// The first of the app's brand elements that is laid out and on screen.
function brandTarget() {
  for (const selector of BRAND_TARGETS) {
    for (const element of document.querySelectorAll(selector)) {
      if (element.closest('#tc-loader')) continue;
      const rect = element.getBoundingClientRect();
      const isOnScreen = rect.bottom > 0 && rect.right > 0 && rect.top < innerHeight && rect.left < innerWidth;
      if (rect.width > 0 && rect.height > 0 && isOnScreen) return element;
    }
  }
  return null;
}

function prefersReducedMotion() {
  return window.matchMedia('(prefers-reduced-motion: reduce)').matches;
}

function nextFrame() {
  return new Promise((resolve) => requestAnimationFrame(() => resolve()));
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
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

// The brand's name as the page title: the copy the app cached on the last visit, else (first visit to
// this server) the server's answer, which the app asks for again right after from the HTTP cache. The
// screen itself always shows the TeamCraft mark; the brand takes over in the handover.
function applyBrandName() {
  const serverUrl = (stored(KEY_SERVER_URL) || '').replace(/\/+$/, '');
  let cached = null;
  try {
    cached = JSON.parse(stored(BRAND_CACHE_PREFIX + serverUrl) || 'null');
  } catch (error) {
    // An unreadable cache is the app's to replace; the screen just asks the server.
  }
  if (cached) {
    showBrandName(cached);
  } else if (serverUrl === '') {
    fetch(BRAND_PATH, { credentials: 'omit' })
      .then((response) => (response.ok ? response.json() : null))
      .then((brand) => brand && showBrandName(brand))
      .catch(() => {});
  }
}

// Only what the app itself accepts: a plain name of bounded length.
function showBrandName(brand) {
  if (!brand || typeof brand !== 'object') return;
  const name = typeof brand.display_name === 'string' ? brand.display_name.trim() : '';
  if (name && [...name].length <= MAX_NAME_CHARS) document.title = name;
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
