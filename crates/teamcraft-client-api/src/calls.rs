//! View models and session state for calls, huddles and the pre-join screen. Media itself lives in
//! `teamcraft-media`; these types only describe what the UI shows and lets the user pick.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{BackgroundId, CallBackgroundMode, OrgBackgroundId};

use crate::model::{ConversationId, TimestampMs, UserId};

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
  /// One of the user's own pictures ([`BackgroundImageInfo`]).
  Custom(BackgroundId),
  /// One of the organization's pictures ([`OrgBackgrounds`]).
  Org(OrgBackgroundId),
}

/// The organization's call backgrounds and the rule for them (admin.md §3.5), from the
/// owner-signed policy the client verified.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgBackgrounds {
  pub mode: CallBackgroundMode,
  /// Whether guests are exempt (shown to admins; `applies_to_me` already accounts for it).
  #[serde(default = "is_exempt_by_default")]
  pub guests_exempt: bool,
  /// In the order the picker shows them (before presets and own pictures).
  pub images: Vec<OrgBackgroundInfo>,
  #[serde(default)]
  pub maybe_default: Option<OrgBackgroundId>,
  /// Whether the rule applies to the signed-in user (guests may be exempt).
  pub applies_to_me: bool,
}

/// What an owner sets for the organization's backgrounds (the pictures themselves are added and
/// removed one by one).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgBackgroundRule {
  pub mode: CallBackgroundMode,
  #[serde(default)]
  pub maybe_default: Option<OrgBackgroundId>,
  pub guests_exempt: bool,
  /// The pictures in their new order (all of them, each once).
  pub order: Vec<OrgBackgroundId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrgBackgroundInfo {
  pub id: OrgBackgroundId,
  pub name: String,
}

fn is_exempt_by_default() -> bool {
  true
}

impl Default for OrgBackgrounds {
  fn default() -> Self {
    Self {
      mode: CallBackgroundMode::Free,
      guests_exempt: true,
      images: Vec::new(),
      maybe_default: None,
      applies_to_me: false,
    }
  }
}

impl OrgBackgrounds {
  /// The rule that applies to this user; a mode this client doesn't know is treated as
  /// `org_only` (restrictive, but leaves blur and no background).
  pub fn rule(&self) -> CallBackgroundMode {
    match &self.mode {
      _ if !self.applies_to_me => CallBackgroundMode::Free,
      CallBackgroundMode::Unknown(_) => CallBackgroundMode::OrgOnly,
      mode => mode.clone(),
    }
  }

  /// Whether the picker may offer `effect`.
  pub fn allows(&self, effect: BackgroundEffect) -> bool {
    let is_listed = |id: OrgBackgroundId| self.images.iter().any(|image| image.id == id);
    match (self.rule(), effect) {
      (_, BackgroundEffect::Org(id)) => is_listed(id),
      (CallBackgroundMode::Required, _) => false,
      (CallBackgroundMode::OrgOnly, effect) => {
        matches!(
          effect,
          BackgroundEffect::None | BackgroundEffect::BlurLight | BackgroundEffect::BlurStrong
        )
      },
      _ => true,
    }
  }

  /// Whether the camera may only send with an organization background behind the person.
  pub fn is_required(&self) -> bool {
    self.rule() == CallBackgroundMode::Required
  }

  /// What the camera uses when the user asked for `requested`: the request if allowed, else the
  /// default organization background (or the first one if a background is required), else none.
  pub fn effective(&self, requested: BackgroundEffect) -> BackgroundEffect {
    if self.allows(requested) {
      return requested;
    }
    let fallback = self
      .maybe_default
      .filter(|id| self.images.iter().any(|image| image.id == *id))
      .or_else(|| {
        self
          .is_required()
          .then(|| self.images.first().map(|image| image.id))
          .flatten()
      });
    fallback.map_or(BackgroundEffect::None, BackgroundEffect::Org)
  }
}

/// One of the user's own background pictures, as a picker lists it (encrypted on the server,
/// synced to the user's devices).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackgroundImageInfo {
  pub id: BackgroundId,
  pub name: String,
  pub size_bytes: u64,
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
      Self::Custom(_) => "custom",
      Self::Org(_) => "org",
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

  mod org_backgrounds {
    use super::*;

    fn org(mode: CallBackgroundMode, maybe_default: Option<&str>, applies_to_me: bool) -> OrgBackgrounds {
      OrgBackgrounds {
        mode,
        guests_exempt: true,
        images: ["1", "2"]
          .iter()
          .map(|id| OrgBackgroundInfo {
            id: id.parse().unwrap(),
            name: format!("Picture {id}"),
          })
          .collect(),
        maybe_default: maybe_default.map(|id| id.parse().unwrap()),
        applies_to_me,
      }
    }

    fn picture(id: &str) -> BackgroundEffect {
      BackgroundEffect::Org(id.parse().unwrap())
    }

    #[test]
    fn free_allows_everything_listed() {
      let policy = org(CallBackgroundMode::Free, None, true);
      for effect in BackgroundEffect::ALL {
        assert!(policy.allows(effect), "{effect:?}");
      }
      assert!(policy.allows(BackgroundEffect::Custom("9".parse().unwrap())));
      assert!(policy.allows(picture("2")));
      assert!(
        !policy.allows(picture("3")),
        "an organization picture that was removed"
      );
      assert_eq!(policy.effective(picture("3")), BackgroundEffect::None);
    }

    #[test]
    fn org_only_leaves_blur_and_none() {
      let policy = org(CallBackgroundMode::OrgOnly, Some("2"), true);
      assert!(policy.allows(BackgroundEffect::BlurStrong) && policy.allows(BackgroundEffect::None));
      assert!(!policy.allows(BackgroundEffect::Preset(BackgroundPreset::Forest)));
      assert!(!policy.allows(BackgroundEffect::Custom("9".parse().unwrap())));
      assert_eq!(
        policy.effective(BackgroundEffect::Custom("9".parse().unwrap())),
        picture("2")
      );
      assert!(!policy.is_required());
      assert_eq!(
        org(CallBackgroundMode::OrgOnly, None, true)
          .effective(BackgroundEffect::Preset(BackgroundPreset::Office)),
        BackgroundEffect::None
      );
    }

    #[test]
    fn required_always_ends_with_an_org_picture() {
      let policy = org(CallBackgroundMode::Required, None, true);
      assert!(policy.is_required());
      assert!(!policy.allows(BackgroundEffect::None) && !policy.allows(BackgroundEffect::BlurLight));
      assert_eq!(policy.effective(BackgroundEffect::None), picture("1"));
      assert_eq!(policy.effective(picture("2")), picture("2"));
      let with_default = org(CallBackgroundMode::Required, Some("2"), true);
      assert_eq!(with_default.effective(BackgroundEffect::BlurLight), picture("2"));
    }

    #[test]
    fn exempt_users_and_unknown_modes() {
      let guest = org(CallBackgroundMode::Required, None, false);
      assert!(!guest.is_required() && guest.allows(BackgroundEffect::None));
      let newer = org(CallBackgroundMode::Unknown("by_team".into()), None, true);
      assert_eq!(newer.rule(), CallBackgroundMode::OrgOnly);
      assert!(newer.allows(BackgroundEffect::BlurLight));
    }
  }

  #[test]
  fn own_backgrounds_have_a_stable_wire_format() {
    let effect = BackgroundEffect::Custom("7".parse().unwrap());
    assert_eq!(
      serde_json::to_value(effect).unwrap(),
      serde_json::json!({"custom": "7"})
    );
    assert_eq!(effect.slug(), "custom");
    let preset: BackgroundEffect = serde_json::from_value(serde_json::json!({"preset": "forest"})).unwrap();
    assert_eq!(preset, BackgroundEffect::Preset(BackgroundPreset::Forest));
  }

  use super::*;

  #[test]
  fn effect_slugs_are_unique() {
    let mut slugs: Vec<&str> = BackgroundEffect::ALL.iter().map(|effect| effect.slug()).collect();
    slugs.sort_unstable();
    slugs.dedup();
    assert_eq!(slugs.len(), BackgroundEffect::ALL.len());
  }
}

/// How a call in the history relates to the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallDirection {
  /// The call rang the user and they never joined.
  Missed,
  /// Somebody else started it and the user joined.
  Incoming,
  /// The user started it.
  Outgoing,
}

/// One call of the history list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallHistoryItem {
  /// The call's ID as text (the server's ID, used to page and as the list key).
  pub id: String,
  pub conversation: ConversationId,
  pub direction: CallDirection,
  pub started_at: TimestampMs,
  /// Absent while the call goes on.
  #[serde(default)]
  pub maybe_ended_at: Option<TimestampMs>,
  /// Who started it, if known.
  #[serde(default)]
  pub maybe_started_by: Option<UserId>,
  /// Everybody who joined at some point.
  pub participants: Vec<UserId>,
}

impl CallHistoryItem {
  /// How long the call lasted; `None` while it goes on.
  pub fn duration_ms(&self) -> Option<i64> {
    self.maybe_ended_at.map(|ended| (ended - self.started_at).max(0))
  }

  pub fn is_ongoing(&self) -> bool {
    self.maybe_ended_at.is_none()
  }
}

#[cfg(test)]
mod history_tests {
  use super::*;
  use crate::model::id_from_raw;

  #[test]
  fn duration_is_known_once_the_call_ended() {
    let mut call = CallHistoryItem {
      id: "1".into(),
      conversation: id_from_raw(1),
      direction: CallDirection::Outgoing,
      started_at: 1_000,
      maybe_ended_at: None,
      maybe_started_by: None,
      participants: Vec::new(),
    };
    assert!(call.is_ongoing());
    assert_eq!(call.duration_ms(), None);
    call.maybe_ended_at = Some(61_000);
    assert_eq!(call.duration_ms(), Some(60_000));
    call.maybe_ended_at = Some(0);
    assert_eq!(call.duration_ms(), Some(0));
  }
}
