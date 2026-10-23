// Service worker of the web client: wake-ups for pushes, and the check that the app code is the
// release that was signed (docs/e2ee.md §9.3).
//
// Verification. `scripts/sign-web-release.sh` writes `release.json` (every file of the bundle with
// its SHA-256, signed with the release key) and puts the release public key into RELEASE_PUBLIC_KEY
// below. This worker then
//   - on install and on every update fetches release.json, checks the signature, fetches every listed
//     file, checks the hashes and only then stores them in a versioned cache and activates;
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
    event.waitUntil(restored.then(() => event.source.postMessage(statusMessage())));
  } else if (data.type === 'push_probe') {
    // Whether the push code loads (tests, diagnostics): answers { type: 'push', ready, reason }.
    event.waitUntil(
      (hasPushCode() ? loadPush() : Promise.reject(new Error('this build has no push code')))
        .then(() => event.source.postMessage({ type: 'push', ready: true, reason: '' }))
        .catch((error) => event.source.postMessage({ type: 'push', ready: false, reason: reasonOf(error) })),
    );
  } else if (data.type === 'push_database' && typeof data.name === 'string') {
    event.waitUntil(rememberDatabase(data.name));
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
    const hit = await (await caches.open(CACHE_PREFIX + candidate)).match(url.pathname);
    if (hit) return hit;
  }
  return notInRelease();
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
    const queue = Object.entries(manifest.files);
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
  throw new Error('hash mismatch for ' + path);
}

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
  return { version: String(manifest.version), files };
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

async function broadcast() {
  for (const client of await self.clients.matchAll({ includeUncontrolled: true })) client.postMessage(statusMessage());
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

async function rememberDatabase(name) {
  await (await caches.open(PUSH_CONFIG_CACHE)).put('/push-database', new Response(name));
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
        await wasm_bindgen.handlePush(name, payload);
        return;
      }
    } catch (error) {
      console.warn('push could not be opened', error);
    }
  }
  // Browsers require a notification for every push.
  await self.registration.showNotification('TeamCraft', {
    body: 'New activity',
    tag: 'teamcraft-wake-up',
    renotify: false,
  });
}

// The browser rotated or dropped the subscription. Where it handed no new one, subscribe again with
// the old one's key; the page that owns the store registers it with the server (e2ee.md §7), now if
// one is open, otherwise when the app is opened next.
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
      await wasm_bindgen.handleNotificationClick(event);
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
