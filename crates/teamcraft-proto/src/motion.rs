//! The motion of a Live Photo or Motion Photo (issue #2181, e2ee.md §4.5): a short video sent as a
//! second encrypted file of the same message and named from the picture's encrypted details. The
//! server never sees which file belongs to which: it only publishes how large a motion part the
//! apps let a member send.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Default of [`MotionLimits::max_bytes`]: a Live Photo's three seconds of H.264 take 2 to 6 MiB.
pub const DEFAULT_MAX_MOTION_BYTES: u64 = 16 * 1024 * 1024;
/// Largest value an admin may set for [`MotionLimits::max_bytes`]: a motion part is a short clip,
/// played inline from memory on every platform.
pub const MAX_MOTION_BYTES_LIMIT: u64 = 64 * 1024 * 1024;
/// Smallest value an admin may set: below it no camera's clip fits, which is what `is_allowed`
/// says more plainly.
pub const MIN_MOTION_BYTES_LIMIT: u64 = 1024 * 1024;

/// Whether and how large a motion part the apps let a member send. The server can't see a motion
/// part (it is an attachment like any other), so this is only what the apps hold senders to;
/// receivers play what they get up to [`MAX_MOTION_BYTES_LIMIT`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MotionLimits {
  /// Off: Live Photos and Motion Photos go out as their still picture only.
  pub is_allowed: bool,
  /// Largest motion part (plain size); a larger one is not sent and the picture goes alone.
  pub max_bytes: u64,
}

impl Default for MotionLimits {
  fn default() -> Self {
    Self {
      is_allowed: true,
      max_bytes: DEFAULT_MAX_MOTION_BYTES,
    }
  }
}

impl MotionLimits {
  /// The limits cut to sensible bounds, so a server's odd values can't make clips nobody plays.
  pub fn bounded(self) -> Self {
    Self {
      is_allowed: self.is_allowed,
      max_bytes: self
        .max_bytes
        .clamp(MIN_MOTION_BYTES_LIMIT, MAX_MOTION_BYTES_LIMIT),
    }
  }

  /// Whether a motion part of `size_bytes` may go out with its picture.
  pub fn admits(&self, size_bytes: u64) -> bool {
    self.is_allowed && size_bytes > 0 && size_bytes <= self.max_bytes
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn defaults_allow_a_live_photo_and_serialize_stably() {
    let limits = MotionLimits::default();
    assert!(limits.admits(4 * 1024 * 1024));
    assert!(!limits.admits(DEFAULT_MAX_MOTION_BYTES + 1));
    assert!(!limits.admits(0), "an empty clip is no motion");
    assert_eq!(
      serde_json::to_string(&limits).unwrap(),
      r#"{"is_allowed":true,"max_bytes":16777216}"#
    );
  }

  #[test]
  fn not_allowed_admits_nothing() {
    let limits = MotionLimits {
      is_allowed: false,
      ..MotionLimits::default()
    };
    assert!(!limits.admits(1));
  }

  #[test]
  fn bounded_clamps_odd_server_values() {
    let tiny = MotionLimits {
      is_allowed: true,
      max_bytes: 1,
    };
    assert_eq!(tiny.bounded().max_bytes, MIN_MOTION_BYTES_LIMIT);
    let huge = MotionLimits {
      is_allowed: true,
      max_bytes: u64::MAX,
    };
    assert_eq!(huge.bounded().max_bytes, MAX_MOTION_BYTES_LIMIT);
  }
}
