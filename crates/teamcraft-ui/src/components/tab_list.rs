//! Keyboard handling shared by every `role="tablist"` (WAI-ARIA tabs pattern, automatic
//! activation): the selected tab is the only one in the tab order (roving `tabindex`), and the
//! arrow keys, Home and End select and focus another one.

use leptos::prelude::*;
use wasm_bindgen::JsCast;

/// Handles a key press on a tab list whose tabs are `tabs`, in order; `tab_id` names the element
/// ID of a tab's button. Keys other than the arrows, Home and End are left alone.
pub fn on_tab_keys<T>(ev: &web_sys::KeyboardEvent, tabs: &[T], tab: RwSignal<T>, tab_id: impl Fn(T) -> String)
where
  T: Copy + PartialEq + Send + Sync + 'static,
{
  let position = tabs
    .iter()
    .position(|candidate| *candidate == tab.get_untracked())
    .unwrap_or(0);
  let Some(next) = next_tab_index(&ev.key(), position, tabs.len()) else {
    return;
  };
  ev.prevent_default();
  tab.set(tabs[next]);
  focus_tab(&tab_id(tabs[next]));
}

/// Handles a key press on a `role="radiogroup"` of `role="radio"` buttons (a device picker): the
/// arrows, Home and End move the focus between the radios (roving `tabindex`) without choosing
/// one, so passing over a camera doesn't switch it; Enter and Space choose, as on any button.
pub fn on_radio_keys(ev: &web_sys::KeyboardEvent) {
  let Some(group) = ev
    .current_target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
  else {
    return;
  };
  let Some(current) = ev
    .target()
    .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
    .and_then(|target| target.closest("[role='radio']").ok().flatten())
  else {
    return;
  };
  let Ok(found) = group.query_selector_all("[role='radio']") else {
    return;
  };
  let radios: Vec<web_sys::HtmlElement> = (0..found.length())
    .filter_map(|index| found.item(index))
    .filter_map(|node| node.dyn_into().ok())
    .collect();
  let Some(position) = radios.iter().position(|radio| radio.is_same_node(Some(&current))) else {
    return;
  };
  let Some(next) = next_tab_index(&ev.key(), position, radios.len()) else {
    return;
  };
  ev.prevent_default();
  for radio in &radios {
    // Only the focused radio stays in the tab order, like the checked one did.
    if radio.set_attribute("tabindex", "-1").is_err() {
      tracing::debug!("could not update a radio's tab stop");
    }
  }
  if radios[next].set_attribute("tabindex", "0").is_err() {
    tracing::debug!("could not update a radio's tab stop");
  }
  drop(radios[next].focus());
}

/// The `tabindex` of a tab: only the selected one is reached with Tab.
pub fn roving_tabindex(is_selected: bool) -> &'static str {
  if is_selected { "0" } else { "-1" }
}

/// The tab a key moves to from `position` among `count` tabs; wraps around at both ends.
fn next_tab_index(key: &str, position: usize, count: usize) -> Option<usize> {
  if count == 0 {
    return None;
  }
  match key {
    "ArrowDown" | "ArrowRight" => Some((position + 1) % count),
    "ArrowUp" | "ArrowLeft" => Some((position + count - 1) % count),
    "Home" => Some(0),
    "End" => Some(count - 1),
    _ => None,
  }
}

fn focus_tab(id: &str) {
  if let Some(button) = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.get_element_by_id(id))
    .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
  {
    // A tab that can't take focus stays selected; there is nothing else to do.
    drop(button.focus());
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn arrows_move_and_wrap() {
    assert_eq!(next_tab_index("ArrowRight", 0, 3), Some(1));
    assert_eq!(next_tab_index("ArrowDown", 2, 3), Some(0));
    assert_eq!(next_tab_index("ArrowLeft", 0, 3), Some(2));
    assert_eq!(next_tab_index("ArrowUp", 1, 3), Some(0));
  }

  #[test]
  fn home_and_end_jump_to_the_ends() {
    assert_eq!(next_tab_index("Home", 2, 4), Some(0));
    assert_eq!(next_tab_index("End", 0, 4), Some(3));
  }

  #[test]
  fn other_keys_and_empty_lists_do_nothing() {
    assert_eq!(next_tab_index("Enter", 0, 3), None);
    assert_eq!(next_tab_index("ArrowRight", 0, 0), None);
  }
}
