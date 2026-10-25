//! What a call needs from the client (e2ee.md §8): a join token, this device's signature over its
//! per-call DAVE key, and the verified certificates of the devices that may be in the call. Media
//! runs outside the core (`teamcraft-media`); the device's private key stays inside it.

use std::fmt;

use serde::{Deserialize, Serialize};
use teamcraft_crypto::call_binding::{CallBindingError, CallId as CryptoCallId, SignedCallKeyBinding};
use teamcraft_crypto::identity::{DeviceKeyPair, VerifiedDevice};
use teamcraft_proto::{
  CallJoinTokenResponse, ConversationId, CreateCallJoinTokenRequest, DeviceId, GatewayEvent, MoveCallRequest,
  UserId,
};

use super::Client;
use crate::api::ServerApi;
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

  /// A join token, the signer and the verified devices for joining the conversation's call.
  pub async fn prepare_call(&mut self, conversation: ConversationId) -> Result<CallPreparation, CoreError> {
    self.ensure_call_group(conversation).await?;
    let join = self.call_join_token(conversation).await?;
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
    };
    note_call_move(&self.store, from, meeting, me, &rung)?;
    self.enqueue(from, None, MessageContent::Event { event })?;
    self.flush_outbox().await?;
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
    let in_dm = self.group_users(from)?;
    if !in_dm.contains(&record.by) {
      return Err(refused("announced by somebody outside the conversation's group"));
    }
    self.ensure_call_group(to).await?;
    let me = self.user_id();
    let is_expected = |user: &UserId| *user == me || in_dm.contains(user) || record.added.contains(user);
    if !self.group_users(to)?.iter().all(is_expected) {
      return Err(refused("the meeting room holds somebody the move didn't name"));
    }
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

  pub(super) fn emit_call_event(&mut self, event: &GatewayEvent) {
    if let Some(sink) = &mut self.maybe_call_sink {
      sink(event);
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
  by: UserId,
  added: &[UserId],
) -> Result<(), CoreError> {
  let record = CallMoveRecord {
    by,
    added: added.to_vec(),
  };
  records::put_json(store, Table::SyncState, &call_move_key(from, meeting), &record)
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
