//! The calls as the UI sees them: a mirror of a [`CallSnapshot`] (ringing, running calls, the
//! active call, devices) and the actions the user takes. On desktop the media runs in the shell,
//! which emits `call-changed` when the mirror should re-read; in the browser it runs in the page
//! (`web_calls`), which hands over each new snapshot.
//!
//! Actions run one after the other, in the order the user took them: a camera turned on right
//! after joining reaches the media layer after the join. A failed action reaches the user
//! (issue #322).

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use leptos::task::spawn_local;
use serde_json::json;
use teamcraft_client_api::calls::{
  ActiveCall, CallRequest, HuddleInfo, IncomingCall, MediaDevices, Participant, RemoteVideoTrack,
};
use teamcraft_client_api::failure::ActionFailure;
use teamcraft_client_api::model::{ConversationId, UserId};
use teamcraft_client_api::remote::{CALL_CHANGED_EVENT, CallAction, CallSnapshot};

use crate::auth::tauri::{call, listen};

type ChangeNotifier = Rc<dyn Fn()>;

#[derive(Clone)]
pub struct RemoteCalls {
  snapshot: Rc<RefCell<CallSnapshot>>,
  notify: Rc<RefCell<Option<ChangeNotifier>>>,
  link: CallLink,
  /// Actions waiting for the one in flight.
  queue: Rc<RefCell<VecDeque<CallAction>>>,
  is_draining: Rc<Cell<bool>>,
  join_failures: Rc<Cell<u64>>,
}

/// Where the media runs.
#[derive(Clone)]
enum CallLink {
  /// The desktop shell, through its commands.
  Shell,
  /// This page (browser).
  #[cfg(target_arch = "wasm32")]
  Page(Rc<crate::web_calls::WebCallHost>),
}

impl RemoteCalls {
  /// Reads the first snapshot and follows the shell's `call-changed` events.
  pub async fn connect() -> Result<Self, String> {
    let snapshot: CallSnapshot = call("call_snapshot", &json!({})).await?;
    let calls = Self::new(snapshot, CallLink::Shell);
    let listener = calls.clone();
    listen(CALL_CHANGED_EVENT, move || listener.refresh());
    Ok(calls)
  }

  /// Calls with the media in this page.
  #[cfg(target_arch = "wasm32")]
  pub fn in_page(host: Rc<crate::web_calls::WebCallHost>) -> Self {
    let calls = Self::new(host.snapshot(), CallLink::Page(host.clone()));
    // Not the calls themselves: the host must not keep what keeps it alive.
    let (snapshot, notify) = (calls.snapshot.clone(), calls.notify.clone());
    host.set_on_change(Rc::new(move |next| {
      *snapshot.borrow_mut() = next;
      let maybe_notify = notify.borrow().clone();
      if let Some(notify) = maybe_notify {
        notify();
      }
    }));
    calls
  }

  fn new(snapshot: CallSnapshot, link: CallLink) -> Self {
    Self {
      snapshot: Rc::new(RefCell::new(snapshot)),
      notify: Rc::new(RefCell::new(None)),
      link,
      queue: Rc::default(),
      is_draining: Rc::default(),
      join_failures: Rc::default(),
    }
  }

  pub fn set_change_notifier(&self, notify: ChangeNotifier) {
    *self.notify.borrow_mut() = Some(notify);
  }

  /// Whether the media runs in this page (the browser), which has no background effects.
  pub fn is_in_page(&self) -> bool {
    !matches!(self.link, CallLink::Shell)
  }

  pub fn join_failures(&self) -> u64 {
    self.join_failures.get()
  }

  pub fn has_pending_actions(&self) -> bool {
    self.is_draining.get() || !self.queue.borrow().is_empty()
  }

  pub fn video_tracks(&self) -> Vec<RemoteVideoTrack> {
    self.snapshot.borrow().video_tracks.clone()
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

  /// Whether the user is in a call (on any device), per the server roster.
  pub fn is_in_call(&self, user: UserId) -> bool {
    let snapshot = self.snapshot.borrow();
    snapshot
      .running
      .iter()
      .any(|running| running.participants.contains(&user))
      || snapshot.maybe_active.as_ref().is_some_and(|active| {
        active
          .participants
          .iter()
          .any(|participant| participant.user == user)
      })
  }

  /// Queues an action behind those still running.
  pub fn act(&self, action: CallAction) {
    self.queue.borrow_mut().push_back(action);
    if self.is_draining.replace(true) {
      return;
    }
    let calls = self.clone();
    spawn_local(async move {
      loop {
        let maybe_action = calls.queue.borrow_mut().pop_front();
        let Some(action) = maybe_action else {
          break;
        };
        let is_join = matches!(action, CallAction::Join { .. });
        if let Err(error) = calls.carry_out(action).await {
          if is_join {
            // async-ok: a Cell the calls share, not a signal.
            calls.join_failures.set(calls.join_failures.get() + 1);
          }
          crate::feedback::report_failure(ActionFailure::from_wire(&error));
        }
      }
      // async-ok: a Cell the calls share, not a signal.
      calls.is_draining.set(false);
      calls.refresh();
    });
  }

  pub fn join(&self, request: CallRequest) {
    self.act(CallAction::Join { request });
  }

  async fn carry_out(&self, action: CallAction) -> Result<(), String> {
    match &self.link {
      CallLink::Shell => call::<()>("call_action", &json!({ "action": action })).await,
      #[cfg(target_arch = "wasm32")]
      CallLink::Page(host) => host.act(action).await,
    }
  }

  fn refresh(&self) {
    #[cfg(target_arch = "wasm32")]
    if let CallLink::Page(host) = &self.link {
      *self.snapshot.borrow_mut() = host.snapshot();
      self.notify_changed();
      return;
    }
    let calls = self.clone();
    spawn_local(async move {
      match call::<CallSnapshot>("call_snapshot", &json!({})).await {
        Ok(snapshot) => {
          *calls.snapshot.borrow_mut() = snapshot;
          calls.notify_changed();
        },
        Err(error) => tracing::warn!(%error, "could not read the call state"),
      }
    });
  }

  fn notify_changed(&self) {
    let maybe_notify = self.notify.borrow().clone();
    if let Some(notify) = maybe_notify {
      notify();
    }
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
        maybe_muted_by_host: None,
        screen_sound: Default::default(),
        is_reconnecting: false,
        reconnecting: Vec::new(),
      }),
      incoming: Vec::new(),
      running: vec![RunningCall {
        conversation,
        participants: vec![carol],
        is_ringing: false,
      }],
      devices: MediaDevices::default(),
      video_tracks: Vec::new(),
      push_to_talk: Default::default(),
      maybe_end_reason: None,
    };
    assert_eq!(roster_of(&snapshot, conversation), vec![carol, dave]);
    assert!(roster_of(&snapshot, id_from_raw(8)).is_empty());
  }
}
