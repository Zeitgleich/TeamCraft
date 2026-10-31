//! The app's own video controls (#638), kept apart from the view so they can be tested without a
//! browser: which key does what, and where the scrubber's played and buffered parts end. The
//! platform's native controls differ per webview (WebKitGTK's GStreamer bar, WKWebView, WebView2),
//! so the player draws its own on every platform.

use std::time::Duration;

/// Seconds the arrow keys seek.
pub const SEEK_STEP_SECONDS: f64 = 5.0;
/// Volume step of the up and down arrow keys.
pub const VOLUME_STEP: f64 = 0.1;
/// How long a video may give no sign of playing before the player says it can't be played (#804):
/// from the file being there to its length and size being known, and from "play" to the first
/// frame or the clock moving. The file is already decrypted in memory, so a webview that can play
/// it does both within a moment. WebKitGTK without a decoder fires no error: it either never gets
/// that far, or it reports the length with a picture size of zero ([`PlaybackState::lacks_picture`]).
pub const STALL_TIMEOUT: Duration = Duration::from_secs(8);

/// What the player waits for, checked [`STALL_TIMEOUT`] after it began waiting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StallCheck {
  /// The video's length and size, from when the player got the file.
  Metadata,
  /// A frame or a moving clock, from "play" at this position.
  Start { from_seconds: f64 },
}

/// What the video element reports when a check is due.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaybackState {
  /// `readyState` reached `HAVE_METADATA`.
  pub has_metadata: bool,
  /// `readyState` reached `HAVE_CURRENT_DATA`. Not proof of a picture: WebKitGTK gets there
  /// without a video decoder too.
  pub has_frame: bool,
  /// The picture's size is known (`videoWidth` above zero).
  pub has_picture: bool,
  pub is_paused: bool,
  pub is_ended: bool,
  pub is_seeking: bool,
  pub current_seconds: f64,
}

/// What a key does to the focused player.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PlayerCommand {
  TogglePlay,
  /// Forward (positive) or back, in seconds.
  SeekBy(f64),
  SeekToStart,
  SeekToEnd,
  /// Louder (positive) or quieter.
  VolumeBy(f64),
  ToggleMute,
  ToggleFullscreen,
}

impl PlayerCommand {
  /// The command of a key pressed while the player has focus; `None` for keys it leaves alone (and
  /// for every key with Ctrl, ⌘ or Alt, which belong to the app and the system).
  pub fn for_key(key: &str, has_modifier: bool) -> Option<Self> {
    if has_modifier {
      return None;
    }
    match key {
      " " | "k" | "K" => Some(Self::TogglePlay),
      "ArrowLeft" => Some(Self::SeekBy(-SEEK_STEP_SECONDS)),
      "ArrowRight" => Some(Self::SeekBy(SEEK_STEP_SECONDS)),
      "Home" => Some(Self::SeekToStart),
      "End" => Some(Self::SeekToEnd),
      "ArrowUp" => Some(Self::VolumeBy(VOLUME_STEP)),
      "ArrowDown" => Some(Self::VolumeBy(-VOLUME_STEP)),
      "m" | "M" => Some(Self::ToggleMute),
      "f" | "F" => Some(Self::ToggleFullscreen),
      _ => None,
    }
  }
}

/// The position `delta` seconds from `current`, within the video. An unknown duration (not loaded,
/// or a live stream: NaN or infinite) only keeps it from going below zero.
pub fn seek_target(current: f64, delta: f64, duration: f64) -> f64 {
  let target = (current + delta).max(0.0);
  match duration.is_finite() {
    true => target.min(duration),
    false => target,
  }
}

/// The volume `delta` from `current`, between silent and full.
pub fn volume_target(current: f64, delta: f64) -> f64 {
  (current + delta).clamp(0.0, 1.0)
}

/// How far along `duration` the time `at` is, in percent for the scrubber's CSS (0 while the
/// duration is unknown).
pub fn percent_of(at: f64, duration: f64) -> f64 {
  if !duration.is_finite() || duration <= 0.0 || !at.is_finite() {
    return 0.0;
  }
  (at / duration * 100.0).clamp(0.0, 100.0)
}

/// Where the buffered stretch that holds `at` ends: the scrubber shows what plays without waiting.
/// `ranges` are the media element's buffered `(start, end)` pairs; `at` itself when none holds it.
pub fn buffered_until(ranges: &[(f64, f64)], at: f64) -> f64 {
  ranges
    .iter()
    .find(|(start, end)| *start <= at && at <= *end)
    .map_or(at, |(_, end)| *end)
}

/// Whole seconds as the player shows them ("0:07", "12:34", "1:02:03"); an unknown duration shows
/// as "0:00".
pub fn format_seconds(seconds: f64) -> String {
  let total = match seconds.is_finite() && seconds > 0.0 {
    // Truncation is intended: a running clock shows the seconds that have passed.
    true => seconds as u64,
    false => 0,
  };
  let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
  match hours {
    0 => format!("{minutes}:{seconds:02}"),
    _ => format!("{hours}:{minutes:02}:{seconds:02}"),
  }
}

impl PlaybackState {
  /// The length is known but the picture has no size: the webview can't decode the video track
  /// (WebKitGTK without an H.264 or HEVC decoder). Known the moment the metadata is there, so the
  /// player says so at once.
  pub fn lacks_picture(self) -> bool {
    self.has_metadata && !self.has_picture
  }
}

impl StallCheck {
  /// Whether the video gave no sign of playing in time: it can't be played here. A user who paused
  /// or seeked meanwhile started something else, which is not a stall.
  pub fn is_stalled(self, state: PlaybackState) -> bool {
    match self {
      Self::Metadata => !state.has_metadata || state.lacks_picture(),
      Self::Start { from_seconds } => {
        let is_waiting = !state.is_paused && !state.is_ended && !state.is_seeking;
        let has_moved = state.current_seconds > from_seconds;
        // Sound alone moves the clock: without a picture it still doesn't play.
        is_waiting && (state.lacks_picture() || (!has_moved && !state.has_frame))
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod keys {
    use super::*;

    #[test]
    fn the_usual_player_keys_work() {
      let cases = [
        (" ", PlayerCommand::TogglePlay),
        ("k", PlayerCommand::TogglePlay),
        ("ArrowLeft", PlayerCommand::SeekBy(-SEEK_STEP_SECONDS)),
        ("ArrowRight", PlayerCommand::SeekBy(SEEK_STEP_SECONDS)),
        ("Home", PlayerCommand::SeekToStart),
        ("End", PlayerCommand::SeekToEnd),
        ("ArrowUp", PlayerCommand::VolumeBy(VOLUME_STEP)),
        ("ArrowDown", PlayerCommand::VolumeBy(-VOLUME_STEP)),
        ("m", PlayerCommand::ToggleMute),
        ("F", PlayerCommand::ToggleFullscreen),
      ];
      for (key, command) in cases {
        assert_eq!(PlayerCommand::for_key(key, false), Some(command), "{key}");
      }
    }

    #[test]
    fn other_keys_and_shortcuts_are_left_alone() {
      assert_eq!(PlayerCommand::for_key("Escape", false), None);
      assert_eq!(PlayerCommand::for_key("j", false), None);
      assert_eq!(
        PlayerCommand::for_key("f", true),
        None,
        "Ctrl+F is the app's search"
      );
      assert_eq!(PlayerCommand::for_key(" ", true), None);
    }
  }

  mod timeline {
    use super::*;

    #[test]
    fn seeking_stays_within_the_video() {
      assert_eq!(seek_target(3.0, -5.0, 60.0), 0.0);
      assert_eq!(seek_target(58.0, 5.0, 60.0), 60.0);
      assert_eq!(seek_target(10.0, 5.0, 60.0), 15.0);
      assert_eq!(
        seek_target(10.0, 5.0, f64::NAN),
        15.0,
        "the duration isn't known yet"
      );
      assert_eq!(seek_target(10.0, 5.0, f64::INFINITY), 15.0);
    }

    #[test]
    fn volume_stays_between_silent_and_full() {
      assert_eq!(volume_target(0.95, VOLUME_STEP), 1.0);
      assert_eq!(volume_target(0.05, -VOLUME_STEP), 0.0);
      assert!((volume_target(0.5, VOLUME_STEP) - 0.6).abs() < 1e-9);
    }

    #[test]
    fn progress_is_a_percentage_and_zero_while_the_duration_is_unknown() {
      assert_eq!(percent_of(15.0, 60.0), 25.0);
      assert_eq!(percent_of(90.0, 60.0), 100.0);
      assert_eq!(percent_of(5.0, f64::NAN), 0.0);
      assert_eq!(percent_of(5.0, f64::INFINITY), 0.0);
      assert_eq!(percent_of(5.0, 0.0), 0.0);
    }

    #[test]
    fn the_buffered_part_is_the_stretch_that_holds_the_position() {
      let ranges = [(0.0, 12.0), (30.0, 45.0)];
      assert_eq!(buffered_until(&ranges, 5.0), 12.0);
      assert_eq!(buffered_until(&ranges, 31.0), 45.0);
      assert_eq!(
        buffered_until(&ranges, 20.0),
        20.0,
        "a gap: nothing ahead is buffered"
      );
      assert_eq!(buffered_until(&[], 7.0), 7.0);
    }

    #[test]
    fn times_read_like_a_clock() {
      assert_eq!(format_seconds(7.9), "0:07");
      assert_eq!(format_seconds(754.0), "12:34");
      assert_eq!(format_seconds(3723.0), "1:02:03");
      assert_eq!(format_seconds(f64::NAN), "0:00");
      assert_eq!(format_seconds(-3.0), "0:00");
    }
  }

  mod stalls {
    use super::*;

    #[test]
    fn a_player_that_never_gets_metadata_is_stalled() {
      assert!(StallCheck::Metadata.is_stalled(nothing_loaded()));
      assert!(!StallCheck::Metadata.is_stalled(loaded()));
    }

    #[test]
    fn metadata_without_a_picture_size_is_unplayable() {
      // What WebKitGTK reports for an H.264 video without a decoder (#804).
      let without_decoder = PlaybackState {
        has_picture: false,
        has_frame: true,
        ..loaded()
      };
      assert!(without_decoder.lacks_picture());
      assert!(StallCheck::Metadata.is_stalled(without_decoder));
      assert!(!loaded().lacks_picture());
      assert!(!nothing_loaded().lacks_picture(), "not known before the metadata");
    }

    #[test]
    fn sound_without_a_picture_is_not_playing() {
      let sound_only = PlaybackState {
        is_paused: false,
        has_picture: false,
        current_seconds: 4.0,
        ..loaded()
      };
      assert!(StallCheck::Start { from_seconds: 0.0 }.is_stalled(sound_only));
    }

    #[test]
    fn playing_at_zero_without_a_frame_is_stalled() {
      let playing = PlaybackState {
        is_paused: false,
        ..loaded()
      };
      assert!(StallCheck::Start { from_seconds: 0.0 }.is_stalled(playing));
    }

    #[test]
    fn a_frame_or_a_moving_clock_is_playing() {
      let playing = PlaybackState {
        is_paused: false,
        ..loaded()
      };
      let check = StallCheck::Start { from_seconds: 3.0 };
      assert!(!check.is_stalled(PlaybackState {
        has_frame: true,
        current_seconds: 3.0,
        ..playing
      }));
      assert!(!check.is_stalled(PlaybackState {
        current_seconds: 3.5,
        ..playing
      }));
    }

    #[test]
    fn pausing_seeking_or_the_end_is_not_a_stall() {
      let check = StallCheck::Start { from_seconds: 0.0 };
      for state in [
        PlaybackState {
          is_paused: true,
          ..nothing_loaded()
        },
        PlaybackState {
          is_paused: false,
          is_seeking: true,
          ..nothing_loaded()
        },
        PlaybackState {
          is_paused: false,
          is_ended: true,
          ..nothing_loaded()
        },
      ] {
        assert!(!check.is_stalled(state), "{state:?}");
      }
    }

    fn nothing_loaded() -> PlaybackState {
      PlaybackState {
        has_metadata: false,
        has_frame: false,
        has_picture: false,
        is_paused: true,
        is_ended: false,
        is_seeking: false,
        current_seconds: 0.0,
      }
    }

    /// Length and size known, nothing decoded yet.
    fn loaded() -> PlaybackState {
      PlaybackState {
        has_metadata: true,
        has_picture: true,
        ..nothing_loaded()
      }
    }
  }
}
