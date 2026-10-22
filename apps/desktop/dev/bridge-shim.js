// Dev only: implements `window.__TAURI__` (core.invoke, event.listen) on top of the dev bridge
// (`serve_dev_bridge` in src/core/dev_bridge.rs), so the web UI in a plain browser talks to a real
// client core. Inject it before the page loads, e.g. with Playwright's `page.addInitScript`.
(() => {
  const base = window.TEAMCRAFT_BRIDGE || 'http://127.0.0.1:8099';
  const post = async (cmd, args) => {
    // text/plain keeps the request "simple", so the browser sends no CORS preflight.
    const response = await fetch(`${base}/invoke`, { method: 'POST', headers: { 'Content-Type': 'text/plain' }, body: JSON.stringify({ cmd, args: args || {} }) });
    const answer = await response.json();
    if ('error' in answer) throw answer.error;
    return answer.ok;
  };
  // System notifications cannot be shown in a browser; they are collected for inspection instead.
  window.__notifications = [];
  const invoke = async (cmd, args) => {
    if (cmd === 'notify_user') {
      window.__notifications.push({ title: args.title, body: args.body });
      return null;
    }
    return post(cmd, args);
  };
  window.__TAURI__ = {
    core: { invoke },
    event: {
      listen: async (_name, handler) => {
        let counter = 0;
        let stopped = false;
        (async () => {
          while (!stopped) {
            try {
              const answer = await (await fetch(`${base}/events?since=${counter}`)).json();
              if (answer.counter > counter) { counter = answer.counter; handler({ payload: null }); }
            } catch (_) { await new Promise((resolve) => setTimeout(resolve, 1000)); }
          }
        })();
        return () => { stopped = true; };
      },
    },
  };
})();
