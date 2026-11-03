//! Date pills in the feed and the menu they open (issue #300): jump to today, yesterday, last week
//! or a chosen day. The pills are the day dividers between messages and the day that floats over
//! the feed while reading older messages.

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::areas::{self, Place, lazy_area};
use crate::clock::{local_day_start, now_ms, parse_local_datetime};
use crate::components::sheet::PopoverBackdrop;
use crate::i18n::use_i18n;
use crate::model::ConversationId;
use crate::state::use_app_state;

/// Width and height the menu needs before it opens the other way.
const MENU_WIDTH_PX: f64 = 232.0;
const MENU_HEIGHT_PX: f64 = 200.0;
const EDGE_PX: f64 = 8.0;
/// Length of `YYYY-MM-DD`, the date part of a local date-time input value.
const DATE_INPUT_LEN: usize = 10;
const DAYS_IN_WEEK: i64 = 7;

/// Where the open menu is anchored (the pill's lower left corner), provided by the feed.
#[derive(Clone, Copy)]
pub struct DateJumpMenu(pub RwSignal<Option<(f64, f64)>>);

/// A day label that opens the jump menu; plain text where no feed provides the menu (threads).
#[component]
pub fn DatePill(label: String, #[prop(optional)] class: &'static str) -> impl IntoView {
  let i18n = use_i18n();
  let Some(menu) = use_context::<DateJumpMenu>() else {
    return view! { <span class=class>{label}</span> }.into_any();
  };
  let aria_date = label.clone();
  view! {
    <button
      type="button"
      class=format!("date-pill {class}")
      aria-haspopup="menu"
      aria-label=move || i18n.t_with("date-jump-open", &[("date", &aria_date)])
      on:click=move |ev| {
        if let Some(button) = ev
          .current_target()
          .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        {
          let rect = button.get_bounding_client_rect();
          menu.0.set(Some((rect.left(), rect.bottom() + 4.0)));
        }
      }
    >
      {label.clone()}
    </button>
  }
  .into_any()
}

/// The menu itself, open while the feed's anchor is set. It loads the first time it opens
/// (`areas::date_jump`).
#[component]
pub fn DateJumpPopover(conversation: ConversationId, menu: DateJumpMenu) -> impl IntoView {
  let is_open = Memo::new(move |_| menu.0.with(Option::is_some));
  move || {
    is_open
      .get()
      .then(|| lazy_area(Place::Popover, move || areas::date_jump(conversation, menu)))
  }
}

/// The menu of [`DateJumpPopover`].
#[component]
pub fn DateJumpPanel(conversation: ConversationId, menu: DateJumpMenu) -> impl IntoView {
  let i18n = use_i18n();
  let state = use_app_state();
  let node = NodeRef::<html::Div>::new();
  let close = move || menu.0.set(None);
  let latest_day = StoredValue::new(
    crate::clock::format_local_datetime_input(now_ms())
      .chars()
      .take(DATE_INPUT_LEN)
      .collect::<String>(),
  );
  let go = move |days_back: i64| {
    jump_to_day(state, conversation, local_day_start(now_ms(), days_back));
    close();
  };
  Effect::new(move |_| {
    if let Some(element) = node.get()
      && let Some(first) = element
        .query_selector("[role='menuitem']")
        .ok()
        .flatten()
        .and_then(|item| item.dyn_into::<web_sys::HtmlElement>().ok())
    {
      drop(first.focus());
    }
  });
  let on_keydown = move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
    "Escape" | "Tab" => {
      ev.prevent_default();
      close();
    },
    "ArrowDown" | "ArrowUp" => {
      ev.prevent_default();
      move_focus(node, ev.key() == "ArrowDown");
    },
    _ => {},
  };
  view! {
    <Show when=move || menu.0.get().is_some() fallback=|| ()>
      {move || {
        let (x, y) = placed(menu.0.get().unwrap_or_default());
        let latest_day = latest_day.get_value();
        view! {
          <PopoverBackdrop on_close=Callback::new(move |()| close()) popover=node />
          <div
            class="popover date-jump-menu"
            role="menu"
            aria-label=move || i18n.t("date-jump-label")
            node_ref=node
            style:--date-jump-x=format!("{x}px")
            style:--date-jump-y=format!("{y}px")
            on:keydown=on_keydown
          >
            <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| go(0)>
              <span class="menu-label">{move || i18n.t("date-jump-today")}</span>
            </button>
            <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| go(1)>
              <span class="menu-label">{move || i18n.t("date-jump-yesterday")}</span>
            </button>
            <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| go(DAYS_IN_WEEK)>
              <span class="menu-label">{move || i18n.t("date-jump-last-week")}</span>
            </button>
            <label class="date-jump-pick">
              <span>{move || i18n.t("date-jump-pick")}</span>
              <input
                type="date"
                class="input"
                max=latest_day
                on:change=move |ev| {
                  if let Some(day) = parse_local_datetime(&format!("{}T00:00", event_target_value(&ev))) {
                    jump_to_day(state, conversation, day);
                    close();
                  }
                }
              />
            </label>
          </div>
        }
      }}
    </Show>
  }
}

/// Scrolls the feed to the first message of the day starting at `day_start`, or to the newest
/// message when nothing was written since.
fn jump_to_day(state: crate::state::AppState, conversation: ConversationId, day_start: i64) {
  let count = state.read(|backend| backend.message_count(conversation));
  let index = first_index_at_or_after(count, day_start, |index| {
    state
      .read(|backend| backend.messages(conversation, index, 1))
      .first()
      .map(|message| message.sent_at)
  });
  if index >= count {
    state.jump_to_latest.update(|jumps| *jumps += 1);
  } else {
    state.jump_to_index.set(Some((conversation, index)));
  }
}

/// The first index in `0..count` whose time is at or after `target`, found by bisection over the
/// time-ordered messages (log time, not a scan of the conversation). A message that is not loaded
/// counts as older, so the search lands on the nearest loaded one.
fn first_index_at_or_after(count: usize, target: i64, sent_at: impl Fn(usize) -> Option<i64>) -> usize {
  let (mut low, mut high) = (0, count);
  while low < high {
    let middle = low + (high - low) / 2;
    if sent_at(middle).is_some_and(|time| time >= target) {
      high = middle;
    } else {
      low = middle + 1;
    }
  }
  low
}

fn placed((x, y): (f64, f64)) -> (f64, f64) {
  let (width, height) = web_sys::window()
    .map(|window| {
      let size = |value: Result<wasm_bindgen::JsValue, _>| value.ok().and_then(|v| v.as_f64()).unwrap_or(0.0);
      (size(window.inner_width()), size(window.inner_height()))
    })
    .unwrap_or_default();
  let fit = |at: f64, extent: f64, menu: f64| at.min(extent - menu - EDGE_PX).max(EDGE_PX);
  (fit(x, width, MENU_WIDTH_PX), fit(y, height, MENU_HEIGHT_PX))
}

fn move_focus(menu: NodeRef<html::Div>, is_forward: bool) {
  let Some(element) = menu.get_untracked() else {
    return;
  };
  let Ok(items) = element.query_selector_all("[role='menuitem']") else {
    return;
  };
  let Some(document) = web_sys::window().and_then(|window| window.document()) else {
    return;
  };
  let active = document.active_element();
  let nodes: Vec<web_sys::HtmlElement> = (0..items.length())
    .filter_map(|index| items.item(index))
    .filter_map(|item| item.dyn_into::<web_sys::HtmlElement>().ok())
    .collect();
  let current = nodes
    .iter()
    .position(|item| active.as_ref().is_some_and(|active| active == item.as_ref()));
  let next = match (current, is_forward) {
    (None, true) => 0,
    (None, false) => nodes.len().saturating_sub(1),
    (Some(at), true) => (at + 1) % nodes.len().max(1),
    (Some(at), false) => (at + nodes.len().max(1) - 1) % nodes.len().max(1),
  };
  if let Some(item) = nodes.get(next) {
    drop(item.focus());
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn finds_the_first_message_of_the_day() {
    let times = [10, 20, 30, 40, 50];
    let at = |target| first_index_at_or_after(times.len(), target, |index| times.get(index).copied());
    assert_eq!(at(0), 0);
    assert_eq!(at(30), 2);
    assert_eq!(at(31), 3);
    assert_eq!(at(51), 5);
  }

  #[test]
  fn an_empty_feed_has_nothing_to_jump_to() {
    assert_eq!(first_index_at_or_after(0, 5, |_| None), 0);
  }

  #[test]
  fn unloaded_messages_count_as_older() {
    // Only the newest are loaded: the search lands on the first loaded one at or after the day.
    let at = |index: usize| (index >= 3).then_some(index as i64 * 10);
    assert_eq!(first_index_at_or_after(5, 25, at), 3);
  }
}
