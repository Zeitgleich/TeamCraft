//! MLS groups per conversation: creation, signed membership changes with rebase on conflict,
//! welcomes, and processing the server's ordered MLS queue (e2ee.md §4, §4.6).

use std::collections::BTreeSet;

use rand_core::OsRng;
use teamcraft_crypto::membership::{
  HistoryShare, MembershipAction, MembershipChange, SignedMembershipChange,
};
use teamcraft_crypto::mls::{
  CommitOutput, ContentKeyBundle, ConversationGroup, GroupEvent, GroupMember, MembershipUpdate, MlsError,
  RejoinRequest, SignedRejoinRequest,
};
use teamcraft_proto::{
  AddConversationMemberRequest, Base64Bytes, ClaimKeyPackagesRequest, ConversationId, CreateChannelRequest,
  CreateDmRequest, CreateGroupDmRequest, DeviceId, HistoryPolicy, Id, IdempotencyKey, ListMlsMessagesQuery,
  MlsMessageKind, MlsMessageResponse, MlsWelcome, PostMlsCommitRequest, PostMlsMessageRequest, SpaceId,
  UserId,
};

use super::workspace::{PendingChange, SharedKeys};
use super::{Client, pending_post_key};
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
/// Marks an MLS "proposal" entry that carries a device's signed request to be added back.
pub(crate) const REJOIN_REQUEST_MAGIC: &[u8] = b"TCRJ";
/// Rounds of "catch up, send the lost commit again" before [`Client::sync_mls`] leaves a commit
/// pending for the next sync.
const PENDING_COMMIT_ROUNDS: u32 = 3;

/// This device's commit, merged: what it did and when the server stored it.
struct OwnCommit {
  update: MembershipUpdate,
  at_ms: i64,
}

/// Where the queue left this device after one pass of [`Client::sync_mls`].
struct CatchUp {
  /// An entry from a later epoch came with every earlier one handled: this device missed or
  /// refused a commit the others merged.
  is_behind: bool,
  current_epoch: u64,
}

/// That this device asked to be added back to a group, and when.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct RejoinRecord {
  requested_at_ms: i64,
}

/// The claimed key packages of one user that can go into a commit.
#[derive(Default)]
struct UsablePackages {
  key_packages: Vec<Vec<u8>>,
  recipients: Vec<teamcraft_proto::DeviceId>,
  /// Why the first left-out package was refused.
  maybe_first_error: Option<MlsError>,
}

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
      maybe_idempotency_key: Some(IdempotencyKey::random()),
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
    // A repeated create (idempotency key) returns the channel we already founded.
    if !self.has_group(conversation.id)? {
      self.found_group(conversation.id)?;
    }
    Ok(conversation.id)
  }

  /// Opens (or returns) the DM with `user`. The first side to get there founds the group and adds
  /// the other; if both try at once, the loser joins the winner's group (first commit wins).
  pub async fn open_dm(&mut self, user: UserId) -> Result<ConversationId, CoreError> {
    let conversation = self.api.create_dm(&CreateDmRequest { user_id: user }).await?;
    self.found_participants_group(conversation, &[user]).await
  }

  /// Opens (or returns) the group DM with exactly these people (and the user). Founding works
  /// like [`Self::open_dm`]: whoever commits first owns the group, the others join by welcome.
  pub async fn open_group_dm(&mut self, users: &[UserId]) -> Result<ConversationId, CoreError> {
    let conversation = self
      .api
      .create_group_dm(&CreateGroupDmRequest {
        user_ids: users.to_vec(),
      })
      .await?;
    let me = self.user_id();
    let others: Vec<UserId> = conversation_members(users, me);
    self.found_participants_group(conversation, &others).await
  }

  /// Stores a DM or group DM and founds its MLS group with `others`, unless this device already
  /// has it or someone else founded it first.
  async fn found_participants_group(
    &mut self,
    conversation: teamcraft_proto::ConversationResponse,
    others: &[UserId],
  ) -> Result<ConversationId, CoreError> {
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
    for user in others {
      self
        .commit_change(
          id,
          ChangeSource::Own(MembershipAction::AddUser {
            user_id: convert::user(*user),
            history: HistoryShare::Full,
          }),
        )
        .await?;
    }
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
      _ => self.implicit_history_in(conversation, user).await?,
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

  /// Adds devices of current members that aren't in the group yet (a colleague's new laptop).
  /// These need no signed change: each device is cross-signed by its user (e2ee.md §4.6).
  pub async fn add_missing_devices(&mut self, conversation: ConversationId) -> Result<usize, CoreError> {
    self.resolve_pending_commit(conversation).await?;
    let members = self.api.list_members(conversation).await?;
    let directory = self
      .trusted_directory(members.members.iter().map(|member| member.user.id))
      .await?;
    let group_devices = self.group_device_ids(conversation)?;
    let group_users: BTreeSet<UserId> = self.group_users(conversation)?.into_iter().collect();
    let mut key_packages = Vec::new();
    let mut recipients = Vec::new();
    for member in &members.members {
      // Only further devices of users in the group go in without a signed change. A user the
      // server still lists but who has no device here is being removed (the server's membership
      // goes after the MLS removal) or not added yet: claiming their key packages would waste them.
      if !group_users.contains(&member.user.id) {
        continue;
      }
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
      // Other users' devices still go in when one user's packages are all bad.
      let usable = self.usable_key_packages(conversation, member.user.id, claimed.packages, &directory)?;
      key_packages.extend(usable.key_packages);
      recipients.extend(usable.recipients);
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
    let Some(own) = self.post_commit(conversation, output, &recipients).await? else {
      return Err(CoreError::GaveUp {
        attempts: 1,
        reason: "another commit won the epoch".into(),
      });
    };
    self.remember_member_keys(conversation, &directory, own.at_ms)?;
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

  /// Processes the conversation's MLS queue after the last entry this device handled. A commit of
  /// this device whose answer was lost is merged when it shows up there, or sent again when the
  /// server never got it. A device that can't follow the group any more asks to be added back
  /// (e2ee.md §4.6).
  pub async fn sync_mls(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let mut catch_up = match self.catch_up_mls(conversation).await {
      Ok(catch_up) => catch_up,
      // The server no longer lets this user read the queue: removed, with the removal commit
      // still to come or refused. Nothing to catch up or ask for; the server's removal event ends
      // the group here.
      Err(CoreError::Api(error)) if error.is_not_found() || error.is_forbidden() => {
        tracing::debug!(conversation_id = %conversation, "no longer allowed to read the group's queue");
        return Ok(());
      },
      Err(error) => return Err(error),
    };
    for _ in 0..PENDING_COMMIT_ROUNDS {
      if catch_up.is_behind || !self.has_pending_commit(conversation)? {
        break;
      }
      // Neither this commit nor another one for the epoch is in the queue: the server never got
      // it. Another commit may win meanwhile; the next pass finds out.
      self.post_pending_again(conversation).await?;
      catch_up = self.catch_up_mls(conversation).await?;
    }
    if catch_up.is_behind {
      return self.request_rejoin(conversation, catch_up.current_epoch).await;
    }
    if !self.has_pending_commit(conversation)? {
      self.mls_current.insert(conversation);
    }
    Ok(())
  }

  /// Catches up the conversation's MLS queue once per gateway connection before the outbox sends
  /// into it, so nothing goes out under a key that a member removed meanwhile still holds.
  pub(crate) async fn ensure_mls_current(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if self.mls_current.contains(&conversation) || !self.has_group(conversation)? {
      return Ok(());
    }
    self.sync_mls(conversation).await
  }

  /// One pass over the queue after the cursor.
  async fn catch_up_mls(&mut self, conversation: ConversationId) -> Result<CatchUp, CoreError> {
    loop {
      let query = ListMlsMessagesQuery {
        maybe_after: self.sync.mls_cursors.get(&conversation).copied(),
        maybe_limit: None,
      };
      let page = self.api.list_mls_messages(conversation, &query).await?;
      for message in &page.messages {
        if self.process_mls(message).await? == MlsProgress::NeedsBacklog {
          // Every earlier entry was handled and the group is still an epoch behind: this device
          // missed or refused a commit the others merged. The cursor stays: once the device is
          // added back, the entries after its welcome are still to be read.
          tracing::warn!(conversation_id = %conversation, mls_message_id = %message.id, "MLS entry from a future epoch");
          return Ok(CatchUp {
            is_behind: true,
            current_epoch: page.current_epoch,
          });
        }
      }
      if !page.has_more || page.messages.is_empty() {
        return Ok(CatchUp {
          is_behind: false,
          current_epoch: page.current_epoch,
        });
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
      if let Some(request) = message.payload.as_slice().strip_prefix(REJOIN_REQUEST_MAGIC)
        && message.sender_device_id != self.account.device_id
      {
        self.note_rejoin_request(conversation, request).await?;
      }
      if let Some(user) = message
        .payload
        .as_slice()
        .strip_prefix(super::member_device::DEVICE_ADD_REQUEST_MAGIC)
        && message.sender_device_id != self.account.device_id
      {
        self
          .note_device_add_request(conversation, message.sender_device_id, user)
          .await?;
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

    let users: Vec<UserId> = match self.api.list_members(conversation).await {
      Ok(members) => members.members.into_iter().map(|member| member.user.id).collect(),
      // The server dropped this user's membership already: the removal commit goes first and the
      // server's membership after it (e2ee.md §4.6), so this is likely that commit. Who is in the
      // group is all it needs to verify.
      Err(error) if error.is_not_found() || error.is_forbidden() => self.group_users(conversation)?,
      Err(error) => return Err(error.into()),
    };
    // A commit may add devices: check them against the newest device lists.
    let directory = if message.kind == MlsMessageKind::Commit {
      self.trusted_directory(users).await?
    } else {
      self.pin_identities(users).await?
    };
    // The server's time of the entry, the same for every member: merge decisions don't depend on
    // the clock of the device that makes them (a skewed clock would split the group).
    let at_ms = message.id.snowflake().unix_millis();
    if message.kind == MlsMessageKind::Commit
      && let Some(pending) = self.group_or_error(conversation)?.pending_commit()
    {
      if is_own && pending.is_commit(message.payload.as_slice()) {
        // This device's commit, whose answer was lost: the server took it.
        let update = self.merge_own_commit(conversation, message)?;
        self.remember_member_keys(conversation, &directory, at_ms)?;
        if let Some(share) = self.after_own_commit(conversation, &update, at_ms)? {
          self.queue_pending(PendingChange::ShareHistory {
            conversation_id: conversation,
            share,
          })?;
        }
        return Ok(MlsProgress::Done);
      }
      // Another commit won the epoch.
      self.discard_pending_commit(conversation)?;
    }
    let authority = self.group_authority(conversation, at_ms)?;
    let group = self
      .groups
      .get_mut(&conversation)
      .ok_or(CoreError::NoGroup(conversation))?;
    let result = group.process_message(&self.provider, message.payload.as_slice(), &directory, &authority);
    if result.is_ok() {
      self.remember_member_keys(conversation, &directory, at_ms)?;
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
          if update.maybe_rejoined == Some(self.own_member()) {
            // A member replaced this device's leaf; its welcome brings the new group.
            records::put_json(
              &self.store,
              Table::AppliedChanges,
              &rejoin_key(conversation),
              &RejoinRecord {
                requested_at_ms: self.now_ms(),
              },
            )?;
          }
          return Ok(MlsProgress::Done);
        }
        if let Some(device) = update.maybe_rejoined {
          // Somebody answered that device's request already.
          self.forget_rejoin_request(conversation, device)?;
        }
        let is_from_join_add = matches!(
          update.maybe_change.as_ref().map(MembershipChange::action),
          Some(MembershipAction::AddUser {
            history: HistoryShare::FromJoin,
            ..
          })
        );
        self.note_removed_senders(conversation, &left_members(&update), at_ms)?;
        // A rejoin too: the device added back takes the new epoch's key.
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
    if let (ChangeSource::Delegated(_), MembershipAction::RemoveUser { user_id }) = (&source, action)
      && user_id == convert::user(self.account.user_id)
    {
      // MLS doesn't let a member commit its own removal: the other members commit it, and the
      // server's membership removal ends the group here.
      return Ok(());
    }
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
    if let (ChangeSource::Delegated(signed), MembershipAction::AddUser { user_id, history }) =
      (&source, action)
      && signed.signer_user() == user_id
      && history != HistoryShare::FromJoin
    {
      // Somebody added themselves and signed the past for themselves: only a member who chooses
      // to share it adds somebody with full history (e2ee.md §4.7).
      tracing::warn!(conversation_id = %conversation, "refused a self-add with full history");
      return Ok(());
    }
    let attempts = self.settings.max_commit_attempts.max(1);
    for _ in 0..attempts {
      if !self.has_group(conversation)? {
        return Err(CoreError::NoGroup(conversation));
      }
      self.resolve_pending_commit(conversation).await?;
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
            .claim_new_devices(conversation, convert::user_from_crypto(user_id)?, &directory)
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
      // This device's own decision to commit; the members check the commit at the server's time.
      let authority = self.group_authority(conversation, self.now_ms())?;
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
      if let Some(own) = self.post_commit(conversation, output, &recipients).await? {
        self.remember_member_keys(conversation, &directory, own.at_ms)?;
        return match self.after_own_commit(conversation, &own.update, own.at_ms)? {
          Some(share) => self.share_history_or_queue(conversation, share).await,
          None => Ok(()),
        };
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

  /// Posts a staged commit and merges it once the server took it. `Ok(None)` if another commit won
  /// the epoch (the commit is discarded). If the outcome is unknown (the answer was lost), the
  /// commit stays pending and is merged when it shows up in the group's queue
  /// ([`Self::sync_mls`]), so the device never falls an epoch behind its own commit (e2ee.md §4.6).
  async fn post_commit(
    &mut self,
    conversation: ConversationId,
    output: CommitOutput,
    recipients: &[DeviceId],
  ) -> Result<Option<OwnCommit>, CoreError> {
    let epoch = self.group_or_error(conversation)?.epoch();
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
      epoch,
      commit: Base64Bytes::new(output.commit),
      welcomes,
      // A very large group's information is left out rather than making the commit too large.
      maybe_group_info: output
        .maybe_group_info
        .filter(|info| info.len() <= self.settings.max_group_info_bytes)
        .map(Base64Bytes::new),
    };
    // Kept before sending, so a restart mid-request still finds the commit.
    self.persist_group(conversation)?;
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      &pending_post_key(conversation),
      &request,
    )?;
    match self.api.post_mls_commit(conversation, &request).await {
      Ok(response) => {
        let update = self.merge_own_commit(conversation, &response.commit)?;
        Ok(Some(OwnCommit {
          update,
          at_ms: response.commit.id.snowflake().unix_millis(),
        }))
      },
      Err(error) if error.is_epoch_conflict() => {
        self.discard_pending_commit(conversation)?;
        Ok(None)
      },
      Err(error) if error.is_transient() => {
        tracing::warn!(conversation_id = %conversation, %error, "commit outcome unknown; kept until the group's queue shows it");
        Err(error.into())
      },
      Err(error) => {
        // A definite refusal: the server didn't store it.
        self.discard_pending_commit(conversation)?;
        Err(error.into())
      },
    }
  }

  /// Settles a commit whose answer was lost before this device commits again: merged if the queue
  /// shows it, discarded if another commit won, sent again if the server never got it.
  async fn resolve_pending_commit(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if !self.has_pending_commit(conversation)? {
      return Ok(());
    }
    self.sync_mls(conversation).await?;
    if self.has_pending_commit(conversation)? {
      return Err(CoreError::CommitPending(conversation));
    }
    Ok(())
  }

  /// Sends the pending commit again, exactly as before. Merged if the server takes it; on an epoch
  /// conflict it stays pending, and the queue tells whether it was this commit or another.
  async fn post_pending_again(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let Some(request) = records::get_json::<PostMlsCommitRequest>(
      &self.store,
      Table::AppliedChanges,
      &pending_post_key(conversation),
    )?
    else {
      // Nothing to send it with: drop it, the queue decides.
      return self.discard_pending_commit(conversation);
    };
    match self.api.post_mls_commit(conversation, &request).await {
      Ok(response) => {
        let update = self.merge_own_commit(conversation, &response.commit)?;
        let directory = self.member_directory(conversation).await?;
        let at_ms = response.commit.id.snowflake().unix_millis();
        self.remember_member_keys(conversation, &directory, at_ms)?;
        if let Some(share) = self.after_own_commit(conversation, &update, at_ms)? {
          self.queue_pending(PendingChange::ShareHistory {
            conversation_id: conversation,
            share,
          })?;
        }
        Ok(())
      },
      Err(error) if error.is_epoch_conflict() => Ok(()),
      Err(error) if error.is_transient() => Err(error.into()),
      Err(error) => {
        self.discard_pending_commit(conversation)?;
        Err(error.into())
      },
    }
  }

  /// Merges this device's pending commit, which the server stored as `entry`.
  fn merge_own_commit(
    &mut self,
    conversation: ConversationId,
    entry: &MlsMessageResponse,
  ) -> Result<MembershipUpdate, CoreError> {
    let group = self
      .groups
      .get_mut(&conversation)
      .ok_or(CoreError::NoGroup(conversation))?;
    let update = group.confirm_commit(&self.provider)?;
    self.persist_group(conversation)?;
    self
      .store
      .delete(Table::AppliedChanges, &pending_post_key(conversation))?;
    if let Some(device) = update.maybe_rejoined {
      self.forget_rejoin_request(conversation, device)?;
    }
    self.advance_cursor(conversation, entry)?;
    Ok(update)
  }

  fn discard_pending_commit(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let group = self
      .groups
      .get_mut(&conversation)
      .ok_or(CoreError::NoGroup(conversation))?;
    group.discard_commit(&self.provider)?;
    self.persist_group(conversation)?;
    self
      .store
      .delete(Table::AppliedChanges, &pending_post_key(conversation))?;
    Ok(())
  }

  fn has_pending_commit(&mut self, conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(
      self
        .group(conversation)?
        .is_some_and(|group| group.has_pending_commit()),
    )
  }

  /// The local consequences of this device's merged commit, which the server stored at `at_ms`.
  /// Returns the content keys the new member still needs.
  fn after_own_commit(
    &mut self,
    conversation: ConversationId,
    update: &MembershipUpdate,
    at_ms: i64,
  ) -> Result<Option<SharedKeys>, CoreError> {
    self.note_removed_senders(conversation, &left_members(update), at_ms)?;
    match update.maybe_change.as_ref().map(MembershipChange::action) {
      Some(MembershipAction::AddUser {
        history: HistoryShare::Full,
        ..
      }) => Ok(Some(SharedKeys::FullHistory)),
      Some(MembershipAction::AddUser {
        history: HistoryShare::FromJoin,
        ..
      }) => {
        self.rotate_to_epoch_key(conversation)?;
        Ok(Some(SharedKeys::CurrentKey))
      },
      // Every remaining member derives the new key from the removal commit (e2ee.md §4.2).
      Some(MembershipAction::RemoveUser { .. } | MembershipAction::RemoveDevice { .. }) => {
        self.rotate_to_epoch_key(conversation)?;
        Ok(None)
      },
      // Everybody switches to the new epoch's key, which the device added back derives from its
      // welcome. Keys of epochs it missed come only from the user's key bag, if another device
      // of the user had them.
      None if update.maybe_rejoined.is_some() => {
        self.rotate_to_epoch_key(conversation)?;
        Ok(None)
      },
      // Further devices of members: the user's key bag carries the history.
      None => Ok(None),
    }
  }

  /// Shares content keys after an addition now; offline, they are queued and sent later.
  async fn share_history_or_queue(
    &mut self,
    conversation: ConversationId,
    share: SharedKeys,
  ) -> Result<(), CoreError> {
    match self.share_history(conversation, share).await {
      Err(error) if error.is_transient() => {
        self.queue_pending(PendingChange::ShareHistory {
          conversation_id: conversation,
          share,
        })?;
        Err(error)
      },
      other => other,
    }
  }

  /// Sends the content keys a new member needs to the group.
  pub(crate) async fn share_history(
    &mut self,
    conversation: ConversationId,
    share: SharedKeys,
  ) -> Result<(), CoreError> {
    let ring = self.account.key_bag.ring_mut(convert::conversation(conversation));
    let bundle = match share {
      SharedKeys::FullHistory => {
        let bundle = ContentKeyBundle::full_history(ring, &mut OsRng);
        self.persist_keys()?;
        bundle
      },
      SharedKeys::CurrentKey => {
        let key = ring
          .current()
          .ok_or(CoreError::NoContentKey(conversation))?
          .duplicate();
        ContentKeyBundle::epoch_key(convert::conversation(conversation), key)
      },
    };
    self.send_bundle(conversation, &bundle).await
  }

  /// Pinned identities of the conversation's members, to check the group's devices against.
  async fn member_directory(&mut self, conversation: ConversationId) -> Result<PinnedDirectory, CoreError> {
    let members = self.api.list_members(conversation).await?;
    self
      .pin_identities(members.members.iter().map(|member| member.user.id))
      .await
  }

  /// Records the verified signing keys of the group's devices, which message signatures are
  /// checked against (security review A10), and retries messages that waited for them.
  /// `at_ms` is the server's time of the commit (or welcome) that showed the members: a device
  /// added back after it left counts again from then on.
  pub(crate) fn remember_member_keys(
    &mut self,
    conversation: ConversationId,
    directory: &PinnedDirectory,
    at_ms: i64,
  ) -> Result<(), CoreError> {
    let Some(group) = self.groups.get(&conversation) else {
      return Ok(());
    };
    let mut is_new = false;
    for device in group.verified_members(directory)? {
      let user = convert::user_from_crypto(device.user_id())?;
      let device_id = convert::device_from_crypto(device.device_id())?;
      // A member again (added back): what it signs from now on counts; what it signed while out
      // still doesn't.
      let key = format!("{conversation}/{user}/{device_id}");
      // Only while it is out: the first commit (or welcome) that shows it again is the one that
      // added it back; later ones must not move its return.
      if let Some(absence) = records::removed_sender(&self.store, &key)?
        && absence.left_at_ms <= at_ms
        && absence
          .maybe_back_at_ms
          .is_none_or(|back| back < absence.left_at_ms)
      {
        records::merge_removed_sender(
          &self.store,
          &key,
          &crate::account_data::SenderAbsence {
            left_at_ms: absence.left_at_ms,
            maybe_back_at_ms: Some(at_ms),
          },
        )?;
        self.update_account_data(|data, _| data.note_sender_back(&key, at_ms))?;
      }
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

  /// Notes when devices left the conversation, at the server's time of the removal commit:
  /// messages they sign for a later time are refused. Server times on both sides, so a skewed local
  /// clock refuses nothing it shouldn't.
  fn note_removed_senders(
    &mut self,
    conversation: ConversationId,
    removed: &[GroupMember],
    at_ms: i64,
  ) -> Result<(), CoreError> {
    for member in removed {
      let key = format!(
        "{conversation}/{}/{}",
        convert::user_from_crypto(member.user_id)?,
        convert::device_from_crypto(member.device_id)?
      );
      records::merge_removed_sender(
        &self.store,
        &key,
        &crate::account_data::SenderAbsence {
          left_at_ms: at_ms,
          maybe_back_at_ms: None,
        },
      )?;
      // The user's other (and future) devices refuse its later messages too.
      self.update_account_data(|data, _| data.note_removed_sender(&key, at_ms))?;
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
    // A new device of the user won't join from group information older than this (e2ee.md §4.6).
    let epoch = self.group_or_error(conversation)?.epoch();
    self.update_account_data(|data, _| data.note_group_epoch(conversation, epoch))?;
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
      self.resolve_pending_commit(conversation).await?;
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
    if self.is_welcome_outdated(conversation, welcome)? {
      return self.welcome_handled(welcome);
    }
    if self.has_group(conversation)? && !self.has_requested_rejoin(conversation)? {
      // A welcome to a later epoch of a group this device follows: the queue decides first. A
      // commit that replaced this device's leaf comes before its welcome and drops the group.
      self.sync_mls(conversation).await?;
      if self.is_welcome_outdated(conversation, welcome)? {
        return self.welcome_handled(welcome);
      }
    }
    let is_rejoin = self.has_requested_rejoin(conversation)?;
    if self.has_group(conversation)? {
      if !is_rejoin {
        return self.welcome_handled(welcome);
      }
      // This device asked to be added back: the welcome replaces the group it can't follow.
      self.drop_group(conversation)?;
    }
    match self.join_welcome_group(conversation, welcome).await? {
      Ok((group, directory)) => {
        // The joining epoch's key: the members switched to it if they shared history from the
        // join on; the committer's bundle says which key is current.
        let key = group.epoch_content_key(&self.provider)?;
        let key_id = key.id();
        let ring = self.account.key_bag.ring_mut(convert::conversation(conversation));
        ring.insert(key)?;
        if is_rejoin {
          // The members switched to this epoch's key with the commit that added this device back;
          // only members of the epoch can derive it (security review A5).
          ring.set_current(key_id)?;
        }
        self.persist_keys()?;
        self.groups.insert(conversation, group);
        self.persist_group(conversation)?;
        self
          .store
          .delete(Table::AppliedChanges, &rejoin_key(conversation))?;
        self.store.delete(
          Table::AppliedChanges,
          &super::member_device::device_add_key(conversation),
        )?;
        self.welcome_handled(welcome)?;
        self.remember_member_keys(conversation, &directory, welcome.id.snowflake().unix_millis())?;
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
        self.welcome_handled(welcome)
      },
    }
  }

  /// Verifies and joins the group a welcome brings, with the directory it was verified against.
  /// Identities are pinned for everybody the welcome's tree holds, not only whom the server lists:
  /// somebody removed after the welcome was made is still in its tree, their removal commit next in
  /// the queue, while the server dropped them already (its membership goes after the MLS removal,
  /// e2ee.md §4.6). Every leaf is verified against its user's pinned identity either way.
  async fn join_welcome_group(
    &mut self,
    conversation: ConversationId,
    welcome: &MlsMessageResponse,
  ) -> Result<Result<(ConversationGroup, PinnedDirectory), MlsError>, CoreError> {
    // Pinned before the welcome is opened, which takes this device's key package: a network error
    // here leaves the welcome for the next try.
    let members = self.api.list_members(conversation).await?;
    self
      .pin_identities(members.members.iter().map(|member| member.user.id))
      .await?;
    let staged = match ConversationGroup::stage_join(
      &self.provider,
      welcome.payload.as_slice(),
      convert::conversation(conversation),
    ) {
      Ok(staged) => staged,
      Err(error) => return Ok(Err(error)),
    };
    let mut users = Vec::new();
    for user in staged.claimed_users()? {
      users.push(convert::user_from_crypto(user)?);
    }
    // Pinned above already, except somebody removed meanwhile.
    let directory = self.pin_identities(users).await?;
    Ok(
      staged
        .complete(&self.provider, &directory)
        .map(|group| (group, directory)),
    )
  }

  /// Whether this device's group is already at or past the welcome's epoch.
  fn is_welcome_outdated(
    &mut self,
    conversation: ConversationId,
    welcome: &MlsMessageResponse,
  ) -> Result<bool, CoreError> {
    Ok(
      self
        .group(conversation)?
        .is_some_and(|group| welcome.epoch <= group.epoch()),
    )
  }

  /// Moves the welcome cursor past `welcome` once it has been joined or definitely rejected. Only
  /// then: a welcome skipped after a network error would be lost, and with it the conversation.
  fn welcome_handled(&mut self, welcome: &MlsMessageResponse) -> Result<(), CoreError> {
    self.sync.maybe_welcome_cursor = Some(
      self
        .sync
        .maybe_welcome_cursor
        .map_or(welcome.id, |cursor| cursor.max(welcome.id)),
    );
    self.persist_sync()
  }

  /// Claims key packages for the user's devices that aren't in the group yet.
  async fn claim_new_devices(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    directory: &PinnedDirectory,
  ) -> Result<(Vec<Vec<u8>>, Vec<teamcraft_proto::DeviceId>), CoreError> {
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
    let usable = self.usable_key_packages(conversation, user, claimed.packages, directory)?;
    match usable.maybe_first_error {
      // Not one device of the user can be added: say why instead of doing nothing.
      Some(error) if usable.key_packages.is_empty() => Err(error.into()),
      _ => Ok((usable.key_packages, usable.recipients)),
    }
  }

  /// The claimed packages of `user`'s devices that aren't in the group yet and pass the checks a
  /// commit makes. A bad package (expired, revoked device, not the device or user the server says)
  /// is left out and logged, so one broken device doesn't keep its user's other devices out.
  fn usable_key_packages(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    claimed: Vec<teamcraft_proto::ClaimedKeyPackage>,
    directory: &PinnedDirectory,
  ) -> Result<UsablePackages, CoreError> {
    let in_group = self.group_device_ids(conversation)?;
    let group = self
      .groups
      .get(&conversation)
      .ok_or(CoreError::NoGroup(conversation))?;
    let mut usable = UsablePackages::default();
    // The server claims for every device of the user, including ones already in the group (and
    // this device); those packages are wasted (crate README: server gaps).
    for package in claimed {
      if in_group.contains(&package.device_id) {
        continue;
      }
      let bytes = package.key_package.into_vec();
      match group.check_key_package(&self.provider, &bytes, directory) {
        Ok(member)
          if member.user_id == convert::user(user)
            && member.device_id == convert::device(package.device_id) =>
        {
          usable.recipients.push(package.device_id);
          usable.key_packages.push(bytes);
        },
        Ok(_) => {
          tracing::warn!(conversation_id = %conversation, device_id = %package.device_id, "skipped a key package of another device");
          usable
            .maybe_first_error
            .get_or_insert(MlsError::UnexpectedMessage("key package of another device"));
        },
        Err(error) => {
          tracing::warn!(conversation_id = %conversation, device_id = %package.device_id, %error, "skipped an invalid key package");
          usable.maybe_first_error.get_or_insert(error);
        },
      }
    }
    Ok(usable)
  }

  /// The devices in the conversation's MLS group on this device.
  pub fn group_device_ids(
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

  /// Who may make signed changes in the group, with the signed roster's freshness judged at
  /// `at_ms` (the server's time of the commit being checked).
  /// The users with a device in the conversation's group.
  pub(crate) fn group_users(&mut self, conversation: ConversationId) -> Result<Vec<UserId>, CoreError> {
    let mut users = Vec::new();
    for member in self.group_or_error(conversation)?.members()? {
      let user = convert::user_from_crypto(member.user_id)?;
      if !users.contains(&user) {
        users.push(user);
      }
    }
    Ok(users)
  }

  fn group_authority(
    &mut self,
    conversation: ConversationId,
    at_ms: i64,
  ) -> Result<WorkspaceAuthority, CoreError> {
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
    let admins: Vec<UserId> = if workspace.is_roster_fresh(at_ms, max_age_ms) {
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

  /// [`Self::implicit_history`] for a conversation: in a public channel nobody chose anything for
  /// the newcomer (they may have joined on their own), so they get history from their join on,
  /// whatever the workspace default says. Full history there takes an explicit choice by the member
  /// who adds them (e2ee.md §4.7). The conversation's visibility is the server's word, but trusting
  /// it can only lower what is shared.
  pub(crate) async fn implicit_history_in(
    &mut self,
    conversation: ConversationId,
    user: UserId,
  ) -> Result<HistoryShare, CoreError> {
    self.refresh_conversation(conversation).await?;
    let maybe_response: Option<teamcraft_proto::ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &conversation.to_string())?;
    let is_public = maybe_response
      .is_some_and(|response| response.visibility == teamcraft_proto::ConversationVisibility::Public);
    if is_public {
      return Ok(HistoryShare::FromJoin);
    }
    self.implicit_history(user)
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

  /// This device fell out of the group: it asks the members, with a request it signs, to add it
  /// back (e2ee.md §4.6). It keeps its old group until the welcome comes, and asks again after
  /// [`ClientSettings::rejoin_retry`] if none does.
  ///
  /// [`ClientSettings::rejoin_retry`]: super::ClientSettings::rejoin_retry
  pub(crate) async fn request_rejoin(
    &mut self,
    conversation: ConversationId,
    current_epoch: u64,
  ) -> Result<(), CoreError> {
    let now = self.now_ms();
    let retry_ms = i64::try_from(self.settings.rejoin_retry.as_millis()).unwrap_or(i64::MAX);
    if let Some(record) =
      records::get_json::<RejoinRecord>(&self.store, Table::AppliedChanges, &rejoin_key(conversation))?
      && now.saturating_sub(record.requested_at_ms) < retry_ms
    {
      return Ok(());
    }
    tracing::warn!(conversation_id = %conversation, "this device fell out of the MLS group; asking to be added back");
    if self.has_pending_commit(conversation)? {
      self.discard_pending_commit(conversation)?;
    }
    // A member needs a key package of this device to add it back.
    if let Err(error) = self.top_up_key_packages().await {
      tracing::debug!(%error, "key packages not topped up before asking to rejoin");
    }
    let signed = RejoinRequest::new(
      convert::conversation(conversation),
      current_epoch,
      self.own_member(),
      &mut OsRng,
    )
    .sign(&self.account.device, self.account.certificate.clone())?;
    let mut payload = REJOIN_REQUEST_MAGIC.to_vec();
    payload.extend_from_slice(&signed.to_bytes()?);
    let request = PostMlsMessageRequest {
      device_id: self.account.device_id,
      epoch: current_epoch,
      kind: MlsMessageKind::Proposal,
      payload: Base64Bytes::new(payload),
    };
    match self.api.post_mls_message(conversation, &request).await {
      Ok(stored) => self.advance_cursor(conversation, &stored)?,
      // The group moved on meanwhile; the next sync asks for the new epoch.
      Err(error) if error.is_epoch_conflict() => return Ok(()),
      Err(error) => return Err(error.into()),
    }
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      &rejoin_key(conversation),
      &RejoinRecord { requested_at_ms: now },
    )
  }

  fn own_member(&self) -> GroupMember {
    GroupMember {
      user_id: convert::user(self.account.user_id),
      device_id: convert::device(self.account.device_id),
    }
  }

  fn has_requested_rejoin(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(
      records::get_json::<RejoinRecord>(&self.store, Table::AppliedChanges, &rejoin_key(conversation))?
        .is_some(),
    )
  }

  /// Queues adding back a device that asked for it, if its request checks out here.
  async fn note_rejoin_request(
    &mut self,
    conversation: ConversationId,
    bytes: &[u8],
  ) -> Result<(), CoreError> {
    if !self.has_group(conversation)? {
      return Ok(());
    }
    let signed = match SignedRejoinRequest::from_bytes(bytes) {
      Ok(signed) => signed,
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "ignored a malformed rejoin request");
        return Ok(());
      },
    };
    let user = convert::user_from_crypto(signed.unverified().device().user_id)?;
    let directory = self.pin_identities([user]).await?;
    let member = match self
      .group_or_error(conversation)?
      .check_rejoin_request(&signed, &directory)
    {
      Ok(member) => member,
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "ignored a rejoin request");
        return Ok(());
      },
    };
    let device = convert::device_from_crypto(member.device_id)?;
    let not_before_ms = self
      .now_ms()
      .saturating_add(self.commit_delay_ms(conversation, device)?);
    self.queue_pending(PendingChange::Rejoin {
      conversation_id: conversation,
      user_id: user,
      device_id: device,
      request: Base64Bytes::new(bytes.to_vec()),
      not_before_ms,
      attempts: 0,
    })
  }

  /// Answers a device's rejoin request: removes its old leaf and adds it from a fresh key
  /// package. `Ok(true)` when done, here or by another member; `Ok(false)` to retry later.
  pub(crate) async fn answer_rejoin(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    device: DeviceId,
    request: &[u8],
  ) -> Result<bool, CoreError> {
    if !self.has_group(conversation)? {
      return Ok(true);
    }
    // Another member may have answered meanwhile.
    self.sync_mls(conversation).await?;
    let key = PendingChange::rejoin_key(conversation, device);
    if self.store.get(Table::PendingChanges, &key)?.is_none() {
      return Ok(true);
    }
    self.resolve_pending_commit(conversation).await?;
    let signed = SignedRejoinRequest::from_bytes(request)?;
    let directory = self.trusted_directory([user]).await?;
    let claim = ClaimKeyPackagesRequest {
      conversation_id: conversation,
      maybe_only_device_ids: Some(vec![device]),
      exclude_device_ids: Vec::new(),
    };
    let claimed = match self.api.claim_key_packages(user, &claim).await {
      Ok(claimed) => claimed,
      Err(error) if error.error_code() == Some(&teamcraft_proto::ErrorCode::KeyPackageExhausted) => {
        return Ok(false);
      },
      Err(error) => return Err(error.into()),
    };
    let Some(package) = claimed
      .packages
      .into_iter()
      .find(|package| package.device_id == device)
    else {
      return Ok(false);
    };
    let group = self
      .groups
      .get_mut(&conversation)
      .ok_or(CoreError::NoGroup(conversation))?;
    let output = group.commit_rejoin(
      &self.provider,
      &self.account.device,
      &signed,
      package.key_package.as_slice(),
      &directory,
    )?;
    let Some(own) = self.post_commit(conversation, output, &[device]).await? else {
      return Ok(false);
    };
    if let Some(share) = self.after_own_commit(conversation, &own.update, own.at_ms)? {
      self.queue_pending(PendingChange::ShareHistory {
        conversation_id: conversation,
        share,
      })?;
    }
    tracing::info!(conversation_id = %conversation, device_id = %device, "added back a device that fell out of the group");
    Ok(true)
  }

  fn forget_rejoin_request(
    &mut self,
    conversation: ConversationId,
    device: GroupMember,
  ) -> Result<(), CoreError> {
    let device = convert::device_from_crypto(device.device_id)?;
    self.store.delete(
      Table::PendingChanges,
      &PendingChange::rejoin_key(conversation, device),
    )?;
    Ok(())
  }

  pub(crate) fn advance_cursor(
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

/// The other people of a group DM: `users` without the user and without duplicates, in order.
fn conversation_members(users: &[UserId], me: UserId) -> Vec<UserId> {
  let mut seen = BTreeSet::new();
  users
    .iter()
    .copied()
    .filter(|user| *user != me && seen.insert(*user))
    .collect()
}

/// Devices a commit took out of the group for good (a rejoining device stays).
fn left_members(update: &MembershipUpdate) -> Vec<GroupMember> {
  update
    .removed
    .iter()
    .filter(|member| Some(**member) != update.maybe_rejoined)
    .copied()
    .collect()
}

/// Where this device notes that it asked to be added back to a group.
pub(crate) fn rejoin_key(conversation: ConversationId) -> String {
  format!("rejoin/{conversation}")
}
