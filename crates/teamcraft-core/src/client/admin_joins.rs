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
  AdminSelfJoin, ClientMessageId, ConversationId, ConversationKind, ConversationResponse,
  ConversationVisibility, Id, MlsMessageId, UserId,
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
  /// is no channel or a channel open to everybody. The line names who let them in ([`LetInBy`]).
  /// Its ID comes from the commit, so a commit processed again after a restart writes it once.
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
    let asks_for_approval = workspace.policy.admin_self_join == AdminSelfJoin::OwnerApproval;
    for user in joined.into_iter().filter(|user| workspace.is_admin(*user)) {
      let (sender, is_approved) = match let_in_by(user, commit, asks_for_approval) {
        // The admin's own line, as for their own join.
        LetInBy::Nobody => (user, false),
        // Who let them in is the line's author, as for the message older versions send.
        LetInBy::Approval(by) => (by, true),
        LetInBy::Addition(by) => (by, false),
      };
      self.store.upsert_message(&StoredMessage {
        conversation_id: conversation,
        client_message_id: line_id(commit.id, user),
        maybe_server_id: None,
        sender_user_id: sender,
        maybe_thread_id: None,
        sent_at_ms: commit.id.snowflake().unix_millis(),
        content: MessageContent::Event {
          event: ConversationEvent::AdminJoined { user, is_approved },
        },
        is_edited: false,
        edits: Vec::new(),
        maybe_edit_id: None,
        is_pinned: false,
        maybe_verified_commit: Some(commit.id),
        maybe_server_change: None,
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

  /// Whether an admin who joins `conversation` is told ([`is_told_in`]), by the record this device
  /// keeps of it.
  fn tells_admin_joins(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    let maybe_response: Option<ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    Ok(is_told_in(maybe_response.as_ref()))
  }
}

/// Who let an admin in, as the line says it (admin.md §2.4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LetInBy {
  /// Their own signed self-join, or a commit of this device whose signer it can't tell.
  Nobody,
  /// A member of the channel, while the verified policy has members approve admins
  /// (`owner_approval`): "approved by".
  Approval(UserId),
  /// Anybody else who signed the addition: a member without an approval to give, or another admin
  /// (admin.md §2.2): "added by".
  Addition(UserId),
}

/// Who let `user` in with `commit`: the verified signer of the addition, unless that is `user`.
fn let_in_by(user: UserId, commit: &JoinCommit<'_>, asks_for_approval: bool) -> LetInBy {
  match commit.maybe_signer {
    None => LetInBy::Nobody,
    Some(signer) if signer == user => LetInBy::Nobody,
    Some(signer) => {
      let was_member = commit
        .before
        .iter()
        .any(|member| member.user_id == convert::user(signer));
      if asks_for_approval && was_member {
        LetInBy::Approval(signer)
      } else {
        LetInBy::Addition(signer)
      }
    },
  }
}

/// Whether an admin who joins a conversation is told: a channel that isn't open to everybody, as
/// `maybe_response` (this device's record) says (its visibility is the server's word, as for the
/// members' check of a self-join, admin.md §2.4). Not a DM, a group DM or a meeting chat: admins
/// have no way into those of their own. A conversation this device has no record of is told.
fn is_told_in(maybe_response: Option<&ConversationResponse>) -> bool {
  maybe_response.is_none_or(|response| {
    response.kind == ConversationKind::Channel
      && !(response.visibility == ConversationVisibility::Public && !response.is_archived)
  })
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

  /// Only channels not open to everybody; nothing known of the conversation is told too.
  #[test]
  fn admin_joins_are_told_in_closed_channels() {
    use ConversationKind::{Channel, Dm, Group, Meeting};
    use ConversationVisibility::{Private, Public};

    let cases = [
      (Channel, Private, false, true),
      (Channel, Private, true, true),
      (Channel, Public, true, true),
      (Channel, Public, false, false),
      (Dm, Private, false, false),
      (Group, Private, false, false),
      (Meeting, Private, false, false),
    ];
    for (kind, visibility, is_archived, is_told) in cases {
      let response = conversation(kind.clone(), visibility.clone(), is_archived);
      assert_eq!(
        is_told_in(Some(&response)),
        is_told,
        "{kind:?} {visibility:?} archived {is_archived}"
      );
    }
    assert!(is_told_in(None), "a conversation this device has no record of");
  }

  /// The signer names who let the admin in; "approved" only for a member while admins need an
  /// approval.
  #[test]
  fn the_signer_lets_the_admin_in() {
    let (carol, dave, admin) = (user(1), user(2), user(3));
    let before = [member(1, 10)];
    let after = [member(1, 10), member(3, 30)];
    let commit = |maybe_signer| JoinCommit {
      id: MlsMessageId::from_snowflake(Snowflake::new(500).unwrap()),
      before: &before,
      after: &after,
      maybe_signer,
    };
    assert_eq!(let_in_by(admin, &commit(Some(admin)), true), LetInBy::Nobody);
    assert_eq!(let_in_by(admin, &commit(None), true), LetInBy::Nobody);
    assert_eq!(
      let_in_by(admin, &commit(Some(carol)), true),
      LetInBy::Approval(carol)
    );
    assert_eq!(
      let_in_by(admin, &commit(Some(carol)), false),
      LetInBy::Addition(carol)
    );
    // Dave signed without being in the channel: another admin's change (admin.md §2.2).
    assert_eq!(
      let_in_by(admin, &commit(Some(dave)), true),
      LetInBy::Addition(dave)
    );
  }

  fn conversation(
    kind: ConversationKind,
    visibility: ConversationVisibility,
    is_archived: bool,
  ) -> ConversationResponse {
    ConversationResponse {
      id: ConversationId::from_snowflake(Snowflake::new(9).unwrap()),
      maybe_space_id: None,
      kind,
      maybe_name: None,
      maybe_topic: None,
      maybe_description: None,
      visibility,
      history_policy: teamcraft_proto::HistoryPolicy::FromJoin,
      maybe_category_id: None,
      is_archived,
      moderation: Default::default(),
      is_integration: false,
      contains_guests: false,
      maybe_organizer_user_id: None,
      maybe_my_permissions: None,
      maybe_my_membership: None,
      participants: Vec::new(),
      maybe_image: None,
    }
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
