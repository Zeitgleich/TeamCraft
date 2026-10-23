//! A scripted call host for the dev bridge: the same wire as the shell's `CallHost`, with carol as
//! the other side, so the call screens can be checked in a plain browser without audio devices or
//! an SFU. The real media path is tested in `teamcraft-media` and `teamcraft-sfu`.

use std::sync::Mutex;

use teamcraft_client_api::calls::{
  ActiveCall, BackgroundEffect, CallRequest, IncomingCall, MediaDevice, MediaDevices, Participant,
  RunningCall, ScreenShareSound,
};
use teamcraft_client_api::remote::{CallAction, CallSnapshot};
use teamcraft_proto::{ConversationId, UserId};

pub struct FakeCalls {
  carol: UserId,
  snapshot: Mutex<CallSnapshot>,
}

impl FakeCalls {
  pub fn new(carol: UserId) -> Self {
    let device = |id: &str, label: &str| MediaDevice {
      id: id.to_owned(),
      label: label.to_owned(),
    };
    Self {
      carol,
      snapshot: Mutex::new(CallSnapshot {
        devices: MediaDevices {
          cameras: Vec::new(),
          microphones: vec![
            device("mic-1", "Built-in microphone"),
            device("mic-2", "USB headset"),
          ],
          speakers: vec![
            device("spk-1", "Built-in speakers"),
            device("spk-2", "USB headset"),
          ],
        },
        ..CallSnapshot::default()
      }),
    }
  }

  pub fn snapshot(&self) -> CallSnapshot {
    self.lock().clone()
  }

  /// Carol calls alice in a conversation.
  pub fn ring(&self, conversation: ConversationId) {
    let mut snapshot = self.lock();
    snapshot.incoming = vec![IncomingCall {
      caller: self.carol,
      conversation,
      with_video: false,
    }];
    snapshot.running = vec![RunningCall {
      conversation,
      participants: vec![self.carol],
      is_ringing: true,
    }];
  }

  pub fn act(&self, action: CallAction) {
    let mut snapshot = self.lock();
    match action {
      CallAction::Join { request } => {
        snapshot.incoming.clear();
        snapshot.running = vec![RunningCall {
          conversation: request.conversation,
          participants: vec![self.carol],
          is_ringing: false,
        }];
        snapshot.maybe_active = Some(active_call(request, self.carol));
      },
      CallAction::Leave => {
        snapshot.maybe_active = None;
        snapshot.running.clear();
      },
      CallAction::Decline { conversation } => {
        snapshot.incoming.retain(|call| call.conversation != conversation)
      },
      CallAction::SetMuted { is_muted } => {
        if let Some(active) = &mut snapshot.maybe_active {
          active.is_muted = is_muted;
        }
      },
      CallAction::SetDeafened { is_deafened } => {
        if let Some(active) = &mut snapshot.maybe_active {
          active.is_deafened = is_deafened;
        }
      },
      CallAction::SetCamera { is_on } => {
        if let Some(active) = &mut snapshot.maybe_active {
          active.is_camera_on = is_on;
        }
      },
      CallAction::SetBackgroundEffect { effect } => {
        if let Some(active) = &mut snapshot.maybe_active {
          active.background = effect;
        }
      },
      CallAction::StartScreenShare { has_sound } => {
        if let Some(active) = &mut snapshot.maybe_active {
          active.is_sharing_screen = true;
          active.screen_sound = if has_sound {
            ScreenShareSound::Shared
          } else {
            ScreenShareSound::Off
          };
        }
      },
      CallAction::StopScreenShare => {
        if let Some(active) = &mut snapshot.maybe_active {
          active.is_sharing_screen = false;
          active.screen_sound = ScreenShareSound::Off;
        }
      },
      CallAction::OpenSystemVideoEffects
      | CallAction::SelectMicrophone { .. }
      | CallAction::SelectSpeakers { .. }
      | CallAction::SetPushToTalk { .. }
      | CallAction::SetTalking { .. }
      | CallAction::SelectCamera { .. }
      | CallAction::SetStatusSounds { .. }
      | CallAction::Unknown => {},
    }
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, CallSnapshot> {
    self
      .snapshot
      .lock()
      .unwrap_or_else(std::sync::PoisonError::into_inner)
  }
}

fn active_call(request: CallRequest, carol: UserId) -> ActiveCall {
  ActiveCall {
    conversation: request.conversation,
    kind: request.kind,
    is_muted: false,
    maybe_muted_by_host: None,
    screen_sound: ScreenShareSound::Off,
    is_reconnecting: false,
    reconnecting: Vec::new(),
    is_deafened: false,
    is_secured: true,
    maybe_privacy_code: Some("31 07 58".to_owned()),
    participants: vec![Participant {
      user: carol,
      is_muted: false,
      is_camera_on: false,
      has_hand_raised: false,
      is_sharing_screen: false,
    }],
    active_speakers: vec![carol],
    is_camera_on: false,
    is_sharing_screen: false,
    background: BackgroundEffect::default(),
  }
}
