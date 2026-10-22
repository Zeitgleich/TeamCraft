//! Workspace roster and policy (verified client-side), admin actions on groups the admin isn't in,
//! and membership changes the server asks for (removed users and devices).

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::membership::{
  HistoryShare, MembershipAction, MembershipChange, SignedMembershipChange,
};
use teamcraft_crypto::workspace::{WorkspaceLog, sign_log_entry};
use teamcraft_proto::{
  AddConversationMemberRequest, AppendWorkspaceLogRequest, Base64Bytes, ConversationId, DeviceId,
  HistoryPolicy, ListWorkspaceLogQuery, MlsMessageKind, PolicyDocument, PostMlsMessageRequest, UserId,
  WorkspaceLogEntryResponse, WorkspaceLogKind, WorkspacePolicy,
};

use super::Client;
use super::groups::{ADMIN_CHANGE_MAGIC, ChangeSource};
use crate::api::{ApiError, ServerApi};
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::workspace::{RosterDocument, RosterMember, RosterRole, WorkspaceState};

const WORKSPACE_KEY: &str = "state";

/// A membership change waiting to be committed by this device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum PendingChange {
  /// An admin's signed change (admin.md §2.2).
  Delegated {
    conversation_id: ConversationId,
    change: Base64Bytes,
  },
  /// The server removed the user from the conversation (admin action, deactivation).
  RemoveUser {
    conversation_id: ConversationId,
    user_id: UserId,
  },
  /// The user's device was removed (lost phone, deactivation).
  RemoveDevice {
    conversation_id: ConversationId,
    user_id: UserId,
    device_id: DeviceId,
  },
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// The workspace roster and policy as verified so far (defaults before the first sync).
  pub fn workspace(&self) -> Result<WorkspaceState, CoreError> {
    Ok(records::get_json(&self.store, Table::Workspace, WORKSPACE_KEY)?.unwrap_or_default())
  }

  /// The verified workspace policy; the default policy if none was signed yet.
  pub fn workspace_policy(&self) -> Result<WorkspacePolicy, CoreError> {
    Ok(self.workspace()?.policy)
  }

  /// Fetches new roster and policy entries and verifies them. A rejected entry stops the chain; the
  /// last verified state stays in force and [`WorkspaceState::maybe_rejection`] says why.
  pub async fn sync_workspace(&mut self) -> Result<(), CoreError> {
    let mut state = self.workspace()?;
    let mut entries = self
      .fetch_log(&WorkspaceLogKind::Roster, state.roster.version)
      .await?;
    entries.extend(
      self
        .fetch_log(&WorkspaceLogKind::Policy, state.policy_version)
        .await?,
    );
    if let Err(error) = state.apply_entries(entries) {
      tracing::warn!(%error, "rejected workspace log entry");
    }
    records::put_json(&self.store, Table::Workspace, WORKSPACE_KEY, &state)
  }

  /// Signs and publishes a new roster (owners only). Identity keys come from what this device
  /// pinned; the first roster makes this user the first owner and the workspace root of trust.
  pub async fn publish_roster(&mut self, roles: &[(UserId, RosterRole)]) -> Result<(), CoreError> {
    self.sync_workspace().await?;
    let directory_users: Vec<UserId> = roles.iter().map(|(user, _)| *user).collect();
    self.pin_identities(directory_users).await?;
    let pinned = records::pinned_identities(&self.store)?;
    let mut members = Vec::new();
    for (user, role) in roles {
      let identity = pinned.get(user).ok_or(CoreError::UnknownIdentity(*user))?;
      members.push(RosterMember {
        user_id: *user,
        role: role.clone(),
        identity_key: Base64Bytes::new(identity.public_key.to_vec()),
      });
    }
    let current = self.workspace()?.roster;
    let version = current.version + 1;
    let document = RosterDocument {
      version,
      members,
      maybe_org_recovery_key: current.maybe_org_recovery_key,
    };
    self
      .append_log(WorkspaceLogKind::Roster, WorkspaceLog::Roster, version, &document)
      .await
  }

  /// Publishes the current roster with a new org recovery key (owners only).
  pub(crate) async fn publish_org_recovery_key(&mut self, key: &[u8; 32]) -> Result<(), CoreError> {
    self.sync_workspace().await?;
    let mut document = self.workspace()?.roster;
    document.version += 1;
    document.maybe_org_recovery_key = Some(Base64Bytes::new(key.to_vec()));
    let version = document.version;
    self
      .append_log(WorkspaceLogKind::Roster, WorkspaceLog::Roster, version, &document)
      .await
  }

  /// Signs and publishes a new workspace policy (owners only).
  pub async fn publish_policy(&mut self, policy: WorkspacePolicy) -> Result<(), CoreError> {
    self.sync_workspace().await?;
    let version = self.workspace()?.policy_version + 1;
    let document = PolicyDocument { version, policy };
    self
      .append_log(WorkspaceLogKind::Policy, WorkspaceLog::Policy, version, &document)
      .await
  }

  /// An admin adds `user` to a conversation they aren't a member of: the server membership changes
  /// now, the signed change waits in the group's queue until a member's client commits it
  /// (admin.md §2.2). Adding oneself is subject to the policy's `admin_self_join`.
  pub async fn admin_add_member(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    self
      .api
      .add_member(
        conversation,
        &AddConversationMemberRequest {
          user_id: user,
          maybe_role: None,
        },
      )
      .await?;
    let history = match self.workspace_policy()?.history_default {
      HistoryPolicy::FromJoin => HistoryShare::FromJoin,
      _ => HistoryShare::Full,
    };
    self
      .post_admin_change(
        conversation,
        MembershipAction::AddUser {
          user_id: convert::user(user),
          history,
        },
      )
      .await
  }

  /// Commits what's waiting in the pending queue. Changes that turned out moot or invalid are
  /// dropped (and logged); ones that failed for network reasons stay for the next round.
  pub async fn apply_pending_changes(&mut self) -> Result<(), CoreError> {
    for (key, value) in self.store.list(Table::PendingChanges)? {
      let change: PendingChange =
        serde_json::from_slice(&value).map_err(|error| CoreError::Corrupt(error.to_string()))?;
      match self.apply_pending(&change).await {
        Ok(()) => self.store.delete(Table::PendingChanges, &key)?,
        Err(error) if error.is_transient() => return Err(error),
        Err(error) => {
          tracing::warn!(%error, "dropped a pending membership change");
          self.store.delete(Table::PendingChanges, &key)?;
        },
      }
    }
    Ok(())
  }

  pub(crate) fn queue_pending(&self, change: PendingChange) -> Result<(), CoreError> {
    let key = match &change {
      PendingChange::Delegated {
        conversation_id,
        change,
      } => format!("delegated/{conversation_id}/{}", change_id_hex(change.as_slice())),
      PendingChange::RemoveUser {
        conversation_id,
        user_id,
      } => format!("remove_user/{conversation_id}/{user_id}"),
      PendingChange::RemoveDevice {
        conversation_id,
        device_id,
        ..
      } => format!("remove_device/{conversation_id}/{device_id}"),
    };
    records::put_json(&self.store, Table::PendingChanges, &key, &change)
  }

  /// Queues the removal of a revoked device from every group it is in.
  pub(crate) fn queue_device_removal(&mut self, user: UserId, device: DeviceId) -> Result<(), CoreError> {
    if device == self.account.device_id {
      tracing::warn!("this device was revoked; it will no longer receive messages");
      return Ok(());
    }
    let conversations: Vec<ConversationId> = self.cached_conversations()?.into_keys().collect();
    for conversation in conversations {
      if self.has_group(conversation)? && self.group_device_ids(conversation)?.contains(&device) {
        self.queue_pending(PendingChange::RemoveDevice {
          conversation_id: conversation,
          user_id: user,
          device_id: device,
        })?;
      }
    }
    Ok(())
  }

  /// Queues the MLS removal of a user the server removed (if this device's group still has them).
  pub(crate) fn queue_user_removal(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(), CoreError> {
    if !self.has_group(conversation)? {
      return Ok(());
    }
    let crypto_user = convert::user(user);
    let group = self.group_or_error(conversation)?;
    if group
      .members()?
      .iter()
      .any(|member| member.user_id == crypto_user)
    {
      self.queue_pending(PendingChange::RemoveUser {
        conversation_id: conversation,
        user_id: user,
      })?;
    }
    Ok(())
  }

  async fn apply_pending(&mut self, change: &PendingChange) -> Result<(), CoreError> {
    match change {
      PendingChange::Delegated {
        conversation_id,
        change,
      } => {
        let signed = SignedMembershipChange::from_bytes(change.as_slice())?;
        self
          .commit_change(*conversation_id, ChangeSource::Delegated(Box::new(signed)))
          .await
      },
      PendingChange::RemoveUser {
        conversation_id,
        user_id,
      } => {
        let action = MembershipAction::RemoveUser {
          user_id: convert::user(*user_id),
        };
        self
          .commit_change(*conversation_id, ChangeSource::Own(action))
          .await
      },
      PendingChange::RemoveDevice {
        conversation_id,
        user_id,
        device_id,
      } => {
        let action = MembershipAction::RemoveDevice {
          user_id: convert::user(*user_id),
          device_id: convert::device(*device_id),
        };
        self
          .commit_change(*conversation_id, ChangeSource::Own(action))
          .await
      },
    }
  }

  async fn post_admin_change(
    &mut self,
    conversation: ConversationId,
    action: MembershipAction,
  ) -> Result<(), CoreError> {
    let signed = MembershipChange::new(convert::conversation(conversation), action, &mut OsRng)
      .sign(&self.account.device, self.account.certificate.clone())?;
    let mut payload = ADMIN_CHANGE_MAGIC.to_vec();
    payload.extend_from_slice(&signed.to_bytes()?);
    // A non-member can't read the group's queue, so it learns the epoch from the conflict answer.
    let mut epoch = 0;
    for _ in 0..2 {
      let request = PostMlsMessageRequest {
        device_id: self.account.device_id,
        epoch,
        kind: MlsMessageKind::Proposal,
        payload: Base64Bytes::new(payload.clone()),
      };
      match self.api.post_mls_message(conversation, &request).await {
        Ok(_) => return Ok(()),
        Err(error) if error.is_epoch_conflict() => match current_epoch_of(&error) {
          Some(current) => epoch = current,
          None => return Err(error.into()),
        },
        Err(error) => return Err(error.into()),
      }
    }
    Err(CoreError::GaveUp {
      attempts: 2,
      reason: "the group's epoch kept changing".into(),
    })
  }

  async fn fetch_log(
    &self,
    kind: &WorkspaceLogKind,
    after: u64,
  ) -> Result<Vec<WorkspaceLogEntryResponse>, CoreError> {
    let mut entries = Vec::new();
    let mut cursor = after;
    loop {
      let page = self
        .api
        .list_workspace_log(
          kind,
          &ListWorkspaceLogQuery {
            maybe_after: Some(cursor),
            maybe_limit: None,
          },
        )
        .await?;
      let has_more = page.has_more;
      if let Some(last) = page.entries.last() {
        cursor = last.version;
      }
      let is_empty = page.entries.is_empty();
      entries.extend(page.entries);
      if !has_more || is_empty {
        return Ok(entries);
      }
    }
  }

  async fn append_log(
    &mut self,
    kind: WorkspaceLogKind,
    log: WorkspaceLog,
    version: u64,
    document: &impl Serialize,
  ) -> Result<(), CoreError> {
    let payload = serde_json::to_vec(document).map_err(|error| CoreError::Corrupt(error.to_string()))?;
    let signature = sign_log_entry(
      &self.account.device,
      &self.account.certificate,
      log,
      version,
      &payload,
    )?;
    let request = AppendWorkspaceLogRequest {
      expected_version: version - 1,
      payload: Base64Bytes::new(payload),
      signature: Base64Bytes::new(signature),
      signer_device_id: self.account.device_id,
    };
    self.api.append_workspace_log(&kind, &request).await?;
    self.sync_workspace().await
  }
}

/// Reads "the group is in epoch N" from an `epoch_conflict` answer (the body has no field for it
/// yet; crate README, server gaps).
fn current_epoch_of(error: &ApiError) -> Option<u64> {
  let ApiError::Status {
    maybe_body: Some(body),
    ..
  } = error
  else {
    return None;
  };
  body
    .message
    .split_whitespace()
    .find_map(|word| word.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok())
}

/// The change ID of an encoded signed change (it follows the version byte): the queue key.
fn change_id_hex(bytes: &[u8]) -> String {
  bytes
    .iter()
    .skip(1)
    .take(16)
    .map(|byte| format!("{byte:02x}"))
    .collect()
}
