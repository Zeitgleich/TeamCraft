// The dedicated worker that runs a member's core over the OPFS store (src/core_worker.rs, #2363).
//
// It downloads nothing of its own: the page sends its compiled wasm module and the URL of its
// bindings (`boot`), and this imports the bindings and instantiates the module here. The module's
// `main` sees a worker and starts the core host instead of the UI (src/main.rs). Messages that come
// before the host listens wait in the inbox, which the host takes over.

const inbox = [];
globalThis.teamcraftCoreWorkerInbox = inbox;
let isBooting = false;

self.onmessage = (event) => {
  const data = event.data;
  if (!isBooting && data && typeof data === 'object' && data.type === 'boot') {
    isBooting = true;
    boot(data.glue, data.module);
    return;
  }
  inbox.push(event);
};

async function boot(glue, module) {
  try {
    // Same origin as the page that sent it (its own bindings); anything else is refused.
    const url = new URL(glue, self.location.href);
    if (url.origin !== self.location.origin) throw new Error('bindings from another origin');
    const bindings = await import(url.href);
    await bindings.default({ module_or_path: module });
  } catch (error) {
    self.postMessage({ type: 'boot_failed', error: String(error && error.message || error) });
  }
}
