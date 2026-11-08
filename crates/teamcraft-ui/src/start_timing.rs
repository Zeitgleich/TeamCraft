//! Local timing of the web app's start (#1465): which part of a start costs what, on a phone where
//! nobody runs a profiler. Off unless asked for; nothing leaves the browser.
//!
//! - Turn it on: `localStorage.setItem("teamcraft.startTiming", "1")` in the page's console (Safari's
//!   Web Inspector for an iPhone, docs/performance.md "Web start on phones"), then start the app
//!   again. `removeItem` turns it off.
//! - `init.js` and the app put `performance.mark`s named `teamcraft:<part>:start` and `…:end` (one
//!   `teamcraft:<part>` for a moment), and a `performance.measure` per part (`teamcraft <part>`),
//!   which the Web Inspector's timeline shows.
//! - Once the first view is there ([`crate::startup`]), the app logs a summary (`start timing`, the
//!   console and the recent log of "Report a problem", which stays on the device unless the user
//!   sends a report) and puts it on `globalThis.teamcraftStartTiming`: each part's duration and when
//!   it ended, in milliseconds since the page's navigation started.
//!
//! The times are wall-clock, of the page's main thread: a part that waits (for IndexedDB, a fetch)
//! counts its wait, and parts overlap where the start runs them side by side.

/// `localStorage` key that turns the timing on ("1").
pub const TIMING_KEY: &str = "teamcraft.startTiming";
/// Every mark's name starts with this.
#[cfg(any(target_arch = "wasm32", test))]
const MARK_PREFIX: &str = "teamcraft:";
/// The global the summary goes to.
#[cfg(target_arch = "wasm32")]
const SUMMARY_GLOBAL: &str = "teamcraftStartTiming";

/// A part of the start, in the order a start goes through them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
  /// The app's wasm from the request to its last byte (`init.js`).
  WasmFetch,
  /// From the last byte until the module is compiled, instantiated and its start function ran
  /// (`init.js`): the compile that streaming didn't hide behind the download.
  WasmCompile,
  /// The UI's texts from the request to their last byte (`init.js`, next to the wasm).
  TextsFetch,
  /// Waiting for the texts and reading them, until the app mounts ([`crate::i18n`]).
  Texts,
  /// The signed-in app's pack, one file with its chunks (`init.js`, next to the wasm, #1465).
  SessionPack,
  /// The signed-in app's chunks (`areas::session`) until its first view can be built: the wait
  /// for the pack and their compile and instantiation.
  SessionChunk,
  /// Reading the account this browser was signed in to last (the token vault).
  CachedAccount,
  /// The long way of a start without a cached account: the session restored with the identity
  /// provider and the account asked of the server.
  Restore,
  /// Starting the client core of the cached account, up to its status: the store's lock, its key,
  /// its records and the client.
  CoreStart,
  /// Unwrapping the local store's database key (Web Crypto, IndexedDB).
  StoreUnlock,
  /// Opening the local store's database and reading every record's ciphertext into memory.
  StoreRead,
  /// The client on the opened store (account, keys, sync state).
  ClientOpen,
  /// The core building the UI's first snapshot of the local data (spaces, conversations, members).
  SnapshotBuild,
  /// The UI asking for that snapshot until it has it: the build, the wait for the core and the
  /// handover.
  Snapshot,
}

impl Part {
  pub const ALL: [Self; 14] = [
    Self::WasmFetch,
    Self::WasmCompile,
    Self::TextsFetch,
    Self::Texts,
    Self::SessionPack,
    Self::SessionChunk,
    Self::CachedAccount,
    Self::Restore,
    Self::CoreStart,
    Self::StoreUnlock,
    Self::StoreRead,
    Self::ClientOpen,
    Self::SnapshotBuild,
    Self::Snapshot,
  ];

  /// The name in the marks (`teamcraft:<name>:start`) and the summary.
  pub fn name(self) -> &'static str {
    match self {
      Self::WasmFetch => "wasm-fetch",
      Self::WasmCompile => "wasm-compile",
      Self::TextsFetch => "texts-fetch",
      Self::Texts => "texts",
      Self::SessionPack => "session-pack",
      Self::SessionChunk => "session-chunk",
      Self::CachedAccount => "cached-account",
      Self::Restore => "restore",
      Self::CoreStart => "core-start",
      Self::StoreUnlock => "store-unlock",
      Self::StoreRead => "store-read",
      Self::ClientOpen => "client-open",
      Self::SnapshotBuild => "snapshot-build",
      Self::Snapshot => "snapshot",
    }
  }

  /// The marks the part goes between. `init.js` marks the wasm's compile from the fetch's end.
  #[cfg(any(target_arch = "wasm32", test))]
  fn marks(self) -> (String, String) {
    match self {
      Self::WasmCompile => (mark_name("wasm-fetch:end"), mark_name("wasm:ready")),
      other => (
        mark_name(&format!("{}:start", other.name())),
        mark_name(&format!("{}:end", other.name())),
      ),
    }
  }
}

/// The name of the moment the first view was on screen.
const FIRST_VIEW: &str = "first-view";

#[cfg(any(target_arch = "wasm32", test))]
fn mark_name(name: &str) -> String {
  format!("{MARK_PREFIX}{name}")
}

/// One part of a summary.
#[derive(Clone, Debug, PartialEq)]
pub struct Measured {
  pub part: Part,
  pub ms: f64,
  /// When it ended, since the navigation started.
  pub end_ms: f64,
}

/// The summary's line: each part with its duration and end, then the first view.
pub fn summary_line(parts: &[Measured], first_view_ms: f64) -> String {
  let mut line = parts
    .iter()
    .map(|measured| {
      format!(
        "{} {:.0} ms (to {:.0})",
        measured.part.name(),
        measured.ms,
        measured.end_ms
      )
    })
    .collect::<Vec<_>>()
    .join(", ");
  if !line.is_empty() {
    line.push_str("; ");
  }
  line.push_str(&format!("{FIRST_VIEW} at {first_view_ms:.0} ms"));
  line
}

/// Marks the start of `part`.
pub fn start(part: Part) {
  mark(&format!("{}:start", part.name()));
}

/// Marks the end of `part`.
pub fn end(part: Part) {
  mark(&format!("{}:end", part.name()));
}

/// Times `future` as `part`.
pub async fn timed<T>(part: Part, future: impl std::future::Future<Output = T>) -> T {
  start(part);
  let output = future.await;
  end(part);
  output
}

/// Times `run` as `part`.
pub fn timed_now<T>(part: Part, run: impl FnOnce() -> T) -> T {
  start(part);
  let output = run();
  end(part);
  output
}

#[cfg(target_arch = "wasm32")]
mod web {
  use std::cell::Cell;

  use js_sys::{Array, Object, Reflect};
  use wasm_bindgen::JsValue;

  use super::{FIRST_VIEW, MARK_PREFIX, Measured, Part, SUMMARY_GLOBAL, TIMING_KEY, mark_name, summary_line};

  thread_local! {
    static IS_ON: Cell<Option<bool>> = const { Cell::new(None) };
    static IS_REPORTED: Cell<bool> = const { Cell::new(false) };
  }

  /// Whether the timing is on, read once per page.
  pub fn is_on() -> bool {
    IS_ON.with(|is_on| match is_on.get() {
      Some(value) => value,
      None => {
        let value = crate::settings::load_string(TIMING_KEY).as_deref() == Some("1");
        is_on.set(Some(value));
        value
      },
    })
  }

  fn performance() -> Option<web_sys::Performance> {
    web_sys::window()?.performance()
  }

  /// Until the summary: a long session doesn't fill the timeline with marks nobody reads.
  pub fn mark(name: &str) {
    if !is_on() || IS_REPORTED.with(Cell::get) {
      return;
    }
    if let Some(performance) = performance() {
      let _ = performance.mark(&format!("{MARK_PREFIX}{name}"));
    }
  }

  /// When the last mark of this name was set, since the navigation started.
  fn mark_at(performance: &web_sys::Performance, name: &str) -> Option<f64> {
    let entries = performance.get_entries_by_name_with_entry_type(name, "mark");
    let last = entries.get(entries.length().checked_sub(1)?);
    Reflect::get(&last, &JsValue::from_str("startTime"))
      .ok()?
      .as_f64()
  }

  /// The first view is on screen: the summary, once per page.
  pub fn report() {
    if !is_on() || IS_REPORTED.with(|is_reported| is_reported.replace(true)) {
      return;
    }
    let Some(performance) = performance() else { return };
    let first_view = mark_name(FIRST_VIEW);
    let _ = performance.mark(&first_view);
    let first_view_ms = mark_at(&performance, &first_view).unwrap_or_else(|| performance.now());
    let mut parts = Vec::new();
    for part in Part::ALL {
      let (start, end) = part.marks();
      let (Some(start_ms), Some(end_ms)) = (mark_at(&performance, &start), mark_at(&performance, &end))
      else {
        continue;
      };
      if end_ms < start_ms {
        continue;
      }
      let _ =
        performance.measure_with_start_mark_and_end_mark(&format!("teamcraft {}", part.name()), &start, &end);
      parts.push(Measured {
        part,
        ms: end_ms - start_ms,
        end_ms,
      });
    }
    // From the navigation's start to now, the first view.
    let _ = performance.measure("teamcraft first view");
    tracing::info!("start timing: {}", summary_line(&parts, first_view_ms));
    let summary = Object::new();
    let list = Array::new();
    for measured in &parts {
      let entry = Object::new();
      let _ = Reflect::set(&entry, &"part".into(), &measured.part.name().into());
      let _ = Reflect::set(&entry, &"ms".into(), &measured.ms.round().into());
      let _ = Reflect::set(&entry, &"endMs".into(), &measured.end_ms.round().into());
      list.push(&entry);
    }
    let _ = Reflect::set(&summary, &"parts".into(), &list);
    let _ = Reflect::set(&summary, &"firstViewMs".into(), &first_view_ms.round().into());
    let _ = Reflect::set(&js_sys::global(), &SUMMARY_GLOBAL.into(), &summary);
  }
}

#[cfg(target_arch = "wasm32")]
pub use web::{is_on, report};

#[cfg(target_arch = "wasm32")]
fn mark(name: &str) {
  web::mark(name);
}

#[cfg(not(target_arch = "wasm32"))]
fn mark(_name: &str) {}

/// Natively nothing is timed.
#[cfg(not(target_arch = "wasm32"))]
pub fn is_on() -> bool {
  false
}

#[cfg(not(target_arch = "wasm32"))]
pub fn report() {}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn parts_have_distinct_names_and_marks_of_their_own() {
    let mut names = Part::ALL.iter().map(|part| part.name()).collect::<Vec<_>>();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), Part::ALL.len());
    assert_eq!(
      Part::StoreRead.marks(),
      (
        "teamcraft:store-read:start".to_owned(),
        "teamcraft:store-read:end".to_owned()
      )
    );
    // init.js marks the compile from the fetch's last byte.
    assert_eq!(Part::WasmCompile.marks().0, "teamcraft:wasm-fetch:end");
  }

  #[test]
  fn the_summary_names_each_part_and_the_first_view() {
    let parts = [
      Measured {
        part: Part::WasmFetch,
        ms: 120.4,
        end_ms: 180.0,
      },
      Measured {
        part: Part::StoreRead,
        ms: 35.0,
        end_ms: 900.6,
      },
    ];
    assert_eq!(
      summary_line(&parts, 1234.4),
      "wasm-fetch 120 ms (to 180), store-read 35 ms (to 901); first-view at 1234 ms"
    );
    assert_eq!(summary_line(&[], 10.0), "first-view at 10 ms");
  }
}
