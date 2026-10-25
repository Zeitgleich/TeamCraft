// Trunk initializer of the web client: drives the loading screen of index.html while the wasm loads
// and starts, and removes it once the app shows its first real view.
//
// The screen itself is plain HTML and CSS, so it is painted before this runs, with the TeamCraft mark
// assembling and typing. This adds what needs script: the saved theme, the brand's name as the page
// title, download progress, an error with a retry button, and the handover. The screen stays while the
// app checks the session and opens the account's data, until the app says its first view is there
// (startup.rs): the sign-in card, then the mark flies into the card's TeamCraft mark (issue #463), or
// the chat, which the screen crossfades into. One screen from the start to the app, no card between.
// Its texts are the `loader-*` messages of
// i18n/*.ftl, which scripts/finish-loader.sh puts into init.js as `globalThis.teamcraftLoaderTexts`
// (this file is pinned by its integrity value, init.js is not).

// The same keys the app uses (settings.rs, auth/config.rs, teamcraft-client-api brand.rs).
const KEY_THEME = 'teamcraft.theme';
const KEY_LOCALE = 'teamcraft.locale';
const KEY_SERVER_URL = 'teamcraft.server_url';
const BRAND_CACHE_PREFIX = 'teamcraft.brand:';
const BRAND_PATH = '/api/v1/brand';
const MAX_NAME_CHARS = 64;
// The app's signal that its first view is on screen (startup.rs): an attribute on <html>, and an event.
const FIRST_VIEW_ATTRIBUTE = 'data-first-view';
const FIRST_VIEW_EVENT = 'teamcraft-first-view';
// A session check that takes this long (a slow server, the identity provider) says so under the bar.
const STATUS_AFTER_MS = 2500;
// The app has not shown a view this long after it started: the screen goes anyway, so it can never
// stick (the app's own screens show what it is waiting for).
const FIRST_VIEW_TIMEOUT_MS = 15000;
// The plain fade of style/loader.css (`.is-done`), for reduced motion.
const FADE_MS = 180;
// The handover onto the sign-in card: the mark moves across the screen (a strong ease-in-out, as the
// app's moves) and lands on the card's TeamCraft mark, which swaps in for the last frames.
const FLIGHT_MS = 560;
const FLIGHT_EASING = 'cubic-bezier(0.77, 0, 0.175, 1)';
const CROSSFADE_AT = 0.8;
// Anything else (the chat, a card without the mark): the mark rises towards the viewer and fades
// while the backdrop fades into the app.
const DEPART_MS = 320;
const DEPART_BLUR = 'blur(3px)';
const EMPHASIZED_EASING = 'cubic-bezier(0.05, 0.7, 0.1, 1)';
// The screen goes this long after the first view even if the handover never finishes (a hidden
// tab, an error): the assembly wait, the images, two frames and the flight, with room for a slow
// device.
const HANDOVER_TIMEOUT_MS = 2500;
// The handover waits for the mark to finish assembling, but never longer than this.
const MAX_ASSEMBLY_WAIT_MS = 900;
// ... and for the card's brand images, so the brand does not pop in after the mark landed.
const MAX_IMAGE_WAIT_MS = 400;
// The sign-in card's TeamCraft mark: alone without a server brand, in the product's tile beside
// the brand's otherwise (login.rs).
const MARK_TARGET = '.auth-brand .auth-product-icon';
const CARD_BRAND = '.auth-brand';
// Beside the product's tile, the server brand's tile: it slides out from behind the product's as
// the mark lands.
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
      if (root) awaitFirstView(root);
    },
    onFailure: (error) => {
      console.error('the app could not be loaded', error);
      if (root) showFailure(root);
    },
  };
}

// The app mounted during init and now checks the session; the screen stays until its first view is
// there, saying what it waits for after a while.
function awaitFirstView(root) {
  const status = setTimeout(() => showStatus(root), STATUS_AFTER_MS);
  Promise.race([firstView(), delay(FIRST_VIEW_TIMEOUT_MS)]).then(() => {
    clearTimeout(status);
    handOver(root);
  });
}

function firstView() {
  if (document.documentElement.hasAttribute(FIRST_VIEW_ATTRIBUTE)) return Promise.resolve();
  return new Promise((resolve) => addEventListener(FIRST_VIEW_EVENT, () => resolve(), { once: true }));
}

function showStatus(root) {
  const status = root.querySelector('.tc-loader-status');
  if (!status || root.classList.contains('has-failed')) return;
  status.textContent = text('loader-checking');
  root.classList.add('is-waiting');
}

// The first view is there. Once the mark has assembled and the view has laid out, the mark flies
// onto the sign-in card's mark or departs into the app, or the screen just fades with reduced
// motion. Whatever happens on the way (a hidden tab that draws no frames, an error), the screen is
// gone by the deadline, which starts before anything is awaited.
function handOver(root) {
  Promise.race([animateHandOver(root), delay(HANDOVER_TIMEOUT_MS)])
    .catch((error) => console.error('the loading screen could not hand over', error))
    .finally(() => root.remove());
}

async function animateHandOver(root) {
  const mark = root.querySelector('.tc-loader-mark');
  if (!mark || !mark.animate || prefersReducedMotion()) {
    await nextFrame();
    root.classList.add('is-done');
    await delay(FADE_MS + 50);
    return;
  }
  await Promise.all([assembled(mark), brandImages()]);
  await nextFrame();
  const target = markTarget();
  root.classList.add('is-handing-over');
  root.setAttribute('aria-busy', 'false');
  const animation = target ? flyInto(mark, target) : depart(mark);
  await animation.finished;
}

// The mark moves onto the card's mark and scales to its size; the card's mark, hidden meanwhile,
// swaps in for the last frames. Returns the movement, which ends last.
function flyInto(mark, target) {
  const from = mark.getBoundingClientRect();
  const to = target.getBoundingClientRect();
  const scale = Math.min(to.width, to.height) / from.width;
  const dx = to.left + to.width / 2 - (from.left + from.width / 2);
  const dy = to.top + to.height / 2 - (from.top + from.height / 2);
  const timing = { duration: FLIGHT_MS, fill: 'forwards' };
  revealBrandTile(target);
  target.animate([{ opacity: 0 }, { opacity: 0, offset: CROSSFADE_AT }, { opacity: 1 }], { duration: FLIGHT_MS });
  mark.animate([{ opacity: 1 }, { opacity: 1, offset: CROSSFADE_AT }, { opacity: 0 }], timing);
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
    { duration: BRAND_TILE_MS, delay: FLIGHT_MS * CROSSFADE_AT, easing: EMPHASIZED_EASING, fill: 'backwards' },
  );
}

function depart(mark) {
  return mark.animate(
    [
      { transform: 'none', opacity: 1, filter: 'blur(0px)' },
      { transform: 'scale(1.15)', opacity: 0, filter: DEPART_BLUR },
    ],
    { duration: DEPART_MS, easing: EMPHASIZED_EASING, fill: 'forwards' },
  );
}

// The mark's entrance (its finite animations) has played; the typing loops go on.
function assembled(mark) {
  const entrance = (mark.getAnimations ? mark.getAnimations({ subtree: true }) : [])
    .filter((animation) => animation.effect && animation.effect.getTiming().iterations !== Infinity)
    .map((animation) => animation.finished.catch(() => {}));
  return Promise.race([Promise.all(entrance), delay(MAX_ASSEMBLY_WAIT_MS)]);
}

// The sign-in card's brand images (the brand's tile and logo) have loaded or failed, or the wait is
// over.
function brandImages() {
  const images = [...document.querySelectorAll(`${CARD_BRAND} img`)].filter((image) => !image.complete);
  const loaded = images.map((image) => image.decode().catch(() => {}));
  return Promise.race([Promise.all(loaded), delay(MAX_IMAGE_WAIT_MS)]);
}

// The sign-in card's TeamCraft mark, when it is laid out and on screen.
function markTarget() {
  const element = document.querySelector(MARK_TARGET);
  if (!element) return null;
  const rect = element.getBoundingClientRect();
  const isOnScreen = rect.bottom > 0 && rect.right > 0 && rect.top < innerHeight && rect.left < innerWidth;
  return rect.width > 0 && rect.height > 0 && isOnScreen ? element : null;
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
  if (name && [...name].length <= MAX_NAME_CHARS) {
    // As the app does (BrandView::page_title): the product name stays in the tab.
    document.title = name === 'TeamCraft' ? name : `${name} – TeamCraft`;
  }
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
