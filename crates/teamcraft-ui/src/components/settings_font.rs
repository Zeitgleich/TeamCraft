//! Settings > Appearance > Font (issue #1494): a menu of the interface fonts, each shown in its own
//! face once the menu opens. The choice applies at once (fonts.rs sets `--font-ui`) and follows the
//! account (`display.font`, settings_sync.rs).
//!
//! A listbox rather than a `<select>`: a native select can't show its options in their own fonts on
//! every platform. It follows the ARIA listbox pattern: the button opens it, the arrow keys, Home
//! and End move, Enter or Space choose, Escape closes, focus leaving closes.

use leptos::ev;
use leptos::html;
use leptos::prelude::*;
use teamcraft_client_api::font::{FontGroup, UiFont};
use wasm_bindgen::JsCast;

use crate::components::icons::{Icon, IconName};
use crate::components::settings_rows::{SettingRow, row_id};
use crate::fonts::{font_stack, register_all};
use crate::i18n::use_i18n;
use crate::state::use_app_state;

const LABEL: &str = "settings-font";

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
  let base = row_id(LABEL);
  let button_id = format!("{base}-button");
  let listbox_id = format!("{base}-listbox");
  let label_id = format!("{base}-label");

  let open = move || {
    // The previews: every shipped font's faces, fetched as their names show.
    register_all();
    let current = chosen.with_untracked(index_of);
    active.set(current);
    is_open.set(true);
    request_animation_frame(move || {
      // The settings may have closed within the frame.
      if let Some(list) = listbox.try_get_untracked().flatten() {
        drop(list.focus());
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
      "Tab" => close(false),
      _ => return,
    }
    ev.prevent_default();
  };
  // Focus going anywhere outside (a click elsewhere, Tab) closes the menu.
  let on_focus_out = move |ev: ev::FocusEvent| {
    let is_inside = ev
      .related_target()
      .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
      .zip(wrapper.get_untracked())
      .is_some_and(|(node, wrapper)| wrapper.contains(Some(&node)));
    if !is_inside {
      is_open.set(false);
    }
  };
  let name_of = move |font: &UiFont| match font.display_name() {
    Some(name) => name.to_owned(),
    None => i18n.t("font-system"),
  };
  let button_labels = format!("{label_id} {button_id}");

  view! {
    <div class="set-font" node_ref=wrapper on:focusout=on_focus_out>
      <button
        type="button"
        class="input set-select set-font-button"
        id=button_id
        node_ref=button
        aria-haspopup="listbox"
        aria-controls=listbox_id.clone()
        aria-expanded=move || is_open.get().to_string()
        aria-labelledby=button_labels
        style:font-family=move || chosen.with(font_stack)
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
      <Show when=move || is_open.get() fallback=|| ()>
        <div
          class="popover set-font-menu"
          id=listbox_id.clone()
          node_ref=listbox
          role="listbox"
          tabindex="-1"
          aria-labelledby=label_id.clone()
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
                      let stack = font_stack(&font);
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
      </Show>
    </div>
  }
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
