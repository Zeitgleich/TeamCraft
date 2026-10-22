//! View models and session state for calls, huddles and the pre-join screen. Media itself lives in
//! `teamcraft-media`; these types only describe what the UI shows and lets the user pick.

use leptos::prelude::*;

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
