//! The desktop shell's native OS calls that cannot be written without `unsafe` (code-style.md,
//! "Native FFI"). Every other crate forbids `unsafe`; this one denies it and allows it only on the
//! few items below, each with a `SAFETY` comment. The public API is safe.
//!
//! | Call                                    | Platform | Module     |
//! |-----------------------------------------|----------|------------|
//! | [`watch_resume`]: the computer woke up  | macOS    | `resume`   |
//! | [`watch_resume`]: the computer woke up  | Windows  | `resume`   |

mod resume;

pub use resume::{ResumeWatchError, watch_resume};
