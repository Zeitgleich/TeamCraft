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

/// A built-in background: a stock photo (`assets/backgrounds/SOURCES.md` lists where each comes
/// from) or one of the drawn illustrations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundPreset {
  // Photos.
  MeetingRoom,
  OpenPlanOffice,
  HomeOffice,
  MinimalOffice,
  LivingRoom,
  BrightRoom,
  LibraryShelves,
  Loft,
  SofaBeige,
  WhitePanels,
  Lounge,
  GreyLounge,
  OrangeWall,
  LibraryAisle,
  EmptyRoom,
  LakeMorning,
  // Illustrations, drawn by code (teamcraft-media).
  Office,
  Bookshelf,
  Sunrise,
  Forest,
}

/// Where the picker puts an effect: the groups are separated by a heading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EffectGroup {
  Basic,
  Photos,
  Illustrations,
  Own,
}

impl BackgroundPreset {
  pub const PHOTOS: [BackgroundPreset; 16] = [
    Self::MeetingRoom,
    Self::OpenPlanOffice,
    Self::HomeOffice,
    Self::MinimalOffice,
    Self::LivingRoom,
    Self::BrightRoom,
    Self::LibraryShelves,
    Self::Loft,
    Self::SofaBeige,
    Self::WhitePanels,
    Self::Lounge,
    Self::GreyLounge,
    Self::OrangeWall,
    Self::LibraryAisle,
    Self::EmptyRoom,
    Self::LakeMorning,
  ];
  pub const ILLUSTRATIONS: [BackgroundPreset; 4] =
    [Self::Office, Self::Bookshelf, Self::Sunrise, Self::Forest];

  /// Stable identifier, also the file name of a photo's picture and thumbnail
  /// (`assets/backgrounds/<slug>.jpg`) and part of its i18n key.
  pub fn slug(self) -> &'static str {
    match self {
      Self::MeetingRoom => "meeting-room",
      Self::OpenPlanOffice => "open-plan-office",
      Self::HomeOffice => "home-office",
      Self::MinimalOffice => "minimal-office",
      Self::LivingRoom => "living-room",
      Self::BrightRoom => "bright-room",
      Self::LibraryShelves => "library-shelves",
      Self::Loft => "loft",
      Self::SofaBeige => "sofa-beige",
      Self::WhitePanels => "white-panels",
      Self::Lounge => "lounge",
      Self::GreyLounge => "grey-lounge",
      Self::OrangeWall => "orange-wall",
      Self::LibraryAisle => "library-aisle",
      Self::EmptyRoom => "empty-room",
      Self::LakeMorning => "lake-morning",
      Self::Office => "office",
      Self::Bookshelf => "bookshelf",
      Self::Sunrise => "sunrise",
      Self::Forest => "forest",
    }
  }

  pub fn is_photo(self) -> bool {
    Self::PHOTOS.contains(&self)
  }
}

impl BackgroundEffect {
  /// Every shipped effect in the picker's order: none, blur, photos, illustrations.
  pub const ALL: [BackgroundEffect;
    3 + BackgroundPreset::PHOTOS.len() + BackgroundPreset::ILLUSTRATIONS.len()] = {
    let mut all = [Self::None; 3 + BackgroundPreset::PHOTOS.len() + BackgroundPreset::ILLUSTRATIONS.len()];
    all[1] = Self::BlurLight;
    all[2] = Self::BlurStrong;
    let mut index = 0;
    while index < BackgroundPreset::PHOTOS.len() {
      all[3 + index] = Self::Preset(BackgroundPreset::PHOTOS[index]);
      index += 1;
    }
    let mut index = 0;
    while index < BackgroundPreset::ILLUSTRATIONS.len() {
      all[3 + BackgroundPreset::PHOTOS.len() + index] = Self::Preset(BackgroundPreset::ILLUSTRATIONS[index]);
      index += 1;
    }
    all
  };

  /// Stable identifier, also used in CSS class names and i18n keys.
  pub fn slug(self) -> &'static str {
    match self {
      Self::None => "none",
      Self::BlurLight => "blur-light",
      Self::BlurStrong => "blur-strong",
      Self::Preset(preset) => preset.slug(),
      Self::Custom(_) => "custom",
      Self::Org(_) => "org",
    }
  }

  pub fn group(self) -> EffectGroup {
    match self {
      Self::None | Self::BlurLight | Self::BlurStrong => EffectGroup::Basic,
      Self::Preset(preset) if preset.is_photo() => EffectGroup::Photos,
      Self::Preset(_) => EffectGroup::Illustrations,
      Self::Custom(_) | Self::Org(_) => EffectGroup::Own,
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
  /// Whether there is a camera at all, beyond the named ones (browsers name devices only once
  /// the page may use them). Older shells send nothing: [`DevicePresence::Unknown`].
  #[serde(default)]
  pub camera_presence: DevicePresence,
  #[serde(default)]
  pub microphone_presence: DevicePresence,
}

/// What listing the devices found for one kind of input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevicePresence {
  /// At least one device, named in the list.
  Present,
  /// At least one device, unnamed until the user allows the page to use it.
  Hidden,
  /// There is a device, but the user or a site setting refused access to it.
  Blocked,
  /// No device of this kind.
  Missing,
  /// Not listed yet, or a platform that doesn't say (also what a newer value reads as).
  #[default]
  #[serde(other)]
  Unknown,
}

/// A browser's permission for the camera or the microphone (`navigator.permissions.query`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserPermission {
  Granted,
  Denied,
  /// The user hasn't been asked yet (or dismissed the question).
  Prompt,
  /// The browser can't say (no Permissions API, or it doesn't know the name).
  Unknown,
}

impl BrowserPermission {
  /// From `PermissionStatus.state`.
  pub fn from_state(state: &str) -> Self {
    match state {
      "granted" => Self::Granted,
      "denied" => Self::Denied,
      "prompt" => Self::Prompt,
      _ => Self::Unknown,
    }
  }
}

impl MediaDevices {
  /// Devices from a platform that names every device it lists (the desktop app).
  pub fn listed(
    cameras: Vec<MediaDevice>,
    microphones: Vec<MediaDevice>,
    speakers: Vec<MediaDevice>,
  ) -> Self {
    Self {
      camera_presence: DevicePresence::of_named(&cameras),
      microphone_presence: DevicePresence::of_named(&microphones),
      cameras,
      microphones,
      speakers,
    }
  }
}

impl DevicePresence {
  /// From a browser's listing of one kind of input (`enumerateDevices`), its permission for that
  /// kind, and whether the listing had any device at all.
  ///
  /// Browsers name devices only once the page may use them; until then they list one unnamed
  /// device per kind that exists. A browser that lists nothing at all before permission tells
  /// nothing, so an empty kind counts as missing only when the permission is granted or the
  /// browser listed other devices.
  pub fn in_browser<'a>(
    ids: impl IntoIterator<Item = &'a str>,
    permission: BrowserPermission,
    has_listed_any: bool,
  ) -> Self {
    match (Self::from_ids(ids), permission) {
      (Self::Present, _) => Self::Present,
      (_, BrowserPermission::Denied) => Self::Blocked,
      (Self::Missing, BrowserPermission::Granted) => Self::Missing,
      (Self::Missing, _) if has_listed_any => Self::Missing,
      (Self::Missing, _) => Self::Unknown,
      (other, _) => other,
    }
  }

  /// From a listing's device ids. Browsers list a device with an empty id (one per kind) until
  /// the page may use that kind.
  pub fn from_ids<'a>(ids: impl IntoIterator<Item = &'a str>) -> Self {
    let mut presence = Self::Missing;
    for id in ids {
      if !id.is_empty() {
        return Self::Present;
      }
      presence = Self::Hidden;
    }
    presence
  }

  /// Whether the device can't be used at all: none there, or access refused.
  pub fn is_unavailable(self) -> bool {
    matches!(self, Self::Missing | Self::Blocked)
  }

  fn of_named(devices: &[MediaDevice]) -> Self {
    if devices.is_empty() {
      Self::Missing
    } else {
      Self::Present
    }
  }
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

/// Why this app can't take part in calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallsUnavailableReason {
  /// This app has no call support at all.
  NotInThisApp,
  /// The browser lacks what calls need (WebTransport, WebCodecs audio, AudioWorklet).
  BrowserUnsupported,
  /// TeamCraft is open in another tab of this browser, which runs the account; calls run there.
  OtherTab,
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
  /// Whether this device's screen share carries the computer's sound.
  #[serde(default)]
  pub screen_sound: ScreenShareSound,
  /// This device lost its connection and is rejoining; media pauses meanwhile.
  #[serde(default)]
  pub is_reconnecting: bool,
  /// Others whose connection was lost and who are rejoining.
  #[serde(default)]
  pub reconnecting: Vec<UserId>,
}

/// The sound of a screen share (the computer's playback without the call itself).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenShareSound {
  #[default]
  Off,
  Shared,
  /// Asked for, but this system can't capture it (no permission, an older system, or the
  /// Flatpak without PipeWire access); the picture is shared without it.
  Unavailable,
  #[serde(other)]
  Unknown,
}

/// Which call status sounds play: the soft repeating tone while this device reconnects, the cues
/// when it is back or the call is lost, and the quieter one when someone else drops.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallStatusSounds {
  #[default]
  All,
  /// Only the sounds about this device's own connection.
  OwnOnly,
  Off,
  #[serde(other)]
  Unknown,
}

impl CallStatusSounds {
  pub fn slug(self) -> &'static str {
    match self {
      Self::All | Self::Unknown => "all",
      Self::OwnOnly => "own_only",
      Self::Off => "off",
    }
  }

  pub fn from_slug(slug: &str) -> Self {
    match slug {
      "own_only" => Self::OwnOnly,
      "off" => Self::Off,
      _ => Self::All,
    }
  }
}

/// Why the last call ended other than by leaving.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum CallEndReason {
  RemovedByHost {
    by: UserId,
  },
  EndedByHost {
    by: UserId,
  },
  /// The connection broke and rejoining didn't succeed within the grace period.
  ConnectionLost,
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
mod device_tests {
  use super::*;

  #[test]
  fn browser_listings_tell_hidden_devices_from_missing_ones() {
    assert_eq!(DevicePresence::from_ids([]), DevicePresence::Missing);
    assert_eq!(DevicePresence::from_ids([""]), DevicePresence::Hidden);
    assert_eq!(DevicePresence::from_ids(["", "cam-1"]), DevicePresence::Present);
    assert_eq!(
      DevicePresence::from_ids(["default", "mic-1"]),
      DevicePresence::Present
    );
  }

  #[test]
  fn an_empty_kind_is_missing_only_when_the_browser_would_have_listed_it() {
    use BrowserPermission::{Granted, Prompt, Unknown};
    // Allowed, or the browser listed other devices (unnamed ones per kind before permission).
    assert_eq!(
      DevicePresence::in_browser([], Granted, false),
      DevicePresence::Missing
    );
    assert_eq!(
      DevicePresence::in_browser([], Prompt, true),
      DevicePresence::Missing
    );
    // A browser that lists nothing before permission tells nothing.
    assert_eq!(
      DevicePresence::in_browser([], Prompt, false),
      DevicePresence::Unknown
    );
    assert_eq!(
      DevicePresence::in_browser([], Unknown, false),
      DevicePresence::Unknown
    );
  }

  #[test]
  fn a_refused_permission_blocks_unless_devices_are_named() {
    use BrowserPermission::{Denied, Prompt};
    assert_eq!(
      DevicePresence::in_browser([""], Denied, true),
      DevicePresence::Blocked
    );
    assert_eq!(
      DevicePresence::in_browser([], Denied, false),
      DevicePresence::Blocked
    );
    assert_eq!(
      DevicePresence::in_browser([""], Prompt, true),
      DevicePresence::Hidden
    );
    assert_eq!(
      DevicePresence::in_browser(["cam-1"], Denied, true),
      DevicePresence::Present
    );
    assert_eq!(BrowserPermission::from_state("denied"), Denied);
    assert_eq!(BrowserPermission::from_state("later"), BrowserPermission::Unknown);
  }

  #[test]
  fn named_listings_are_present_or_missing() {
    let camera = MediaDevice {
      id: "cam-1".into(),
      label: "Camera".into(),
    };
    let devices = MediaDevices::listed(vec![camera], Vec::new(), Vec::new());
    assert_eq!(devices.camera_presence, DevicePresence::Present);
    assert_eq!(devices.microphone_presence, DevicePresence::Missing);
  }

  #[test]
  fn presence_is_forward_compatible_on_the_wire() {
    let old: MediaDevices = serde_json::from_str(r#"{"cameras":[],"microphones":[],"speakers":[]}"#).unwrap();
    assert_eq!(old.camera_presence, DevicePresence::Unknown);
    let newer: MediaDevices = serde_json::from_str(
      r#"{"cameras":[],"microphones":[],"speakers":[],"camera_presence":"on_mars","microphone_presence":"blocked"}"#,
    )
    .unwrap();
    assert_eq!(newer.camera_presence, DevicePresence::Unknown);
    assert_eq!(newer.microphone_presence, DevicePresence::Blocked);
    assert_eq!(
      serde_json::to_value(DevicePresence::Missing).unwrap(),
      serde_json::json!("missing")
    );
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

#[cfg(test)]
mod preset_tests {
  use std::collections::HashSet;
  use std::path::Path;

  use super::*;

  #[test]
  fn effects_list_each_preset_once_in_picker_order() {
    let slugs: Vec<&str> = BackgroundEffect::ALL.iter().map(|effect| effect.slug()).collect();
    assert_eq!(slugs.iter().collect::<HashSet<_>>().len(), slugs.len());
    assert_eq!(&slugs[..3], ["none", "blur-light", "blur-strong"]);
    let groups: Vec<EffectGroup> = BackgroundEffect::ALL
      .iter()
      .map(|effect| effect.group())
      .collect();
    assert!(groups.is_sorted_by_key(|group| *group as u8));
    assert_eq!(
      groups
        .iter()
        .filter(|group| **group == EffectGroup::Photos)
        .count(),
      BackgroundPreset::PHOTOS.len()
    );
    assert!(BackgroundEffect::ALL.contains(&BackgroundEffect::Preset(BackgroundPreset::Forest)));
  }

  #[test]
  fn every_photo_has_a_picture_and_a_thumbnail() {
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/backgrounds");
    for preset in BackgroundPreset::PHOTOS {
      let name = format!("{}.jpg", preset.slug());
      assert!(assets.join(&name).is_file(), "missing {name}");
      assert!(
        assets.join("thumbs").join(&name).is_file(),
        "missing thumbs/{name}"
      );
    }
  }

  #[test]
  fn presets_keep_their_wire_names() {
    // Stored preferences and policies contain these; never rename without a migration.
    for (preset, wire) in [
      (BackgroundPreset::Office, "\"office\""),
      (BackgroundPreset::Forest, "\"forest\""),
      (BackgroundPreset::LivingRoom, "\"living_room\""),
      (BackgroundPreset::LakeMorning, "\"lake_morning\""),
    ] {
      assert_eq!(serde_json::to_string(&preset).unwrap(), wire);
    }
  }
}
