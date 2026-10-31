//! Call session state of the UI. The view models (devices, participants, effects) live in
//! `teamcraft-client-api`; this file only adds the reactive state around them.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use leptos::prelude::*;

pub use teamcraft_client_api::calls::*;
use teamcraft_client_api::failure::CallFailure;
use teamcraft_client_api::model::{ConversationId, UserId};
use teamcraft_client_api::remote::CoreConnection;
use teamcraft_proto::MediaStreamId;

/// The DM call this device is in moved into a meeting room, announced by `by` in the DM's
/// encryption group with the people they added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveOffer {
  pub from: ConversationId,
  pub to: ConversationId,
  pub by: UserId,
  pub added: Vec<UserId>,
  /// This device asked for the move: it follows without a tap.
  pub is_requested_here: bool,
}

/// What the user did that the call backend has to carry out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallEvent {
  /// Joins with the microphone and camera as chosen (#453).
  Join {
    request: CallRequest,
    media: JoinMedia,
  },
  Leave,
  Decline(ConversationId),
  Muted(bool),
  Deafened(bool),
  /// Takes the call over from the account's other devices, which leave it.
  MoveHere,
  /// The microphone to use from now on, also mid-call (an empty id: the system default).
  Microphone(String),
  /// The speakers to use from now on, also mid-call (an empty id: the system default).
  Speakers(String),
  /// Plays the test sound on these speakers (an empty id: the default).
  TestSpeakers(String),
  /// Lists the devices again.
  RefreshDevices,
  /// Starts, changes or ends the pre-join check of the camera and microphone (#442).
  Preview(CallPreview),
  Camera(bool),
  CameraDevice(String),
  Effect(BackgroundEffect),
  StartScreenShare {
    has_sound: bool,
    mode: ScreenContentMode,
    /// What the user picked; the app keeps it within the workspace's limit (#565).
    resolution: ScreenShareResolution,
  },
  StopScreenShare,
  StatusSounds(CallStatusSounds),
  /// The highest quality the own camera is sent at (the setting, #967).
  CameraQuality(VideoQuality),
  /// How sharp another device's video comes (its tile's quality menu, #967).
  VideoQuality {
    stream_id: MediaStreamId,
    quality: VideoQuality,
  },
}

type CallHandler = Rc<dyn Fn(CallEvent)>;

/// A kind of audio device the call uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioKind {
  Microphone,
  Speakers,
}

/// A microphone or speakers plugged in during a call, which the user may switch to (#986). A
/// headset is one offer for both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioOffer {
  pub label: String,
  pub maybe_microphone: Option<String>,
  pub maybe_speakers: Option<String>,
}

/// What a new listing of the microphones or speakers means for the running call (#986).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioDeviceChange {
  /// The user follows the system default, and the system made another device the default: the
  /// call opens it.
  DefaultMoved(MediaDevice),
  /// A device came that the call doesn't use: the user may switch to it.
  Appeared(MediaDevice),
  /// The chosen device went away: the call falls back to the system default (`None`: there is
  /// none left).
  Gone {
    gone: MediaDevice,
    maybe_default: Option<MediaDevice>,
  },
}

/// One entry of the call's microphone or speaker menu (#986).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioMenuEntry {
  /// What choosing it sends (an empty id: the system default).
  pub id: String,
  /// The device's name; for the system default the name of the device it is now.
  pub label: String,
  /// The entry follows the system default (ours, or a browser's own "default" device).
  pub is_system_default: bool,
  /// The call uses it.
  pub is_checked: bool,
}

/// How long "Call ended" shows before the app is back in the chat (#997).
const CALL_ENDED_SHOWN: Duration = Duration::from_millis(1500);

/// Ids browsers give stand-ins for other devices: Chrome's "default" and Windows'
/// "communications" device. They are no device of their own to offer.
const DEVICE_ALIASES: [&str; 2] = ["default", "communications"];
/// Chrome's own entry for the system default.
const BROWSER_DEFAULT_ID: &str = "default";

impl AudioKind {
  pub const ALL: [Self; 2] = [Self::Microphone, Self::Speakers];

  /// This kind's devices in a listing.
  pub fn of(self, devices: &MediaDevices) -> &[MediaDevice] {
    match self {
      Self::Microphone => &devices.microphones,
      Self::Speakers => &devices.speakers,
    }
  }

  fn event(self, id: String) -> CallEvent {
    match self {
      Self::Microphone => CallEvent::Microphone(id),
      Self::Speakers => CallEvent::Speakers(id),
    }
  }
}

/// Whether a chosen id follows the system default: nothing chosen, or a browser's "default".
pub fn follows_system_default(chosen: &str) -> bool {
  chosen.is_empty() || chosen == BROWSER_DEFAULT_ID
}

/// What changed between two listings of one kind of audio device, for a call that uses `chosen`
/// (#986). The first listing (`previous` empty: browsers name devices only with permission) and
/// an unchanged one mean nothing.
pub fn audio_device_change(
  previous: &[MediaDevice],
  current: &[MediaDevice],
  chosen: &str,
) -> Option<AudioDeviceChange> {
  if previous.is_empty() || previous == current {
    return None;
  }
  let is_listed = |list: &[MediaDevice], id: &str| list.iter().any(|device| device.id == id);
  // A choice that wasn't listed before is one the backend already replaced with the default.
  let follows_default = follows_system_default(chosen) || !is_listed(previous, chosen);
  if !follows_default && !is_listed(current, chosen) {
    let gone = previous.iter().find(|device| device.id == chosen)?.clone();
    return Some(AudioDeviceChange::Gone {
      gone,
      maybe_default: current
        .first()
        .map(|default| real_device(current, default).clone()),
    });
  }
  // The system default is listed first (on the desktop and in browsers); Chrome's own "default"
  // entry keeps its id and changes its name.
  if follows_default
    && let Some(default) = current.first()
    && previous.first() != Some(default)
  {
    return Some(AudioDeviceChange::DefaultMoved(
      real_device(current, default).clone(),
    ));
  }
  current
    .iter()
    .find(|device| {
      !DEVICE_ALIASES.contains(&device.id.as_str()) && device.id != chosen && !is_listed(previous, &device.id)
    })
    .map(|device| AudioDeviceChange::Appeared(device.clone()))
}

/// The device a browser's stand-in ("Default - Jabra Evolve") is: the one whose name ends its
/// name, in whatever language the browser words the rest. Any other device is itself.
fn real_device<'a>(devices: &'a [MediaDevice], device: &'a MediaDevice) -> &'a MediaDevice {
  if !DEVICE_ALIASES.contains(&device.id.as_str()) {
    return device;
  }
  devices
    .iter()
    .filter(|candidate| {
      !DEVICE_ALIASES.contains(&candidate.id.as_str())
        && !candidate.label.is_empty()
        && device.label.ends_with(&candidate.label)
    })
    .max_by_key(|candidate| candidate.label.len())
    .unwrap_or(device)
}

/// The entries of the call's microphone or speaker menu (#986): the system default first (a
/// browser's own "default" device, or ours where the list has none), then every device, the one
/// in use checked. A choice that isn't listed (unplugged, from another computer) is the default,
/// which is what the call uses then.
pub fn audio_menu_entries(devices: &[MediaDevice], chosen: &str) -> Vec<AudioMenuEntry> {
  let has_browser_default = devices.iter().any(|device| device.id == BROWSER_DEFAULT_ID);
  let checked_id = if !follows_system_default(chosen) && devices.iter().any(|device| device.id == chosen) {
    chosen
  } else if has_browser_default {
    BROWSER_DEFAULT_ID
  } else {
    ""
  };
  let ours = (!has_browser_default).then(|| AudioMenuEntry {
    id: String::new(),
    label: devices
      .first()
      .map(|device| device.label.clone())
      .unwrap_or_default(),
    is_system_default: true,
    is_checked: checked_id.is_empty(),
  });
  ours
    .into_iter()
    .chain(
      devices
        .iter()
        // Windows' communications device is the default of another kind of sound.
        .filter(|device| device.id != "communications")
        .map(|device| AudioMenuEntry {
          id: device.id.clone(),
          label: device.label.clone(),
          is_system_default: device.id == BROWSER_DEFAULT_ID,
          is_checked: device.id == checked_id,
        }),
    )
    .collect()
}

/// Session state of the user's own call. Device choices survive between calls.
#[derive(Clone, Copy)]
pub struct CallState {
  pub phase: RwSignal<CallPhase>,
  /// The other side of the 1:1 call left, so it ended for both (#997): the call screen, or the
  /// call pill, says "Call ended" for a moment before the app is back in the chat.
  pub has_ended: RwSignal<bool>,
  pub maybe_request: RwSignal<Option<CallRequest>>,
  pub incoming: RwSignal<Option<IncomingCall>>,
  pub camera_id: RwSignal<String>,
  pub microphone_id: RwSignal<String>,
  pub speaker_id: RwSignal<String>,
  pub is_mic_on: RwSignal<bool>,
  pub is_camera_on: RwSignal<bool>,
  pub effect: RwSignal<BackgroundEffect>,
  pub mirror_self_view: RwSignal<bool>,
  pub hide_self_view: RwSignal<bool>,
  pub layout: RwSignal<CallLayout>,
  /// The call keeps running while the user browses channels; a bar in the channel stays visible.
  pub is_minimized: RwSignal<bool>,
  pub is_sharing_screen: RwSignal<bool>,
  /// Screen shares include the computer's sound (the user's choice, remembered in the settings).
  pub share_sound: RwSignal<bool>,
  /// What a screen share keeps under pressure: resolution (detail) or frame rate (motion).
  pub screen_mode: RwSignal<ScreenContentMode>,
  /// How large a screen share may be: up to 1440p, or 4K where the workspace allows it (#565).
  pub screen_resolution: RwSignal<ScreenShareResolution>,
  pub has_hand_raised: RwSignal<bool>,
  pub is_deafened: RwSignal<bool>,
  /// An announced move of this call into a meeting room the user hasn't followed yet.
  pub move_offer: RwSignal<Option<MoveOffer>>,
  /// Asks for the camera choice: the call screen's camera menu, or the pre-join screen's camera
  /// field (the "Choose camera" of a camera error, #819).
  pub is_camera_menu_open: RwSignal<bool>,
  /// The call screen's menu of microphones and speakers (#986).
  pub is_audio_menu_open: RwSignal<bool>,
  /// A microphone or speakers plugged in during the call, offered on a toast (#986).
  pub maybe_audio_offer: RwSignal<Option<AudioOffer>>,
  /// "Stats for nerds" (#938) over every video of the call.
  pub shows_video_stats: RwSignal<bool>,
  /// The chat panel next to the call's video (#936); it stays open while the call is minimized.
  pub is_chat_open: RwSignal<bool>,
  /// The quality picked for others' videos in this call (#967); Auto where none is listed.
  pub video_qualities: RwSignal<HashMap<MediaStreamId, VideoQuality>>,
  /// The video whose quality menu is open.
  pub maybe_quality_menu: RwSignal<Option<MediaStreamId>>,
  /// The camera the backend was told about or uses on its own ([`CallState::send_camera_choice`]).
  sent_camera: StoredValue<String>,
  /// The microphone and speakers the backend was told about ([`CallState::send_audio_choices`]);
  /// it starts on the system default.
  sent_microphone: StoredValue<String>,
  sent_speakers: StoredValue<String>,
  /// Carries the user's actions to the call backend; set once by the shell.
  handler: StoredValue<Option<CallHandler>, LocalStorage>,
}

impl CallState {
  pub fn new() -> Self {
    Self {
      phase: RwSignal::new(CallPhase::Idle),
      has_ended: RwSignal::new(false),
      maybe_request: RwSignal::new(None),
      incoming: RwSignal::new(None),
      camera_id: RwSignal::new(String::new()),
      microphone_id: RwSignal::new(String::new()),
      speaker_id: RwSignal::new(String::new()),
      is_mic_on: RwSignal::new(true),
      is_camera_on: RwSignal::new(true),
      effect: RwSignal::new(BackgroundEffect::None),
      mirror_self_view: RwSignal::new(true),
      hide_self_view: RwSignal::new(false),
      layout: RwSignal::new(CallLayout::Gallery),
      is_minimized: RwSignal::new(false),
      is_sharing_screen: RwSignal::new(false),
      share_sound: RwSignal::new(false),
      screen_mode: RwSignal::new(ScreenContentMode::default()),
      screen_resolution: RwSignal::new(ScreenShareResolution::default()),
      has_hand_raised: RwSignal::new(false),
      is_deafened: RwSignal::new(false),
      move_offer: RwSignal::new(None),
      is_camera_menu_open: RwSignal::new(false),
      is_audio_menu_open: RwSignal::new(false),
      maybe_audio_offer: RwSignal::new(None),
      shows_video_stats: RwSignal::new(false),
      is_chat_open: RwSignal::new(false),
      video_qualities: RwSignal::new(HashMap::new()),
      maybe_quality_menu: RwSignal::new(None),
      sent_camera: StoredValue::new(String::new()),
      sent_microphone: StoredValue::new(String::new()),
      sent_speakers: StoredValue::new(String::new()),
      handler: StoredValue::new_local(None),
    }
  }

  pub fn set_handler(&self, handler: impl Fn(CallEvent) + 'static) {
    self.handler.set_value(Some(Rc::new(handler)));
  }

  /// Hands the event to the backend. The handler runs untracked: an effect that emits (the effect
  /// picker, the status sounds setting) must not subscribe to what the handler reads. Otherwise
  /// the backend's change notification after the action re-runs the effect, which emits again: a
  /// loop of call actions and refreshes that kept the desktop app busy while idle (issue #399).
  pub fn emit(&self, event: CallEvent) {
    // Gone only while the app itself is torn down (signing out): nothing left to tell.
    if let Some(handler) = self.handler.try_get_value().flatten() {
      untrack(|| handler(event));
    }
  }

  /// Microphone on or off, in the UI and in the call. Before joining (the pre-join screen) it is
  /// only the choice the call starts with.
  pub fn set_muted(&self, is_muted: bool) {
    self.is_mic_on.set(!is_muted);
    self.emit_in_call(CallEvent::Muted(is_muted));
  }

  pub fn set_deafened(&self, is_deafened: bool) {
    self.is_deafened.set(is_deafened);
    self.emit_in_call(CallEvent::Deafened(is_deafened));
  }

  /// Camera on or off, in the UI and in the call (or the choice the call starts with).
  pub fn set_camera(&self, is_on: bool) {
    self.is_camera_on.set(is_on);
    self.emit_in_call(CallEvent::Camera(is_on));
  }

  /// Switches to the next camera of `cameras` (front and back on a phone), wrapping around; the
  /// running camera restarts on it. Nothing to switch to with fewer than two.
  pub fn flip_camera(&self, cameras: &[MediaDevice]) {
    if let Some(next) = next_camera(cameras, &self.camera_id.get_untracked()) {
      self.choose_camera(next.id.clone());
    }
  }

  /// The user picked this camera: the running camera (or the pre-join check's) restarts on it.
  /// Remembering it is the caller's part (the preferences).
  pub fn choose_camera(&self, id: String) {
    self.camera_id.set(id);
    self.send_camera_choice();
  }

  /// Shows `id` as the camera without telling the backend, which uses it already: its default
  /// (the first one listed) or a camera that stood in for the chosen one (#819).
  pub fn show_camera_in_use(&self, id: String) {
    self.sent_camera.set_value(id.clone());
    self.camera_id.set(id);
  }

  /// Tells the backend the chosen camera unless it knows it already, so a choice reaches it once:
  /// the remembered one at start, a pick, a change in the settings.
  pub fn send_camera_choice(&self) {
    let Some(id) = self.camera_id.try_get_untracked() else {
      return;
    };
    if id.is_empty() || self.sent_camera.try_with_value(|sent| *sent == id) != Some(false) {
      return;
    }
    self.sent_camera.set_value(id.clone());
    self.emit(CallEvent::CameraDevice(id));
  }

  /// The chosen microphone or speakers (an empty id: the system default).
  pub fn audio_id(&self, kind: AudioKind) -> RwSignal<String> {
    match kind {
      AudioKind::Microphone => self.microphone_id,
      AudioKind::Speakers => self.speaker_id,
    }
  }

  fn sent_audio(&self, kind: AudioKind) -> StoredValue<String> {
    match kind {
      AudioKind::Microphone => self.sent_microphone,
      AudioKind::Speakers => self.sent_speakers,
    }
  }

  /// The user picked this microphone or speakers (an empty id: the system default): a running
  /// call switches at once, muted or not (#986), and the pre-join check measures it. Remembering
  /// it is the caller's part (the preferences).
  pub fn choose_audio(&self, kind: AudioKind, id: String) {
    self.audio_id(kind).set(id);
    self.send_audio_choices();
  }

  /// Tells the backend the chosen microphone and speakers unless it knows them already, so a
  /// choice reaches it once: the remembered one at start, a pick, a change in the settings.
  pub fn send_audio_choices(&self) {
    for kind in AudioKind::ALL {
      let (Some(id), sent) = (self.audio_id(kind).try_get_untracked(), self.sent_audio(kind)) else {
        return;
      };
      if sent.try_with_value(|sent| *sent == id) != Some(false) {
        continue;
      }
      sent.set_value(id.clone());
      self.emit(kind.event(id));
    }
  }

  /// The system made another device its default while the call follows it: the call opens the
  /// default again, which is that device now.
  pub fn reopen_audio(&self, kind: AudioKind) {
    if let Some(id) = self.audio_id(kind).try_get_untracked() {
      self.emit(kind.event(id));
    }
  }

  fn emit_in_call(&self, event: CallEvent) {
    if self.phase.get_untracked() == CallPhase::Active {
      self.emit(event);
    }
  }

  /// Starts sharing (with the sound if the user chose so) or stops.
  pub fn set_screen_share(&self, is_sharing: bool) {
    self.is_sharing_screen.set(is_sharing);
    if is_sharing {
      self.emit(CallEvent::StartScreenShare {
        has_sound: self.share_sound.get_untracked(),
        mode: self.screen_mode.get_untracked(),
        resolution: self.screen_resolution.get_untracked(),
      });
    } else {
      self.emit(CallEvent::StopScreenShare);
    }
  }

  /// The quality picked for another device's video in this call; Auto unless one was picked.
  pub fn video_quality(&self, stream_id: MediaStreamId) -> VideoQuality {
    self
      .video_qualities
      .with(|qualities| qualities.get(&stream_id).copied())
      .unwrap_or_default()
  }

  /// The user picked how sharp another device's video comes (#967); kept for this call.
  pub fn set_video_quality(&self, stream_id: MediaStreamId, quality: VideoQuality) {
    self.video_qualities.update(|qualities| {
      match quality.max_lines() {
        Some(_) => qualities.insert(stream_id, quality),
        None => qualities.remove(&stream_id),
      };
    });
    self.emit_in_call(CallEvent::VideoQuality { stream_id, quality });
  }

  /// Stops ringing for an incoming call on this device.
  pub fn decline_incoming(&self) {
    if let Some(incoming) = self.incoming.get_untracked() {
      self.incoming.set(None);
      self.emit(CallEvent::Decline(incoming.conversation));
    }
  }

  /// Opens the pre-join screen for a conversation.
  pub fn prepare(&self, request: CallRequest, with_video: bool) {
    self.is_camera_on.set(with_video);
    self.maybe_request.set(Some(request));
    self.phase.set(CallPhase::PreJoin);
  }

  /// Joins without a pre-join screen (huddles, accepted incoming calls).
  pub fn join(&self, request: CallRequest) {
    self.maybe_request.set(Some(request));
    self.enter();
  }

  /// The account is in this call on another device: joins next to it with the microphone muted,
  /// so the two don't echo each other.
  pub fn join_alongside(&self, request: CallRequest) {
    self.is_mic_on.set(false);
    self.join(request);
  }

  /// The account is in this call on another device: joins and takes the call over, so the other
  /// device leaves.
  pub fn join_and_take_over(&self, request: CallRequest) {
    self.join(request);
    self.emit(CallEvent::MoveHere);
  }

  /// Leaves the pre-join screen and enters the call.
  pub fn enter(&self) {
    let is_huddle = self
      .maybe_request
      .get_untracked()
      .is_some_and(|request| request.kind == CallKind::Huddle);
    self.is_minimized.set(is_huddle);
    self.is_sharing_screen.set(false);
    self.has_hand_raised.set(false);
    self.is_deafened.set(false);
    self.has_ended.set(false);
    self.phase.set(CallPhase::Active);
    if let Some(request) = self.maybe_request.get_untracked() {
      // The media layer starts as chosen, in one action (#453): a muted microphone stays muted
      // from the first frame (also when a call moves on into a meeting room), and the camera
      // starts with the call. The phone button joins with the camera off and never asks for it.
      self.emit(CallEvent::Join {
        request,
        media: JoinMedia {
          is_muted: !self.is_mic_on.get_untracked(),
          is_camera_on: self.is_camera_on.get_untracked(),
        },
      });
    }
  }

  /// The call ended for both (the other side of a 1:1 call left, #997): "Call ended" shows for a
  /// moment, then the call screen closes as on leaving. A new call started meanwhile stays.
  pub fn show_ended(&self) {
    self.has_ended.set(true);
    let call = *self;
    set_timeout(
      move || {
        if call.has_ended.try_get_untracked().unwrap_or(false) {
          call.leave();
        }
      },
      CALL_ENDED_SHOWN,
    );
  }

  pub fn leave(&self) {
    let was_active = self.phase.get_untracked() == CallPhase::Active;
    self.has_ended.set(false);
    self.phase.set(CallPhase::Idle);
    self.maybe_request.set(None);
    self.is_sharing_screen.set(false);
    self.has_hand_raised.set(false);
    self.is_chat_open.set(false);
    self.video_qualities.set(HashMap::new());
    self.maybe_quality_menu.set(None);
    self.is_audio_menu_open.set(false);
    self.maybe_audio_offer.set(None);
    if was_active {
      self.emit(CallEvent::Leave);
    }
  }

  /// Whether a call screen (the pre-join screen or the call itself) covers the app: what the app
  /// shows below it is out of sight, so its messages don't count as read (#936).
  pub fn covers_app(&self) -> bool {
    match self.phase.get() {
      CallPhase::PreJoin => true,
      CallPhase::Active => !self.is_minimized.get(),
      CallPhase::Idle => false,
    }
  }

  pub fn is_in_call_for(&self, conversation: ConversationId) -> bool {
    self.phase.get() == CallPhase::Active
      && self
        .maybe_request
        .get()
        .is_some_and(|request| request.conversation == conversation)
  }
}

impl Default for CallState {
  fn default() -> Self {
    Self::new()
  }
}

/// The camera after `current_id` in the list, wrapping around. A `current_id` that isn't listed
/// (nothing chosen yet: the system default) counts as the first, so the flip lands on the second.
pub fn next_camera<'a>(cameras: &'a [MediaDevice], current_id: &str) -> Option<&'a MediaDevice> {
  if cameras.len() < 2 {
    return None;
  }
  let current = cameras
    .iter()
    .position(|camera| camera.id == current_id)
    .unwrap_or(0);
  cameras.get((current + 1) % cameras.len())
}

/// Whether a camera faces away from the user, judged by its label (browsers give no facing
/// without opening it): "Back Camera" (iOS), "camera2 0, facing back" (Android), "environment".
/// Such a camera is shown unmirrored; only the front camera is mirrored like a mirror.
pub fn is_rear_camera(label: &str) -> bool {
  let label = label.to_lowercase();
  ["back", "rear", "environment", "hinten", "rück"]
    .iter()
    .any(|word| label.contains(word))
}

/// Whether the camera chosen for the call faces away from the user; the system default (nothing
/// chosen) counts as the first one listed.
pub fn selected_camera_is_rear(cameras: &[MediaDevice], selected_id: &str) -> bool {
  cameras
    .iter()
    .find(|camera| camera.id == selected_id)
    .or(cameras.first())
    .is_some_and(|camera| is_rear_camera(&camera.label))
}

/// Why a call that broke off couldn't be resumed (issue #427): with the chat server still
/// connected, the call server went away (restarted or crashed, and not back within the reconnect
/// grace period); otherwise this device's own connection broke.
pub fn lost_call_failure(maybe_connection: Option<CoreConnection>) -> CallFailure {
  match maybe_connection {
    Some(CoreConnection::Connected) => CallFailure::ServerLost,
    _ => CallFailure::ConnectionLost,
  }
}

#[cfg(test)]
mod tests {
  use std::cell::RefCell;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicU32, Ordering};

  use teamcraft_client_api::model::id_from_raw;

  use super::*;

  #[test]
  fn flipping_cycles_through_the_cameras() {
    let cameras = [
      camera("a", "Front Camera"),
      camera("b", "Back Camera"),
      camera("c", "USB"),
    ];
    assert_eq!(next_camera(&cameras, "a").map(|c| c.id.as_str()), Some("b"));
    assert_eq!(next_camera(&cameras, "c").map(|c| c.id.as_str()), Some("a"));
    assert_eq!(next_camera(&cameras, "").map(|c| c.id.as_str()), Some("b"));
    assert_eq!(next_camera(&cameras[..1], "a"), None);
    assert_eq!(next_camera(&[], ""), None);
  }

  #[test]
  fn rear_cameras_are_recognised_by_label() {
    assert!(is_rear_camera("Back Camera"));
    assert!(is_rear_camera("camera2 0, facing back"));
    assert!(is_rear_camera("Rückkamera"));
    assert!(!is_rear_camera("Front Camera"));
    assert!(!is_rear_camera("camera2 1, facing front"));
    assert!(!is_rear_camera(""));
  }

  #[test]
  fn a_lost_call_blames_the_call_server_only_while_the_chat_server_is_connected() {
    let offline = CoreConnection::Offline {
      retry_at_ms: 0,
      maybe_interception: None,
    };

    assert_eq!(
      lost_call_failure(Some(CoreConnection::Connected)),
      CallFailure::ServerLost
    );
    assert_eq!(lost_call_failure(Some(offline)), CallFailure::ConnectionLost);
    assert_eq!(
      lost_call_failure(Some(CoreConnection::Connecting)),
      CallFailure::ConnectionLost
    );
    assert_eq!(lost_call_failure(None), CallFailure::ConnectionLost);
  }

  /// #936: the app is out of sight below the pre-join screen and the call, not below a minimized
  /// call; leaving the call closes its chat panel.
  #[test]
  fn call_screens_cover_the_app_and_leaving_closes_the_chat() {
    let owner = Owner::new();
    owner.with(|| {
      let call = CallState::new();
      let covers = || untrack(|| call.covers_app());
      assert!(!covers());
      call.phase.set(CallPhase::PreJoin);
      assert!(covers());
      call.phase.set(CallPhase::Active);
      assert!(covers());
      call.is_minimized.set(true);
      assert!(!covers());
      call.is_chat_open.set(true);
      call.leave();
      assert!(!covers());
      assert!(!call.is_chat_open.get_untracked());
    });
  }

  /// #967: a quality picked for another person's video goes to the backend and holds for the
  /// call; Auto is no entry, and leaving forgets the picks and closes the menu.
  #[test]
  fn a_picked_video_quality_reaches_the_backend_and_holds_for_the_call() {
    let owner = Owner::new();
    owner.with(|| {
      let call = CallState::new();
      let events = Rc::new(RefCell::new(Vec::new()));
      let seen = events.clone();
      call.set_handler(move |event| seen.borrow_mut().push(event));
      call.phase.set(CallPhase::Active);
      let stream_id = MediaStreamId(9);
      assert_eq!(untrack(|| call.video_quality(stream_id)), VideoQuality::Auto);
      call.set_video_quality(stream_id, VideoQuality::Lines(360));
      assert_eq!(
        untrack(|| call.video_quality(stream_id)),
        VideoQuality::Lines(360)
      );
      call.set_video_quality(stream_id, VideoQuality::Auto);
      assert!(
        call
          .video_qualities
          .with_untracked(|qualities| qualities.is_empty())
      );
      assert_eq!(
        *events.borrow(),
        vec![
          CallEvent::VideoQuality {
            stream_id,
            quality: VideoQuality::Lines(360)
          },
          CallEvent::VideoQuality {
            stream_id,
            quality: VideoQuality::Auto
          }
        ]
      );
      call.set_video_quality(stream_id, VideoQuality::Lines(720));
      call.maybe_quality_menu.set(Some(stream_id));
      call.leave();
      assert!(
        call
          .video_qualities
          .with_untracked(|qualities| qualities.is_empty())
      );
      assert_eq!(call.maybe_quality_menu.get_untracked(), None);
    });
  }

  /// #453: the pre-join choice goes with the join, in one event; no camera action follows it.
  #[test]
  fn entering_joins_with_the_chosen_microphone_and_camera() {
    let owner = Owner::new();
    owner.with(|| {
      let call = CallState::new();
      let events = Rc::new(RefCell::new(Vec::new()));
      let seen = events.clone();
      call.set_handler(move |event| seen.borrow_mut().push(event));
      let request = CallRequest {
        conversation: id_from_raw(7),
        kind: CallKind::Group,
      };
      call.prepare(request, true);
      // Before joining, a toggle is only the choice the call starts with.
      call.set_muted(true);
      assert!(events.borrow().is_empty());
      call.enter();
      let joined = CallEvent::Join {
        request,
        media: JoinMedia {
          is_muted: true,
          is_camera_on: true,
        },
      };
      assert_eq!(*events.borrow(), vec![joined]);

      // The phone button: camera off, never asked for.
      call.leave();
      events.borrow_mut().clear();
      call.set_muted(false);
      call.prepare(request, false);
      call.enter();
      let joined = CallEvent::Join {
        request,
        media: JoinMedia::default(),
      };
      assert_eq!(*events.borrow(), vec![joined]);
    });
  }

  /// #819: a camera choice reaches the backend once, also when a setting and a pick both carry it,
  /// and a camera the backend already uses (its default, a stand-in) isn't sent back to it.
  #[test]
  fn a_camera_choice_reaches_the_backend_once() {
    let owner = Owner::new();
    owner.with(|| {
      let call = CallState::new();
      let events = Rc::new(RefCell::new(Vec::new()));
      let seen = events.clone();
      call.set_handler(move |event| seen.borrow_mut().push(event));

      // The remembered choice at start (the preferences set the signal).
      call.camera_id.set("iphone".into());
      call.send_camera_choice();
      call.send_camera_choice();
      assert_eq!(*events.borrow(), vec![CallEvent::CameraDevice("iphone".into())]);

      // The iPhone declined, the built-in camera stood in: shown, not sent back.
      events.borrow_mut().clear();
      call.show_camera_in_use("built-in".into());
      call.send_camera_choice();
      assert!(events.borrow().is_empty());
      assert_eq!(call.camera_id.get_untracked(), "built-in");

      // Picking the iPhone again tries it again.
      call.choose_camera("iphone".into());
      call.send_camera_choice();
      assert_eq!(*events.borrow(), vec![CallEvent::CameraDevice("iphone".into())]);

      // Nothing chosen: nothing to send.
      events.borrow_mut().clear();
      call.camera_id.set(String::new());
      call.send_camera_choice();
      assert!(events.borrow().is_empty());
    });
  }

  /// #986: a microphone or speakers choice reaches the backend once (an empty one is the system
  /// default, which the backend starts on), and a default that moved is opened again.
  #[test]
  fn an_audio_choice_reaches_the_backend_once() {
    let owner = Owner::new();
    owner.with(|| {
      let call = CallState::new();
      let events = Rc::new(RefCell::new(Vec::new()));
      let seen = events.clone();
      call.set_handler(move |event| seen.borrow_mut().push(event));

      // Nothing remembered: the backend is on the default already.
      call.send_audio_choices();
      assert!(events.borrow().is_empty());

      // The remembered microphone at start, then a pick in the call's menu: each once.
      call.microphone_id.set("usb".into());
      call.send_audio_choices();
      call.send_audio_choices();
      call.choose_audio(AudioKind::Speakers, "headset".into());
      assert_eq!(
        *events.borrow(),
        vec![
          CallEvent::Microphone("usb".into()),
          CallEvent::Speakers("headset".into())
        ]
      );

      // Back to the system default is a choice too.
      events.borrow_mut().clear();
      call.choose_audio(AudioKind::Microphone, String::new());
      assert_eq!(*events.borrow(), vec![CallEvent::Microphone(String::new())]);

      // The default moved: opened again although the choice is the same.
      events.borrow_mut().clear();
      call.reopen_audio(AudioKind::Microphone);
      assert_eq!(*events.borrow(), vec![CallEvent::Microphone(String::new())]);
    });
  }

  /// #986: a switch mid-call keeps the microphone muted: choosing a device sends no unmute.
  #[test]
  fn switching_the_microphone_keeps_it_muted() {
    let owner = Owner::new();
    owner.with(|| {
      let call = CallState::new();
      let events = Rc::new(RefCell::new(Vec::new()));
      let seen = events.clone();
      call.set_handler(move |event| seen.borrow_mut().push(event));
      call.phase.set(CallPhase::Active);
      call.set_muted(true);
      call.choose_audio(AudioKind::Microphone, "headset".into());
      assert!(!call.is_mic_on.get_untracked());
      assert_eq!(
        *events.borrow(),
        vec![CallEvent::Muted(true), CallEvent::Microphone("headset".into())]
      );
    });
  }

  mod audio_devices {
    use super::*;

    #[test]
    fn nothing_changes_on_the_first_or_an_unchanged_listing() {
      let listed = [camera("mic", "Built-in")];
      assert_eq!(audio_device_change(&[], &listed, ""), None);
      assert_eq!(audio_device_change(&listed, &listed, "mic"), None);
    }

    #[test]
    fn a_plugged_in_headset_is_offered_to_a_call_on_a_chosen_device() {
      let before = [camera("built-in", "MacBook Microphone")];
      let after = [
        camera("built-in", "MacBook Microphone"),
        camera("jabra", "Jabra Evolve"),
      ];
      assert_eq!(
        audio_device_change(&before, &after, "built-in"),
        Some(AudioDeviceChange::Appeared(camera("jabra", "Jabra Evolve")))
      );
    }

    #[test]
    fn a_call_on_the_system_default_follows_it() {
      // The desktop lists the default first; macOS made the headset the default.
      let before = [camera("built-in", "MacBook Microphone")];
      let after = [
        camera("jabra", "Jabra Evolve"),
        camera("built-in", "MacBook Microphone"),
      ];
      assert_eq!(
        audio_device_change(&before, &after, ""),
        Some(AudioDeviceChange::DefaultMoved(camera("jabra", "Jabra Evolve")))
      );
      // Chrome keeps its "default" entry and renames it.
      let before = [
        camera("default", "Default - MacBook Microphone"),
        camera("built-in", "MacBook Microphone"),
      ];
      let after = [
        camera("default", "Default - Jabra Evolve"),
        camera("built-in", "MacBook Microphone"),
        camera("jabra", "Jabra Evolve"),
      ];
      // The device is named as itself, not as Chrome's stand-in.
      for chosen in ["", "default"] {
        assert_eq!(
          audio_device_change(&before, &after, chosen),
          Some(AudioDeviceChange::DefaultMoved(camera("jabra", "Jabra Evolve"))),
          "{chosen:?}"
        );
      }
      // The default stayed: the new device is offered instead.
      let after = [
        camera("default", "Default - MacBook Microphone"),
        camera("built-in", "MacBook Microphone"),
        camera("jabra", "Jabra Evolve"),
      ];
      assert_eq!(
        audio_device_change(&before, &after, ""),
        Some(AudioDeviceChange::Appeared(camera("jabra", "Jabra Evolve")))
      );
    }

    #[test]
    fn the_device_in_use_unplugged_gives_way_to_the_default() {
      let before = [
        camera("built-in", "MacBook Microphone"),
        camera("jabra", "Jabra Evolve"),
      ];
      let after = [camera("built-in", "MacBook Microphone")];
      assert_eq!(
        audio_device_change(&before, &after, "jabra"),
        Some(AudioDeviceChange::Gone {
          gone: camera("jabra", "Jabra Evolve"),
          maybe_default: Some(camera("built-in", "MacBook Microphone")),
        })
      );
      // The last one gone: no default left.
      assert_eq!(
        audio_device_change(&after, &[], "built-in"),
        Some(AudioDeviceChange::Gone {
          gone: camera("built-in", "MacBook Microphone"),
          maybe_default: None,
        })
      );
      // Another device unplugged changes nothing for the call.
      assert_eq!(audio_device_change(&before, &after, "built-in"), None);
    }

    #[test]
    fn a_choice_that_was_not_listed_counts_as_the_default() {
      // Remembered from another computer: the backend opened the default instead.
      let before = [camera("built-in", "MacBook Microphone")];
      let after = [
        camera("jabra", "Jabra Evolve"),
        camera("built-in", "MacBook Microphone"),
      ];
      assert_eq!(
        audio_device_change(&before, &after, "elsewhere"),
        Some(AudioDeviceChange::DefaultMoved(camera("jabra", "Jabra Evolve")))
      );
    }

    #[test]
    fn the_menu_starts_with_the_system_default_and_checks_the_device_in_use() {
      let devices = [
        camera("built-in", "MacBook Microphone"),
        camera("jabra", "Jabra Evolve"),
      ];
      let checked = |entries: Vec<AudioMenuEntry>| {
        entries
          .into_iter()
          .filter(|entry| entry.is_checked)
          .map(|entry| entry.id)
          .collect::<Vec<_>>()
      };
      let entries = audio_menu_entries(&devices, "");
      assert_eq!(entries.len(), 3);
      assert!(entries[0].is_system_default && entries[0].id.is_empty());
      assert_eq!(entries[0].label, "MacBook Microphone");
      assert_eq!(checked(entries), vec![String::new()]);
      assert_eq!(
        checked(audio_menu_entries(&devices, "jabra")),
        vec!["jabra".to_owned()]
      );
      // Unplugged: the call is on the default.
      assert_eq!(checked(audio_menu_entries(&devices, "gone")), vec![String::new()]);
      // Chrome's own default entry stands for the system default; nothing is added.
      let browser = [
        camera("default", "Default - MacBook Microphone"),
        camera("communications", "Communications - MacBook Microphone"),
        camera("built-in", "MacBook Microphone"),
      ];
      let entries = audio_menu_entries(&browser, "");
      assert_eq!(
        entries.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
        vec!["default", "built-in"]
      );
      assert!(entries[0].is_system_default);
      assert_eq!(checked(entries), vec!["default".to_owned()]);
      assert_eq!(
        checked(audio_menu_entries(&browser, "built-in")),
        vec!["built-in".to_owned()]
      );
      // No device at all: only the default.
      assert_eq!(audio_menu_entries(&[], "").len(), 1);
    }
  }

  fn camera(id: &str, label: &str) -> MediaDevice {
    MediaDevice {
      id: id.into(),
      label: label.into(),
    }
  }

  /// Issue #399: the effect picker's effect emitted, the handler read the backend, and the
  /// backend's change notification re-ran the effect, which emitted again, forever.
  #[test]
  fn a_computation_that_emits_does_not_track_what_the_handler_reads() {
    let owner = Owner::new();
    owner.with(|| {
      let call = CallState::new();
      let backend_revision = RwSignal::new(0_u32);
      call.set_handler(move |_| {
        backend_revision.get();
      });
      let runs = Arc::new(AtomicU32::new(0));
      let counted = runs.clone();
      let emitting = Memo::new(move |_| {
        counted.fetch_add(1, Ordering::Relaxed);
        call.emit(CallEvent::Effect(call.effect.get()));
      });
      emitting.get();
      backend_revision.set(1);
      emitting.get();
      assert_eq!(
        runs.load(Ordering::Relaxed),
        1,
        "a backend change re-ran the emitting computation"
      );
      call.effect.set(BackgroundEffect::BlurLight);
      emitting.get();
      assert_eq!(
        runs.load(Ordering::Relaxed),
        2,
        "the computation's own input still re-runs it"
      );
    });
  }
}
