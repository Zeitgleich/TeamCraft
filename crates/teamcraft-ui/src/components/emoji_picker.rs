//! Emoji picker for reactions and the composer: search, recently used first, keyboard navigation.

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::areas::{self, Place, lazy_area};
use crate::components::sheet::{PopoverBackdrop, Sheet};
use crate::emoji_catalog::{search, shortcode};
use crate::i18n::use_i18n;
use crate::motion::PopoverSide;
use crate::state::use_app_state;

/// Emoji per row of the grid; ↑/↓ move by this many.
const COLUMNS: usize = 8;
/// The picker's size in rem (`.emoji-picker`, style/main/550-emoji-picker.css): it grows with the
/// text size.
const PICKER_WIDTH_REM: f64 = 20.25;
const PICKER_HEIGHT_REM: f64 = 18.75;
/// Space kept between the picker and the window edge.
const EDGE_PX: f64 = 8.0;

/// A floating emoji picker next to `anchor` (the button that opened it). `on_pick` gets the chosen
/// emoji, `on_close` runs on Escape or a click outside; neither closes the picker by itself.
///
/// It loads the first time it opens (`areas::emoji_picker`).
#[component]
pub fn EmojiPicker(
  anchor: web_sys::Element,
  on_pick: Callback<String>,
  on_close: Callback<()>,
  /// Give the keyboard focus back to `anchor` when the picker goes away (the composer focuses its
  /// input itself instead).
  #[prop(default = true)]
  returns_focus: bool,
) -> impl IntoView {
  lazy_area(Place::Popover, move || {
    areas::emoji_picker(anchor.clone(), on_pick, on_close, returns_focus)
  })
}

/// The picker of [`EmojiPicker`].
#[component]
pub fn EmojiPickerPanel(
  anchor: web_sys::Element,
  on_pick: Callback<String>,
  on_close: Callback<()>,
  returns_focus: bool,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let query = RwSignal::new(String::new());
  let active = RwSignal::new(0_usize);
  let input = NodeRef::<html::Input>::new();
  let picker = NodeRef::<html::Div>::new();
  let (top, left, side) = placement(&anchor);
  if returns_focus {
    let anchor = StoredValue::new_local(anchor.clone());
    on_cleanup(move || {
      anchor.try_with_value(|anchor| {
        if let Some(element) = anchor.dyn_ref::<web_sys::HtmlElement>() {
          drop(element.focus());
        }
      });
    });
  }

  let items = Memo::new(move |_| {
    let query = query.get();
    let found = search(&query);
    if !query.trim().is_empty() {
      return found.into_iter().map(str::to_owned).collect::<Vec<_>>();
    }
    let mut list = state.settings.recent_emoji.get();
    list.extend(
      found
        .into_iter()
        .filter(|emoji| !list.iter().any(|recent| recent == emoji))
        .map(str::to_owned)
        .collect::<Vec<_>>(),
    );
    list
  });
  Effect::new(move |_| {
    query.track();
    active.set(0);
  });
  // On a phone the sheet opens without the on-screen keyboard; the search is a tap away.
  let is_sheet = state.layout.get_untracked().is_compact();
  Effect::new(move |_| {
    if let Some(element) = input.get()
      && !is_sheet
    {
      drop(element.focus());
    }
  });

  let pick = move |emoji: String| {
    state.settings.remember_emoji(&emoji);
    on_pick.run(emoji);
  };
  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    let count = items.with_untracked(Vec::len);
    let at = active.get_untracked();
    let maybe_next = match ev.key().as_str() {
      "ArrowRight" => Some((at + 1).min(count.saturating_sub(1))),
      "ArrowLeft" => Some(at.saturating_sub(1)),
      "ArrowDown" => Some((at + COLUMNS).min(count.saturating_sub(1))),
      "ArrowUp" => Some(at.saturating_sub(COLUMNS)),
      "Enter" => {
        ev.prevent_default();
        if let Some(emoji) = items.with_untracked(|list| list.get(at).cloned()) {
          pick(emoji);
        }
        None
      },
      "Escape" => {
        ev.prevent_default();
        ev.stop_propagation();
        on_close.run(());
        None
      },
      _ => None,
    };
    if let Some(next) = maybe_next {
      ev.prevent_default();
      active.set(next);
    }
  };

  let content = view! {
      <input
        class="input emoji-search"
        type="text"
        role="combobox"
        aria-expanded="true"
        aria-controls="emoji-grid"
        aria-activedescendant=move || format!("emoji-opt-{}", active.get())
        aria-label=move || i18n.t("emoji-search")
        placeholder=move || i18n.t("emoji-search")
        autocomplete="off"
        spellcheck="false"
        node_ref=input
        prop:value=move || query.get()
        on:input=move |ev| query.set(event_target_value(&ev))
        on:keydown=on_keydown
      />
      <Show when=move || query.with(|query| query.trim().is_empty()) && !state.settings.recent_emoji.with(Vec::is_empty) fallback=|| ()>
        <p class="emoji-hint">{move || i18n.t("emoji-recent")}</p>
      </Show>
      <ul id="emoji-grid" class="emoji-grid" role="listbox" aria-label=move || i18n.t("emoji-picker-title")>
        {move || {
          let list = items.get();
          if list.is_empty() {
            return view! { <li class="emoji-empty">{move || i18n.t("emoji-none")}</li> }.into_any();
          }
          list
            .into_iter()
            .enumerate()
            .map(|(index, emoji)| {
              let label = shortcode(&emoji).map_or_else(|| emoji.clone(), |code| format!(":{code}:"));
              let chosen = emoji.clone();
              view! {
                <li
                  id=format!("emoji-opt-{index}")
                  role="option"
                  title=label.clone()
                  aria-label=label
                  aria-selected=move || (active.get() == index).to_string()
                  class:active=move || active.get() == index
                  on:mousemove=move |_| active.set(index)
                  on:mousedown=move |ev| ev.prevent_default()
                  on:click=move |_| pick(chosen.clone())
                >
                  {emoji}
                </li>
              }
            })
            .collect_view()
            .into_any()
        }}
      </ul>
  };
  if is_sheet {
    return view! {
      <Sheet class="emoji-sheet" label=Signal::derive(move || i18n.t("emoji-picker-title")) on_close=on_close>
        {content}
      </Sheet>
    }
    .into_any();
  }
  view! {
    <PopoverBackdrop on_close=on_close popover=picker />
    <div
      class=format!("emoji-picker {}", side.class())
      node_ref=picker
      role="dialog"
      aria-label=move || i18n.t("emoji-picker-title")
      style:top=format!("{top}px")
      style:left=format!("{left}px")
    >
      {content}
    </div>
  }
  .into_any()
}

/// Fixed position (top, left) that keeps the picker inside the window: above the anchor when there is
/// room, otherwise below it; right-aligned with it.
fn placement(anchor: &web_sys::Element) -> (f64, f64, PopoverSide) {
  let rect = anchor.get_bounding_client_rect();
  let rem = crate::viewport::rem_px();
  let (picker_width, picker_height) = (PICKER_WIDTH_REM * rem, PICKER_HEIGHT_REM * rem);
  let (width, height) = web_sys::window()
    .map(|window| {
      let read = |value: Result<wasm_bindgen::JsValue, wasm_bindgen::JsValue>| {
        value.ok().and_then(|value| value.as_f64()).unwrap_or(0.0)
      };
      (read(window.inner_width()), read(window.inner_height()))
    })
    .unwrap_or((1024.0, 768.0));
  let (top, side) = if rect.top() - picker_height - EDGE_PX >= 0.0 {
    (rect.top() - picker_height - 4.0, PopoverSide::Above)
  } else {
    (
      (rect.bottom() + 4.0).min(height - picker_height - EDGE_PX),
      PopoverSide::Below,
    )
  };
  let left = (rect.right() - picker_width).clamp(EDGE_PX, (width - picker_width - EDGE_PX).max(EDGE_PX));
  (top.max(EDGE_PX), left, side)
}
