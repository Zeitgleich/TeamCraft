//! Call session state of the UI. The view models (devices, participants, effects) live in
//! `teamcraft-client-api`; this file only adds the reactive state around them.

use std::rc::Rc;

use leptos::prelude::*;

pub use teamcraft_client_api::calls::*;
use teamcraft_client_api::model::ConversationId;

/// What the user did that the call backend has to carry out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CallEvent {
  Join(CallRequest),
  Leave,
  Decline(ConversationId),
  Muted(bool),
  Deafened(bool),
  Microphone(String),
  Speakers(String),
  Camera(bool),
  CameraDevice(String),
  Effect(BackgroundEffect),
  ScreenShare(bool),
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
  pub has_hand_raised: RwSignal<bool>,
  pub is_deafened: RwSignal<bool>,
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
      has_hand_raised: RwSignal::new(false),
      is_deafened: RwSignal::new(false),
      handler: StoredValue::new_local(None),
    }
  }

  pub fn set_handler(&self, handler: impl Fn(CallEvent) + 'static) {
    self.handler.set_value(Some(Rc::new(handler)));
  }

  pub fn emit(&self, event: CallEvent) {
    if let Some(handler) = self.handler.get_value() {
      handler(event);
    }
  }

  /// Microphone on or off, in the UI and in the call.
  pub fn set_muted(&self, is_muted: bool) {
    self.is_mic_on.set(!is_muted);
    self.emit(CallEvent::Muted(is_muted));
  }

  pub fn set_deafened(&self, is_deafened: bool) {
    self.is_deafened.set(is_deafened);
    self.emit(CallEvent::Deafened(is_deafened));
  }

  /// Camera on or off, in the UI and in the call.
  pub fn set_camera(&self, is_on: bool) {
    self.is_camera_on.set(is_on);
    self.emit(CallEvent::Camera(is_on));
  }

  pub fn set_screen_share(&self, is_sharing: bool) {
    self.is_sharing_screen.set(is_sharing);
    self.emit(CallEvent::ScreenShare(is_sharing));
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

  /// Leaves the pre-join screen and enters the call.
  pub fn enter(&self) {
    let is_huddle = self
      .maybe_request
      .get_untracked()
      .is_some_and(|request| request.kind == CallKind::Huddle);
    self.is_minimized.set(is_huddle);
    self.is_sharing_screen.set(false);
    self.has_hand_raised.set(false);
    self.phase.set(CallPhase::Active);
    if let Some(request) = self.maybe_request.get_untracked() {
      self.emit(CallEvent::Join(request));
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
