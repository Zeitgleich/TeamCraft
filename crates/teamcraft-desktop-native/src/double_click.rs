//! System Settings → Desktop & Dock → "Double-click a window's title bar to" (#174). macOS keeps
//! the choice as `AppleActionOnDoubleClick` in the global defaults; the custom title bar reads it
//! so a double-click does what the user chose everywhere else. The shell turns the text into an
//! action.

use objc2_foundation::{NSString, NSUserDefaults};

/// The defaults key of the setting.
const ACTION_ON_DOUBLE_CLICK_KEY: &str = "AppleActionOnDoubleClick";

/// The raw value (`Maximize`, `Minimize`, `Fill`, `None`), or `None` when the user never changed it
/// (macOS then zooms).
pub fn apple_action_on_double_click() -> Option<String> {
  let key = NSString::from_str(ACTION_ON_DOUBLE_CLICK_KEY);
  NSUserDefaults::standardUserDefaults()
    .stringForKey(&key)
    .map(|value| value.to_string())
}
