//! The ring that shows how far a file got: an upload's (`UploadControl`), and the full picture
//! coming down behind a thumbnail, in the conversation and in the lightbox. One ring everywhere, so
//! sending and receiving look alike. It fills while the size is known and turns before the first
//! bytes moved; with reduced motion it doesn't turn (the stylesheet).

use leptos::prelude::*;

/// Where the full picture behind a thumbnail stands, as the tile or the lightbox knows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadPhase {
  /// Nothing asked for yet (a preview that waits for its turn, an animation not playing).
  Idle,
  /// Coming down, or being decrypted and decoded.
  Loading,
  /// On screen.
  Ready,
  /// Didn't arrive (offline, server trouble, gone from storage): it can be tried again.
  Failed,
}

/// What stands over the thumbnail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoadRing {
  /// Nothing: the picture is there, or nothing loads.
  Hidden,
  /// Loading, how far unknown (before the first bytes, or a platform that doesn't say).
  Turning,
  /// Loading, this share of it is here (0 to 1).
  Filling(f64),
  /// The load failed: a retry action instead of the ring.
  Retry,
}

impl LoadRing {
  /// The ring for `phase`, with the transfer's `(done_bytes, total_bytes)` where the core reports
  /// them.
  pub fn of(phase: LoadPhase, maybe_progress: Option<(u64, u64)>) -> Self {
    match phase {
      LoadPhase::Idle | LoadPhase::Ready => Self::Hidden,
      LoadPhase::Failed => Self::Retry,
      LoadPhase::Loading => match maybe_progress {
        Some((done_bytes, total_bytes)) if done_bytes > 0 && total_bytes > 0 => {
          // Precision: a file's size is far below 2^53.
          Self::Filling((done_bytes.min(total_bytes) as f64) / (total_bytes as f64))
        },
        _ => Self::Turning,
      },
    }
  }

  pub fn is_shown(self) -> bool {
    matches!(self, Self::Turning | Self::Filling(_))
  }

  /// The filled share, `None` while it turns.
  pub fn maybe_fraction(self) -> Option<f64> {
    match self {
      Self::Filling(fraction) => Some(fraction),
      _ => None,
    }
  }
}

/// The ring itself: filled to `maybe_fraction`, turning while that is `None`. It follows the signal
/// without being built again, so it fills smoothly.
#[component]
pub fn ProgressRing(#[prop(into)] maybe_fraction: Signal<Option<f64>>) -> impl IntoView {
  // The share as a dash offset: 0 is full, 100 empty (the circle's path length is 100).
  let offset = move || format!("{:.2}", 100.0 - maybe_fraction.get().unwrap_or(0.0) * 100.0);
  let is_turning = move || maybe_fraction.get().is_none();
  view! {
    <svg class="upload-ring" class:is-turning=is_turning viewBox="0 0 36 36" aria-hidden="true">
      <circle class="upload-ring-track" cx="18" cy="18" r="16" />
      <circle class="upload-ring-fill" cx="18" cy="18" r="16" pathLength="100" stroke-dashoffset=offset />
    </svg>
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_load_without_progress_turns() {
    assert_eq!(LoadRing::of(LoadPhase::Loading, None), LoadRing::Turning);
    assert_eq!(
      LoadRing::of(LoadPhase::Loading, Some((0, 4096))),
      LoadRing::Turning,
      "no bytes moved yet"
    );
    assert_eq!(
      LoadRing::of(LoadPhase::Loading, Some((10, 0))),
      LoadRing::Turning,
      "the size is unknown"
    );
  }

  #[test]
  fn a_load_with_a_known_size_fills() {
    assert_eq!(
      LoadRing::of(LoadPhase::Loading, Some((1024, 4096))),
      LoadRing::Filling(0.25)
    );
    assert_eq!(
      LoadRing::of(LoadPhase::Loading, Some((5000, 4096))),
      LoadRing::Filling(1.0),
      "the ciphertext's overhead doesn't overfill it"
    );
  }

  #[test]
  fn a_shown_picture_has_no_ring() {
    assert_eq!(LoadRing::of(LoadPhase::Ready, Some((10, 20))), LoadRing::Hidden);
    assert_eq!(LoadRing::of(LoadPhase::Idle, None), LoadRing::Hidden);
    assert!(!LoadRing::Hidden.is_shown());
  }

  #[test]
  fn a_failed_load_offers_a_retry_and_a_retry_turns_again() {
    let failed = LoadRing::of(LoadPhase::Failed, Some((10, 20)));
    assert_eq!(failed, LoadRing::Retry);
    assert!(!failed.is_shown(), "no stuck ring");
    let retried = LoadRing::of(LoadPhase::Loading, None);
    assert!(retried.is_shown());
    assert_eq!(retried.maybe_fraction(), None);
  }
}
