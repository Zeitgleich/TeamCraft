//! The desktop shell's native OS calls that cannot be written without `unsafe` (code-style.md,
//! "Native FFI"). Every other crate forbids `unsafe`; this one denies it and allows it only on the
//! few items below, each with a `SAFETY` comment. The public API is safe.
//!
//! | Call                                      | Platform | Module                    |
//! |-------------------------------------------|----------|---------------------------|
//! | [`watch_resume`]: the computer woke up    | macOS    | `resume`                  |
//! | [`watch_resume`]: the computer woke up    | Windows  | `resume`                  |
//! | [`watch_network_up`]: the network is back | macOS    | `network`                 |
//! | [`watch_network_up`]: the network is back | Windows  | `network`                 |
//! | [`install_notification_delegate`]         | macOS    | `notification_delegate`   |
//! | `notification_attachment`: a picture      | macOS    | `notification_attachment` |
//! | `notification_authorization`: allowed?    | macOS    | `notification_settings`   |
//! | `apple_action_on_double_click`: setting   | macOS    | `double_click`            |
//! | `focus_state`: is a Focus on?             | macOS    | `quiet_state`             |
//! | `windows_notification_state`: quiet?      | Windows  | `quiet_state`             |
//! | `webkit_version`: the library in use      | Linux    | `webkit_version`          |
//! | `trim_allocator`: free memory goes back   | Linux    | `allocator`               |
//! | `watch_process_failed`: page process died | Windows  | `webview2_process`        |
//! | `show_window_menu`: system menu           | Windows  | `window_menu`             |
//! | `install_snap_layouts`: maximize button   | Windows  | `snap_layouts`            |
//! | JNI: audio context for cpal (calls)       | Android  | `android_context`         |
//! | `set_openh264_rate`: rate in place        | all      | `openh264_rate`           |
//!
//! The Android entry is in here too, as the one crate that may hold `unsafe`; the mobile library
//! (`teamcraft-ffi`) links this crate only on Android, for it. The OpenH264 call (feature
//! `openh264`) serves the native video encoders of `teamcraft-media`, on every native platform.

#[cfg(all(feature = "shell", target_os = "linux", target_env = "gnu"))]
mod allocator;
#[cfg(target_os = "android")]
mod android_context;
#[cfg(all(feature = "shell", target_os = "macos"))]
mod double_click;
#[cfg(feature = "shell")]
mod network;
#[cfg(feature = "shell")]
mod notification_attachment;
#[cfg(feature = "shell")]
mod notification_delegate;
#[cfg(feature = "shell")]
mod notification_settings;
#[cfg(feature = "openh264")]
mod openh264_rate;
#[cfg(feature = "shell")]
mod quiet_state;
#[cfg(feature = "shell")]
mod resume;
#[cfg(feature = "shell")]
mod snap_layouts;
#[cfg(all(feature = "shell", target_os = "linux"))]
mod webkit_version;
#[cfg(feature = "shell")]
mod webview2_process;
#[cfg(feature = "shell")]
mod window_menu;

#[cfg(all(feature = "shell", target_os = "linux", target_env = "gnu"))]
pub use allocator::trim_allocator;
#[cfg(all(feature = "shell", target_os = "macos"))]
pub use double_click::apple_action_on_double_click;
#[cfg(feature = "shell")]
pub use notification_attachment::AttachmentError;
#[cfg(all(feature = "shell", target_os = "macos"))]
pub use notification_attachment::notification_attachment;
#[cfg(feature = "shell")]
pub use notification_delegate::{NotificationResponse, ResponseAction, install_notification_delegate};
#[cfg(feature = "shell")]
pub use notification_settings::NotificationAuthorization;
#[cfg(all(feature = "shell", target_os = "macos"))]
pub use notification_settings::notification_authorization;
#[cfg(feature = "openh264")]
pub use openh264_rate::{OpenH264RateError, set_openh264_rate};
#[cfg(all(feature = "shell", windows))]
pub use quiet_state::windows_notification_state;
#[cfg(feature = "shell")]
pub use quiet_state::{QuietState, windows_quiet_state};
#[cfg(all(feature = "shell", target_os = "macos"))]
pub use quiet_state::{focus_state, request_focus_authorization};

#[cfg(target_os = "android")]
pub use android_context::Java_de_zeitgleich_teamcraft_calls_AudioContext_initialize;
#[cfg(feature = "shell")]
pub use network::{NetworkWatchError, watch_network_up};
#[cfg(feature = "shell")]
pub use resume::{ResumeWatchError, watch_resume};
#[cfg(feature = "shell")]
pub use snap_layouts::ButtonRect;
#[cfg(all(feature = "shell", windows))]
pub use snap_layouts::{SnapLayoutsError, install_snap_layouts, set_maximize_button};
#[cfg(all(feature = "shell", target_os = "linux"))]
pub use webkit_version::webkit_version;
#[cfg(all(feature = "shell", windows))]
pub use webview2_process::{ProcessFailedWatchError, watch_process_failed};
#[cfg(feature = "shell")]
pub use webview2_process::{WebviewProcessFailure, WebviewProcessKind};
#[cfg(all(feature = "shell", windows))]
pub use window_menu::show_window_menu;
