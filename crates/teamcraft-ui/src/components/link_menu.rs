//! The menu of a right click on a link in the desktop app (#548): open in the browser, copy the
//! address. It replaces the webview's own menu, which offers new tabs and downloads an app has no
//! use for.

use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::components::message_actions::copy_to_clipboard;
use crate::components::sheet::PopoverBackdrop;
use crate::external_links::{self, LinkMenuTarget};
use crate::i18n::use_i18n;

/// Room the menu needs below and to the right of the pointer before it opens the other way.
const MENU_WIDTH_PX: i32 = 232;
const MENU_HEIGHT_PX: i32 = 96;
const EDGE_PX: i32 = 8;

/// Shows the menu while a link was right-clicked ([`external_links::start`] fills the context).
#[component]
pub fn LinkMenu() -> impl IntoView {
  let i18n = use_i18n();
  let Some(menu) = use_context::<RwSignal<Option<LinkMenuTarget>>>() else {
    return ().into_any();
  };
  let node = NodeRef::<html::Div>::new();
  let close = move || menu.set(None);
  Effect::new(move |_| focus_first_item(node));
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
    <Show when=move || menu.get().is_some() fallback=|| ()>
      {move || {
        let target = menu.get().unwrap_or_else(|| LinkMenuTarget { url: String::new(), x: 0, y: 0 });
        let (x, y) = placed(target.x, target.y);
        let open_url = target.url.clone();
        let copy_url = target.url;
        view! {
          <PopoverBackdrop on_close=Callback::new(move |()| close()) popover=node />
          <div
            class="popover link-menu"
            role="menu"
            aria-label=move || i18n.t("link-menu-label")
            node_ref=node
            style:--link-menu-x=format!("{x}px")
            style:--link-menu-y=format!("{y}px")
            on:keydown=on_keydown
          >
            <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| {
              external_links::open(&open_url);
              close();
            }>
              <span class="menu-label">{move || i18n.t("link-menu-open")}</span>
            </button>
            <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| {
              copy_to_clipboard(&copy_url);
              close();
            }>
              <span class="menu-label">{move || i18n.t("action-copy-link")}</span>
            </button>
          </div>
        }
      }}
    </Show>
  }
  .into_any()
}

/// Focuses the menu's first entry once it is shown, so the keyboard can go on from there.
pub fn focus_first_item(menu: NodeRef<html::Div>) {
  if let Some(element) = menu.get()
    && let Some(first) = element
      .query_selector("[role='menuitem']")
      .ok()
      .flatten()
      .and_then(|item| item.dyn_into::<web_sys::HtmlElement>().ok())
  {
    drop(first.focus());
  }
}

/// Where the menu's corner goes: at the pointer, moved back inside the window near its edges.
pub fn placed(x: i32, y: i32) -> (i32, i32) {
  placed_with(x, y, MENU_HEIGHT_PX)
}

/// [`placed`] for a menu of `height_px`.
pub fn placed_with(x: i32, y: i32, height_px: i32) -> (i32, i32) {
  let (width, height) = web_sys::window()
    .map(|window| {
      let size =
        |value: Result<wasm_bindgen::JsValue, _>| value.ok().and_then(|v| v.as_f64()).unwrap_or(0.0) as i32;
      (size(window.inner_width()), size(window.inner_height()))
    })
    .unwrap_or_default();
  clamp_into(x, y, width, height, height_px)
}

fn clamp_into(x: i32, y: i32, width: i32, height: i32, menu_height: i32) -> (i32, i32) {
  let fit = |at: i32, extent: i32, menu: i32| {
    // A window too small for the menu keeps it at the start edge.
    at.min(extent - menu - EDGE_PX).max(EDGE_PX)
  };
  (fit(x, width, MENU_WIDTH_PX), fit(y, height, menu_height))
}

/// Moves the focus to the next (or previous) entry of the menu, round the ends.
pub fn move_focus(menu: NodeRef<html::Div>, is_forward: bool) {
  let Some(element) = menu.get_untracked() else {
    return;
  };
  let Ok(items) = element.query_selector_all("[role='menuitem']") else {
    return;
  };
  let count = items.length() as i32;
  let active = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element());
  let current = (0..count)
    .find(|index| {
      items
        .item(*index as u32)
        .zip(active.as_ref())
        .is_some_and(|(item, active)| item.is_same_node(Some(active)))
    })
    .unwrap_or(-1);
  let next = if is_forward { current + 1 } else { current - 1 }.rem_euclid(count.max(1));
  if let Some(item) = items
    .item(next as u32)
    .and_then(|item| item.dyn_into::<web_sys::HtmlElement>().ok())
  {
    drop(item.focus());
  }
}

#[cfg(test)]
mod tests {
  use super::{EDGE_PX, MENU_HEIGHT_PX, MENU_WIDTH_PX, clamp_into};

  #[test]
  fn menu_stays_inside_the_window() {
    assert_eq!(clamp_into(100, 200, 1000, 800, MENU_HEIGHT_PX), (100, 200));
    assert_eq!(
      clamp_into(990, 790, 1000, 800, MENU_HEIGHT_PX),
      (1000 - MENU_WIDTH_PX - EDGE_PX, 800 - MENU_HEIGHT_PX - EDGE_PX)
    );
    assert_eq!(clamp_into(-5, 0, 1000, 800, MENU_HEIGHT_PX), (EDGE_PX, EDGE_PX));
    // Smaller than the menu: the start edge wins.
    assert_eq!(clamp_into(50, 50, 100, 60, MENU_HEIGHT_PX), (EDGE_PX, EDGE_PX));
    // A taller menu opens upwards sooner.
    assert_eq!(clamp_into(100, 700, 1000, 800, 236), (100, 800 - 236 - EDGE_PX));
  }
}
