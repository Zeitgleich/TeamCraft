# Vendored: wry

| Item         | Value                                                                              |
|--------------|------------------------------------------------------------------------------------|
| Upstream     | https://github.com/tauri-apps/wry (crate `wry` on crates.io)                       |
| Version      | 0.57.0, upstream commit `792d0359ba6501a4fc360ece17de2ae42329a47c`                 |
| License      | Apache-2.0 OR MIT (`LICENSE-APACHE`, `LICENSE-MIT`, from the package)              |
| Used by      | `tauri-runtime-wry` (every Tauri webview), `teamcraft-desktop`; `[patch.crates-io]` |
| Why vendored | WebKitGTK's memory pressure settings for the app's web process (#1708)             |

## Why

WebKitGTK gives memory back (caches, decoded pictures, JIT code, a full garbage collection) only
when its web process passes a fraction of a memory limit: by default a third of the machine's RAM,
at most 3 GB, so about 1 GB on any current machine. The desktop app's budget is 400 MiB for all of
it (.claude/rules/robustness.md, "Performance and resources"), and its page alone sat at about
350 MiB with a large workspace (docs/performance.md, "Desktop on Linux: memory pressure and the allocator").

The limit and thresholds are `WebKitMemoryPressureSettings`, a construct-only property of the
`WebKitWebContext`: they can't be changed on a context that exists, and no environment variable sets
them. Tauri creates the context through `wry::WebContext::new` (in `tauri-runtime-wry`) without a
hook, so only wry can pass them.

## Changes against upstream 0.57.0

The first commit adding this directory is the unmodified crates.io package (without cargo's
bookkeeping files); `git log -p -- crates/vendor/wry` shows every change since.

1. `src/webkitgtk/web_context.rs`: new `set_memory_pressure_settings(Option<MemoryPressureSettings>)`,
   kept in a thread local (the GTK main thread builds the webviews). `WebContextImpl::new` passes
   what it holds to the `WebContext` builder; nothing set, nothing changes.
2. `src/webkitgtk/mod.rs`, `src/lib.rs`: export it on the GTK platforms (`wry::set_memory_pressure_settings`).
3. Added this file and `UPSTREAM_PR.md`.

The desktop shell calls it before it builds its first window (`apps/desktop/src/webview_memory.rs`).

## Updating

When Tauri moves to a newer wry, `[patch.crates-io]` stops applying (the version no longer matches)
and `teamcraft-desktop` stops compiling (no `wry::set_memory_pressure_settings`). Take the new
upstream package, repeat changes 1 and 2, or drop the vendored copy once upstream has a hook
(`UPSTREAM_PR.md`).
