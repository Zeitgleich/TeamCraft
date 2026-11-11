# Upstream PR draft for tauri-apps/wry

Not opened yet. The change is `git log -p -- crates/vendor/wry` after the first (unmodified) commit,
without this file and `VENDORED.md`. Tauri would need no change: it creates its contexts through
`wry::WebContext::new`, which picks the settings up.

---

**Title:** Linux: set WebKit's memory pressure settings for new web contexts

**Body:**

WebKitGTK releases memory in a web process (caches, decoded images, JIT code, a full collection)
only once the process passes a fraction of `WebKitMemoryPressureSettings`' memory limit. The limit
defaults to the machine's RAM, at most 3 GB, and the first threshold to a third of it, so on most
machines a webview grows to about 1 GB before WebKit gives anything back. Apps that stay open all
day (chat, mail, dashboards) want a lower limit.

The settings are a construct-only property of `WebKitWebContext` (since 2.34), and wry builds the
context in `WebContextImpl::new` without a way to pass them; Tauri creates its contexts through
`wry::WebContext::new`, so neither can set them today.

This adds `wry::set_memory_pressure_settings(Option<webkit2gtk::MemoryPressureSettings>)` on the
GTK platforms. It is kept per thread (webviews are built on the GTK main thread) and applied by
every `WebContext` created afterwards. Nothing set, nothing changes. A global setter instead of a
`WebContext` constructor argument keeps Tauri unchanged: an app calls it in `setup` before building
its first window.

Alternative considered: a `WebContextExtUnix::new_with_memory_pressure_settings` constructor. It
would need a matching option in `tauri-runtime-wry`, which creates the contexts.
