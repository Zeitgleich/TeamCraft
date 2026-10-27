//! The line "… joined this channel as a workspace admin" (admin.md §2.4, issue #811).
//!
//! An admin reading a private channel is never silent: every member's device writes the line
//! itself from the MLS commit that brought the admin in, once it verified that commit (or made it
//! itself). Nothing a member or the server sends can make the line or keep it away. The message
//! older versions show instead ([`ConversationEvent::AdminJoined`]) is still sent for them; this
//! version keeps it hidden ([`StoredMessage::is_hidden`]).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use teamcraft_crypto::mls::GroupMember;
use teamcraft_proto::{
  ClientMessageId, ConversationId, ConversationKind, ConversationResponse, ConversationVisibility, Id,
  MlsMessageId, UserId,
};

use super::Client;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{ConversationEvent, LocalStore, MessageContent, StoredMessage, Table};

/// What the line needs to know of a commit this device verified or made.
pub(crate) struct JoinCommit<'a> {
  /// The commit's entry in the group's queue: where the line sits among the messages, and when.
  pub id: MlsMessageId,
  /// The group's devices before the commit.
  pub before: &'a [GroupMember],
  /// The group's devices after it.
  pub after: &'a [GroupMember],
  /// Who signed the change that let somebody in, verified with it; `None` when this device can't
  /// tell (its own commit, staged by an older version, merged after an update).
  pub maybe_signer: Option<UserId>,
}

/// Who signed this device's commit that waits for the server's answer, kept until the commit is
/// merged or dropped: the line names who let an admin in also when the answer was lost and the
/// commit is merged from the group's queue.
#[derive(Serialize, Deserialize)]
struct PendingSigner {
  /// The epoch the commit was made in.
  epoch: u64,
  signer: UserId,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Writes the line for every user `commit` brought into the conversation's group (none of their
  /// devices was in it before) whom the verified roster lists as an admin, unless the conversation
  /// is no channel or a channel open to everybody. The line names who let them in: the verified
  /// signer of the addition, a channel owner who approved (`owner_approval`) or a member who added
  /// them; nobody for the admin's own signed self-join (`visible`). Its ID comes from the commit,
  /// so a commit processed again after a restart writes it once.
  pub(crate) fn note_admin_joins(
    &mut self,
    conversation: ConversationId,
    commit: &JoinCommit<'_>,
  ) -> Result<(), CoreError> {
    let joined = new_users(commit.before, commit.after)?;
    if joined.is_empty() || !self.tells_admin_joins(conversation)? {
      return Ok(());
    }
    let workspace = self.workspace()?;
    for user in joined.into_iter().filter(|user| workspace.is_admin(*user)) {
      let maybe_approved_by = commit.maybe_signer.filter(|signer| *signer != user);
      self.store.upsert_message(&StoredMessage {
        conversation_id: conversation,
        client_message_id: line_id(commit.id, user),
        maybe_server_id: None,
        // Who approved is the line's author, as for the message older versions send; a self-join
        // is the admin's own.
        sender_user_id: maybe_approved_by.unwrap_or(user),
        maybe_thread_id: None,
        sent_at_ms: commit.id.snowflake().unix_millis(),
        content: MessageContent::Event {
          event: ConversationEvent::AdminJoined {
            user,
            is_approved: maybe_approved_by.is_some(),
          },
        },
        is_edited: false,
        edits: Vec::new(),
        maybe_edit_id: None,
        is_pinned: false,
        maybe_verified_commit: Some(commit.id),
      })?;
      tracing::info!(conversation_id = %conversation, mls_message_id = %commit.id, "a workspace admin joined the channel");
    }
    Ok(())
  }

  /// Keeps who signed the commit this device is about to post in `epoch` ([`PendingSigner`]).
  pub(crate) fn note_pending_signer(
    &self,
    conversation: ConversationId,
    epoch: u64,
    signer: UserId,
  ) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      &pending_signer_key(conversation),
      &PendingSigner { epoch, signer },
    )
  }

  /// Who signed this device's commit made in `epoch`, which is being merged; forgets it.
  pub(crate) fn take_pending_signer(
    &self,
    conversation: ConversationId,
    epoch: u64,
  ) -> Result<Option<UserId>, CoreError> {
    let key = pending_signer_key(conversation);
    let maybe_pending: Option<PendingSigner> = records::get_json(&self.store, Table::AppliedChanges, &key)?;
    self.store.delete(Table::AppliedChanges, &key)?;
    Ok(
      maybe_pending
        .filter(|pending| pending.epoch == epoch)
        .map(|pending| pending.signer),
    )
  }

  /// Whether an admin who joins `conversation` is told: a channel that isn't open to everybody, as
  /// this device knows it (its visibility is the server's word, as for the members' check of a
  /// self-join, admin.md §2.4). Not a DM, a group DM or a meeting chat: admins have no way into
  /// those of their own. A conversation this device has no record of is told.
  fn tells_admin_joins(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    let maybe_response: Option<ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    Ok(maybe_response.is_none_or(|response| {
      response.kind == ConversationKind::Channel
        && !(response.visibility == ConversationVisibility::Public && !response.is_archived)
    }))
  }
}

/// Where [`PendingSigner`] is kept.
pub(crate) fn pending_signer_key(conversation: ConversationId) -> String {
  format!("pending_signer/{conversation}")
}

/// The users with a device in `after` and none in `before`, each once. Every merged commit asks,
/// so it stays linear in the group's size.
fn new_users(before: &[GroupMember], after: &[GroupMember]) -> Result<Vec<UserId>, CoreError> {
  let earlier: BTreeSet<_> = before.iter().map(|member| member.user_id).collect();
  let mut users = Vec::new();
  for member in after.iter().filter(|member| !earlier.contains(&member.user_id)) {
    let user = convert::user_from_crypto(member.user_id)?;
    if !users.contains(&user) {
      users.push(user);
    }
  }
  Ok(users)
}

/// The line's ID: the commit's and the admin's, so every processing of the commit writes the same
/// line. Known to others, which is why no message may replace or change a line
/// ([`StoredMessage::maybe_verified_commit`]).
fn line_id(commit: MlsMessageId, user: UserId) -> ClientMessageId {
  let mut bytes = [0u8; 16];
  bytes[..8].copy_from_slice(&commit.snowflake().get().to_be_bytes());
  bytes[8..].copy_from_slice(&user.snowflake().get().to_be_bytes());
  ClientMessageId::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{DeviceId, Snowflake};

  use super::*;

  #[test]
  fn only_users_without_a_device_before_are_new() {
    let before = [member(1, 10), member(2, 20)];
    let after = [
      member(1, 10),
      member(2, 20),
      member(2, 21),
      member(3, 30),
      member(3, 31),
    ];
    assert_eq!(new_users(&before, &after).unwrap(), vec![user(3)]);
    assert!(
      new_users(&after, &before).unwrap().is_empty(),
      "removals bring nobody in"
    );
  }

  #[test]
  fn a_line_has_one_id_per_commit_and_admin() {
    let commit = MlsMessageId::from_snowflake(Snowflake::new(500).unwrap());
    let other = MlsMessageId::from_snowflake(Snowflake::new(501).unwrap());
    assert_eq!(line_id(commit, user(3)), line_id(commit, user(3)));
    assert_ne!(line_id(commit, user(3)), line_id(commit, user(4)));
    assert_ne!(line_id(commit, user(3)), line_id(other, user(3)));
  }

  fn member(raw_user: i64, raw_device: i64) -> GroupMember {
    GroupMember {
      user_id: convert::user(user(raw_user)),
      device_id: convert::device(DeviceId::from_snowflake(Snowflake::new(raw_device).unwrap())),
    }
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
