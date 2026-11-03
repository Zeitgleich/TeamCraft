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
