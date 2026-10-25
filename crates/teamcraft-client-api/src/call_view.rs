//! The call state the UI shows ([`crate::remote::CallSnapshot`]), kept from the server's call events
//! (roster, ringing) and from what the media layer reports about the call this device is in. The
//! desktop shell and the browser both keep a [`CallView`], so calls look and behave the same in
//! either; `teamcraft-media` (`call_view`) applies the media engine's events to it.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_proto::{ConversationId, GatewayEvent, MediaStreamId, UserId};

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

/// What a gateway call event says about the user's calls: the part [`CallView`] needs. The web
/// client hands these to its call module (`page_calls`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "signal", rename_all = "snake_case")]
pub enum CallSignal {
  /// A call started or its roster changed. `participants` may name a person once per device.
  Running {
    conversation: ConversationId,
    participants: Vec<UserId>,
    is_ringing: bool,
  },
  Ended {
    conversation: ConversationId,
  },
  /// Someone calls the user.
  Ring {
    conversation: ConversationId,
    caller: UserId,
  },
  /// Somebody added people to the call: it moves into the meeting chat `to`.
  Moved {
    conversation: ConversationId,
    to: ConversationId,
  },
  #[serde(other)]
  Unknown,
}

impl CallSignal {
  /// The call part of a gateway event; `None` for every other event.
  pub fn from_gateway(event: &GatewayEvent) -> Option<Self> {
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
      } => Some(Self::Running {
        conversation: *conversation_id,
        participants: participants.iter().map(|entry| entry.user_id).collect(),
        is_ringing: *is_ringing,
      }),
      GatewayEvent::CallEnd { conversation_id, .. } => Some(Self::Ended {
        conversation: *conversation_id,
      }),
      GatewayEvent::CallRing {
        conversation_id,
        from_user_id,
        ..
      } => Some(Self::Ring {
        conversation: *conversation_id,
        caller: *from_user_id,
      }),
      GatewayEvent::CallMove {
        conversation_id,
        to_conversation_id,
        ..
      } => Some(Self::Moved {
        conversation: *conversation_id,
        to: *to_conversation_id,
      }),
      _ => None,
    }
  }
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
      maybe_moved_to: None,
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
    CallSignal::from_gateway(event).is_some_and(|signal| self.apply_signal(signal))
  }

  /// Applies a call event; `true` if the UI should read again.
  pub fn apply_signal(&mut self, signal: CallSignal) -> bool {
    match signal {
      CallSignal::Running {
        conversation,
        participants,
        is_ringing,
      } => {
        if !is_ringing {
          self.incoming.remove(&conversation);
        }
        self.running.insert(
          conversation,
          RunningCall {
            conversation,
            participants: people(participants),
            is_ringing,
          },
        );
      },
      CallSignal::Ended { conversation } => {
        self.running.remove(&conversation);
        self.incoming.remove(&conversation);
      },
      CallSignal::Ring { conversation, caller } => {
        let is_in_it = self
          .maybe_active
          .as_ref()
          .is_some_and(|active| active.conversation == conversation);
        if !is_in_it {
          self.incoming.insert(
            conversation,
            IncomingCall {
              caller,
              conversation,
              with_video: false,
            },
          );
        }
      },
      CallSignal::Moved { conversation, to } => {
        let Some(active) = self
          .maybe_active
          .as_mut()
          .filter(|active| active.conversation == conversation)
        else {
          // Not in that call on this device: nothing to switch.
          return false;
        };
        active.maybe_moved_to = Some(to);
      },
      CallSignal::Unknown => return false,
    }
    true
  }
}

/// Each person once (they may be in a call with several devices).
fn people(mut users: Vec<UserId>) -> Vec<UserId> {
  users.sort_unstable();
  users.dedup();
  users
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{CallId, CallRosterEntry, Id, Snowflake, UnixMillis};

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

  mod moves {
    use super::*;

    #[test]
    fn the_call_this_device_is_in_learns_where_it_moved() {
      let mut view = active_view();
      let is_changed = view.apply_gateway_event(&GatewayEvent::CallMove {
        conversation_id: conversation(5),
        call_id: call(),
        to_conversation_id: conversation(6),
        moved_by: user(1),
      });
      assert!(is_changed);
      assert_eq!(
        view
          .maybe_active
          .as_ref()
          .and_then(|active| active.maybe_moved_to),
        Some(conversation(6))
      );
      // Joining the meeting's call starts afresh.
      view.start_active(
        CallRequest {
          conversation: conversation(6),
          kind: CallKind::Group,
        },
        BackgroundEffect::None,
      );
      assert_eq!(
        view
          .maybe_active
          .as_ref()
          .and_then(|active| active.maybe_moved_to),
        None
      );
    }

    #[test]
    fn devices_outside_the_call_ignore_the_move() {
      let mut view = CallView::default();
      let event = GatewayEvent::CallMove {
        conversation_id: conversation(5),
        call_id: call(),
        to_conversation_id: conversation(6),
        moved_by: user(1),
      };
      assert!(!view.apply_gateway_event(&event));
      let mut elsewhere = CallView::default();
      elsewhere.start_active(
        CallRequest {
          conversation: conversation(7),
          kind: CallKind::Direct,
        },
        BackgroundEffect::None,
      );
      assert!(!elsewhere.apply_gateway_event(&event));
      assert_eq!(
        elsewhere.maybe_active.and_then(|active| active.maybe_moved_to),
        None
      );
    }
  }

  #[test]
  fn signals_round_trip_and_unknown_ones_are_ignored() {
    let signal = CallSignal::Ring {
      conversation: conversation(5),
      caller: user(1),
    };
    let json = serde_json::to_string(&signal).unwrap();
    assert_eq!(serde_json::from_str::<CallSignal>(&json).unwrap(), signal);
    let future: CallSignal = serde_json::from_str(r#"{"signal":"hold","conversation":"5"}"#).unwrap();
    assert_eq!(future, CallSignal::Unknown);
    assert!(!CallView::default().apply_signal(future));
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
