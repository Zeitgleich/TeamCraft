// TeamCraft Web Push service worker (e2ee.md §7).
//
// Only the glue the platform forces: listeners must be registered synchronously while the worker
// starts, before wasm has loaded. The logic is Rust (teamcraft-core, feature "service-worker",
// src/web_push.rs): it opens the encrypted store, decrypts the wake-up and shows the notification.
//
// Expects the web app's wasm-bindgen output built with `--target no-modules` next to this file
// (TEAMCRAFT_WASM_NAME below) and the store's IndexedDB name as `?db=` in the registration URL.

const TEAMCRAFT_WASM_NAME = 'teamcraft_web';
const DATABASE_NAME = new URL(self.location.href).searchParams.get('db') || 'teamcraft';

importScripts(`./${TEAMCRAFT_WASM_NAME}.js`);
const ready = wasm_bindgen({ module_or_path: `./${TEAMCRAFT_WASM_NAME}_bg.wasm` });

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));

self.addEventListener('push', (event) => {
  // The browser already removed the Web Push encryption; this is the sealed wake-up.
  const payload = event.data ? new Uint8Array(event.data.arrayBuffer()) : new Uint8Array();
  event.waitUntil(ready.then(() => wasm_bindgen.handlePush(DATABASE_NAME, payload)));
});

self.addEventListener('notificationclick', (event) => {
  event.waitUntil(ready.then(() => wasm_bindgen.handleNotificationClick(event)));
});
