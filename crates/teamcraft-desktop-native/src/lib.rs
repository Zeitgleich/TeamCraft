//! The desktop shell's native OS calls that cannot be written without `unsafe` (code-style.md,
//! "Native FFI"). Every other crate forbids `unsafe`; this one denies it and allows it only on the
//! few items below, each with a `SAFETY` comment. The public API is safe.
//!
//! | Call                                    | Platform | Module     |
//! |-----------------------------------------|----------|------------|
//! | [`watch_resume`]: the computer woke up  | macOS    | `resume`   |
//! | [`watch_resume`]: the computer woke up  | Windows  | `resume`   |
//! | [`install_notification_delegate`]       | macOS    | `notification_delegate` |
//! | `notification_attachment`: a picture    | macOS    | `notification_attachment` |

mod notification_attachment;
mod notification_delegate;
mod resume;

pub use notification_attachment::AttachmentError;
#[cfg(target_os = "macos")]
pub use notification_attachment::notification_attachment;
pub use notification_delegate::{NotificationResponse, ResponseAction, install_notification_delegate};

pub use resume::{ResumeWatchError, watch_resume};
