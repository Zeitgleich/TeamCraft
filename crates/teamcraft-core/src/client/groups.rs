//! MLS groups per conversation: creation, signed membership changes with rebase on conflict,
//! welcomes, and processing the server's ordered MLS queue (e2ee.md §4, §4.6).

use std::collections::BTreeSet;

use rand_core::OsRng;
use teamcraft_crypto::membership::{
  HistoryShare, MembershipAction, MembershipChange, SignedMembershipChange,
};
use teamcraft_crypto::mls::{CommitOutput, ContentKeyBundle, ConversationGroup, GroupEvent};
use teamcraft_proto::{
  AddConversationMemberRequest, Base64Bytes, ClaimKeyPackagesRequest, ConversationId, CreateChannelRequest,
  CreateDmRequest, HistoryPolicy, ListMlsMessagesQuery, MlsMessageKind, MlsMessageResponse, MlsWelcome,
  PostMlsCommitRequest, PostMlsMessageRequest, SpaceId, UserId,
};

use super::Client;
use super::workspace::PendingChange;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{ConversationEvent, LocalStore, Table};
use crate::trust::{PinnedDirectory, WorkspaceAuthority};

/// Who signs a membership change this device commits.
#[derive(Clone, Debug)]
pub(crate) enum ChangeSource {
  /// This device signs the change itself.
  Own(MembershipAction),
  /// Another user (a workspace admin who isn't a member) signed it; this device commits it on
  /// their behalf (admin.md §2.2).
  Delegated(Box<SignedMembershipChange>),
}

/// Marks an MLS "proposal" entry that carries a signed admin change instead of an MLS proposal.
pub(crate) const ADMIN_CHANGE_MAGIC: &[u8] = b"TCAC";

/// What processing one MLS queue entry led to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MlsProgress {
  Done,
  /// The entry is from a later epoch than ours: fetch the backlog first.
  NeedsBacklog,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Creates a channel; this device founds its MLS group and first content key.
  pub async fn create_channel(&mut self, space: SpaceId, name: &str) -> Result<ConversationId, CoreError> {
    let request = CreateChannelRequest {
      name: name.to_string(),
      visibility: teamcraft_proto::ConversationVisibility::Private,
      maybe_category_id: None,
      maybe_history_policy: None,
      maybe_topic: None,
      maybe_description: None,
      is_integration: false,
    };
    self.create_channel_from(space, &request).await
  }

  pub(crate) async fn create_channel_from(
    &mut self,
    space: SpaceId,
    request: &CreateChannelRequest,
  ) -> Result<ConversationId, CoreError> {
    let conversation = self.api.create_channel(space, request).await?;
    records::put_json(
      &self.store,
      Table::Conversations,
      &conversation.id.to_string(),
      &conversation,
    )?;
    self.found_group(conversation.id)?;
    Ok(conversation.id)
  }

  /// Opens (or returns) the DM with `user`. The first side to get there founds the group and adds
  /// the other; if both try at once, the loser joins the winner's group (first commit wins).
  pub async fn open_dm(&mut self, user: UserId) -> Result<ConversationId, CoreError> {
    let conversation = self.api.create_dm(&CreateDmRequest { user_id: user }).await?;
    let id = conversation.id;
    records::put_json(&self.store, Table::Conversations, &id.to_string(), &conversation)?;
    if self.has_group(id)? {
      return Ok(id);
    }
    self.sync_welcomes().await?;
    if self.has_group(id)? {
      return Ok(id);
    }
    let backlog = self
      .api
      .list_mls_messages(id, &ListMlsMessagesQuery::default())
      .await?;
    if backlog.current_epoch > 0 {
      // Someone else founded it; their welcome for this device is on the way.
      return Ok(id);
    }
    self.found_group(id)?;
    self
      .commit_change(
        id,
        ChangeSource::Own(MembershipAction::AddUser {
          user_id: convert::user(user),
          history: HistoryShare::Full,
        }),
      )
      .await?;
    Ok(id)
  }

  /// Adds `user` to the conversation on the server and in the MLS group, and shares history keys
  /// according to the conversation's history policy (e2ee.md §4.3).
  pub async fn add_member(&mut self, conversation: ConversationId, user: UserId) -> Result<(), CoreError> {
    self.add_member_with_history(conversation, user, None).await
  }

  /// Like [`Self::add_member`], with an explicit "share history" choice (guests still only get
  /// what is sent from their join on). Announces the addition to the members.
  pub async fn add_member_with_history(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    maybe_history: Option<HistoryPolicy>,
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
    let requested = match maybe_history {
      Some(HistoryPolicy::FromJoin) => HistoryShare::FromJoin,
      Some(HistoryPolicy::Full) => HistoryShare::Full,
      // No choice made: the signed default, not the server's word for the channel (A9).
      _ => self.implicit_history(user)?,
    };
    let history = self.effective_history(user, requested).await?;
    self
      .commit_change(
        conversation,
        ChangeSource::Own(MembershipAction::AddUser {
          user_id: convert::user(user),
          history,
        }),
      )
      .await?;
    let shared = match history {
      HistoryShare::FromJoin => HistoryPolicy::FromJoin,
      _ => HistoryPolicy::Full,
    };
    self
      .announce(
        conversation,
        ConversationEvent::MemberAdded {
          user,
          history: shared,
        },
      )
      .await
  }

  /// Removes `user` on the server and from the MLS group, then rotates the content key so the
  /// removed person can't read anything new (e2ee.md §4.1, §4.2).
  pub async fn remove_member(&mut self, conversation: ConversationId, user: UserId) -> Result<(), CoreError> {
    self.api.remove_member(conversation, user).await?;
    self
      .commit_change(
        conversation,
        ChangeSource::Own(MembershipAction::RemoveUser {
          user_id: convert::user(user),
        }),
      )
      .await?;
    // After the key rotation, so the removed person doesn't read it.
    self
      .announce(conversation, ConversationEvent::MemberRemoved { user })
      .await
  }

  /// Adds devices of current members that aren't in the group yet (a colleague's new laptop).
  /// These need no signed change: each device is cross-signed by its user (e2ee.md §4.6).
  pub async fn add_missing_devices(&mut self, conversation: ConversationId) -> Result<usize, CoreError> {
    let members = self.api.list_members(conversation).await?;
    let directory = self
      .trusted_directory(members.members.iter().map(|member| member.user.id))
      .await?;
    let group_devices = self.group_device_ids(conversation)?;
    let mut key_packages = Vec::new();
    let mut recipients = Vec::new();
    for member in &members.members {
      let devices = self.api.list_user_devices(member.user.id).await?;
      if devices
        .devices
        .iter()
        .all(|device| group_devices.contains(&device.id))
      {
        continue;
      }
      let claimed = self
        .api
        .claim_key_packages(member.user.id, &ClaimKeyPackagesRequest::new(conversation))
        .await?;
      for package in claimed
        .packages
        .into_iter()
        .filter(|package| !group_devices.contains(&package.device_id))
      {
        recipients.push(package.device_id);
        key_packages.push(package.key_package.into_vec());
      }
    }
    if key_packages.is_empty() {
      return Ok(0);
    }
    let group = self
      .groups
      .get_mut(&conversation)
      .ok_or(CoreError::NoGroup(conversation))?;
    let output =
      group.commit_device_additions(&self.provider, &self.account.device, &key_packages, &directory)?;
    if !self.post_commit(conversation, output, &recipients).await? {
      return Err(CoreError::GaveUp {
        attempts: 1,
        reason: "another commit won the epoch".into(),
      });
    }
    self.remember_member_keys(conversation, &directory)?;
    Ok(recipients.len())
  }

  /// Fetches welcomes for this device and joins those groups.
  pub async fn sync_welcomes(&mut self) -> Result<(), CoreError> {
    loop {
      let query = ListMlsMessagesQuery {
        maybe_after: self.sync.maybe_welcome_cursor,
        maybe_limit: None,
      };
      let page = self.api.list_welcomes(self.account.device_id, &query).await?;
      for welcome in &page.messages {
        self.join_from_welcome(welcome).await?;
      }
      if !page.has_more || page.messages.is_empty() {
        return Ok(());
      }
    }
  }

  /// Processes the conversation's MLS queue after the last entry this device handled.
  pub async fn sync_mls(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    loop {
      let query = ListMlsMessagesQuery {
        maybe_after: self.sync.mls_cursors.get(&conversation).copied(),
        maybe_limit: None,
      };
      let page = self.api.list_mls_messages(conversation, &query).await?;
      for message in &page.messages {
        if self.process_mls(message).await? == MlsProgress::NeedsBacklog {
          // A later epoch than ours in the backlog means an earlier commit was rejected; this
          // device can't follow the group any more until it is re-added (crate README).
          tracing::warn!(conversation_id = %conversation, mls_message_id = %message.id, "MLS entry from a future epoch");
          self.advance_cursor(conversation, message)?;
        }
      }
      if !page.has_more || page.messages.is_empty() {
        return Ok(());
      }
    }
  }

  /// Handles one MLS queue entry of a group (commit, proposal or application message; welcomes go
  /// through [`Self::join_from_welcome`]).
  pub(crate) async fn process_mls(&mut self, message: &MlsMessageResponse) -> Result<MlsProgress, CoreError> {
    let conversation = message.conversation_id;
    if message.kind == MlsMessageKind::Welcome {
      return Ok(MlsProgress::Done);
    }
    if message.kind == MlsMessageKind::Proposal {
      // An admin's signed change for this group; queued and verified when it is committed.
      if let Some(signed) = message.payload.as_slice().strip_prefix(ADMIN_CHANGE_MAGIC) {
        self.queue_pending(PendingChange::Delegated {
          conversation_id: conversation,
          change: Base64Bytes::new(signed.to_vec()),
        })?;
      }
      self.advance_cursor(conversation, message)?;
      return Ok(MlsProgress::Done);
    }
    if self
      .sync
      .mls_cursors
      .get(&conversation)
      .is_some_and(|cursor| *cursor >= message.id)
    {
      return Ok(MlsProgress::Done);
    }
    let Some(group) = self.group(conversation)? else {
      // Not (yet) a member of the group; the welcome brings the state.
      return Ok(MlsProgress::Done);
    };
    let epoch = group.epoch();
    let is_own = message.sender_device_id == self.account.device_id;
    if message.epoch < epoch || (is_own && message.kind == MlsMessageKind::Application) {
      self.advance_cursor(conversation, message)?;
      return Ok(MlsProgress::Done);
    }
    if message.epoch > epoch {
      return Ok(MlsProgress::NeedsBacklog);
    }

    let members = self.api.list_members(conversation).await?;
    let users = members.members.iter().map(|member| member.user.id);
    // A commit may add devices: check them against the newest device lists.
    let directory = if message.kind == MlsMessageKind::Commit {
      self.trusted_directory(users).await?
    } else {
      self.pin_identities(users).await?
    };
    let authority = self.group_authority(conversation)?;
    let group = self
      .groups
      .get_mut(&conversation)
      .ok_or(CoreError::NoGroup(conversation))?;
    let result = group.process_message(&self.provider, message.payload.as_slice(), &directory, &authority);
    if result.is_ok() {
      self.remember_member_keys(conversation, &directory)?;
    }
    match result {
      Ok(GroupEvent::ContentKeys { bundle, .. }) => {
        bundle.apply_to(self.account.key_bag.ring_mut(convert::conversation(conversation)))?;
        self.persist_keys()?;
        self.retry_undecrypted(conversation)?;
      },
      Ok(GroupEvent::MembershipChanged(update)) => {
        if update.is_self_removed {
          self.drop_group(conversation)?;
          return Ok(MlsProgress::Done);
        }
        let is_from_join_add = matches!(
          update.maybe_change.as_ref().map(MembershipChange::action),
          Some(MembershipAction::AddUser {
            history: HistoryShare::FromJoin,
            ..
          })
        );
        self.note_removed_senders(conversation, &update.removed)?;
        if !update.removed.is_empty() || is_from_join_add {
          self.rotate_to_epoch_key(conversation)?;
        }
      },
      Err(error) => {
        // A rejected commit (unauthorized, forged credential) or undecryptable entry is skipped
        // and reported; the group stays in its epoch (e2ee.md §4.6, admin.md §2.3).
        tracing::warn!(conversation_id = %conversation, mls_message_id = %message.id, %error, "rejected MLS message");
      },
    }
    self.persist_group(conversation)?;
    self.advance_cursor(conversation, message)?;
    Ok(MlsProgress::Done)
  }

  /// Founds the MLS group of a conversation this device created, with a first content key.
  fn found_group(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let group = ConversationGroup::create(
      &self.provider,
      &self.account.device,
      &self.account.certificate,
      convert::conversation(conversation),
    )?;
    self.groups.insert(conversation, group);
    let ring = self.account.key_bag.ring_mut(convert::conversation(conversation));
    if ring.current().is_none() {
      ring.rotate(&mut OsRng);
    }
    self.persist_group(conversation)?;
    self.persist_keys()
  }

  /// Signs and commits a membership change; on an epoch conflict, catches up and tries again.
  pub(crate) async fn commit_change(
    &mut self,
    conversation: ConversationId,
    source: ChangeSource,
  ) -> Result<(), CoreError> {
    let source = match source {
      // The history choice is settled before signing: every member rotates (or not) by it.
      ChangeSource::Own(MembershipAction::AddUser { user_id, history }) => {
        let history = self
          .effective_history(convert::user_from_crypto(user_id)?, history)
          .await?;
        ChangeSource::Own(MembershipAction::AddUser { user_id, history })
      },
      other => other,
    };
    let action = match &source {
      ChangeSource::Own(action) => *action,
      ChangeSource::Delegated(signed) => *signed.unverified().action(),
    };
    if let MembershipAction::AddUser { user_id, history } = action
      && history
        != self
          .effective_history(convert::user_from_crypto(user_id)?, history)
          .await?
    {
      // An admin signed full history for a guest; that isn't shared (e2ee.md §4.3).
      tracing::warn!(conversation_id = %conversation, "refused to add a guest with full history");
      return Ok(());
    }
    let attempts = self.settings.max_commit_attempts.max(1);
    for _ in 0..attempts {
      if !self.has_group(conversation)? {
        return Err(CoreError::NoGroup(conversation));
      }
      let members = self.api.list_members(conversation).await?;
      let mut users: Vec<UserId> = members.members.iter().map(|member| member.user.id).collect();
      users.push(convert::user_from_crypto(action.target_user())?);
      if let ChangeSource::Delegated(signed) = &source {
        users.push(convert::user_from_crypto(signed.signer_user())?);
      }
      let directory = self.trusted_directory(users).await?;

      let (key_packages, recipients) = match action {
        MembershipAction::AddUser { user_id, .. } => {
          self
            .claim_new_devices(conversation, convert::user_from_crypto(user_id)?)
            .await?
        },
        MembershipAction::RemoveUser { .. } | MembershipAction::RemoveDevice { .. } => {
          (Vec::new(), Vec::new())
        },
      };
      if matches!(action, MembershipAction::AddUser { .. }) && key_packages.is_empty() {
        // Every device is in the group already (or the user has none yet).
        return Ok(());
      }
      let signed = match &source {
        ChangeSource::Own(action) => {
          let epoch = self.group_or_error(conversation)?.epoch();
          MembershipChange::new(convert::conversation(conversation), epoch, *action, &mut OsRng)
            .sign(&self.account.device, self.account.certificate.clone())?
        },
        ChangeSource::Delegated(signed) => (**signed).clone(),
      };
      let authority = self.group_authority(conversation)?;
      let group = self
        .groups
        .get_mut(&conversation)
        .ok_or(CoreError::NoGroup(conversation))?;
      let output = match group.commit_membership_change(
        &self.provider,
        &self.account.device,
        &signed,
        &key_packages,
        &directory,
        &authority,
      ) {
        Ok(output) => output,
        // The user is already gone from the group, or another member committed this change.
        Err(
          teamcraft_crypto::mls::MlsError::ChangeDoesNotMatchCommit
          | teamcraft_crypto::mls::MlsError::ReplayedChange(_),
        ) => {
          return Ok(());
        },
        // Another commit about the user came first; the signer has to decide again.
        Err(error @ teamcraft_crypto::mls::MlsError::StaleChange { .. }) => {
          tracing::warn!(conversation_id = %conversation, %error, "dropped a stale membership change");
          return Ok(());
        },
        Err(error) => return Err(error.into()),
      };
      let is_founding_commit = group.epoch() == 0;
      let removed: Vec<_> = group
        .members()?
        .into_iter()
        .filter(|member| match action {
          MembershipAction::RemoveUser { user_id } => member.user_id == user_id,
          MembershipAction::RemoveDevice { user_id, device_id } => {
            member.user_id == user_id && member.device_id == device_id
          },
          MembershipAction::AddUser { .. } => false,
        })
        .collect();
      if self.post_commit(conversation, output, &recipients).await? {
        self.note_removed_senders(conversation, &removed)?;
        self.remember_member_keys(conversation, &directory)?;
        let bundle = match action {
          MembershipAction::AddUser {
            history: HistoryShare::Full,
            ..
          } => {
            let bundle = ContentKeyBundle::full_history(
              self.account.key_bag.ring_mut(convert::conversation(conversation)),
              &mut OsRng,
            );
            self.persist_keys()?;
            bundle
          },
          MembershipAction::AddUser {
            history: HistoryShare::FromJoin,
            ..
          } => {
            let key = self.rotate_to_epoch_key(conversation)?;
            ContentKeyBundle::epoch_key(convert::conversation(conversation), key)
          },
          // Every remaining member derives the new key from the removal commit (e2ee.md §4.2).
          MembershipAction::RemoveUser { .. } | MembershipAction::RemoveDevice { .. } => {
            self.rotate_to_epoch_key(conversation)?;
            return Ok(());
          },
        };
        return self.send_bundle(conversation, &bundle).await;
      }
      if is_founding_commit {
        // Both sides founded the group (DM opened at once): theirs won, join it via welcome.
        self.drop_group(conversation)?;
        return self.sync_welcomes().await;
      }
      self.sync_mls(conversation).await?;
    }
    Err(CoreError::GaveUp {
      attempts,
      reason: "other commits kept winning the epoch".into(),
    })
  }

  /// Posts a staged commit. `Ok(false)` if another commit won the epoch (the commit is discarded).
  async fn post_commit(
    &mut self,
    conversation: ConversationId,
    output: CommitOutput,
    recipients: &[teamcraft_proto::DeviceId],
  ) -> Result<bool, CoreError> {
    let group = self
      .groups
      .get_mut(&conversation)
      .ok_or(CoreError::NoGroup(conversation))?;
    let welcomes = match &output.maybe_welcome {
      Some(welcome) => recipients
        .iter()
        .map(|device| MlsWelcome {
          device_id: *device,
          welcome: Base64Bytes::new(welcome.clone()),
        })
        .collect(),
      None => Vec::new(),
    };
    let request = PostMlsCommitRequest {
      device_id: self.account.device_id,
      epoch: group.epoch(),
      commit: Base64Bytes::new(output.commit),
      welcomes,
    };
    match self.api.post_mls_commit(conversation, &request).await {
      Ok(response) => {
        group.confirm_commit(&self.provider)?;
        self.persist_group(conversation)?;
        self.advance_cursor(conversation, &response.commit)?;
        Ok(true)
      },
      Err(error) => {
        group.discard_commit(&self.provider)?;
        self.persist_group(conversation)?;
        if error.is_epoch_conflict() {
          Ok(false)
        } else {
          Err(error.into())
        }
      },
    }
  }

  /// Records the verified signing keys of the group's devices, which message signatures are
  /// checked against (security review A10), and retries messages that waited for them.
  fn remember_member_keys(
    &mut self,
    conversation: ConversationId,
    directory: &PinnedDirectory,
  ) -> Result<(), CoreError> {
    let Some(group) = self.groups.get(&conversation) else {
      return Ok(());
    };
    let mut is_new = false;
    for device in group.verified_members(directory)? {
      let user = convert::user_from_crypto(device.user_id())?;
      let device_id = convert::device_from_crypto(device.device_id())?;
      // A member again (added back): its new messages count.
      self.store.delete(
        Table::RemovedSenders,
        &format!("{conversation}/{user}/{device_id}"),
      )?;
      if records::device_signing_key(&self.store, user, device_id)?.as_ref() != Some(device.signing_key()) {
        records::put_device_signing_key(&self.store, user, device_id, device.signing_key())?;
        is_new = true;
      }
    }
    if is_new {
      self.retry_undecrypted(conversation)?;
    }
    Ok(())
  }

  /// Notes when devices left the conversation: messages they sign for a later time are refused.
  fn note_removed_senders(
    &mut self,
    conversation: ConversationId,
    removed: &[teamcraft_crypto::mls::GroupMember],
  ) -> Result<(), CoreError> {
    let now = self.now_ms();
    for member in removed {
      let key = format!(
        "{conversation}/{}/{}",
        convert::user_from_crypto(member.user_id)?,
        convert::device_from_crypto(member.device_id)?
      );
      records::put_json(&self.store, Table::RemovedSenders, &key, &now)?;
    }
    Ok(())
  }

  /// Switches to the content key of the group's current epoch (after a removal or a "from join"
  /// addition, security review A5).
  fn rotate_to_epoch_key(
    &mut self,
    conversation: ConversationId,
  ) -> Result<teamcraft_crypto::content_key::ContentKey, CoreError> {
    self.group_or_error(conversation)?;
    let key = self
      .groups
      .get(&conversation)
      .ok_or(CoreError::NoGroup(conversation))?
      .epoch_content_key(&self.provider)?;
    let ring = self.account.key_bag.ring_mut(convert::conversation(conversation));
    ring.insert(key.duplicate())?;
    ring.set_current(key.id())?;
    self.persist_keys()?;
    Ok(key)
  }

  /// Sends content keys to the group as an MLS application message.
  async fn send_bundle(
    &mut self,
    conversation: ConversationId,
    bundle: &ContentKeyBundle,
  ) -> Result<(), CoreError> {
    let attempts = self.settings.max_commit_attempts.max(1);
    for _ in 0..attempts {
      let group = self
        .groups
        .get_mut(&conversation)
        .ok_or(CoreError::NoGroup(conversation))?;
      let payload = group.send_content_keys(&self.provider, &self.account.device, bundle)?;
      let request = PostMlsMessageRequest {
        device_id: self.account.device_id,
        epoch: group.epoch(),
        kind: MlsMessageKind::Application,
        payload: Base64Bytes::new(payload),
      };
      self.persist_group(conversation)?;
      match self.api.post_mls_message(conversation, &request).await {
        Ok(stored) => return self.advance_cursor(conversation, &stored),
        Err(error) if error.is_epoch_conflict() => self.sync_mls(conversation).await?,
        Err(error) => return Err(error.into()),
      }
    }
    Err(CoreError::GaveUp {
      attempts,
      reason: "content keys could not be sent in the current epoch".into(),
    })
  }

  pub(crate) async fn join_from_welcome(&mut self, welcome: &MlsMessageResponse) -> Result<(), CoreError> {
    let conversation = welcome.conversation_id;
    self.sync.maybe_welcome_cursor = Some(
      self
        .sync
        .maybe_welcome_cursor
        .map_or(welcome.id, |cursor| cursor.max(welcome.id)),
    );
    if self.has_group(conversation)? {
      return self.persist_sync();
    }
    let members = self.api.list_members(conversation).await?;
    let directory: PinnedDirectory = self
      .pin_identities(members.members.iter().map(|member| member.user.id))
      .await?;
    match ConversationGroup::join(
      &self.provider,
      welcome.payload.as_slice(),
      convert::conversation(conversation),
      &directory,
    ) {
      Ok(group) => {
        // The joining epoch's key: the members switched to it if they shared history from the
        // join on; the committer's bundle says which key is current.
        let key = group.epoch_content_key(&self.provider)?;
        self
          .account
          .key_bag
          .ring_mut(convert::conversation(conversation))
          .insert(key)?;
        self.persist_keys()?;
        self.groups.insert(conversation, group);
        self.persist_group(conversation)?;
        self.remember_member_keys(conversation, &directory)?;
        // Everything up to the welcome belongs to epochs before we joined.
        self.advance_cursor(conversation, welcome)?;
        if let Ok(response) = self.api.conversation(conversation).await {
          records::put_json(
            &self.store,
            Table::Conversations,
            &conversation.to_string(),
            &response,
          )?;
        }
        self.sync_mls(conversation).await
      },
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "rejected welcome");
        self.persist_sync()
      },
    }
  }

  /// Claims key packages for the user's devices that aren't in the group yet.
  async fn claim_new_devices(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<(Vec<Vec<u8>>, Vec<teamcraft_proto::DeviceId>), CoreError> {
    let in_group = self.group_device_ids(conversation)?;
    let claimed = match self
      .api
      .claim_key_packages(user, &ClaimKeyPackagesRequest::new(conversation))
      .await
    {
      Ok(claimed) => claimed,
      Err(error) if error.error_code() == Some(&teamcraft_proto::ErrorCode::KeyPackageExhausted) => {
        return Ok((Vec::new(), Vec::new()));
      },
      Err(error) => return Err(error.into()),
    };
    let mut key_packages = Vec::new();
    let mut recipients = Vec::new();
    // The server claims for every device of the user, including ones already in the group (and
    // this device); those packages are wasted (crate README: server gaps).
    for package in claimed.packages {
      if !in_group.contains(&package.device_id) {
        recipients.push(package.device_id);
        key_packages.push(package.key_package.into_vec());
      }
    }
    Ok((key_packages, recipients))
  }

  pub(crate) fn group_device_ids(
    &mut self,
    conversation: ConversationId,
  ) -> Result<BTreeSet<teamcraft_proto::DeviceId>, CoreError> {
    let group = self.group_or_error(conversation)?;
    group
      .members()?
      .into_iter()
      .map(|member| convert::device_from_crypto(member.device_id))
      .collect()
  }

  fn group_authority(&mut self, conversation: ConversationId) -> Result<WorkspaceAuthority, CoreError> {
    let workspace = self.workspace()?;
    let group = self.group_or_error(conversation)?;
    let members: Vec<_> = group
      .members()?
      .into_iter()
      .map(|member| member.user_id)
      .collect();
    let maybe_response: Option<teamcraft_proto::ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    let is_public_channel = maybe_response.is_some_and(|response| {
      response.kind == teamcraft_proto::ConversationKind::Channel
        && response.visibility == teamcraft_proto::ConversationVisibility::Public
        && !response.is_archived
    });
    // A roster too old to trust (an owner's re-signing is overdue) grants no admin authority.
    let max_age_ms = i64::try_from(self.settings.workspace_max_age.as_millis()).unwrap_or(i64::MAX);
    let admins: Vec<UserId> = if workspace.is_roster_fresh(self.now_ms(), max_age_ms) {
      workspace.admins().collect()
    } else {
      tracing::warn!("the signed roster is stale; admins have no authority until an owner re-signs it");
      Vec::new()
    };
    Ok(
      WorkspaceAuthority::new(members, admins, workspace.policy.admin_self_join.clone())
        .for_public_channel(is_public_channel),
    )
  }

  /// The history to share when nobody chose: the signed workspace default, but full history only
  /// for users the signed roster lists. The server's per-channel policy and its "not a guest" are
  /// unsigned, so a user this client can't vouch for gets history from the join on (security
  /// review A9).
  pub(crate) fn implicit_history(&self, user: UserId) -> Result<HistoryShare, CoreError> {
    let workspace = self.workspace()?;
    let is_vouched = workspace
      .roster
      .members
      .iter()
      .any(|member| member.user_id == user);
    Ok(match workspace.policy.history_default {
      HistoryPolicy::Full if is_vouched => HistoryShare::Full,
      _ => HistoryShare::FromJoin,
    })
  }

  /// Guests only ever see history from their join on (e2ee.md §4.3), whatever was asked for.
  async fn effective_history(
    &mut self,
    user: UserId,
    requested: HistoryShare,
  ) -> Result<HistoryShare, CoreError> {
    let summary = match records::get_json::<teamcraft_proto::UserSummary>(
      &self.store,
      Table::Users,
      &user.to_string(),
    )? {
      Some(summary) => summary,
      None => self.api.user(user).await?,
    };
    Ok(if summary.kind == teamcraft_proto::UserKind::Guest {
      HistoryShare::FromJoin
    } else {
      requested
    })
  }

  fn advance_cursor(
    &mut self,
    conversation: ConversationId,
    message: &MlsMessageResponse,
  ) -> Result<(), CoreError> {
    let cursor = self.sync.mls_cursors.entry(conversation).or_insert(message.id);
    if *cursor < message.id {
      *cursor = message.id;
    }
    self.persist_sync()
  }
}
