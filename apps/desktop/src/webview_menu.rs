//! The webview's own context menu (#719). The page cancels it everywhere except on text (a text
//! field, selected text), where the app has nothing better: there the menu keeps what edits and
//! checks text, and nothing that treats the app as a web page (Back, Forward, Stop, Reload, open
//! or download a link, image or frame). WebKitGTK puts those into the text menus too, so on Linux
//! the shell filters every menu before it shows; macOS and Windows show none of them on text.
//!
//! `TEAMCRAFT_DEVTOOLS=1` keeps the webview's full menu (with "Inspect Element" where the build has
//! the inspector), in every build; debug builds keep "Inspect Element" in the filtered menu.

/// Set to `1` to keep the webview's own menu everywhere.
pub const DEVTOOLS_ENV: &str = "TEAMCRAFT_DEVTOOLS";
/// Tells the page to leave every context menu to the webview (devtools).
pub const NATIVE_MENU_SCRIPT: &str = "window.__TEAMCRAFT_NATIVE_MENU__ = true;";

/// Whether the developer asked for the webview's own menu.
pub fn is_devtools_requested() -> bool {
  std::env::var(DEVTOOLS_ENV).is_ok_and(|value| value == "1")
}

/// What a menu entry does, as far as the filter cares (Linux; the other webviews need no filter).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuEntry {
  /// Cut, copy, paste, delete, select all, input methods, Unicode characters.
  Editing,
  /// A spelling or grammar suggestion, or "Ignore"/"Learn" spelling.
  Spelling,
  /// "Inspect Element".
  Inspect,
  Separator,
  /// Anything else: navigation, links, images, media, frames, fonts.
  Other,
}

/// Which entries of a menu stay, in order: editing and spelling entries, "Inspect Element" when
/// `keeps_inspect`, and separators only between two kept groups.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn kept_entries(entries: &[MenuEntry], keeps_inspect: bool) -> Vec<bool> {
  let is_kept = |entry: MenuEntry| match entry {
    MenuEntry::Editing | MenuEntry::Spelling => true,
    MenuEntry::Inspect => keeps_inspect,
    MenuEntry::Separator | MenuEntry::Other => false,
  };
  let mut kept: Vec<bool> = entries.iter().map(|entry| is_kept(*entry)).collect();
  let mut has_item_before = false;
  for (index, entry) in entries.iter().enumerate() {
    if *entry == MenuEntry::Separator {
      let has_item_after = entries[index + 1..]
        .iter()
        .zip(&kept[index + 1..])
        .take_while(|(entry, _)| **entry != MenuEntry::Separator)
        .any(|(_, is_kept)| *is_kept);
      kept[index] = has_item_before && has_item_after;
      if kept[index] {
        has_item_before = false;
      }
    } else if kept[index] {
      has_item_before = true;
    }
  }
  kept
}

/// Filters the WebKitGTK menus as the module says; with devtools requested, leaves them alone.
#[cfg(target_os = "linux")]
pub fn filter_menus(webview: &webkit2gtk::WebView) {
  use webkit2gtk::{ContextMenuExt, ContextMenuItemExt, WebViewExt};

  if is_devtools_requested() {
    return;
  }
  let keeps_inspect = cfg!(debug_assertions);
  webview.connect_context_menu(move |_, menu, _, _| {
    let items = menu.items();
    let entries: Vec<MenuEntry> = items
      .iter()
      .map(|item| match item.is_separator() {
        true => MenuEntry::Separator,
        false => entry_of(item.stock_action()),
      })
      .collect();
    for (item, is_kept) in items.iter().zip(kept_entries(&entries, keeps_inspect)) {
      if !is_kept {
        menu.remove(item);
      }
    }
    // `true` shows nothing: a menu with no entry left.
    menu.n_items() == 0
  });
}

#[cfg(target_os = "linux")]
fn entry_of(action: webkit2gtk::ContextMenuAction) -> MenuEntry {
  use webkit2gtk::ContextMenuAction as Action;

  match action {
    Action::Copy
    | Action::Cut
    | Action::Paste
    | Action::Delete
    | Action::SelectAll
    | Action::InputMethods
    | Action::Unicode => MenuEntry::Editing,
    Action::SpellingGuess
    | Action::NoGuessesFound
    | Action::IgnoreSpelling
    | Action::LearnSpelling
    | Action::IgnoreGrammar => MenuEntry::Spelling,
    Action::InspectElement => MenuEntry::Inspect,
    _ => MenuEntry::Other,
  }
}

#[cfg(test)]
mod tests {
  use super::MenuEntry::{Editing, Inspect, Other, Separator, Spelling};
  use super::*;

  #[test]
  fn a_text_field_keeps_spelling_and_editing_without_navigation() {
    // WebKitGTK's menu on a misspelled word in a text field.
    let entries = [
      Spelling, Spelling, Separator, Spelling, Spelling, Separator, Other, Other, Other, Other, Separator,
      Editing, Editing, Editing, Editing, Separator, Editing, Separator, Inspect,
    ];
    let kept = kept_entries(&entries, false);
    let kept_entries: Vec<MenuEntry> = entries
      .iter()
      .zip(&kept)
      .filter(|(_, is_kept)| **is_kept)
      .map(|(entry, _)| *entry)
      .collect();
    assert_eq!(
      kept_entries,
      [
        Spelling, Spelling, Separator, Spelling, Spelling, Separator, Editing, Editing, Editing, Editing,
        Separator, Editing
      ]
    );
  }

  #[test]
  fn an_empty_area_keeps_nothing() {
    let entries = [Other, Other, Other, Other, Separator, Inspect];
    assert!(kept_entries(&entries, false).iter().all(|is_kept| !is_kept));
  }

  #[test]
  fn debug_builds_keep_inspect() {
    let entries = [Other, Separator, Inspect];
    assert_eq!(kept_entries(&entries, true), [false, false, true]);
    let entries = [Editing, Separator, Inspect];
    assert_eq!(kept_entries(&entries, true), [true, true, true]);
  }

  #[test]
  fn separators_never_lead_trail_or_double() {
    let entries = [
      Separator, Editing, Separator, Separator, Other, Separator, Spelling, Separator,
    ];
    assert_eq!(
      kept_entries(&entries, false),
      [false, true, false, false, false, true, true, false]
    );
  }
}
