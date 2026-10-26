//! The app's own video controls (#638), kept apart from the view so they can be tested without a
//! browser: which key does what, and where the scrubber's played and buffered parts end. The
//! platform's native controls differ per webview (WebKitGTK's GStreamer bar, WKWebView, WebView2),
//! so the player draws its own on every platform.

/// Seconds the arrow keys seek.
pub const SEEK_STEP_SECONDS: f64 = 5.0;
/// Volume step of the up and down arrow keys.
pub const VOLUME_STEP: f64 = 0.1;

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
}
