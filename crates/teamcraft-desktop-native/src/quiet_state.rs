//! Whether the desktop asked not to be disturbed, read from the system's own public API (#191): the
//! shell keeps the incoming-call ringtone (and the message sound) quiet while it says so. The
//! notification itself stays; the system decides on its banner.
//!
//! | Platform | Read from                                                              |
//! |----------|------------------------------------------------------------------------|
//! | Windows  | `SHQueryUserNotificationState` (busy, full screen, presentation, quiet) |
//! | macOS    | `INFocusStatusCenter`, when the user and the app's signing allow it     |
//!
//! Linux has its own read in the shell (the desktop's notification service on the session bus).
//! Anything the system does not answer is [`QuietState::Unknown`], which never silences: a missed
//! call costs more than an unwanted ring.

/// What the system says about being disturbed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuietState {
  /// The user asked for quiet.
  Quiet,
  /// The system lets apps notify.
  Open,
  /// The system did not answer, or has nothing to ask (no permission, no signing for it).
  Unknown,
}

impl QuietState {
  /// Whether the ringtone must stay silent: only when the system said so.
  pub fn is_quiet(self) -> bool {
    self == Self::Quiet
  }
}

/// The `QUERY_USER_NOTIFICATION_STATE` values of `shellapi.h`.
const QUNS_NOT_PRESENT: i32 = 1;
const QUNS_BUSY: i32 = 2;
const QUNS_RUNNING_D3D_FULL_SCREEN: i32 = 3;
const QUNS_PRESENTATION_MODE: i32 = 4;
const QUNS_ACCEPTS_NOTIFICATIONS: i32 = 5;
const QUNS_QUIET_TIME: i32 = 6;
const QUNS_APP: i32 = 7;

/// Maps a Windows `QUERY_USER_NOTIFICATION_STATE`. A full-screen app, presentation mode and quiet
/// time (do not disturb, Focus assist) ask apps not to notify. `QUNS_APP` (a full-screen Store
/// app) and the screen being locked or off (`QUNS_NOT_PRESENT`) do not: a call should still ring
/// at an unattended desk.
pub fn windows_quiet_state(raw: i32) -> QuietState {
  match raw {
    QUNS_BUSY | QUNS_RUNNING_D3D_FULL_SCREEN | QUNS_PRESENTATION_MODE | QUNS_QUIET_TIME => QuietState::Quiet,
    QUNS_NOT_PRESENT | QUNS_ACCEPTS_NOTIFICATIONS | QUNS_APP => QuietState::Open,
    _ => QuietState::Unknown,
  }
}

/// Windows: what `SHQueryUserNotificationState` says now.
#[cfg(windows)]
pub fn windows_notification_state() -> QuietState {
  use windows::Win32::UI::Shell::SHQueryUserNotificationState;
  #[allow(unsafe_code)]
  // SAFETY: the call takes no arguments and only reads the shell's state.
  let state = unsafe { SHQueryUserNotificationState() };
  match state {
    Ok(state) => windows_quiet_state(state.0),
    Err(error) => {
      tracing::debug!(%error, "the shell did not report the notification state");
      QuietState::Unknown
    },
  }
}

/// macOS: whether a Focus is on, from `INFocusStatusCenter`. Needs the Focus Status capability in
/// the app's signing and the user's consent (`request_focus_authorization`); without either the
/// answer is [`QuietState::Unknown`].
#[cfg(target_os = "macos")]
pub fn focus_state() -> QuietState {
  mac::focus_state()
}

/// macOS: asks the user for access to the Focus status (a system prompt, once; the app needs the
/// Focus Status capability or the system never asks). Call it from a click, not at start-up.
#[cfg(target_os = "macos")]
pub fn request_focus_authorization() {
  mac::request_authorization();
}

#[cfg(target_os = "macos")]
mod mac {
  use block2::RcBlock;
  use objc2::msg_send;
  use objc2::rc::Retained;
  use objc2::runtime::{AnyClass, AnyObject, Bool};

  use super::QuietState;

  #[allow(unsafe_code)]
  #[link(name = "Intents", kind = "framework")]
  unsafe extern "C" {}

  /// `INFocusStatusAuthorizationStatus.authorized`.
  const AUTHORIZED: isize = 3;

  fn center() -> Option<Retained<AnyObject>> {
    let class = AnyClass::get(c"INFocusStatusCenter")?;
    #[allow(unsafe_code)]
    // SAFETY: `defaultCenter` is a class property returning the shared center.
    unsafe {
      msg_send![class, defaultCenter]
    }
  }

  pub(super) fn focus_state() -> QuietState {
    let Some(center) = center() else {
      return QuietState::Unknown;
    };
    #[allow(unsafe_code)]
    // SAFETY: `authorizationStatus` returns an `NSInteger` enum; `focusStatus` an object whose
    // `isFocused` is a nullable `NSNumber`, read with `boolValue`.
    unsafe {
      let status: isize = msg_send![&*center, authorizationStatus];
      if status != AUTHORIZED {
        return QuietState::Unknown;
      }
      let focus: Option<Retained<AnyObject>> = msg_send![&*center, focusStatus];
      let Some(focus) = focus else {
        return QuietState::Unknown;
      };
      let maybe_flag: Option<Retained<AnyObject>> = msg_send![&*focus, isFocused];
      match maybe_flag {
        None => QuietState::Unknown,
        Some(flag) => {
          let is_focused: Bool = msg_send![&*flag, boolValue];
          if is_focused.as_bool() {
            QuietState::Quiet
          } else {
            QuietState::Open
          }
        },
      }
    }
  }

  pub(super) fn request_authorization() {
    let Some(center) = center() else {
      return;
    };
    let done = RcBlock::new(|_status: isize| {});
    #[allow(unsafe_code)]
    // SAFETY: the selector takes a completion block with the new status as `NSInteger`.
    unsafe {
      let () = msg_send![&*center, requestAuthorizationWithCompletionHandler: &*done];
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn windows_busy_states_and_quiet_time_silence_the_ringtone() {
    for raw in [
      QUNS_BUSY,
      QUNS_RUNNING_D3D_FULL_SCREEN,
      QUNS_PRESENTATION_MODE,
      QUNS_QUIET_TIME,
    ] {
      assert!(windows_quiet_state(raw).is_quiet(), "state {raw}");
    }
  }

  #[test]
  fn windows_open_states_let_it_ring() {
    for raw in [QUNS_NOT_PRESENT, QUNS_ACCEPTS_NOTIFICATIONS, QUNS_APP] {
      assert_eq!(windows_quiet_state(raw), QuietState::Open, "state {raw}");
    }
  }

  #[test]
  fn a_state_nobody_knows_never_silences() {
    for raw in [0, 8, -1, i32::MAX] {
      assert_eq!(windows_quiet_state(raw), QuietState::Unknown);
      assert!(!windows_quiet_state(raw).is_quiet());
    }
  }

  #[test]
  fn only_quiet_is_quiet() {
    assert!(QuietState::Quiet.is_quiet());
    assert!(!QuietState::Open.is_quiet());
    assert!(!QuietState::Unknown.is_quiet());
  }
}
