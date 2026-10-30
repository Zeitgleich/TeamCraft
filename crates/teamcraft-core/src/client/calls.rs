//! What a call needs from the client (e2ee.md §8): a join token, this device's signature over its
//! per-call DAVE key, and the verified certificates of the devices that may be in the call. Media
//! runs outside the core (`teamcraft-media`); the device's private key stays inside it.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_crypto::identity::{DeviceKeyPair, VerifiedDevice};
use teamcraft_proto::{
  CallId, CallJoinTokenResponse, CallRosterEntry, CallStateResponse, ConversationId, ConversationKind,
  CreateCallJoinTokenRequest, CreateInstantMeetingRequest, DeviceId, GatewayEvent, IdempotencyKey,
  MoveCallRequest, UserId,
};

use super::Client;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::records;
use crate::store::{ConversationEvent, LocalStore, MessageContent, Table};
use crate::trust::PinnedDirectory;

/// Prefix of the call move announcements in [`Table::SyncState`], per DM and meeting chat.
const CALL_MOVE_PREFIX: &str = "call_move/";

/// Receives the gateway's call events (start, update, end, ring) for the app's call handling.
pub type CallEventSink = Box<dyn FnMut(&GatewayEvent)>;

/// Signs call key bindings with this device's key, without handing the key out. Cheap to keep for
/// the length of a call; it can be moved to the thread that runs the media.
pub struct CallKeySigner {
  device: DeviceKeyPair,
}

/// A move of a DM's call into a meeting chat, as a member of the DM's group announced it in an
/// encrypted `CallMoved` event (docs/meetings.md, "A call becomes a meeting").
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallMoveRecord {
  pub by: UserId,
  pub added: Vec<UserId>,
  /// The DM call it moves; a record without one (or of another call) is never followed.
  #[serde(rename = "call", default, skip_serializing_if = "Option::is_none")]
  pub maybe_call: Option<CallId>,
  /// This device asked for the move: it follows without the user's tap.
  #[serde(default)]
  pub is_requested_here: bool,
}

/// A call as the call event sink last heard of it (`call_start` / `call_update`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct HeardCall {
  call_id: CallId,
  is_ringing: bool,
  participants: Vec<CallRosterEntry>,
}

/// Everything joining a conversation's call needs from the core (e2ee.md §8).
pub struct CallPreparation {
  pub join: CallJoinTokenResponse,
  pub user_id: UserId,
  pub device_id: DeviceId,
  /// Signs this device's per-call DAVE key; the device key itself stays in the core.
  pub signer: CallKeySigner,
  /// The conversation's devices (this one included) whose certificates verified.
  pub devices: Vec<VerifiedDevice>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Call events from the gateway go to `sink` from now on (one sink; a new one replaces it).
  pub fn set_call_event_sink(&mut self, sink: CallEventSink) {
    self.maybe_call_sink = Some(sink);
  }

  /// A token for joining (or starting) the conversation's call with this device.
  pub async fn call_join_token(
    &self,
    conversation: ConversationId,
  ) -> Result<CallJoinTokenResponse, CoreError> {
    let request = CreateCallJoinTokenRequest {
      device_id: self.account.device_id,
    };
    Ok(self.api.create_call_join_token(conversation, &request).await?)
  }

  /// The call running in the conversation, `None` if none runs. Never starts one (headless
  /// participants such as the notetaker only join calls people started).
  pub async fn running_call(
    &self,
    conversation: ConversationId,
  ) -> Result<Option<teamcraft_proto::CallStateResponse>, CoreError> {
    Ok(self.api.running_call(conversation).await?)
  }

  /// The users of the conversation's encryption group with the names this device knows them by
  /// (the notetaker's transcript); a user without a known name gets `None`.
  pub fn group_member_names(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Vec<(UserId, Option<String>)>, CoreError> {
    let users = self.group_users(conversation)?;
    let mut names = Vec::with_capacity(users.len());
    for user in users {
      let maybe_summary: Option<teamcraft_proto::UserSummary> =
        records::get_json(&self.store, Table::Users, &user.to_string())?;
      names.push((user, maybe_summary.map(|summary| summary.display_name)));
    }
    Ok(names)
  }

  /// A join token, the signer and the verified devices for joining the conversation's call.
  pub async fn prepare_call(&mut self, conversation: ConversationId) -> Result<CallPreparation, CoreError> {
    self.ensure_call_group(conversation).await?;
    let join = self.call_join_token(conversation).await?;
    self.call_ids.insert(conversation, join.call_id);
    let devices = self.verified_call_devices(conversation).await?;
    Ok(CallPreparation {
      join,
      user_id: self.user_id(),
      device_id: self.device_id(),
      signer: self.call_key_signer(),
      devices,
    })
  }

  pub fn call_key_signer(&self) -> CallKeySigner {
    let (signing, agreement) = self.account.device.to_secrets();
    CallKeySigner {
      device: DeviceKeyPair::from_secrets(
        self.account.device.user_id(),
        self.account.device.device_id(),
        &signing,
        &agreement,
      ),
    }
  }

  /// A call needs this device in the conversation's group (e2ee.md §8). Without one, it tries now
  /// what would get it in: the setup this device started (a meeting chat it scheduled, a DM it
  /// opened), a welcome that is waiting, or joining as a further device of a user who is in. It
  /// never founds a group here: only the device that scheduled a meeting founds its chat's group
  /// (`set_up_meeting_chat`), so a server that hides a group can't make another device fork it. A
  /// group still missing after that (the organizer's app hasn't set the chat up, nobody has let
  /// this device in) is a clear failure the user can retry, instead of a call that waits for keys
  /// it can't get.
  pub async fn ensure_call_group(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if self.has_group(conversation)? {
      return Ok(());
    }
    if !self.continue_participants_setup(conversation).await? {
      self.sync_welcomes().await?;
      if !self.has_group(conversation)?
        && let Err(error) = self.join_as_member_device(conversation).await
      {
        if error.is_transient() {
          return Err(error);
        }
        tracing::debug!(conversation_id = %conversation, %error, "not joined as a further device");
      }
    }
    if self.has_group(conversation)? {
      return Ok(());
    }
    Err(CoreError::CallEncryptionPending(conversation))
  }

  /// "Add people" in this user's DM or group DM call: starts a meeting room with everybody in the
  /// call and the added people, and moves the call into it ([`Self::move_call_to_meeting`]). The
  /// same `key` again (a retry after a lost answer) gets the same room. Returns the room's chat.
  pub async fn add_people_to_call(
    &mut self,
    from: ConversationId,
    in_call: &[UserId],
    added: &[UserId],
    maybe_title: Option<String>,
    key: IdempotencyKey,
  ) -> Result<ConversationId, CoreError> {
    let me = self.user_id();
    let invitees: Vec<UserId> = in_call
      .iter()
      .chain(added)
      .copied()
      .filter(|user| *user != me)
      .collect();
    let meeting = self
      .api
      .create_instant_meeting(&CreateInstantMeetingRequest {
        maybe_title,
        invitees,
        maybe_idempotency_key: Some(key),
      })
      .await?;
    let result = self
      .move_call_to_meeting(from, meeting.conversation_id, in_call, added)
      .await;
    if let Err(CoreError::Api(ApiError::Status { status: 409, .. })) = &result {
      // Somebody else moved the call first: this room stays empty, so it goes again.
      if let Err(cancel_error) = self.api.cancel_meeting(meeting.id).await {
        tracing::warn!(error = %cancel_error, "cancelling the room of a refused move failed");
      }
    }
    result.map(|()| meeting.conversation_id)
  }

  /// Moves this user's DM or group DM call into the meeting chat `meeting` they just started for it,
  /// because they added people to the call (docs/meetings.md, "A call becomes a meeting"). The
  /// meeting's encryption group is founded with the people in the call (`in_call`, as the call
  /// shows them) who are in the conversation's own group, plus `added`: the user's own choice and
  /// the call's verified group, never the server's member list of either conversation. Only then
  /// does the server tell everybody in the call to switch over (`call_move`) and ring the added
  /// people, so they find the group when they join. The DM itself doesn't change.
  pub async fn move_call_to_meeting(
    &mut self,
    from: ConversationId,
    meeting: ConversationId,
    in_call: &[UserId],
    added: &[UserId],
  ) -> Result<(), CoreError> {
    let me = self.user_id();
    // The announcement names this very call, so it can't move a later one.
    let Some(call) = self.call_ids.get(&from).copied() else {
      return Err(CoreError::CallMoveRefused {
        from,
        to: meeting,
        reason: "this device isn't in that call",
      });
    };
    // The DM's group is what its members agreed on in signed commits, not what the server lists.
    let in_group = self.group_users(from)?;
    let mut invitees: Vec<UserId> = in_call
      .iter()
      .copied()
      .filter(|user| *user != me && in_group.contains(user))
      .collect();
    let mut rung = Vec::new();
    for user in added.iter().copied() {
      if user != me && !invitees.contains(&user) {
        invitees.push(user);
        rung.push(user);
      }
    }
    self.set_up_meeting_chat(meeting, &invitees).await?;
    if !self.has_group(meeting)? {
      return Err(CoreError::CallEncryptionPending(meeting));
    }
    let request = MoveCallRequest {
      to_conversation_id: meeting,
      ring_user_ids: rung.clone(),
    };
    // A second move of the same call (somebody else added people at the same time) is refused
    // with a conflict: everybody follows the first one.
    self.api.move_call(from, &request).await?;
    // The others follow only this encrypted announcement, never the server's `call_move` alone.
    let event = ConversationEvent::CallMoved {
      meeting,
      added: rung.clone(),
      maybe_call: Some(call),
    };
    let record = CallMoveRecord {
      by: me,
      added: rung,
      maybe_call: Some(call),
      is_requested_here: true,
    };
    note_call_move(&self.store, from, meeting, &record)?;
    self.enqueue(from, None, MessageContent::Event { event })?;
    self.flush_outbox().await?;
    Ok(())
  }

  /// A call is over: its id and the moves announced for it are gone, so a leftover record (a follow
  /// that failed halfway) can never make this device follow a later call that reuses the id
  /// without a tap. A record of another call stays; it belongs to a call still running.
  pub(super) fn forget_call(&mut self, conversation: ConversationId, call: CallId) -> Result<(), CoreError> {
    if self.call_ids.get(&conversation) == Some(&call) {
      self.call_ids.remove(&conversation);
    }
    let prefix = format!("{CALL_MOVE_PREFIX}{conversation}/");
    for (key, value) in self.store.list(Table::SyncState)? {
      if !key.starts_with(&prefix) {
        continue;
      }
      // An unreadable record can't be followed either; it goes with the call.
      let is_of_call = serde_json::from_slice::<CallMoveRecord>(&value)
        .map(|record| record.maybe_call.is_none() || record.maybe_call == Some(call))
        .unwrap_or(true);
      if is_of_call {
        self.store.delete(Table::SyncState, &key)?;
      }
    }
    Ok(())
  }

  /// Checks the move of `from`'s call into `to` before this device follows it (docs/meetings.md):
  /// a member of `from`'s group announced it, and `to`'s group, once this device is in it, holds
  /// nobody but `from`'s members and the people the announcement added. A server that points the
  /// call somewhere else gets nobody's audio or video there.
  pub async fn accept_call_move(
    &mut self,
    from: ConversationId,
    to: ConversationId,
  ) -> Result<(), CoreError> {
    let refused = |reason| CoreError::CallMoveRefused { from, to, reason };
    let Some(record) = call_move_announcement(&self.store, from, to)? else {
      return Err(refused("nobody in the conversation announced it"));
    };
    // An announcement of an earlier call (replayed by the server) moves nothing now.
    if record.maybe_call.is_none() || record.maybe_call != self.call_ids.get(&from).copied() {
      return Err(refused("announced for another call"));
    }
    let in_dm = self.group_users(from)?;
    if !in_dm.contains(&record.by) {
      return Err(refused("announced by somebody outside the conversation's group"));
    }
    if self.api.conversation(to).await?.kind != ConversationKind::Meeting {
      return Err(refused("not a meeting room"));
    }
    self.ensure_call_group(to).await?;
    let me = self.user_id();
    let is_expected = |user: &UserId| *user == me || in_dm.contains(user) || record.added.contains(user);
    if !self.group_users(to)?.iter().all(is_expected) {
      return Err(refused("the meeting room holds somebody the move didn't name"));
    }
    // Followed once: the same announcement never moves this device again.
    self.store.delete(Table::SyncState, &call_move_key(from, to))?;
    Ok(())
  }

  /// The devices of the conversation's MLS group, their certificates verified against the pinned
  /// identities: who may be in its call. This device included.
  pub async fn verified_call_devices(
    &mut self,
    conversation: ConversationId,
  ) -> Result<Vec<VerifiedDevice>, CoreError> {
    let users = self.group_users(conversation)?;
    let directory: PinnedDirectory = self.pin_identities(users).await?;
    Ok(self.group_or_error(conversation)?.verified_members(&directory)?)
  }

  /// Compares the calls the call event sink heard of with the server's list of running calls and
  /// tells the sink what it missed (issue #976): a call that ended without its `call_end` reaching
  /// this device ends, and a call whose start or roster change it missed comes as `call_update`.
  /// Events can be lost for good: a session the server forgot (a restart, the resume window passed)
  /// starts afresh without the events in between, and a call the server lost in a restart ends
  /// without any. Runs after every gateway connection and when the user comes back to the app. A
  /// client without a sink, or a server without the list, changes nothing.
  pub async fn reconcile_calls(&mut self) -> Result<(), CoreError> {
    if self.maybe_call_sink.is_none() {
      return Ok(());
    }
    let Some(running) = self.api.running_calls().await? else {
      return Ok(());
    };
    let listed: BTreeMap<ConversationId, CallStateResponse> = running
      .into_iter()
      .map(|call| (call.conversation_id, call))
      .collect();
    let mut ended: Vec<(ConversationId, CallId)> = self
      .heard_calls
      .iter()
      .filter(|(conversation, heard)| {
        listed
          .get(conversation)
          .is_none_or(|call| call.call_id != heard.call_id)
      })
      .map(|(conversation, heard)| (*conversation, heard.call_id))
      .collect();
    ended.sort_unstable();
    for (conversation, call_id) in ended {
      tracing::info!(conversation_id = %conversation, %call_id, "a call ended without this device hearing of it");
      self.forget_call(conversation, call_id)?;
      // Why it ended is unknown here; a device still in a 1:1 call that ended for both is
      // disconnected by the SFU.
      self.emit_call_event(&GatewayEvent::CallEnd {
        conversation_id: conversation,
        call_id,
        cause: teamcraft_proto::CallEndCause::LastLeft,
      });
    }
    for (conversation, call) in listed {
      if self.heard_calls.get(&conversation) == Some(&HeardCall::of(&call)) {
        continue;
      }
      tracing::debug!(conversation_id = %conversation, call_id = %call.call_id, "a call roster this device missed");
      self.emit_call_event(&GatewayEvent::CallUpdate {
        conversation_id: conversation,
        call_id: call.call_id,
        is_ringing: call.is_ringing,
        participants: call.participants,
      });
    }
    Ok(())
  }

  /// Hands a call event to the sink and notes what the sink now knows of the call.
  pub(super) fn emit_call_event(&mut self, event: &GatewayEvent) {
    match event {
      GatewayEvent::CallStart {
        conversation_id,
        call_id,
        is_ringing,
        participants,
        ..
      }
      | GatewayEvent::CallUpdate {
        conversation_id,
        call_id,
        is_ringing,
        participants,
      } => {
        self.heard_calls.insert(
          *conversation_id,
          HeardCall {
            call_id: *call_id,
            is_ringing: *is_ringing,
            participants: participants.clone(),
          },
        );
      },
      // The sink drops the conversation's call, whichever call the event names.
      GatewayEvent::CallEnd { conversation_id, .. } => {
        self.heard_calls.remove(conversation_id);
      },
      _ => {},
    }
    if let Some(sink) = &mut self.maybe_call_sink {
      sink(event);
    }
  }
}

impl HeardCall {
  fn of(call: &CallStateResponse) -> Self {
    Self {
      call_id: call.call_id,
      is_ringing: call.is_ringing,
      participants: call.participants.clone(),
    }
  }
}

impl CallKeySigner {
  pub fn sign_call_key_binding(
    &self,
    call_id: CryptoCallId,
    dave_public_key: &[u8],
    not_after: u64,
  ) -> Result<SignedCallKeyBinding, CallBindingError> {
    self
      .device
      .sign_call_key_binding(call_id, dave_public_key, not_after)
  }
}

// The key pair must not end up in logs.
impl fmt::Debug for CallKeySigner {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("CallKeySigner").finish_non_exhaustive()
  }
}

/// Notes an announced move of `from`'s call into `meeting` (an own one or a member's, received in
/// `from`'s group).
pub(crate) fn note_call_move(
  store: &impl LocalStore,
  from: ConversationId,
  meeting: ConversationId,
  record: &CallMoveRecord,
) -> Result<(), CoreError> {
  records::put_json(store, Table::SyncState, &call_move_key(from, meeting), record)
}

/// The announced move of `from`'s call into `meeting`, if any.
pub fn call_move_announcement(
  store: &impl LocalStore,
  from: ConversationId,
  meeting: ConversationId,
) -> Result<Option<CallMoveRecord>, CoreError> {
  records::get_json(store, Table::SyncState, &call_move_key(from, meeting))
}

fn call_move_key(from: ConversationId, meeting: ConversationId) -> String {
  format!("{CALL_MOVE_PREFIX}{from}/{meeting}")
}
