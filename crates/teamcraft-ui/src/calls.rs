//! Call session state of the UI. The view models (devices, participants, effects) live in
//! `teamcraft-client-api`; this file only adds the reactive state around them.

use std::rc::Rc;

use leptos::prelude::*;

pub use teamcraft_client_api::calls::*;
use teamcraft_client_api::model::{ConversationId, UserId};

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
  Microphone(String),
  Speakers(String),
  /// Plays the test sound on these speakers (an empty id: the default).
  TestSpeakers(String),
  /// Lists the devices again.
  RefreshDevices,
  Camera(bool),
  CameraDevice(String),
  Effect(BackgroundEffect),
  StartScreenShare {
    has_sound: bool,
    mode: ScreenContentMode,
  },
  StopScreenShare,
  StatusSounds(CallStatusSounds),
}

type CallHandler = Rc<dyn Fn(CallEvent)>;

/// Session state of the user's own call. Device choices survive between calls.
#[derive(Clone, Copy)]
pub struct CallState {
  pub phase: RwSignal<CallPhase>,
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
  pub has_hand_raised: RwSignal<bool>,
  pub is_deafened: RwSignal<bool>,
  /// An announced move of this call into a meeting room the user hasn't followed yet.
  pub move_offer: RwSignal<Option<MoveOffer>>,
  /// Carries the user's actions to the call backend; set once by the shell.
  handler: StoredValue<Option<CallHandler>, LocalStorage>,
}

impl CallState {
  pub fn new() -> Self {
    Self {
      phase: RwSignal::new(CallPhase::Idle),
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
      has_hand_raised: RwSignal::new(false),
      is_deafened: RwSignal::new(false),
      move_offer: RwSignal::new(None),
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
    if let Some(handler) = self.handler.get_value() {
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
      self.camera_id.set(next.id.clone());
      self.emit(CallEvent::CameraDevice(next.id.clone()));
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
      });
    } else {
      self.emit(CallEvent::StopScreenShare);
    }
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

  pub fn leave(&self) {
    let was_active = self.phase.get_untracked() == CallPhase::Active;
    self.phase.set(CallPhase::Idle);
    self.maybe_request.set(None);
    self.is_sharing_screen.set(false);
    self.has_hand_raised.set(false);
    if was_active {
      self.emit(CallEvent::Leave);
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
