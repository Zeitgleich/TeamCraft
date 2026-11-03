//! Whether a composer takes the keyboard focus when it appears (#722): opening a conversation on
//! the desktop or in a browser with a mouse and keyboard lets the user type at once. Touch devices
//! never get it, the on-screen keyboard would cover half the conversation.

use wasm_bindgen::JsCast;

/// What decides whether a composer that just appeared takes the focus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FocusSituation {
  /// The primary input is a precise pointer with hover (mouse, trackpad), not a finger.
  pub is_fine_pointer: bool,
  /// The user may write here: a read-only channel has no composer to focus.
  pub can_post: bool,
  /// A dialog or overlay (quick switcher, settings, a modal) is open.
  pub is_overlay_open: bool,
  /// The focus already is in a text field (the search field, another composer) or in a dialog.
  pub is_focus_taken: bool,
  /// This is a thread's composer, not the conversation's.
  pub is_thread_composer: bool,
  /// A thread is open beside the conversation: its composer takes the focus instead.
  pub is_thread_open: bool,
}

/// Whether the composer takes the focus in `situation`.
pub fn should_focus(situation: FocusSituation) -> bool {
  situation.is_fine_pointer
    && situation.can_post
    && !situation.is_overlay_open
    && !situation.is_focus_taken
    && (situation.is_thread_composer || !situation.is_thread_open)
}

/// A mouse or trackpad with hover is the primary input (the desktop app, a desktop browser).
pub fn is_fine_pointer() -> bool {
  web_sys::window().is_some_and(|window| {
    ["(pointer: fine)", "(hover: hover)"].iter().all(|query| {
      window
        .match_media(query)
        .ok()
        .flatten()
        .is_some_and(|list| list.matches())
    })
  })
}

/// Whether the focus is somewhere the user is typing or deciding: a text field, an editable
/// element or anything inside a dialog. A field that is gone from the page doesn't count.
pub fn is_focus_taken() -> bool {
  let Some(active) = web_sys::window()
    .and_then(|window| window.document())
    .and_then(|document| document.active_element())
  else {
    return false;
  };
  if !active.is_connected() {
    return false;
  }
  let is_field = matches!(
    active.tag_name().to_lowercase().as_str(),
    "input" | "textarea" | "select"
  ) || active
    .dyn_ref::<web_sys::HtmlElement>()
    .is_some_and(web_sys::HtmlElement::is_content_editable);
  let is_in_dialog = active
    .closest("dialog, [role=dialog], [role=alertdialog], [aria-modal=true]")
    .ok()
    .flatten()
    .is_some();
  is_field || is_in_dialog
}

#[cfg(test)]
mod tests {
  use super::*;

  const OPENED_ON_DESKTOP: FocusSituation = FocusSituation {
    is_fine_pointer: true,
    can_post: true,
    is_overlay_open: false,
    is_focus_taken: false,
    is_thread_composer: false,
    is_thread_open: false,
  };

  #[test]
  fn a_conversation_opened_with_mouse_or_keyboard_focuses_its_composer() {
    assert!(should_focus(OPENED_ON_DESKTOP));
  }

  #[test]
  fn touch_devices_never_get_the_keyboard_unasked() {
    assert!(!should_focus(FocusSituation {
      is_fine_pointer: false,
      ..OPENED_ON_DESKTOP
    }));
  }

  #[test]
  fn read_only_channels_have_nothing_to_focus() {
    assert!(!should_focus(FocusSituation {
      can_post: false,
      ..OPENED_ON_DESKTOP
    }));
  }

  #[test]
  fn dialogs_and_fields_keep_their_focus() {
    assert!(!should_focus(FocusSituation {
      is_overlay_open: true,
      ..OPENED_ON_DESKTOP
    }));
    assert!(!should_focus(FocusSituation {
      is_focus_taken: true,
      ..OPENED_ON_DESKTOP
    }));
  }

  #[test]
  fn an_open_thread_takes_the_focus_for_its_own_composer() {
    let with_thread = FocusSituation {
      is_thread_open: true,
      ..OPENED_ON_DESKTOP
    };
    assert!(!should_focus(with_thread), "the conversation's composer");
    assert!(
      should_focus(FocusSituation {
        is_thread_composer: true,
        ..with_thread
      }),
      "the thread's composer"
    );
  }
}
