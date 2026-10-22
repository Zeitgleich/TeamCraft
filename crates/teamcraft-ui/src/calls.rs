//! Call session state of the UI. The view models (devices, participants, effects) live in
//! `teamcraft-client-api`; this file only adds the reactive state around them.

use leptos::prelude::*;

pub use teamcraft_client_api::calls::*;
use teamcraft_client_api::model::ConversationId;

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
  }

  pub fn leave(&self) {
    self.phase.set(CallPhase::Idle);
    self.maybe_request.set(None);
    self.is_sharing_screen.set(false);
    self.has_hand_raised.set(false);
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
