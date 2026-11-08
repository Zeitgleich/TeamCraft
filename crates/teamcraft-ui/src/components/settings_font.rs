//! Settings > Appearance > Font (issue #1494): a menu of the interface fonts, each shown in its own
//! face once the menu opens. The choice applies at once (fonts.rs sets `--font-ui`) and follows the
//! account (`display.font`, settings_sync.rs).
//!
//! A listbox rather than a `<select>`: a native select can't show its options in their own fonts on
//! every platform. It follows the ARIA listbox pattern: the button opens it, the arrow keys, Home
//! and End move, Enter or Space choose, Escape closes, focus leaving closes.

use leptos::ev;
use leptos::html;
use leptos::portal::Portal;
use leptos::prelude::*;
use teamcraft_client_api::font::{FontGroup, UiFont};
use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;

use crate::components::icons::{Icon, IconName};
use crate::components::settings_rows::SettingRow;
use crate::fonts::{preview_stack, register_all};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

/// The ids of the row's parts: `row_id("settings-font")` with a suffix (a test checks them), as
/// `&'static str` so the menu's children can use them every time they render.
const LABEL_ID: &str = "set-settings-font-label";
const BUTTON_ID: &str = "set-settings-font-button";
const LISTBOX_ID: &str = "set-settings-font-listbox";

/// The row with the font menu.
#[component]
pub fn FontRow() -> impl IntoView {
  view! {
    <SettingRow label="settings-font" hint="settings-font-hint">
      <FontPicker />
    </SettingRow>
  }
}

#[component]
fn FontPicker() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let chosen = Memo::new(move |_| prefs.with(|all| all.display.font.clone()));
  let is_open = RwSignal::new(false);
  let active = RwSignal::new(0_usize);
  let wrapper = NodeRef::<html::Div>::new();
  let button = NodeRef::<html::Button>::new();
  let listbox = NodeRef::<html::Div>::new();
  let placement = RwSignal::new(Placement::default());

  let open = move || {
    // The previews: every shipped font's faces, fetched as their names show.
    register_all();
    let current = chosen.with_untracked(index_of);
    if let Some(button) = button.get_untracked() {
      placement.set(Placement::below_or_above(&button));
    }
    active.set(current);
    is_open.set(true);
    request_animation_frame(move || {
      // The settings may have closed within the frame.
      if let Some(list) = listbox.try_get_untracked().flatten() {
        let options = web_sys::FocusOptions::new();
        options.set_prevent_scroll(true);
        drop(list.focus_with_options(&options));
      }
      scroll_to_option(current);
    });
  };
  let close = move |refocus: bool| {
    is_open.set(false);
    if refocus && let Some(button) = button.get_untracked() {
      drop(button.focus());
    }
  };
  let choose = move |index: usize| {
    if let Some(font) = UiFont::ALL.get(index) {
      let font = font.clone();
      prefs.update(|all| all.display.font = font);
    }
    close(true);
  };
  let move_to = move |index: usize| {
    active.set(index);
    scroll_to_option(index);
  };
  let on_key = move |ev: ev::KeyboardEvent| {
    let last = UiFont::ALL.len() - 1;
    let current = active.get_untracked();
    match ev.key().as_str() {
      "ArrowDown" => move_to((current + 1).min(last)),
      "ArrowUp" => move_to(current.saturating_sub(1)),
      "Home" => move_to(0),
      "End" => move_to(last),
      "Enter" | " " => choose(current),
      "Escape" => close(true),
      // The menu sits at the end of the page: Tab goes back to the button, not past the dialog.
      "Tab" => close(true),
      _ => return,
    }
    ev.prevent_default();
  };
  // Focus going anywhere but the button or the menu (a click elsewhere) closes the menu.
  let on_focus_out = move |ev: ev::FocusEvent| {
    let Some(node) = ev
      .related_target()
      .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
    else {
      is_open.set(false);
      return;
    };
    let is_in = |maybe_root: Option<web_sys::Node>| maybe_root.is_some_and(|root| root.contains(Some(&node)));
    let is_inside =
      is_in(wrapper.get_untracked().map(Into::into)) || is_in(listbox.get_untracked().map(Into::into));
    if !is_inside {
      is_open.set(false);
    }
  };
  // The menu is fixed to the window (a card would clip it): it closes when anything else scrolls
  // or the window changes size, rather than drifting away from its button.
  Effect::new(move |_| {
    if is_open.get() {
      close_on_outside_scroll(is_open, listbox);
    }
  });
  let name_of = move |font: &UiFont| match font.display_name() {
    Some(name) => name.to_owned(),
    None => i18n.t("font-system"),
  };

  view! {
    <div class="set-font" node_ref=wrapper on:focusout=on_focus_out>
      <button
        type="button"
        class="input set-select set-font-button"
        id=BUTTON_ID
        node_ref=button
        aria-haspopup="listbox"
        aria-controls=LISTBOX_ID
        aria-expanded=move || is_open.get().to_string()
        aria-labelledby=format!("{LABEL_ID} {BUTTON_ID}")
        style:font-family=move || chosen.with(preview_stack)
        on:click=move |_| if is_open.get_untracked() { close(false) } else { open() }
        on:keydown=move |ev: ev::KeyboardEvent| {
          if matches!(ev.key().as_str(), "ArrowDown" | "ArrowUp") && !is_open.get_untracked() {
            ev.prevent_default();
            open();
          }
        }
      >
        <span class="set-font-current">{move || chosen.with(|font| name_of(font))}</span>
        <span class="set-font-chevron" aria-hidden="true"><Icon name=IconName::ChevronDown /></span>
      </button>
      // In the body: the settings' scroller and its container query would clip and offset a
      // fixed menu inside them.
      <Show when=move || is_open.get() fallback=|| ()>
        <Portal>
        <div
          class="popover set-font-menu"
          on:focusout=on_focus_out
          class:opens-up=move || placement.with(|place| place.is_up)
          style:top=move || placement.with(|place| place.top.clone())
          style:bottom=move || placement.with(|place| place.bottom.clone())
          style:right=move || placement.with(|place| place.right.clone())
          style:width=move || placement.with(|place| place.width.clone())
          style:max-height=move || placement.with(|place| place.max_height.clone())
          id=LISTBOX_ID
          node_ref=listbox
          role="listbox"
          tabindex="-1"
          aria-labelledby=LABEL_ID
          aria-activedescendant=move || option_id(active.get())
          on:keydown=on_key
          // Choosing with the pointer must not move the focus out first, which would close it.
          on:mousedown=|ev: ev::MouseEvent| ev.prevent_default()
        >
          {FontGroup::ALL
            .into_iter()
            .map(|group| {
              let heading_id = format!("set-font-group-{}", group.slug());
              let labelled_by = heading_id.clone();
              let heading_key = format!("font-group-{}", group.slug());
              view! {
                <div class="set-font-group" role="group" aria-labelledby=labelled_by>
                  <div class="set-font-group-title" id=heading_id role="presentation">
                    {move || i18n.t(&heading_key)}
                  </div>
                  {UiFont::ALL
                    .iter()
                    .enumerate()
                    .filter(|(_, font)| font.group() == group)
                    .map(|(index, font)| {
                      let font = font.clone();
                      let stack = preview_stack(&font);
                      let is_selected = {
                        let font = font.clone();
                        Memo::new(move |_| chosen.with(|current| *current == font))
                      };
                      view! {
                        <div
                          class="set-font-option"
                          id=option_id(index)
                          role="option"
                          aria-selected=move || is_selected.get().to_string()
                          class:is-active=move || active.get() == index
                          style:font-family=stack
                          on:pointermove=move |_| active.set(index)
                          on:click=move |_| choose(index)
                        >
                          <span class="set-font-option-name">{name_of(&font)}</span>
                          <span class="set-font-check" aria-hidden="true">
                            <Show when=move || is_selected.get() fallback=|| ()>
                              <Icon name=IconName::Check />
                            </Show>
                          </span>
                        </div>
                      }
                    })
                    .collect_view()}
                </div>
              }
            })
            .collect_view()}
        </div>
        </Portal>
      </Show>
    </div>
  }
}

/// Where the fixed menu sits: under its button, or above it when there is more room there. Lengths
/// in CSS pixels; an empty one leaves the stylesheet's.
#[derive(Clone, Debug, Default, PartialEq)]
struct Placement {
  top: String,
  bottom: String,
  right: String,
  width: String,
  max_height: String,
  is_up: bool,
}

impl Placement {
  /// The room the menu wants before it opens upwards instead.
  const PREFERRED_HEIGHT: f64 = 320.0;
  /// The gap to the button and to the window's edges.
  const GAP: f64 = 4.0;
  const EDGE: f64 = 12.0;
  /// As in the stylesheet (18.125rem at the default size).
  const MIN_WIDTH: f64 = 290.0;

  fn below_or_above(button: &web_sys::HtmlButtonElement) -> Self {
    let rect = button.get_bounding_client_rect();
    let Some(window) = web_sys::window() else {
      return Self::default();
    };
    let viewport_width = window
      .inner_width()
      .ok()
      .and_then(|value| value.as_f64())
      .unwrap_or(0.0);
    let viewport_height = window
      .inner_height()
      .ok()
      .and_then(|value| value.as_f64())
      .unwrap_or(0.0);
    let below = viewport_height - rect.bottom() - Self::GAP - Self::EDGE;
    let above = rect.top() - Self::GAP - Self::EDGE;
    let is_up = below < Self::PREFERRED_HEIGHT && above > below;
    let px = |value: f64| format!("{}px", value.max(0.0).round());
    Self {
      top: if is_up {
        String::new()
      } else {
        px(rect.bottom() + Self::GAP)
      },
      bottom: if is_up {
        px(viewport_height - rect.top() + Self::GAP)
      } else {
        String::new()
      },
      right: px(viewport_width - rect.right()),
      // At least as wide as the button, wider where there is room to its left.
      width: px(rect.width().max(Self::MIN_WIDTH.min(rect.right() - Self::EDGE))),
      max_height: px(if is_up { above } else { below }),
      is_up,
    }
  }
}

/// Closes the menu on a scroll outside it or a resize of the window, until it closes.
fn close_on_outside_scroll(is_open: RwSignal<bool>, listbox: NodeRef<html::Div>) {
  let Some(window) = web_sys::window() else {
    return;
  };
  let on_event = Closure::<dyn FnMut(web_sys::Event)>::new(move |event: web_sys::Event| {
    let is_inside = event
      .target()
      .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
      .zip(listbox.try_get_untracked().flatten())
      .is_some_and(|(node, list)| list.contains(Some(&node)));
    if !is_inside {
      is_open.try_set(false);
    }
  });
  let options = web_sys::AddEventListenerOptions::new();
  options.set_capture(true);
  options.set_passive(true);
  let callback = on_event.as_ref().unchecked_ref::<js_sys::Function>().clone();
  for name in ["scroll", "resize"] {
    drop(window.add_event_listener_with_callback_and_add_event_listener_options(name, &callback, &options));
  }
  // Kept until the menu closes; the cleanup needs a handle it may send, the listener stays local.
  let listener = StoredValue::new_local(Some((callback, on_event)));
  on_cleanup(move || {
    let Some((callback, _on_event)) = listener.try_update_value(Option::take).flatten() else {
      return;
    };
    let Some(window) = web_sys::window() else {
      return;
    };
    for name in ["scroll", "resize"] {
      drop(window.remove_event_listener_with_callback_and_bool(name, &callback, true));
    }
  });
}

fn index_of(font: &UiFont) -> usize {
  UiFont::ALL.iter().position(|known| known == font).unwrap_or(0)
}

fn option_id(index: usize) -> String {
  format!("set-font-option-{index}")
}

/// Keeps the option the keys moved to in view, without scrolling the page.
fn scroll_to_option(index: usize) {
  let Some(option) = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|doc| doc.get_element_by_id(&option_id(index)))
  else {
    return;
  };
  let options = web_sys::ScrollIntoViewOptions::new();
  options.set_block(web_sys::ScrollLogicalPosition::Nearest);
  option.scroll_into_view_with_scroll_into_view_options(&options);
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::components::settings_rows::row_id;

  #[test]
  fn ids_follow_the_row() {
    let base = row_id("settings-font");
    assert_eq!(LABEL_ID, format!("{base}-label"));
    assert_eq!(BUTTON_ID, format!("{base}-button"));
    assert_eq!(LISTBOX_ID, format!("{base}-listbox"));
  }
}
