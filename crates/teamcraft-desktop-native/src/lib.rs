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
//! | `webkit_version`: the library in use      | Linux    | `webkit_version`          |
//! | JNI: audio context for cpal (calls)       | Android  | `android_context`         |
//!
//! The Android entry is in here too, as the one crate that may hold `unsafe`; the mobile library
//! (`teamcraft-ffi`) links this crate only on Android, for it.

#[cfg(target_os = "android")]
mod android_context;
mod network;
mod notification_attachment;
mod notification_delegate;
mod resume;
#[cfg(target_os = "linux")]
mod webkit_version;

pub use notification_attachment::AttachmentError;
#[cfg(target_os = "macos")]
pub use notification_attachment::notification_attachment;
pub use notification_delegate::{NotificationResponse, ResponseAction, install_notification_delegate};

#[cfg(target_os = "android")]
pub use android_context::Java_de_zeitgleich_teamcraft_calls_AudioContext_initialize;
pub use network::{NetworkWatchError, watch_network_up};
pub use resume::{ResumeWatchError, watch_resume};
#[cfg(target_os = "linux")]
pub use webkit_version::webkit_version;
