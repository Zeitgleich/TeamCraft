//! The sticker part of the composer's picker (issue #2132): the workspace's packs, one at a time
//! behind a row of pack tabs, so a pack's stickers are all the page holds. Picking one sends it.

use leptos::prelude::*;
use teamcraft_proto::{StickerPackKey, StickerRef};

use crate::i18n::use_i18n;
use crate::state::use_app_state;
use crate::stickers::StickerPicture;

/// Stickers per row of the grid; ↑/↓ move by rows.
const COLUMNS: usize = 4;

/// The packs' tabs and the chosen pack's stickers. `on_pick` gets the picked sticker.
#[component]
pub fn StickerGrid(on_pick: Callback<StickerRef>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  crate::stickers::want_packs(state);
  let set = Memo::new(move |_| state.read(|backend| backend.sticker_packs()));
  let maybe_chosen = RwSignal::new(None::<StickerPackKey>);
  let active = RwSignal::new(0_usize);
  let pack = Memo::new(move |_| {
    set.with(|set| {
      maybe_chosen
        .get()
        .and_then(|key| set.pack(key))
        .or_else(|| set.packs.first())
        .cloned()
    })
  });
  let pick = move |index: usize| {
    if let Some(pack) = pack.get_untracked()
      && let Some(sticker) = pack.stickers.get(index)
    {
      on_pick.run(sticker.reference(pack.pack));
    }
  };
  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    let count = pack.with_untracked(|pack| pack.as_ref().map_or(0, |pack| pack.stickers.len()));
    if count == 0 {
      return;
    }
    let at = active.get_untracked().min(count - 1);
    let next = match ev.key().as_str() {
      "ArrowRight" => (at + 1).min(count - 1),
      "ArrowLeft" => at.saturating_sub(1),
      "ArrowDown" => (at + COLUMNS).min(count - 1),
      "ArrowUp" => at.saturating_sub(COLUMNS),
      "Home" => 0,
      "End" => count - 1,
      "Enter" | " " => {
        ev.prevent_default();
        pick(at);
        return;
      },
      _ => return,
    };
    ev.prevent_default();
    active.set(next);
  };
  let tabs = move || {
    set.with(|set| {
      set
        .packs
        .iter()
        .map(|candidate| {
          let key = candidate.pack;
          let name = candidate.name.clone();
          let is_current = move || pack.with(|pack| pack.as_ref().is_some_and(|pack| pack.pack == key));
          view! {
            <button
              type="button"
              role="tab"
              class="sticker-pack-tab"
              class:current=is_current
              aria-selected=move || is_current().to_string()
              on:mousedown=move |ev| ev.prevent_default()
              on:click=move |_| {
                maybe_chosen.set(Some(key));
                active.set(0);
              }
            >
              {name}
            </button>
          }
        })
        .collect_view()
    })
  };
  let grid = move || {
    let Some(pack) = pack.get() else {
      return view! { <p class="sticker-empty muted-text">{move || i18n.t("sticker-none")}</p> }.into_any();
    };
    if pack.stickers.is_empty() {
      return view! { <p class="sticker-empty muted-text">{move || i18n.t("sticker-pack-empty")}</p> }
        .into_any();
    }
    let name = pack.name.clone();
    view! {
      <div
        class="sticker-grid"
        role="listbox"
        tabindex="0"
        aria-label=name
        aria-activedescendant=move || format!("sticker-opt-{}", active.get())
        on:keydown=on_keydown
      >
        {pack
          .stickers
          .into_iter()
          .enumerate()
          .map(|(index, sticker)| {
            view! {
              <button
                type="button"
                id=format!("sticker-opt-{index}")
                class="sticker-cell"
                role="option"
                tabindex="-1"
                aria-label=sticker.alt.clone()
                title=sticker.alt.clone()
                aria-selected=move || (active.get() == index).to_string()
                class:active=move || active.get() == index
                on:mousemove=move |_| active.set(index)
                on:mousedown=move |ev| ev.prevent_default()
                on:click=move |_| pick(index)
              >
                <StickerPicture sha256=sticker.sha256 alt=sticker.alt.clone() class="sticker-thumb" />
              </button>
            }
          })
          .collect_view()}
      </div>
    }
    .into_any()
  };
  view! {
    <div class="sticker-pack-tabs" role="tablist" aria-label=move || i18n.t("sticker-packs")>{tabs}</div>
    <div class="sticker-scroller">{grid}</div>
  }
}
