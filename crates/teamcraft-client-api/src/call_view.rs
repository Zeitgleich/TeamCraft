//! The call state the UI shows ([`crate::remote::CallSnapshot`]), kept from the server's call events
//! (roster, ringing) and from what the media layer reports about the call this device is in. The
//! desktop shell and the browser both keep a [`CallView`], so calls look and behave the same in
//! either; `teamcraft-media` (`call_view`) applies the media engine's events to it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{CallRosterEntry, ConversationId, GatewayEvent, MediaStreamId, UserId};

use crate::calls::{
  ActiveCall, BackgroundEffect, CallEndReason, CallRequest, IncomingCall, RemoteVideoTrack, RunningCall,
  ScreenShareSound,
};

/// Calls of the user's conversations and the call this device is in.
#[derive(Clone, Debug, Default)]
pub struct CallView {
  pub running: BTreeMap<ConversationId, RunningCall>,
  pub incoming: BTreeMap<ConversationId, IncomingCall>,
  pub maybe_active: Option<ActiveCall>,
  /// Others' video in the active call.
  pub video_tracks: BTreeMap<MediaStreamId, RemoteVideoTrack>,
  pub maybe_end_reason: Option<CallEndReason>,
}

/// The part of a [`CallView`] the media layer keeps: the call this device is in. The browser runs the
/// media in a module of its own, which reports this after every change.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaCallState {
  #[serde(default)]
  pub maybe_active: Option<ActiveCall>,
  #[serde(default)]
  pub video_tracks: Vec<RemoteVideoTrack>,
  #[serde(default)]
  pub maybe_end_reason: Option<CallEndReason>,
}

impl CallView {
  /// This device joined the conversation's call: it shows as active, no longer ringing, and the end
  /// reason of the previous call is gone.
  pub fn start_active(&mut self, request: CallRequest, background: BackgroundEffect) {
    self.incoming.remove(&request.conversation);
    self.maybe_active = Some(ActiveCall {
      conversation: request.conversation,
      kind: request.kind,
      is_muted: false,
      is_deafened: false,
      is_secured: false,
      maybe_privacy_code: None,
      participants: Vec::new(),
      active_speakers: Vec::new(),
      is_camera_on: false,
      is_sharing_screen: false,
      background,
      maybe_muted_by_host: None,
      screen_sound: ScreenShareSound::Off,
      is_reconnecting: false,
      reconnecting: Vec::new(),
    });
    self.maybe_end_reason = None;
  }

  /// This device left the call (or it ended).
  pub fn end_active(&mut self) {
    self.maybe_active = None;
    self.video_tracks.clear();
  }

  /// Stops ringing on this device; the call goes on for the others.
  pub fn decline(&mut self, conversation: ConversationId) {
    self.incoming.remove(&conversation);
  }

  pub fn is_camera_on(&self) -> bool {
    self
      .maybe_active
      .as_ref()
      .is_some_and(|active| active.is_camera_on)
  }

  /// Applies one of the gateway's call events; `true` if the UI should read again.
  pub fn apply_gateway_event(&mut self, event: &GatewayEvent) -> bool {
    match event {
      GatewayEvent::CallStart {
        conversation_id,
        is_ringing,
        participants,
        ..
      }
      | GatewayEvent::CallUpdate {
        conversation_id,
        is_ringing,
        participants,
        ..
      } => {
        if !is_ringing {
          self.incoming.remove(conversation_id);
        }
        self.running.insert(
          *conversation_id,
          RunningCall {
            conversation: *conversation_id,
            participants: users_of(participants),
            is_ringing: *is_ringing,
          },
        );
      },
      GatewayEvent::CallEnd { conversation_id, .. } => {
        self.running.remove(conversation_id);
        self.incoming.remove(conversation_id);
      },
      GatewayEvent::CallRing {
        conversation_id,
        from_user_id,
        ..
      } => {
        let is_in_it = self
          .maybe_active
          .as_ref()
          .is_some_and(|active| active.conversation == *conversation_id);
        if !is_in_it {
          self.incoming.insert(
            *conversation_id,
            IncomingCall {
              caller: *from_user_id,
              conversation: *conversation_id,
              with_video: false,
            },
          );
        }
      },
      _ => return false,
    }
    true
  }

  /// What the media layer knows about the call this device is in.
  pub fn media_state(&self) -> MediaCallState {
    MediaCallState {
      maybe_active: self.maybe_active.clone(),
      video_tracks: self.video_tracks.values().cloned().collect(),
      maybe_end_reason: self.maybe_end_reason,
    }
  }

  /// Takes over what the media layer reported (see [`MediaCallState`]).
  pub fn set_media_state(&mut self, state: MediaCallState) {
    if let Some(active) = &state.maybe_active {
      self.incoming.remove(&active.conversation);
    }
    self.maybe_active = state.maybe_active;
    self.video_tracks = state
      .video_tracks
      .into_iter()
      .map(|track| (track.stream_id, track))
      .collect();
    self.maybe_end_reason = state.maybe_end_reason;
  }
}

fn users_of(entries: &[CallRosterEntry]) -> Vec<UserId> {
  let mut users: Vec<UserId> = entries.iter().map(|entry| entry.user_id).collect();
  users.sort_unstable();
  users.dedup();
  users
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{CallId, Id, Snowflake, UnixMillis};

  use super::*;
  use crate::calls::CallKind;

  mod ringing {
    use super::*;

    #[test]
    fn ring_shows_until_the_call_stops_ringing() {
      let mut view = CallView::default();
      view.apply_gateway_event(&GatewayEvent::CallRing {
        conversation_id: conversation(5),
        call_id: call(),
        from_user_id: user(1),
      });
      assert_eq!(view.incoming.len(), 1);
      view.apply_gateway_event(&GatewayEvent::CallUpdate {
        conversation_id: conversation(5),
        call_id: call(),
        is_ringing: false,
        participants: vec![entry(1, 10), entry(2, 20), entry(2, 21)],
      });
      assert!(view.incoming.is_empty());
      assert_eq!(
        view.running[&conversation(5)].participants,
        vec![user(1), user(2)]
      );
      view.apply_gateway_event(&GatewayEvent::CallEnd {
        conversation_id: conversation(5),
        call_id: call(),
      });
      assert!(view.running.is_empty());
    }

    #[test]
    fn declined_ring_disappears_but_the_call_stays_listed() {
      let mut view = CallView::default();
      view.apply_gateway_event(&GatewayEvent::CallStart {
        conversation_id: conversation(5),
        call_id: call(),
        started_by: user(1),
        started_at: UnixMillis(0),
        is_ringing: true,
        participants: vec![entry(1, 10)],
      });
      view.apply_gateway_event(&GatewayEvent::CallRing {
        conversation_id: conversation(5),
        call_id: call(),
        from_user_id: user(1),
      });
      view.decline(conversation(5));
      assert!(view.incoming.is_empty());
      assert_eq!(view.running.len(), 1);
    }

    #[test]
    fn a_ring_for_the_call_this_device_is_in_does_not_show() {
      let mut view = active_view();
      view.apply_gateway_event(&GatewayEvent::CallRing {
        conversation_id: conversation(5),
        call_id: call(),
        from_user_id: user(1),
      });
      assert!(view.incoming.is_empty());
    }
  }

  #[test]
  fn the_media_state_round_trips_and_ends_the_ring() {
    let mut view = CallView::default();
    view.apply_gateway_event(&GatewayEvent::CallRing {
      conversation_id: conversation(5),
      call_id: call(),
      from_user_id: user(1),
    });
    let reported = active_view().media_state();
    view.set_media_state(reported.clone());
    assert!(
      view.incoming.is_empty(),
      "joined elsewhere in the page: no longer ringing"
    );
    assert_eq!(view.media_state(), reported);
  }

  #[test]
  fn joining_clears_the_ring_and_the_last_end_reason() {
    let mut view = CallView::default();
    view.apply_gateway_event(&GatewayEvent::CallRing {
      conversation_id: conversation(5),
      call_id: call(),
      from_user_id: user(1),
    });
    view.maybe_end_reason = Some(CallEndReason::ConnectionLost);
    view.start_active(request(), BackgroundEffect::BlurLight);
    assert!(view.incoming.is_empty());
    assert!(view.maybe_end_reason.is_none());
    assert_eq!(
      view.maybe_active.as_ref().map(|active| active.background),
      Some(BackgroundEffect::BlurLight)
    );
  }

  fn active_view() -> CallView {
    let mut view = CallView::default();
    view.start_active(request(), BackgroundEffect::None);
    view
  }

  fn request() -> CallRequest {
    CallRequest {
      conversation: conversation(5),
      kind: CallKind::Group,
    }
  }

  fn entry(user_id: i64, device_id: i64) -> CallRosterEntry {
    CallRosterEntry {
      user_id: user(user_id),
      device_id: device(device_id),
    }
  }

  fn call() -> CallId {
    CallId::from_snowflake(Snowflake::new(99).unwrap())
  }

  fn conversation(raw: i64) -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn device(raw: i64) -> teamcraft_proto::DeviceId {
    teamcraft_proto::DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
