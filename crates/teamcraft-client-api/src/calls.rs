//! View models and session state for calls, huddles and the pre-join screen. Media itself lives in
//! `teamcraft-media`; these types only describe what the UI shows and lets the user pick.

use serde::{Deserialize, Serialize};

use crate::model::{ConversationId, UserId};

/// What kind of call a conversation is joined in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallKind {
  /// Ring-style 1:1 call from a DM.
  Direct,
  /// Group call in a group DM or channel.
  Group,
  /// Drop-in audio room in a channel; the UI stays in the channel while it runs.
  Huddle,
}

/// Arrangement of the tiles in the call screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallLayout {
  Gallery,
  ActiveSpeaker,
}

impl CallLayout {
  pub fn slug(self) -> &'static str {
    match self {
      Self::Gallery => "gallery",
      Self::ActiveSpeaker => "speaker",
    }
  }
}

/// Background shown behind the user's camera image (applied on the device, before encoding).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundEffect {
  #[default]
  None,
  BlurLight,
  BlurStrong,
  Preset(BackgroundPreset),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundPreset {
  Office,
  Bookshelf,
  Sunrise,
  Forest,
}

impl BackgroundEffect {
  pub const ALL: [BackgroundEffect; 7] = [
    Self::None,
    Self::BlurLight,
    Self::BlurStrong,
    Self::Preset(BackgroundPreset::Office),
    Self::Preset(BackgroundPreset::Bookshelf),
    Self::Preset(BackgroundPreset::Sunrise),
    Self::Preset(BackgroundPreset::Forest),
  ];

  /// Stable identifier, also used in CSS class names and i18n keys.
  pub fn slug(self) -> &'static str {
    match self {
      Self::None => "none",
      Self::BlurLight => "blur-light",
      Self::BlurStrong => "blur-strong",
      Self::Preset(BackgroundPreset::Office) => "office",
      Self::Preset(BackgroundPreset::Bookshelf) => "bookshelf",
      Self::Preset(BackgroundPreset::Sunrise) => "sunrise",
      Self::Preset(BackgroundPreset::Forest) => "forest",
    }
  }
}

/// One selectable input or output device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaDevice {
  pub id: String,
  pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MediaDevices {
  pub cameras: Vec<MediaDevice>,
  pub microphones: Vec<MediaDevice>,
  pub speakers: Vec<MediaDevice>,
}

/// A remote participant as the media layer reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Participant {
  pub user: UserId,
  pub is_muted: bool,
  pub is_camera_on: bool,
  pub has_hand_raised: bool,
  pub is_sharing_screen: bool,
}

/// A running drop-in huddle in a channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HuddleInfo {
  pub participants: Vec<UserId>,
}

/// A call someone started that the user can accept or decline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IncomingCall {
  pub caller: UserId,
  pub conversation: ConversationId,
  pub with_video: bool,
}

/// What the user is about to join.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallRequest {
  pub conversation: ConversationId,
  pub kind: CallKind,
}

/// The call this device is in, as the media layer reports it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveCall {
  pub conversation: ConversationId,
  pub kind: CallKind,
  pub is_muted: bool,
  pub is_deafened: bool,
  /// End-to-end encryption is set up; media flows only then.
  pub is_secured: bool,
  /// Group code to compare out of band (DAVE privacy code), once secured.
  pub maybe_privacy_code: Option<String>,
  pub participants: Vec<Participant>,
  /// Whose audio is forwarded right now, loudest first.
  pub active_speakers: Vec<UserId>,
  #[serde(default)]
  pub is_camera_on: bool,
  #[serde(default)]
  pub is_sharing_screen: bool,
  /// The effect behind this device's camera image.
  #[serde(default)]
  pub background: BackgroundEffect,
  /// The meeting host who last muted everyone (cleared when the user unmutes).
  #[serde(default)]
  pub maybe_muted_by_host: Option<UserId>,
}

/// Why the last call ended other than by leaving.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CallEndReason {
  RemovedByHost { by: UserId },
  EndedByHost { by: UserId },
}

/// Push-to-talk: the microphone is open only while the shortcut is held.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushToTalk {
  pub is_enabled: bool,
  /// Accelerator string, e.g. `CommandOrControl+Shift+Space`.
  pub shortcut: String,
  /// The shortcut works while another app has focus. `false` where the desktop doesn't allow
  /// global shortcuts (e.g. Wayland): the UI then handles the key inside its window, like the
  /// browser does.
  pub is_global: bool,
  /// The shortcut is held right now.
  pub is_talking: bool,
}

impl Default for PushToTalk {
  fn default() -> Self {
    Self {
      is_enabled: false,
      shortcut: "CommandOrControl+Shift+Space".into(),
      is_global: false,
      is_talking: false,
    }
  }
}

/// Another device's video track in the active call, to watch (desktop: `call_watch_video`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteVideoTrack {
  pub stream_id: teamcraft_proto::MediaStreamId,
  pub user: UserId,
  pub kind: teamcraft_proto::VideoTrackKind,
  /// (width, height) of each simulcast layer, lowest first; pick `max_layer` by tile size.
  pub layers: Vec<(u16, u16)>,
}

/// A call running in a conversation (server roster), whether or not this device is in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningCall {
  pub conversation: ConversationId,
  pub participants: Vec<UserId>,
  /// DMs ring until someone else joins.
  pub is_ringing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPhase {
  Idle,
  /// Pre-join screen: pick devices and effects, then join.
  PreJoin,
  Active,
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn effect_slugs_are_unique() {
    let mut slugs: Vec<&str> = BackgroundEffect::ALL.iter().map(|effect| effect.slug()).collect();
    slugs.sort_unstable();
    slugs.dedup();
    assert_eq!(slugs.len(), BackgroundEffect::ALL.len());
  }
}
