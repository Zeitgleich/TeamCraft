//! View models and session state for calls, huddles and the pre-join screen. Media itself lives in
//! `teamcraft-media`; these types only describe what the UI shows and lets the user pick.

use crate::model::{ConversationId, UserId};

/// What kind of call a conversation is joined in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BackgroundEffect {
  None,
  BlurLight,
  BlurStrong,
  Preset(BackgroundPreset),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaDevice {
  pub id: String,
  pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct MediaDevices {
  pub cameras: Vec<MediaDevice>,
  pub microphones: Vec<MediaDevice>,
  pub speakers: Vec<MediaDevice>,
}

/// A remote participant as the media layer reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Participant {
  pub user: UserId,
  pub is_muted: bool,
  pub is_camera_on: bool,
  pub has_hand_raised: bool,
  pub is_sharing_screen: bool,
}

/// A running drop-in huddle in a channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HuddleInfo {
  pub participants: Vec<UserId>,
}

/// A call someone started that the user can accept or decline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IncomingCall {
  pub caller: UserId,
  pub conversation: ConversationId,
  pub with_video: bool,
}

/// What the user is about to join.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallRequest {
  pub conversation: ConversationId,
  pub kind: CallKind,
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
