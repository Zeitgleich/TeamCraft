//! Who else has a page of the notes open, and their carets (issue #1456, e2ee.md §4.11, "Who has
//! the page open").
//!
//! While a page is open the editor says so to the other members connected now: when it opens,
//! when its caret moves (at most once per [`CANVAS_PRESENCE_INTERVAL_MS`]), every
//! [`CANVAS_PRESENCE_REFRESH_MS`] while the window is visible, at once when another device appears
//! (so it learns of this one without waiting for the refresh), and "closed" when the page is left.
//! The core seals each word with the channel's content key; the server relays it and keeps
//! nothing. The setting "Show when I'm typing" covers it: with it off, this device says nothing.
//!
//! The others show as avatars in the line under the toolbar, and their carets as thin bars in
//! their colour (the hue of their avatar) with their name, drawn over the block they are in.

use leptos::html;
use leptos::leptos_dom::helpers::{TimeoutHandle, set_timeout_with_handle};
use leptos::prelude::*;
use std::collections::BTreeSet;
use std::time::Duration;
use teamcraft_client_api::model::{CanvasPeer, CanvasPresenceNote, User};
use teamcraft_proto::{
  CANVAS_PRESENCE_INTERVAL_MS, CANVAS_PRESENCE_REFRESH_MS, CANVAS_PRESENCE_TIMEOUT_MS, CanvasCaret,
  CanvasPageId, CanvasPageState, DeviceId,
};

use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::canvas_editor;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, UserId};
use crate::state::AppState;

/// How often the editor looks again at who has the page open: an entry runs out without an event.
const PEERS_CHECK_MS: u64 = 10_000;

/// A caret of another member, placed over a block.
#[derive(Clone, Debug, PartialEq)]
struct PlacedCaret {
  key: (UserId, DeviceId),
  name: String,
  hue: u16,
  x: f64,
  y: f64,
  height: f64,
}

/// The other devices with this page open, kept current, and this device's own words about it.
/// `caret` is where this device's caret is (none while read-only or outside the page's blocks).
pub fn use_page_presence(
  state: AppState,
  conversation: ConversationId,
  maybe_page: Option<CanvasPageId>,
  caret: Signal<Option<CanvasCaret>>,
) -> Memo<Vec<CanvasPeer>> {
  let now = RwSignal::new(js_sys::Date::now());
  crate::reactive::use_interval_while_visible(PEERS_CHECK_MS, move || {
    let _ = now.try_set(js_sys::Date::now());
  });
  // Owned by the leaving word too: the stored value may be gone by the time the page closes.
  let closing_page = maybe_page.clone();
  let page = StoredValue::new(maybe_page);
  let peers = Memo::new(move |_| {
    let since = now.get() as i64 - CANVAS_PRESENCE_TIMEOUT_MS;
    let peers = page
      .with_value(|maybe_page| state.read(|backend| backend.canvas_peers(conversation, maybe_page.as_ref())));
    peers
      .into_iter()
      .filter(|peer| peer.heard_at_ms > since)
      .collect::<Vec<_>>()
  });

  let sent_at = StoredValue::new(f64::NEG_INFINITY);
  let timer = StoredValue::new_local(None::<TimeoutHandle>);
  let announce = move |page_state: CanvasPageState, maybe_page: Option<CanvasPageId>| {
    let is_shared = state
      .settings
      .prefs
      .with_untracked(|prefs| prefs.privacy.share_typing);
    if !is_shared {
      return;
    }
    let note = CanvasPresenceNote {
      maybe_page,
      state: page_state,
      maybe_caret: caret.try_get_untracked().flatten(),
    };
    let _ = sent_at.try_set_value(js_sys::Date::now());
    state.read(|backend| backend.announce_canvas(conversation, note));
  };
  // Says "open" now, or as soon as the interval since the last word allows.
  let announce_soon = move || {
    if timer.try_with_value(Option::is_some).unwrap_or(true) {
      return;
    }
    let Some(last) = sent_at.try_get_value() else {
      return;
    };
    let open = move || {
      if let Some(maybe_page) = page.try_get_value() {
        announce(CanvasPageState::Open, maybe_page);
      }
    };
    let wait = last + CANVAS_PRESENCE_INTERVAL_MS as f64 - js_sys::Date::now();
    if wait <= 0.0 {
      open();
      return;
    }
    let fire = move || {
      let _ = timer.try_set_value(None);
      open();
    };
    if let Ok(handle) = set_timeout_with_handle(fire, Duration::from_millis(wait.ceil() as u64)) {
      let _ = timer.try_set_value(Some(handle));
    }
  };

  // Opening the page and every move of the caret.
  Effect::new(move |_| {
    caret.track();
    announce_soon();
  });
  crate::reactive::use_interval_while_visible(CANVAS_PRESENCE_REFRESH_MS as u64, announce_soon);
  // A device that just opened the page learns of this one at once.
  let known = StoredValue::new(BTreeSet::<(UserId, DeviceId)>::new());
  Effect::new(move |_| {
    let devices: BTreeSet<(UserId, DeviceId)> =
      peers.with(|peers| peers.iter().map(|peer| (peer.user, peer.device)).collect());
    let is_new = known.with_value(|known| devices.iter().any(|device| !known.contains(device)));
    known.set_value(devices);
    if is_new {
      announce_soon();
    }
  });
  on_cleanup(move || {
    if let Some(Some(handle)) = timer.try_update_value(Option::take) {
      handle.clear();
    }
    announce(CanvasPageState::Closed, closing_page.clone());
  });
  peers
}

/// The avatars of the others who have the page open, and who edited it lately without having it
/// open (a phone, a client from before presence): "Anna has this page open · Ben is editing".
#[component]
pub fn PeersLine(state: AppState, peers: Memo<Vec<CanvasPeer>>, editors: Memo<Vec<UserId>>) -> impl IntoView {
  let i18n = use_i18n();
  let present = Memo::new(move |_| {
    let users: BTreeSet<UserId> = peers.with(|peers| peers.iter().map(|peer| peer.user).collect());
    users
      .into_iter()
      .filter_map(|user| state.read(|backend| backend.user(user)))
      .collect::<Vec<User>>()
  });
  let text = move || {
    let mut parts = Vec::new();
    let names: Vec<String> =
      present.with(|users| users.iter().map(|user| user.display_name.clone()).collect());
    if !names.is_empty() {
      let joined = names.join(", ");
      parts.push(i18n.t_count_with("canvas-open-by", names.len(), &[("names", joined.as_str())]));
    }
    let editing: Vec<String> = editors
      .get()
      .into_iter()
      .filter(|user| present.with(|users| !users.iter().any(|present| present.id == *user)))
      .filter_map(|user| state.read(|backend| backend.user(user)))
      .map(|user| user.display_name)
      .collect();
    if !editing.is_empty() {
      let joined = editing.join(", ");
      parts.push(i18n.t_count_with("canvas-editing", editing.len(), &[("names", joined.as_str())]));
    }
    parts.join(" · ")
  };
  view! {
    <div class="canvas-presence" class:is-empty=move || text().is_empty()>
      <span class="canvas-presence-avatars" aria-hidden="true">
        <For each=move || present.get() key=|user| user.id let:user>
          <Avatar user=user size=AvatarSize::Small />
        </For>
      </span>
      <span class="canvas-presence-text" aria-live="polite">{text}</span>
    </div>
  }
}

/// The carets of the others in one block, over its text field. Decorative for screen readers: the
/// line above names who has the page open.
#[component]
pub fn PeerCarets(
  state: AppState,
  peers: Memo<Vec<CanvasPeer>>,
  block: String,
  field: NodeRef<html::Div>,
  /// The block's text, so the carets are placed again when it changes.
  text: Signal<String>,
) -> impl IntoView {
  let here = Memo::new(move |_| {
    peers.with(|peers| {
      peers
        .iter()
        .filter_map(|peer| {
          let caret = peer.maybe_caret.as_ref()?;
          (caret.block == block).then_some((peer.user, peer.device, caret.end))
        })
        .collect::<Vec<_>>()
    })
  });
  let placed = RwSignal::new(Vec::<PlacedCaret>::new());
  let measure = move || {
    let Some(element) = field.get_untracked() else {
      return;
    };
    let carets = here.get_untracked();
    let next: Vec<PlacedCaret> = carets
      .into_iter()
      .filter_map(|(user, device, at)| {
        let person = state.read(|backend| backend.user(user))?;
        let (x, y, height) = canvas_editor::caret_box(&element, at)?;
        Some(PlacedCaret {
          key: (user, device),
          name: person.display_name,
          hue: person.avatar_hue,
          x,
          y,
          height,
        })
      })
      .collect();
    let _ = placed.try_set(next);
  };
  // After the block rendered its text: placed where the text is now.
  Effect::new(move |_| {
    here.track();
    text.track();
    request_animation_frame(measure);
  });
  let resize = window_event_listener(leptos::ev::resize, move |_| measure());
  on_cleanup(move || resize.remove());
  view! {
    <For
      each=move || placed.with(|carets| carets.iter().map(|caret| caret.key).collect::<Vec<_>>())
      key=|key| *key
      let:key
    >
      {
        // Kept while it moves, so it glides to its new place instead of popping up there.
        let caret = move || placed.with(|carets| carets.iter().find(|caret| caret.key == key).cloned());
        view! {
          <span
            class="canvas-peer-caret"
            aria-hidden="true"
            style:--hue=move || caret().map(|caret| caret.hue.to_string()).unwrap_or_default()
            style:transform=move || caret().map(|caret| format!("translate({}px, {}px)", caret.x, caret.y)).unwrap_or_default()
            style:height=move || caret().map(|caret| format!("{}px", caret.height)).unwrap_or_default()
          >
            <span class="canvas-peer-name">{move || caret().map(|caret| caret.name).unwrap_or_default()}</span>
          </span>
        }
      }
    </For>
  }
}
