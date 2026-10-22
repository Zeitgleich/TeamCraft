//! The call side of the desktop shell as the UI sees it: a mirror of the shell's [`CallSnapshot`]
//! (ringing, running calls, the active call, devices) and the actions the user takes. The media
//! (audio, WebTransport to the SFU, DAVE) runs in the shell; the shell emits `call-changed` and
//! the mirror re-reads.

use std::cell::RefCell;
use std::rc::Rc;

use leptos::task::spawn_local;
use serde_json::json;
use teamcraft_client_api::calls::{
  ActiveCall, CallRequest, HuddleInfo, IncomingCall, MediaDevices, Participant,
};
use teamcraft_client_api::model::{ConversationId, UserId};
use teamcraft_client_api::remote::{CALL_CHANGED_EVENT, CallAction, CallSnapshot};

use crate::auth::tauri::{call, listen};

type ChangeNotifier = Rc<dyn Fn()>;

#[derive(Clone)]
pub struct RemoteCalls {
  snapshot: Rc<RefCell<CallSnapshot>>,
  notify: Rc<RefCell<Option<ChangeNotifier>>>,
}

impl RemoteCalls {
  /// Reads the first snapshot and follows the shell's `call-changed` events.
  pub async fn connect() -> Result<Self, String> {
    let snapshot: CallSnapshot = call("call_snapshot", &json!({})).await?;
    let calls = Self {
      snapshot: Rc::new(RefCell::new(snapshot)),
      notify: Rc::new(RefCell::new(None)),
    };
    let listener = calls.clone();
    listen(CALL_CHANGED_EVENT, move || listener.refresh());
    Ok(calls)
  }

  pub fn set_change_notifier(&self, notify: ChangeNotifier) {
    *self.notify.borrow_mut() = Some(notify);
  }

  pub fn devices(&self) -> MediaDevices {
    self.snapshot.borrow().devices.clone()
  }

  pub fn active(&self) -> Option<ActiveCall> {
    self.snapshot.borrow().maybe_active.clone()
  }

  pub fn incoming(&self) -> Vec<IncomingCall> {
    self.snapshot.borrow().incoming.clone()
  }

  /// People in the call of a conversation: the media layer's view while this device is in it, else
  /// the server roster (without media state).
  pub fn participants(&self, conversation: ConversationId) -> Vec<Participant> {
    let snapshot = self.snapshot.borrow();
    if let Some(active) = snapshot
      .maybe_active
      .as_ref()
      .filter(|active| active.conversation == conversation)
    {
      return active.participants.clone();
    }
    roster_of(&snapshot, conversation)
      .into_iter()
      .map(|user| Participant {
        user,
        is_muted: false,
        is_camera_on: false,
        has_hand_raised: false,
        is_sharing_screen: false,
      })
      .collect()
  }

  /// The drop-in call running in a conversation, if anyone is in it.
  pub fn huddle(&self, conversation: ConversationId) -> Option<HuddleInfo> {
    let participants = roster_of(&self.snapshot.borrow(), conversation);
    (!participants.is_empty()).then_some(HuddleInfo { participants })
  }

  pub fn act(&self, action: CallAction) {
    let calls = self.clone();
    spawn_local(async move {
      if let Err(error) = call::<()>("call_action", &json!({ "action": action })).await {
        tracing::warn!(%error, "call action failed");
      }
      calls.refresh();
    });
  }

  pub fn join(&self, request: CallRequest) {
    self.act(CallAction::Join { request });
  }

  fn refresh(&self) {
    let calls = self.clone();
    spawn_local(async move {
      match call::<CallSnapshot>("call_snapshot", &json!({})).await {
        Ok(snapshot) => {
          *calls.snapshot.borrow_mut() = snapshot;
          let notify = calls.notify.borrow().clone();
          if let Some(notify) = notify {
            notify();
          }
        },
        Err(error) => tracing::warn!(%error, "could not read the call state"),
      }
    });
  }
}

/// Server roster of a conversation's call, plus the media layer's view when this device is in it.
fn roster_of(snapshot: &CallSnapshot, conversation: ConversationId) -> Vec<UserId> {
  let mut users: Vec<UserId> = snapshot
    .running
    .iter()
    .find(|running| running.conversation == conversation)
    .map(|running| running.participants.clone())
    .unwrap_or_default();
  if let Some(active) = snapshot
    .maybe_active
    .as_ref()
    .filter(|active| active.conversation == conversation)
  {
    for participant in &active.participants {
      if !users.contains(&participant.user) {
        users.push(participant.user);
      }
    }
  }
  users
}

#[cfg(test)]
mod tests {
  use super::*;
  use teamcraft_client_api::calls::{BackgroundEffect, CallKind, RunningCall};
  use teamcraft_client_api::model::id_from_raw;

  #[test]
  fn the_roster_comes_from_the_server_and_the_media_layer() {
    let conversation: ConversationId = id_from_raw(7);
    let (carol, dave): (UserId, UserId) = (id_from_raw(2), id_from_raw(3));
    let snapshot = CallSnapshot {
      maybe_active: Some(ActiveCall {
        conversation,
        kind: CallKind::Group,
        is_muted: false,
        is_deafened: false,
        is_secured: true,
        maybe_privacy_code: None,
        participants: vec![Participant {
          user: dave,
          is_muted: true,
          is_camera_on: false,
          has_hand_raised: false,
          is_sharing_screen: false,
        }],
        active_speakers: Vec::new(),
        is_camera_on: false,
        is_sharing_screen: false,
        background: BackgroundEffect::default(),
      }),
      incoming: Vec::new(),
      running: vec![RunningCall {
        conversation,
        participants: vec![carol],
        is_ringing: false,
      }],
      devices: MediaDevices::default(),
    };
    assert_eq!(roster_of(&snapshot, conversation), vec![carol, dave]);
    assert!(roster_of(&snapshot, id_from_raw(8)).is_empty());
  }
}
