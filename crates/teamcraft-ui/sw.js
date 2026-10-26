// Service worker of the web client: wake-ups for pushes, and the check that the app code is the
// release that was signed (docs/e2ee.md §9.3).
//
// Verification. `scripts/sign-web-release.sh` writes `release.json` (every file of the bundle with
// its SHA-256, signed with the release key) and puts the release public key into RELEASE_PUBLIC_KEY
// below. This worker then
//   - on install and on every update fetches release.json, checks the signature, fetches every listed
//     file, checks the hashes and only then stores them in a versioned cache and activates;
//   - except the parts of the app that load when first used (`lazy` in release.json, #334): those are
//     fetched and checked against the signed hash when a page first asks for one, and in the
//     background a little after a page (not a guest's meeting page) asked for the status, so they
//     work offline too;
//   - serves the app from that cache, never from the network;
//   - keeps running the previous verified version if a new one does not verify, and tells the pages.
// A build without a pinned key (development) skips all of this and says so.
//
// Updates. A verified update waits until the user reloads (the page offers it) or every tab closed;
// it never takes over a page on its own. Each page is served from the release it was loaded with, and
// a release's cache is kept while a page still uses it, so a page never mixes two releases.
//
// Push. The Rust half of the worker (teamcraft-web-sw: opens the sealed wake-up, builds the
// notification) is built by trunk; scripts/finish-sw.sh inlines its JS glue at the end of this file and
// names its wasm file in PUSH_WASM. The wasm is a file of the signed release, so it is only ever read
// from the verified cache. A build without the glue shows a generic notification.
//
// Limit: the browser fetches this file itself on update checks, outside the worker's control, so a
// server that replaces sw.js defeats the check. See e2ee.md §9.3 for what it does and does not give.

const RELEASE_PUBLIC_KEY = '__RELEASE_PUBLIC_KEY__';
const IS_PINNED = !RELEASE_PUBLIC_KEY.startsWith('__');
const SIGNATURE_LABEL = 'teamcraft-web-release-v1\n';
const CACHE_PREFIX = 'teamcraft-release-';
// Files fetched at the same time while a release is installed.
const PARALLEL_DOWNLOADS = 4;
// Where a release's cache keeps its verified file list, for the files fetched later (no file of a
// release has this path).
const FILES_KEY = '/.release-files';
// The lazily loaded files are fetched this long after a page loaded, so they don't compete with what
// the page itself loads first.
const WARM_DELAY_MS = 15 * 1000;
// Guests' meeting pages (`/meeting/<id>`, meetings.rs) and share-link download pages (`/f/<id>`,
// share_link.rs) never use the signed-in app: no background
// fetch for them. A member who opens one gets the files with the next other page.
const GUEST_PATH_PREFIXES = ['/meeting/', '/f/'];

const STATE_CACHE = 'teamcraft-sw-state';
// The verified status of a release that is installed but not active yet; it becomes `/state` on
// activation, so the running worker, restarted meanwhile, keeps serving its own release.
const PENDING_STATE = '/pending';
const PUSH_CONFIG_CACHE = 'teamcraft-push-config';
const PUSH_WASM = '__PUSH_WASM__';

let status = { state: IS_PINNED ? 'pending' : 'unsigned', version: '', key_fingerprint: '', reason: '' };
// Which release each page was loaded with (client ID -> { version, at }), and releases that were
// active once.
let releases = { clients: {}, retired: [] };
// A page is listed before the browser reports it as a client; until then it counts as alive.
const NEW_CLIENT_GRACE_MS = 60 * 1000;
// A worker the browser stopped and started again has forgotten what it verified; the result is kept
// in a cache and read back before the first request is answered.
const restored = restoreStatus();

self.addEventListener('install', (event) => {
  event.waitUntil(IS_PINNED ? installVerified() : self.skipWaiting());
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    (async () => {
      await restored;
      // The previous worker kept recording pages while this one waited.
      await restoreReleases();
      const previous = status.version;
      await adoptPendingStatus();
      if (previous && previous !== status.version && !releases.retired.includes(previous)) {
        releases.retired.push(previous);
      }
      // Caches of releases that were never active (a failed install) go now; retired ones only once
      // no page uses them.
      for (const name of await caches.keys()) {
        const version = name.slice(CACHE_PREFIX.length);
        if (name.startsWith(CACHE_PREFIX) && version !== status.version && !releases.retired.includes(version)) {
          await caches.delete(name);
        }
      }
      await pruneReleases();
      await self.clients.claim();
      await broadcast();
    })(),
  );
});

self.addEventListener('message', (event) => {
  // Only pages of this origin talk to the worker; anything else is ignored.
  if (event.origin !== self.location.origin || !event.source) return;
  const data = event.data || {};
  if (data.type === 'skip_waiting') {
    // The user chose to reload into the new release (sent to the waiting worker).
    event.waitUntil(self.skipWaiting());
  } else if (data.type === 'status') {
    // Every page asks once when it loaded: the moment to fetch the lazily loaded files.
    event.waitUntil(restored.then(() => event.source.postMessage(statusMessage())));
    if (IS_PINNED && !isGuestPage(event.source)) event.waitUntil(restored.then(() => warmLazyFiles(status.version)));
  } else if (data.type === 'push_probe') {
    // Whether the push code loads (tests, diagnostics): answers { type: 'push', ready, reason }.
    event.waitUntil(
      (hasPushCode() ? loadPush() : Promise.reject(new Error('this build has no push code')))
        .then(() => event.source.postMessage({ type: 'push', ready: true, reason: '' }))
        .catch((error) => event.source.postMessage({ type: 'push', ready: false, reason: reasonOf(error) })),
    );
  } else if (data.type === 'push_database' && typeof data.name === 'string') {
    event.waitUntil(rememberDatabase(data.name, typeof data.labels === 'string' ? data.labels : ''));
  }
});

self.addEventListener('fetch', (event) => {
  if (!IS_PINNED) return;
  const request = event.request;
  const url = new URL(request.url);
  if (request.method !== 'GET' || url.origin !== self.location.origin) return;
  if (url.pathname.startsWith('/api/') || url.pathname === '/sw.js' || url.pathname === '/release.json') return;
  event.respondWith(
    restored.then(() => (status.state === 'verified' ? fromVerifiedCache(event, url) : fetch(request))),
  );
});

async function restoreStatus() {
  if (!IS_PINNED) return;
  try {
    const state = await caches.open(STATE_CACHE);
    const hit = await state.match('/state');
    if (hit) status = await hit.json();
    await restoreReleases();
  } catch (error) {
    // No stored state: the worker serves from the network until the next verified install.
  }
}

async function restoreReleases() {
  try {
    const known = await (await caches.open(STATE_CACHE)).match('/releases');
    if (known) releases = await known.json();
  } catch (error) {
    // Unknown pages only keep old releases longer.
  }
}

async function saveState(path, value) {
  await (await caches.open(STATE_CACHE)).put(path, new Response(JSON.stringify(value)));
}

// On activation: the release this worker installed becomes the one it serves.
async function adoptPendingStatus() {
  const state = await caches.open(STATE_CACHE);
  const pending = await state.match(PENDING_STATE);
  if (!pending) return;
  status = await pending.json();
  await saveState('/state', status);
  await state.delete(PENDING_STATE);
}

async function fromVerifiedCache(event, url) {
  const request = event.request;
  // Every page route (/, /meeting/<id>, /f/<id>) is the same app shell, of the current release.
  if (request.mode === 'navigate') {
    if (event.resultingClientId) event.waitUntil(rememberClient(event.resultingClientId));
    const shell = await (await caches.open(CACHE_PREFIX + status.version)).match('/index.html');
    return shell || notInRelease();
  }
  // Everything else from the release the page was loaded with; a page from before this worker
  // tracked releases may find its file in a retired release.
  const known = releases.clients[event.clientId];
  const versions = known ? [known.version] : [status.version, ...releases.retired.slice().reverse()];
  for (const candidate of versions) {
    const cache = await caches.open(CACHE_PREFIX + candidate);
    const hit = (await cache.match(url.pathname)) || (await fetchLazyFile(cache, url.pathname));
    if (hit) return hit;
  }
  return notInRelease();
}

// A file of the release that is fetched when first used (#334): downloaded and checked against the
// release's signed hash, then kept like the others. Null when the release doesn't list it.
async function fetchLazyFile(cache, pathname) {
  const files = await (await cache.match(FILES_KEY))?.json();
  const path = pathname.slice(1);
  if (!files || !Object.hasOwn(files, path)) return null;
  try {
    await storeVerified(cache, [cache], path, files[path]);
  } catch (error) {
    // Not what was signed: the pages warn like for a release that doesn't verify (the release
    // itself stays verified and keeps running; the part just doesn't load).
    if (error instanceof HashMismatch) await broadcast({ ...statusMessage(), state: 'rejected', reason: reasonOf(error) });
    throw error;
  }
  return cache.match(pathname);
}

function isGuestPage(client) {
  try {
    const { pathname } = new URL(client.url);
    return GUEST_PATH_PREFIXES.some((prefix) => pathname.startsWith(prefix));
  } catch (error) {
    return false;
  }
}

// Fetches the release's lazily loaded files that aren't cached yet, one at a time, after a delay.
// Failures are left for the page's own request (it retries, and says so when it can't).
let warming = null;
function warmLazyFiles(version) {
  if (status.state !== 'verified' || !version) return Promise.resolve();
  if (!warming) {
    warming = (async () => {
      await new Promise((resolve) => setTimeout(resolve, WARM_DELAY_MS));
      const cache = await caches.open(CACHE_PREFIX + version);
      const files = await (await cache.match(FILES_KEY))?.json();
      for (const path of Object.keys(files || {})) {
        if (!(await cache.match('/' + path))) await storeVerified(cache, [cache], path, files[path]);
      }
    })()
      .catch(() => {})
      .finally(() => {
        warming = null;
      });
  }
  return warming;
}

// Not part of the signed release: nothing the app needs. Let it fail instead of trusting the network.
function notInRelease() {
  return new Response('not part of the signed release', { status: 404 });
}

async function rememberClient(clientId) {
  releases.clients[clientId] = { version: status.version, at: Date.now() };
  await pruneReleases();
}

// Forgets pages that are gone and deletes retired releases no page uses any more. Pages whose release
// is unknown (loaded before this worker tracked releases) keep every retired release alive.
async function pruneReleases() {
  const windows = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
  const live = new Set(windows.map((client) => client.id));
  const now = Date.now();
  for (const [id, known] of Object.entries(releases.clients)) {
    if (!live.has(id) && now - known.at > NEW_CLIENT_GRACE_MS) delete releases.clients[id];
  }
  const hasUnknown = windows.some((client) => !(client.id in releases.clients));
  const used = new Set(Object.values(releases.clients).map((known) => known.version));
  if (!hasUnknown) {
    for (const version of releases.retired.filter((retired) => !used.has(retired))) {
      await caches.delete(CACHE_PREFIX + version);
    }
    releases.retired = releases.retired.filter((retired) => used.has(retired));
  }
  await saveState('/releases', releases);
}

async function installVerified() {
  try {
    await restored;
    const manifest = await verifiedManifest();
    const cache = await caches.open(CACHE_PREFIX + manifest.version);
    // Files of the running release (or of an interrupted install of this one) that did not change are
    // taken over instead of downloaded again; their hash is checked like a download's.
    const sources = [cache];
    if (status.state === 'verified' && status.version && status.version !== manifest.version) {
      sources.push(await caches.open(CACHE_PREFIX + status.version));
    }
    // Kept first: an install that stops halfway resumes with it, and later fetches check against it.
    await cache.put(FILES_KEY, new Response(JSON.stringify(manifest.files)));
    const queue = Object.entries(manifest.files).filter(([path]) => !manifest.lazy.has(path));
    const worker = async () => {
      for (let next = queue.shift(); next; next = queue.shift()) await storeVerified(cache, sources, ...next);
    };
    await Promise.all(Array.from({ length: PARALLEL_DOWNLOADS }, worker));
    const verified = {
      state: 'verified',
      version: manifest.version,
      key_fingerprint: await keyFingerprint(),
      reason: '',
    };
    if (self.registration.active) {
      // An update: it waits until the user reloads or every tab closed, and serves nothing before.
      await saveState(PENDING_STATE, verified);
    } else {
      // The first install: nothing runs yet.
      status = verified;
      await saveState('/state', status);
      await self.skipWaiting();
    }
  } catch (error) {
    const unsupported = error && error.name === 'NotSupportedError';
    status = { ...status, state: unsupported ? 'unsupported' : 'rejected', reason: reasonOf(error) };
    await broadcast();
    // Unsupported browsers run the app unverified (and say so); a failed check keeps the old version.
    if (unsupported) await self.skipWaiting();
    else throw error;
  }
}

// One file of the release into the new cache: a matching copy from `sources`, else a download.
// Downloads may come from the HTTP cache (the page has just loaded the same immutable files, and the
// server's precompressed variants arrive decoded); the hash decides, and a mismatch is retried once
// from the network before the release is rejected.
async function storeVerified(cache, sources, path, expected) {
  const url = '/' + path;
  for (const source of sources) {
    const hit = await source.match(url);
    if (hit && (await sha256Hex(await hit.clone().arrayBuffer())) === expected) {
      if (source !== cache) await cache.put(url, hit);
      return;
    }
  }
  for (const mode of ['default', 'no-store']) {
    const response = await fetch(url, { cache: mode });
    if (!response.ok) throw new Error('missing file ' + path);
    if ((await sha256Hex(await response.clone().arrayBuffer())) === expected) {
      await cache.put(url, response);
      return;
    }
  }
  throw new HashMismatch('hash mismatch for ' + path);
}

class HashMismatch extends Error {}

async function verifiedManifest() {
  const response = await fetch('/release.json', { cache: 'no-store' });
  if (!response.ok) throw new Error('the release manifest is missing');
  const manifest = await response.json();
  const files = {};
  for (const path of Object.keys(manifest.files || {}).sort()) files[path] = manifest.files[path];
  const message = new TextEncoder().encode(SIGNATURE_LABEL + JSON.stringify({ files, version: manifest.version }));
  const key = await crypto.subtle.importKey('raw', fromBase64(RELEASE_PUBLIC_KEY), { name: 'Ed25519' }, false, ['verify']);
  const signature = fromBase64(manifest.signature || '');
  if (!(await crypto.subtle.verify({ name: 'Ed25519' }, key, signature, message))) {
    throw new Error('the release manifest is not signed by the release key');
  }
  if (!manifest.files['index.html']) throw new Error('the release has no index.html');
  // Which files wait until they are used is a hint outside the signature: every file is still checked
  // against its signed hash, whenever it is fetched.
  const lazy = new Set((Array.isArray(manifest.lazy) ? manifest.lazy : []).filter((path) => path !== 'index.html'));
  return { version: String(manifest.version), files, lazy };
}

async function sha256Hex(bytes) {
  const digest = new Uint8Array(await crypto.subtle.digest('SHA-256', bytes));
  return Array.from(digest, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

async function keyFingerprint() {
  return (await sha256Hex(fromBase64(RELEASE_PUBLIC_KEY))).slice(0, 32);
}

function fromBase64(text) {
  const binary = atob(text.replace(/-/g, '+').replace(/_/g, '/'));
  return Uint8Array.from(binary, (char) => char.charCodeAt(0));
}

// What pages (and the stored status) learn about a failure: the message only, never a stack.
function reasonOf(error) {
  return error instanceof Error ? error.message : 'unknown error';
}

function statusMessage() {
  return { type: 'bundle', ...status };
}

async function broadcast(message = statusMessage()) {
  for (const client of await self.clients.matchAll({ includeUncontrolled: true })) client.postMessage(message);
}

// ---- Push wake-ups ---------------------------------------------------------------------------------
// The push payload is sealed to this device (e2ee.md §7). The wasm half opens it with the device's push
// key, from the encrypted store of the account whose name the page left in PUSH_CONFIG_CACHE, and shows
// the notification. Without it the worker can only say that something arrived, never what.
let pushReady = null;

function hasPushCode() {
  return !PUSH_WASM.startsWith('__') && typeof wasm_bindgen === 'function';
}

function loadPush() {
  if (!pushReady) {
    pushReady = (async () => {
      await restored;
      let response;
      if (IS_PINNED) {
        if (status.state !== 'verified') throw new Error('no verified release');
        response = await (await caches.open(CACHE_PREFIX + status.version)).match('/' + PUSH_WASM);
      } else {
        response = await fetch('/' + PUSH_WASM);
      }
      if (!response) throw new Error('the push code is not part of the verified release');
      await wasm_bindgen({ module_or_path: await response.arrayBuffer() });
    })().catch((error) => {
      pushReady = null;
      throw error;
    });
  }
  return pushReady;
}

// The page also leaves the notification texts in the user's language (JSON, `NotificationLabels` in
// teamcraft-core): this worker has no translations, and no page may be open when a push arrives.
async function rememberDatabase(name, labels) {
  const cache = await caches.open(PUSH_CONFIG_CACHE);
  await cache.put('/push-database', new Response(name));
  await cache.put('/push-labels', new Response(labels));
}

async function notificationLabels() {
  const hit = await (await caches.open(PUSH_CONFIG_CACHE)).match('/push-labels');
  return hit ? (await hit.text()) || null : null;
}

// "New message" in the user's language for the fallback below; English without the page's labels.
async function newMessageText() {
  try {
    const labels = JSON.parse((await notificationLabels()) || '{}');
    if (typeof labels.new_message === 'string' && labels.new_message.trim()) return labels.new_message;
  } catch (error) {
    console.warn('notification labels unreadable', error);
  }
  return 'New message';
}

async function databaseName() {
  const hit = await (await caches.open(PUSH_CONFIG_CACHE)).match('/push-database');
  return hit ? hit.text() : null;
}

self.addEventListener('push', (event) => {
  event.waitUntil(showPush(event));
});

async function showPush(event) {
  if (hasPushCode()) {
    try {
      const name = await databaseName();
      if (name) {
        await loadPush();
        const payload = event.data ? new Uint8Array(await event.data.arrayBuffer()) : new Uint8Array();
        await wasm_bindgen.handlePush(name, payload, await notificationLabels());
        return;
      }
    } catch (error) {
      console.warn('push could not be opened', error);
    }
  }
  // Browsers require a notification for every push; it says at least that a message came.
  await self.registration.showNotification('TeamCraft', {
    body: await newMessageText(),
    tag: 'teamcraft-wake-up',
    renotify: false,
  });
}

// The browser rotated or dropped the subscription. Where it handed no new one, subscribe again with
// the old one's key, then tell the server about the current subscription (the push code proves it
// with the device's push key, e2ee.md §7), so pushes don't go to the old one while no page is open.
// Open pages are told too; the page that owns the store registers it as usual.
self.addEventListener('pushsubscriptionchange', (event) => {
  event.waitUntil(renewSubscription(event));
});

async function renewSubscription(event) {
  try {
    const manager = self.registration.pushManager;
    const key = event.oldSubscription && event.oldSubscription.options.applicationServerKey;
    if (!event.newSubscription && !(await manager.getSubscription()) && key) {
      await manager.subscribe({ userVisibleOnly: true, applicationServerKey: key });
    }
  } catch (error) {
    console.warn('push subscription not renewed', error);
  }
  try {
    const name = await databaseName();
    if (name && hasPushCode()) {
      await loadPush();
      const old = event.oldSubscription ? JSON.stringify(event.oldSubscription.toJSON()) : null;
      await wasm_bindgen.handleSubscriptionChange(name, old);
    }
  } catch (error) {
    console.warn('renewed push subscription not sent to the server', error);
  }
  for (const client of await self.clients.matchAll({ type: 'window', includeUncontrolled: true })) {
    client.postMessage({ type: 'push_subscription_changed' });
  }
}

self.addEventListener('notificationclick', (event) => {
  event.waitUntil(clickNotification(event));
});

async function clickNotification(event) {
  if (hasPushCode()) {
    try {
      await loadPush();
      await wasm_bindgen.handleNotificationClick(event, await databaseName());
      return;
    } catch (error) {
      console.warn('notification click not handled by the push code', error);
    }
  }
  event.notification.close();
  const windows = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
  const open = windows.find((client) => 'focus' in client);
  if (open) await open.focus();
  else await self.clients.openWindow('/');
}

// ---- Generated glue --------------------------------------------------------------------------------
// scripts/finish-sw.sh puts the wasm-bindgen glue of teamcraft-web-sw here.
//__PUSH_GLUE__
