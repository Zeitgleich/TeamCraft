//! What the operating system knows about the user for the attention report (notifications.md,
//! "Attention"): time since the last input anywhere and whether the screen is locked. The webview
//! combines it with focus, visibility and in-app input.
//!
//! | Platform | Idle time                                          | Screen lock                              |
//! |----------|----------------------------------------------------|------------------------------------------|
//! | Linux    | GNOME idle monitor, else freedesktop screen saver  | logind `LockedHint`, screen saver active |
//! | macOS    | Core Graphics event source                         | not read; locking takes the focus away   |
//! | Windows  | `GetLastInputInfo` (`user-idle-time`)              | not read; locking takes the focus away   |

use serde::Serialize;
use tauri::State;

/// The answer of [`os_attention`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct OsAttention {
  /// Milliseconds since the last input on this computer; absent where the platform doesn't tell.
  #[serde(rename = "idle_ms", skip_serializing_if = "Option::is_none")]
  pub maybe_idle_ms: Option<u64>,
  pub is_locked: bool,
}

/// Connections the probes keep between calls.
#[derive(Default)]
pub struct OsProbe {
  #[cfg(target_os = "linux")]
  buses: std::sync::Arc<crate::freedesktop::Buses>,
}

#[tauri::command(rename_all = "snake_case")]
pub async fn os_attention(probe: State<'_, OsProbe>) -> Result<OsAttention, ()> {
  Ok(probe.read().await)
}

impl OsProbe {
  #[cfg(target_os = "linux")]
  pub fn buses(&self) -> std::sync::Arc<crate::freedesktop::Buses> {
    self.buses.clone()
  }

  async fn read(&self) -> OsAttention {
    #[cfg(target_os = "linux")]
    {
      let state = self.buses.desktop_state().await;
      OsAttention {
        maybe_idle_ms: state.maybe_idle.map(millis),
        is_locked: state.is_locked,
      }
    }
    #[cfg(target_os = "macos")]
    {
      use objc2_core_graphics::{CGEventSource, CGEventSourceStateID, CGEventType};
      // kCGAnyInputEventType: the newest input of any kind.
      let any_input = CGEventType(u32::MAX);
      let seconds =
        CGEventSource::seconds_since_last_event_type(CGEventSourceStateID::CombinedSessionState, any_input);
      let maybe_idle =
        (seconds.is_finite() && seconds >= 0.0).then(|| std::time::Duration::from_secs_f64(seconds));
      OsAttention {
        maybe_idle_ms: maybe_idle.map(millis),
        is_locked: false,
      }
    }
    #[cfg(target_os = "windows")]
    {
      let maybe_idle = user_idle_time::get_idle_time()
        .inspect_err(|error| tracing::debug!(%error, "no idle time"))
        .ok();
      OsAttention {
        maybe_idle_ms: maybe_idle.map(millis),
        is_locked: false,
      }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
      OsAttention::default()
    }
  }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn millis(duration: std::time::Duration) -> u64 {
  u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_webview_reads_idle_time_and_lock() {
    let answer = OsAttention {
      maybe_idle_ms: Some(1500),
      is_locked: true,
    };
    assert_eq!(
      serde_json::to_value(answer).unwrap(),
      serde_json::json!({ "idle_ms": 1500, "is_locked": true })
    );
    assert_eq!(
      serde_json::to_value(OsAttention::default()).unwrap(),
      serde_json::json!({ "is_locked": false })
    );
  }
}
