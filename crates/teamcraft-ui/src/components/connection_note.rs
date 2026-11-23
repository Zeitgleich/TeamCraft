//! The one place the app says it can't reach the server while it reconnects by itself: a quiet line
//! of text just above the composer ("Server can't be reached right now"), never a toast. It comes
//! after a short grace period, at once when a request of the user found the server down, and goes
//! by itself once the server answers (offline_banner.rs decides when).
//!
//! The composer of the open conversation shows it; where no composer is on screen (the files, the
//! activity, settings) the same line sits at the bottom of the window instead.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use leptos::prelude::*;

use crate::i18n::use_i18n;

/// What [`request_unreachable`] calls, with the number it was installed under.
type Sink = (u64, Rc<dyn Fn()>);

thread_local! {
  /// Where [`request_unreachable`] goes: the connection state's owner (offline_banner.rs).
  /// The installed sink and the number it was installed under.
  static REQUEST_FAILURE_SINK: RefCell<Option<Sink>> = RefCell::new(None);
  static NEXT_SINK: Cell<u64> = const { Cell::new(0) };
}

/// What the line says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionNote {
  /// The device itself has no network.
  DeviceOffline,
  /// Opening the connection takes long (the first sync of a fresh session included).
  Connecting,
  /// The server can't be reached (down, restarting, a proxy in front of it says so). After it stayed
  /// away long the line offers to try now.
  ServerUnreachable { can_retry: bool },
  /// Connected, but the server answers late.
  Slow,
}

/// The current note and how many composers show it, shared by the app (provided in `App`).
#[derive(Clone, Copy)]
pub struct ConnectionNoteSlot {
  maybe_note: RwSignal<Option<ConnectionNote>>,
  composers: RwSignal<usize>,
}

impl ConnectionNoteSlot {
  pub fn new() -> Self {
    Self {
      maybe_note: RwSignal::new(None),
      composers: RwSignal::new(0),
    }
  }

  /// Sets the note; `None` hides it. Tolerates a slot whose owner is gone.
  pub fn set_note(&self, maybe_note: Option<ConnectionNote>) {
    if self.maybe_note.try_get_untracked() != Some(maybe_note) {
      let _ = self.maybe_note.try_set(maybe_note);
    }
  }
}

impl Default for ConnectionNoteSlot {
  fn default() -> Self {
    Self::new()
  }
}

/// A request found the server unreachable (feedback.rs): the note shows at once instead of an error.
/// `false` where no note is on the page (a meeting guest's): the caller says so another way.
pub fn request_unreachable() -> bool {
  // Cloned out of the cell: the sink may start work that reports again.
  let maybe_sink = REQUEST_FAILURE_SINK.with(|sink| sink.borrow().as_ref().map(|(_, sink)| sink.clone()));
  match maybe_sink {
    Some(sink) => {
      sink();
      true
    },
    None => false,
  }
}

/// Where [`request_unreachable`] goes until the current reactive owner is cleaned up.
pub fn set_request_failure_sink(sink: impl Fn() + 'static) {
  let number = NEXT_SINK.with(|next| next.replace(next.get() + 1));
  REQUEST_FAILURE_SINK.with(|slot| *slot.borrow_mut() = Some((number, Rc::new(sink))));
  on_cleanup(move || {
    // Only this owner's sink: a newer shell may have installed its own meanwhile.
    REQUEST_FAILURE_SINK.with(|slot| {
      let mut slot = slot.borrow_mut();
      if slot.as_ref().is_some_and(|(current, _)| *current == number) {
        *slot = None;
      }
    });
  });
}

/// The line above a composer. Each mounted one counts, so the window's own line knows to stay away.
#[component]
pub fn ComposerConnectionNote() -> impl IntoView {
  let slot = use_context::<ConnectionNoteSlot>()?;
  slot.composers.update(|count| *count += 1);
  on_cleanup(move || {
    let _ = slot
      .composers
      .try_update(|count| *count = count.saturating_sub(1));
  });
  Some(view! { <NoteLine note_slot=slot is_floating=false /> })
}

/// The line at the bottom of the window while no composer is on screen.
#[component]
pub fn WindowConnectionNote() -> impl IntoView {
  let slot = use_context::<ConnectionNoteSlot>()?;
  Some(view! {
    <Show when=move || slot.composers.get() == 0 fallback=|| ()>
      <NoteLine note_slot=slot is_floating=true />
    </Show>
  })
}

/// The text of the note, red where something is wrong and muted while the app only waits; screen
/// readers hear it once as a status.
#[component]
fn NoteLine(note_slot: ConnectionNoteSlot, is_floating: bool) -> impl IntoView {
  let i18n = use_i18n();
  // Always in the page: a live region announces only changes of a region that already existed.
  view! {
    <div class="connection-note-region" class:is-floating=is_floating role="status">
      {move || {
        note_slot.maybe_note.get().map(|note| {
          let (key, is_trouble) = match note {
            ConnectionNote::DeviceOffline => ("connection-note-offline", true),
            ConnectionNote::Connecting => ("connection-hint-connecting", false),
            ConnectionNote::ServerUnreachable { .. } => ("connection-note-unreachable", true),
            ConnectionNote::Slow => ("connection-hint-slow", false),
          };
          let can_retry = matches!(note, ConnectionNote::ServerUnreachable { can_retry: true });
          view! {
            <p class="connection-note" class:is-trouble=is_trouble>
              <span>{move || i18n.t(key)}</span>
              {can_retry.then(|| view! {
                <button
                  type="button"
                  class="connection-note-retry"
                  on:click=move |_| crate::components::offline_banner::network_changed()
                >
                  {move || i18n.t("connection-banner-retry")}
                </button>
              })}
            </p>
          }
        })
      }}
    </div>
  }
}
