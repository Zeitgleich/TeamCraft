//! MLS groups per conversation: creation, signed membership changes with rebase on conflict,
//! welcomes, and processing the server's ordered MLS queue (e2ee.md §4, §4.6).

use std::collections::BTreeSet;

use rand_core::OsRng;
use teamcraft_crypto::content_key::{ContentKey, ContentKeyId};
use teamcraft_crypto::identity::VerifiedDevice;
use teamcraft_crypto::membership::{
  HistoryShare, MembershipAction, MembershipChange, SignedMembershipChange,
};
use teamcraft_crypto::mls::{
  CommitOutput, ContentKeyBundle, ConversationGroup, GroupEvent, GroupMember, MembershipUpdate, MlsError,
  RejoinRequest, SignedRejoinRequest, VerifiedWelcome,
};
use teamcraft_proto::{
  AddConversationMemberRequest, Base64Bytes, ClaimKeyPackagesRequest, ConversationId, CreateChannelRequest,
  CreateDmRequest, CreateGroupDmRequest, DeviceId, HistoryPolicy, Id, IdempotencyKey, ListMlsMessagesQuery,
  ListMlsMessagesResponse, MlsMessageKind, MlsMessageResponse, MlsWelcome, PostMlsCommitRequest,
  PostMlsMessageRequest, SpaceId, UserId,
};

use super::workspace::{PendingChange, SharedKeys};
use super::{Client, pending_post_key};
use crate::api::{ApiError, ServerApi};
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
/// Tries of a create request whose answer was lost; all carry the same idempotency key.
pub(crate) const CREATE_ATTEMPTS: u32 = 3;
/// Key prefix of [`DmSetupRecord`]s in the sync state.
const DM_SETUP_PREFIX: &str = "dm-setup/";
/// Key prefix of [`ReaddedRecord`]s in the applied changes.
const READDED_PREFIX: &str = "readded/";

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
  /// Requests of a device without a group (a welcome it couldn't join) so far: each doubles the
  /// wait before the next, up to `rejoin_retry_max`.
  #[serde(default)]
  attempts: u32,
}

/// When a device was last added back to a group, in server time (the commit's).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ReaddedRecord {
  at_ms: i64,
}

/// A DM or group DM this device created whose MLS group doesn't hold everybody yet: kept until
/// it does, so a setup cut short is finished later (issue #228).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct DmSetupRecord {
  /// Who the user opened it with (from the user's own request, not the server's member list).
  others: Vec<UserId>,
  tried_at_ms: i64,
  /// Attempts so far: each doubles the wait before the next, up to `rejoin_retry_max`.
  attempts: u32,
  /// The content key this device made when it founded the group, while it may still lose the
  /// founding race; it stops being current if it does.
  #[serde(default)]
  maybe_founding_key_id: Option<[u8; 16]>,
  /// Everybody else is in the group except people without a device, whose first device makes
  /// their [`PendingChange::AddUser`] due: nothing to repair until then. The record stays, so a
  /// founding race with that person's first device is still settled as above.
  #[serde(default)]
  is_waiting_for_device: bool,
  /// Waiting for a welcome (another founding won, or someone else founded): until when that is
  /// shown as "setting up encryption", and as failed afterwards, with a retry (#278).
  #[serde(default)]
  maybe_welcome_deadline_ms: Option<i64>,
}

/// Where the setup of a DM this device created stands, for the UI (#278).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmSetupStatus {
  /// Waiting for the welcome into the group the other side founded; shown as failed after
  /// `remaining` (the UI plans one timer for it).
  WaitingForWelcome { remaining: std::time::Duration },
  /// No welcome within `dm_welcome_timeout`: the user can retry ([`Client::retry_dm_setup`]).
  Failed,
}

/// The setup status of `conversation` at `now_ms`; `None` when nothing is waited for. Read by the
/// UI from the store.
pub fn dm_setup_status(
  store: &impl LocalStore,
  conversation: ConversationId,
  now_ms: i64,
) -> Result<Option<DmSetupStatus>, CoreError> {
  let maybe_setup: Option<DmSetupRecord> =
    records::get_json(store, Table::SyncState, &dm_setup_key(conversation))?;
  Ok(maybe_setup.and_then(|setup| {
    let deadline = setup.maybe_welcome_deadline_ms?;
    Some(match u64::try_from(deadline.saturating_sub(now_ms)) {
      Ok(remaining_ms) if remaining_ms > 0 => DmSetupStatus::WaitingForWelcome {
        remaining: std::time::Duration::from_millis(remaining_ms),
      },
      _ => DmSetupStatus::Failed,
    })
  }))
}

/// What became of opening a welcome.
enum WelcomeJoin {
  /// With the member whose commit made the welcome.
  Joined(Box<(ConversationGroup, PinnedDirectory, GroupMember)>),
  /// Not for this device or not acceptable; the welcome is done with.
  Rejected(MlsError),
  /// This device's leaf is in the members' tree, but it has no group and can't open the welcome
  /// again, so it asks to be added back: the welcome was opened (which spends the key package)
  /// and a network error stopped the join, or its key package was deleted already (e2ee.md
  /// §4.3.1) while the server still lists the user in the conversation.
  KeyPackageSpent,
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
    let mut attempt = 1;
    // A lost answer is retried with the same request, hence the same idempotency key: the server
    // returns the channel it already made instead of a second one.
    let conversation = loop {
      match self.api.create_channel(space, request).await {
        Err(ApiError::Network(_)) if request.maybe_idempotency_key.is_some() && attempt < CREATE_ATTEMPTS => {
          attempt += 1;
        },
        other => break other?,
      }
    };
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
  /// has it or someone else founded it first. Until the group holds everybody, a setup record
  /// stays behind so that a setup cut short (crash, lost network) is finished on a later sync
  /// ([`Self::repair_dm_setups`]).
  async fn found_participants_group(
    &mut self,
    conversation: teamcraft_proto::ConversationResponse,
    others: &[UserId],
  ) -> Result<ConversationId, CoreError> {
    let id = conversation.id;
    records::put_json(&self.store, Table::Conversations, &id.to_string(), &conversation)?;
    let maybe_setup = records::get_json::<DmSetupRecord>(&self.store, Table::SyncState, &dm_setup_key(id))?;
    if self.has_group(id)? && maybe_setup.is_none() {
      return Ok(id);
    }
    self.start_participants_setup(id, others).await?;
    Ok(id)
  }

  /// Notes (or renews) the setup record of a DM, group DM or meeting chat with `others` and takes
  /// the first step of [`Self::finish_dm_setup`]: founds the group when nobody has one, joins
  /// one somebody else founded, and adds whoever of `others` is missing.
  pub(super) async fn start_participants_setup(
    &mut self,
    id: ConversationId,
    others: &[UserId],
  ) -> Result<bool, CoreError> {
    let maybe_setup = self.dm_setup(id)?;
    // Asked for by the user: tried now, whatever the backoff of an earlier attempt says.
    let setup = DmSetupRecord {
      others: others.to_vec(),
      tried_at_ms: self.now_ms(),
      attempts: maybe_setup
        .as_ref()
        .map_or(0, |record| record.attempts)
        .saturating_add(1),
      maybe_founding_key_id: maybe_setup.and_then(|record| record.maybe_founding_key_id),
      is_waiting_for_device: false,
      maybe_welcome_deadline_ms: None,
    };
    records::put_json(&self.store, Table::SyncState, &dm_setup_key(id), &setup)?;
    self.finish_dm_setup(id, others).await
  }

  /// Finishes the MLS setup of DMs this device created whose setup was cut short, at a growing
  /// interval per DM (`rejoin_retry` doubled per attempt, up to `rejoin_retry_max`). One DM
  /// failing doesn't keep the others from being repaired. Returns how many were completed.
  pub(crate) async fn repair_dm_setups(&mut self) -> Result<usize, CoreError> {
    let mut repaired = 0;
    let mut maybe_first_error = None;
    let conversations = self.cached_conversations()?;
    let setups: Vec<(String, DmSetupRecord)> =
      records::list_json_with_prefix(&self.store, Table::SyncState, DM_SETUP_PREFIX)?;
    for (key, setup) in setups {
      let Some(id) = key
        .strip_prefix(DM_SETUP_PREFIX)
        .and_then(|id| id.parse::<ConversationId>().ok())
      else {
        continue;
      };
      let is_member = conversations
        .get(&id)
        .is_some_and(|response| response.maybe_my_membership.is_some());
      if !is_member {
        // Gone or left: nothing to set up any more.
        self.store.delete(Table::SyncState, &key)?;
        continue;
      }
      let now = self.now_ms();
      // Waiting for somebody's first device costs nothing; `device_add` wakes it.
      if setup.is_waiting_for_device
        || now.saturating_sub(setup.tried_at_ms) < self.rejoin_wait_ms(setup.attempts)
      {
        continue;
      }
      records::put_json(
        &self.store,
        Table::SyncState,
        &key,
        &DmSetupRecord {
          tried_at_ms: now,
          attempts: setup.attempts.saturating_add(1),
          ..setup.clone()
        },
      )?;
      match self.finish_dm_setup(id, &setup.others).await {
        Ok(true) => repaired += 1,
        Ok(false) => {},
        // Network trouble fails the sync (it is retried); anything else waits for the next try
        // here instead of failing every sync.
        Err(error) if error.is_transient() => {
          maybe_first_error.get_or_insert(error);
        },
        Err(error) => {
          tracing::warn!(conversation_id = %id, %error, "could not finish setting up a DM");
        },
      }
    }
    maybe_first_error.map_or(Ok(repaired), Err)
  }

  /// One idempotent step towards a complete DM group: settles a commit whose answer was lost,
  /// joins a group someone else founded, founds the group only when neither the server nor this
  /// device has one, and adds whoever is missing. A group of one this device founded is given up
  /// only for a verified welcome of the winner's group ([`Self::join_from_welcome`]), never on
  /// the server's word, so the group never forks and a server can't make devices found again and
  /// again (e2ee.md §4.6). `Ok(true)` once everybody is in.
  pub(super) async fn finish_dm_setup(
    &mut self,
    id: ConversationId,
    others: &[UserId],
  ) -> Result<bool, CoreError> {
    if self.has_group(id)? {
      self.resolve_pending_commit(id).await?;
    }
    self.sync_welcomes().await?;
    let query = ListMlsMessagesQuery {
      maybe_after: None,
      maybe_limit: Some(1),
    };
    let server_epoch = self.api.list_mls_messages(id, &query).await?.current_epoch;
    match self.mls_epoch(id)? {
      Some(0) if server_epoch > 0 => {
        // Another founding won, says the server: this device waits for the winner's welcome.
        // Adding now would only lose the epoch again and spend the others' key packages.
        tracing::info!(conversation_id = %id, "waiting for the welcome of the DM group that was founded first");
        self.note_waiting_for_welcome(id)?;
        if self.has_requested_rejoin(id)? {
          // That welcome was opened but not joined (its key package is spent): ask the members
          // again, at the rejoin backoff; their new welcome replaces this group once it checks out.
          self.request_rejoin(id, server_epoch).await?;
        }
        return self.complete_dm_setup(id, others);
      },
      Some(_) => {},
      None if server_epoch == 0 => self.found_dm_group(id)?,
      None => {
        // Someone else founded it: their welcome is on the way, or another device of this user is
        // in the group and adds this one.
        if let Err(error) = self.join_as_member_device(id).await {
          tracing::debug!(conversation_id = %id, %error, "not joined as a further device");
        }
        if !self.has_group(id)? {
          self.note_waiting_for_welcome(id)?;
        }
        return self.complete_dm_setup(id, others);
      },
    }
    for user in others {
      if self.group_users(id)?.contains(user) {
        continue;
      }
      self
        .commit_change(
          id,
          ChangeSource::Own(MembershipAction::AddUser {
            user_id: convert::user(*user),
            history: HistoryShare::Full,
          }),
        )
        .await?;
      if self.group_users(id)?.contains(user) {
        continue;
      }
      // Somebody who hasn't set up the app yet goes in when their first device appears.
      if self.wait_for_first_device(id, *user, HistoryShare::Full).await? {
        continue;
      }
      if self.is_unfinished_dm_founding(id)? {
        // Nothing went in (another founding won the epoch, or no key packages): try later.
        break;
      }
    }
    self.complete_dm_setup(id, others)
  }

  /// "Retry" on a DM whose setup waits for a welcome (#278): tries the setup again now, and if
  /// the welcome is still missing, asks the members once more, with a signed rejoin request, to
  /// add this device (the welcome may be lost, or its key package spent). The wait starts anew.
  pub async fn retry_dm_setup(&mut self, id: ConversationId) -> Result<(), CoreError> {
    let Some(setup) = self.dm_setup(id)? else {
      return Ok(());
    };
    let setup = DmSetupRecord {
      tried_at_ms: self.now_ms(),
      maybe_welcome_deadline_ms: None,
      ..setup
    };
    records::put_json(&self.store, Table::SyncState, &dm_setup_key(id), &setup)?;
    if self.finish_dm_setup(id, &setup.others).await? {
      return Ok(());
    }
    let is_waiting = self
      .dm_setup(id)?
      .is_some_and(|setup| setup.maybe_welcome_deadline_ms.is_some());
    if !is_waiting {
      return Ok(());
    }
    let query = ListMlsMessagesQuery {
      maybe_after: None,
      maybe_limit: Some(1),
    };
    let server_epoch = self.api.list_mls_messages(id, &query).await?.current_epoch;
    // The user asked: not held back by the backoff of an earlier request.
    self.store.delete(Table::AppliedChanges, &rejoin_key(id))?;
    self.request_rejoin(id, server_epoch).await
  }

  /// Starts the wait for a welcome, unless it runs already.
  fn note_waiting_for_welcome(&mut self, id: ConversationId) -> Result<(), CoreError> {
    let Some(setup) = self.dm_setup(id)? else {
      return Ok(());
    };
    if setup.maybe_welcome_deadline_ms.is_some() {
      return Ok(());
    }
    let timeout_ms = i64::try_from(self.settings.dm_welcome_timeout.as_millis()).unwrap_or(i64::MAX);
    let setup = DmSetupRecord {
      maybe_welcome_deadline_ms: Some(self.now_ms().saturating_add(timeout_ms)),
      ..setup
    };
    records::put_json(&self.store, Table::SyncState, &dm_setup_key(id), &setup)
  }

  /// The welcome a DM's setup waited for was joined: the setup is done if everybody is in, and
  /// waits for no welcome any more either way.
  fn note_dm_welcome_joined(&mut self, id: ConversationId) -> Result<(), CoreError> {
    let Some(setup) = self.dm_setup(id)? else {
      return Ok(());
    };
    let setup = DmSetupRecord {
      maybe_welcome_deadline_ms: None,
      ..setup
    };
    records::put_json(&self.store, Table::SyncState, &dm_setup_key(id), &setup)?;
    self.complete_dm_setup(id, &setup.others).map(|_| ())
  }

  /// Founds a DM's group. A content key this device makes for it is noted in the setup record
  /// first, so that giving the group up later (another founding won) can stop using it.
  fn found_dm_group(&mut self, id: ConversationId) -> Result<(), CoreError> {
    let has_current = self
      .account
      .key_bag
      .ring(convert::conversation(id))
      .is_some_and(|ring| ring.current().is_some());
    let maybe_key = (!has_current).then(|| ContentKey::generate(&mut OsRng));
    if let Some(key) = &maybe_key
      && let Some(setup) = self.dm_setup(id)?
    {
      let setup = DmSetupRecord {
        maybe_founding_key_id: Some(*key.id().as_bytes()),
        ..setup
      };
      records::put_json(&self.store, Table::SyncState, &dm_setup_key(id), &setup)?;
    }
    self.found_group_with_key(id, maybe_key)
  }

  /// Forgets the setup record once this device is in a group that holds everybody. Done as well
  /// when the only ones missing wait for their first device: the record stays, marked so that
  /// nothing retries it ([`DmSetupRecord::is_waiting_for_device`]).
  fn complete_dm_setup(&mut self, id: ConversationId, others: &[UserId]) -> Result<bool, CoreError> {
    if !self.has_group(id)? {
      return Ok(false);
    }
    let users = self.group_users(id)?;
    let missing: Vec<UserId> = others
      .iter()
      .copied()
      .filter(|user| !users.contains(user))
      .collect();
    if missing.is_empty() {
      self.store.delete(Table::SyncState, &dm_setup_key(id))?;
      return Ok(true);
    }
    let waiting = super::pending_user_additions(&self.store, id)?;
    let is_waiting_for_device = missing
      .iter()
      .all(|user| waiting.contains(&(*user, super::UserAddWait::NoDevice)));
    if !is_waiting_for_device {
      return Ok(false);
    }
    if let Some(setup) = self.dm_setup(id)? {
      let setup = DmSetupRecord {
        is_waiting_for_device: true,
        ..setup
      };
      records::put_json(&self.store, Table::SyncState, &dm_setup_key(id), &setup)?;
    }
    Ok(true)
  }

  /// The people a DM was opened with, while its setup isn't finished; the setup is retried by
  /// [`Self::repair_dm_setups`] again (no longer waiting for a device).
  pub(super) fn resume_dm_setup(&mut self, id: ConversationId) -> Result<Option<Vec<UserId>>, CoreError> {
    let Some(setup) = self.dm_setup(id)? else {
      return Ok(None);
    };
    let others = setup.others.clone();
    let setup = DmSetupRecord {
      is_waiting_for_device: false,
      ..setup
    };
    records::put_json(&self.store, Table::SyncState, &dm_setup_key(id), &setup)?;
    Ok(Some(others))
  }

  /// Gives up a group of one that lost the founding race, once the winner's welcome checked out.
  /// The founding content key stops being current, or this device would keep encrypting with a
  /// key nobody else has; it stays in the ring (and the key bag) for what was encrypted with it.
  fn give_up_founded_group(&mut self, id: ConversationId) -> Result<(), CoreError> {
    // The joined group took over the MLS state stored under this group ID: only the records
    // about the founded one go, deleting its MLS state would delete the joined group's.
    self.forget_group_records(id)?;
    let Some(setup) = self.dm_setup(id)? else {
      return Ok(());
    };
    if let Some(key_id) = setup.maybe_founding_key_id {
      self
        .account
        .key_bag
        .ring_mut(convert::conversation(id))
        .unset_current(ContentKeyId::from_bytes(key_id));
      self.persist_keys()?;
    }
    let setup = DmSetupRecord {
      maybe_founding_key_id: None,
      ..setup
    };
    records::put_json(&self.store, Table::SyncState, &dm_setup_key(id), &setup)
  }

  /// Whether this device holds a DM group it founded and nobody else is in: still at epoch 0, no
  /// commit of its own in flight, and the DM's setup not finished.
  pub(crate) fn is_unfinished_dm_founding(&mut self, id: ConversationId) -> Result<bool, CoreError> {
    Ok(
      self.store.get(Table::SyncState, &dm_setup_key(id))?.is_some()
        && self.mls_epoch(id)? == Some(0)
        && !self.has_pending_commit(id)?,
    )
  }

  /// Whether this device opened the DM and keeps its setup record.
  pub(crate) fn has_dm_setup(&self, id: ConversationId) -> Result<bool, CoreError> {
    Ok(self.dm_setup(id)?.is_some())
  }

  fn dm_setup(&self, id: ConversationId) -> Result<Option<DmSetupRecord>, CoreError> {
    records::get_json(&self.store, Table::SyncState, &dm_setup_key(id))
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
    self.cancel_removal_on_add(conversation, user)?;
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
    self
      .queue_user_addition_if_missing(conversation, user, history)
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

  /// Fetches welcomes for this device and joins those groups. Spent key packages aren't deleted
  /// while this runs (a welcome later in the queue may need one, issue #395): a rejoin request for
  /// an unopenable welcome tops up the key packages, whose cleanup waits for the last page.
  pub async fn sync_welcomes(&mut self) -> Result<(), CoreError> {
    self.are_welcomes_synced = false;
    self.is_welcome_cursor_held = false;
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
        self.are_welcomes_synced = true;
        return Ok(());
      }
    }
  }

  /// Processes the conversation's MLS queue after the last entry this device handled. A commit of
  /// this device whose answer was lost is merged when it shows up there, or sent again when the
  /// server never got it. A device that can't follow the group any more asks to be added back
  /// (e2ee.md §4.6).
  pub async fn sync_mls(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.sync_mls_from(conversation, None).await
  }

  /// [`Self::sync_mls`] with the queue's first page already read (a catch-up reads several
  /// conversations' pages at once, issue #247).
  pub(crate) async fn sync_mls_from(
    &mut self,
    conversation: ConversationId,
    maybe_first: Option<Result<ListMlsMessagesResponse, ApiError>>,
  ) -> Result<(), CoreError> {
    if self.is_unfinished_dm_founding(conversation)? {
      // Nobody else is in this group, so the queue can only hold another founding's group
      // (whose welcome replaces this one) or nothing: following it would only refuse its entries
      // and ask to be added back to a group this device was never in.
      return Ok(());
    }
    let mut catch_up = match self.catch_up_mls(conversation, maybe_first).await {
      Ok(catch_up) => catch_up,
      // The server no longer lets this user read the queue: removed, with the removal commit
      // still to come or refused, or a server that wants this device to stop following the
      // group. Quiet only for the first; otherwise logged and shown (`note_queue_refused`).
      Err(CoreError::Api(error)) if error.is_not_found() || error.is_forbidden() => {
        return self.note_queue_refused(conversation).await;
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
      catch_up = self.catch_up_mls(conversation, None).await?;
    }
    // A request that went unanswered (members ignore repeats within `rejoin_cooldown`, issue
    // #149): the cursor went past the entries that showed this device behind, the epoch still does.
    let is_still_out = !catch_up.is_behind
      && self.has_requested_rejoin(conversation)?
      && self
        .mls_epoch(conversation)?
        .is_some_and(|epoch| epoch < catch_up.current_epoch);
    if catch_up.is_behind || is_still_out {
      return self.request_rejoin(conversation, catch_up.current_epoch).await;
    }
    if !self.has_pending_commit(conversation)? {
      self.mls_current.insert(conversation);
    }
    self.clear_queue_refused(conversation)?;
    self.note_group_leftovers(conversation)
  }

  /// Catches up the conversation's MLS queue once per gateway connection before the outbox sends
  /// into it, so nothing goes out under a key that a member removed meanwhile still holds.
  pub(crate) async fn ensure_mls_current(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    if self.mls_current.contains(&conversation) || !self.has_group(conversation)? {
      return Ok(());
    }
    self.sync_mls(conversation).await
  }

  /// One pass over the queue after the cursor, starting with `maybe_first` when it was read ahead.
  async fn catch_up_mls(
    &mut self,
    conversation: ConversationId,
    mut maybe_first: Option<Result<ListMlsMessagesResponse, ApiError>>,
  ) -> Result<CatchUp, CoreError> {
    loop {
      let page = match maybe_first.take() {
        Some(first) => first?,
        None => {
          let query = ListMlsMessagesQuery {
            maybe_after: self.sync.mls_cursors.get(&conversation).copied(),
            maybe_limit: None,
          };
          self.api.list_mls_messages(conversation, &query).await?
        },
      };
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
        let requested_at_ms = message.id.snowflake().unix_millis();
        self
          .note_rejoin_request(conversation, request, requested_at_ms)
          .await?;
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
      // group is all it needs to verify. Anything else is a server refusing a member: logged, and
      // `sync_mls` shows it if it refuses the queue too.
      Err(error) if error.is_not_found() || error.is_forbidden() => {
        if !self.has_own_pending_leave(conversation)? {
          tracing::warn!(conversation_id = %conversation, %error, "the server refused the member list; verifying against the group");
        }
        self.group_users(conversation)?
      },
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
    let before = group.members()?;
    let result = group.process_message(&self.provider, message.payload.as_slice(), &directory, &authority);
    if result.is_ok() {
      self.remember_member_keys(conversation, &directory, at_ms)?;
    }
    match result {
      Ok(GroupEvent::ContentKeys { sender, bundle }) => {
        self.apply_bundle(conversation, bundle, epoch, sender)?;
      },
      Ok(GroupEvent::SealedContentKeys { sender, sealed }) => {
        if sealed.recipient() != self.own_member() {
          self.note_sealed_keys_sent(conversation, sender, sealed.recipient())?;
        } else {
          match sealed.open(&self.account.device) {
            // Keys of epochs this device missed; none of them becomes current (e2ee.md §4.6).
            Ok(bundle) => {
              let ring = self.account.key_bag.ring_mut(convert::conversation(conversation));
              let clashing = add_sealed_keys(ring, &bundle);
              if clashing > 0 {
                // A member sent other key material under a known ID: never replaces a key, and
                // doesn't hold up the queue.
                tracing::warn!(conversation_id = %conversation, mls_message_id = %message.id, clashing, "skipped sealed content keys that clash with known ones");
              }
              self.persist_keys()?;
              self.retry_undecrypted(conversation)?;
            },
            Err(error) => {
              tracing::warn!(conversation_id = %conversation, mls_message_id = %message.id, %error, "rejected content keys sealed to this device");
            },
          }
        }
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
                attempts: 0,
              },
            )?;
          }
          return Ok(MlsProgress::Done);
        }
        if let Some(device) = update.maybe_rejoined {
          // Somebody answered that device's request already.
          self.note_device_readded(conversation, device, at_ms)?;
        }
        self.note_member_changes(conversation, &before, update.maybe_rejoined)?;
        if let (Some(rejoined), Some(committer)) = (update.maybe_rejoined, update.maybe_committer) {
          self.follow_up_rejoin(conversation, rejoined, committer)?;
        }
        if update.maybe_change.is_none() && update.maybe_rejoined.is_none() {
          // Further devices of members (an external join, or a colleague's commit): their key
          // bag may lack the current key (#178). One member hands it over (e2ee.md §4.6).
          for added in &update.added {
            self.queue_current_key_share(conversation, *added)?;
          }
        }
        let is_from_join_add = matches!(
          update.maybe_change.as_ref().map(MembershipChange::action),
          Some(MembershipAction::AddUser {
            history: HistoryShare::FromJoin,
            ..
          })
        );
        self.note_removed_senders(conversation, &left_members(&update), at_ms)?;
        // A signed addition committed now is newer than this device's removal of the user: an
        // earlier one about them would make it stale (Freshness).
        if let Some(MembershipAction::AddUser { user_id, .. }) =
          update.maybe_change.as_ref().map(MembershipChange::action)
        {
          self.cancel_removal_on_add(conversation, convert::user_from_crypto(*user_id)?)?;
        }
        // A rejoin too: the device added back takes the new epoch's key.
        if !update.removed.is_empty() || is_from_join_add {
          self.rotate_to_epoch_key(conversation)?;
        }
        if !update.removed.is_empty() {
          self.note_group_leftovers(conversation)?;
        }
      },
      Err(error) => {
        // A rejected commit (unauthorized, forged credential) or undecryptable entry is skipped
        // and reported; the group stays in its epoch (e2ee.md §4.6, admin.md §2.3).
        tracing::warn!(conversation_id = %conversation, mls_message_id = %message.id, %error, "rejected MLS message");
      },
    }
    self.persist_group_at(conversation, message)?;
    Ok(MlsProgress::Done)
  }

  /// Founds the MLS group of a conversation this device created, with a first content key.
  fn found_group(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.found_group_with_key(conversation, None)
  }

  /// Like [`Self::found_group`], with `maybe_key` as the first content key; without one, a new
  /// key is made unless the conversation has a current key already.
  fn found_group_with_key(
    &mut self,
    conversation: ConversationId,
    maybe_key: Option<ContentKey>,
  ) -> Result<(), CoreError> {
    let group = ConversationGroup::create(
      &self.provider,
      &self.account.device,
      &self.account.certificate,
      convert::conversation(conversation),
    )?;
    self.groups.insert(conversation, group);
    let ring = self.account.key_bag.ring_mut(convert::conversation(conversation));
    match maybe_key {
      Some(key) => {
        let key_id = key.id();
        ring.insert(key)?;
        ring.set_current(key_id)?;
      },
      None if ring.current().is_none() => {
        ring.rotate(&mut OsRng);
      },
      None => {},
    }
    let maybe_current = ring.current().map(ContentKey::id);
    self.persist_group(conversation)?;
    self.persist_keys()?;
    self.forget_key_history(conversation)?;
    match maybe_current {
      Some(current) => self.note_key_epochs(conversation, &[current], 0),
      None => Ok(()),
    }
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
        // Another founding won the epoch, says the server (a DM opened on both sides at once).
        // The winner's welcome replaces this group once it checks out (`join_from_welcome`);
        // nothing is dropped on the server's word alone (e2ee.md §4.6).
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
    let before = group.members()?;
    let update = group.confirm_commit(&self.provider)?;
    let after = group.members()?;
    // The merged state, what it noted and the cursor past the commit go to the store together
    // (issue #160).
    self.atomically(|client| {
      client.note_member_changes(conversation, &before, update.maybe_rejoined)?;
      if update.maybe_change.is_none() && update.maybe_rejoined.is_none() {
        // This device's own addition of further devices of members: they wait for a current key
        // like any (#178); this device takes its turn with the others.
        for added in after.iter().filter(|member| !before.contains(member)) {
          client.queue_current_key_share(conversation, *added)?;
        }
      }
      client.persist_group(conversation)?;
      client
        .store
        .delete(Table::AppliedChanges, &pending_post_key(conversation))?;
      if let Some(device) = update.maybe_rejoined {
        client.note_device_readded(conversation, device, entry.id.snowflake().unix_millis())?;
      }
      client.advance_cursor(conversation, entry)
    })?;
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
    if let Some(MembershipAction::AddUser { user_id, .. }) =
      update.maybe_change.as_ref().map(MembershipChange::action)
    {
      // Also an admin's signed addition this device committed.
      self.cancel_removal_on_add(conversation, convert::user_from_crypto(*user_id)?)?;
    }
    match update.maybe_change.as_ref().map(MembershipChange::action) {
      Some(MembershipAction::AddUser {
        history: HistoryShare::Full,
        user_id,
      }) => Ok(Some(SharedKeys::HistoryFor {
        user_id: convert::user_from_crypto(*user_id)?,
        added_epoch: self.group_or_error(conversation)?.epoch(),
      })),
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
        self.note_group_leftovers(conversation)?;
        Ok(None)
      },
      // Everybody switches to the new epoch's key, which the device added back derives from its
      // welcome. The keys of the epochs it missed are sealed to it (e2ee.md §4.6).
      None => match update.maybe_rejoined {
        Some(device) => {
          self.rotate_to_epoch_key(conversation)?;
          Ok(Some(SharedKeys::MissedKeys {
            user_id: convert::user_from_crypto(device.user_id)?,
            device_id: convert::device_from_crypto(device.device_id)?,
          }))
        },
        // Further devices of members: the user's key bag carries the history.
        None => Ok(None),
      },
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

  /// Sends the content keys a new member needs: the current key to the group, the history (if
  /// shared) sealed to the new member's devices.
  pub(crate) async fn share_history(
    &mut self,
    conversation: ConversationId,
    share: SharedKeys,
  ) -> Result<(), CoreError> {
    // The bundle names a current key the new member takes: never one only the key bag knows.
    let (bundle, handed) = match share {
      SharedKeys::HistoryFor { user_id, added_epoch } => {
        let handed = self
          .hand_over_key(conversation, true)?
          .ok_or(CoreError::NoContentKey(conversation))?;
        let bundle =
          ContentKeyBundle::epoch_key(convert::conversation(conversation), handed.key().duplicate());
        self.send_bundle(conversation, &bundle, None).await?;
        self.adopt_handed_over_key(conversation, &handed)?;
        return self.send_history(conversation, user_id, added_epoch).await;
      },
      SharedKeys::FullHistory => {
        tracing::info!(conversation_id = %conversation, "a history share of an earlier version sends only the current key");
        let handed = self
          .hand_over_key(conversation, true)?
          .ok_or(CoreError::NoContentKey(conversation))?;
        (
          ContentKeyBundle::epoch_key(convert::conversation(conversation), handed.key().duplicate()),
          handed,
        )
      },
      SharedKeys::CurrentKey => {
        let handed = self
          .hand_over_key(conversation, false)?
          .ok_or(CoreError::NoContentKey(conversation))?;
        (
          ContentKeyBundle::epoch_key(convert::conversation(conversation), handed.key().duplicate()),
          handed,
        )
      },
      SharedKeys::MissedKeys { user_id, device_id } => {
        return self.send_missed_keys(conversation, user_id, device_id).await;
      },
    };
    self.send_bundle(conversation, &bundle, None).await?;
    self.adopt_handed_over_key(conversation, &handed)
  }

  /// Seals every key of the conversation to each of `user`'s devices in the group, in bundles of at
  /// most `max_missed_keys` keys: the history of a member added with it in `added_epoch` (e2ee.md
  /// §4.3). The other members, who may have joined from the join on, see that keys went to them,
  /// not which (#403). Nothing goes out unless this device saw the user stay a member since: a
  /// share still pending after a removal (and maybe a new addition from the join on) is dropped.
  async fn send_history(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    added_epoch: u64,
  ) -> Result<(), CoreError> {
    if !self.stayed_member_since(conversation, user, added_epoch)? {
      tracing::info!(conversation_id = %conversation, "dropped a history share: the member left since the addition");
      return Ok(());
    }
    let keys: Vec<ContentKey> = self
      .account
      .key_bag
      .ring(convert::conversation(conversation))
      .map(|ring| ring.keys().iter().map(ContentKey::duplicate).collect())
      .unwrap_or_default();
    if keys.is_empty() {
      return Ok(());
    }
    let directory = self.member_directory(conversation).await?;
    let crypto_user = convert::user(user);
    let group = self.group_or_error(conversation)?;
    let mut recipients = Vec::new();
    for member in group.members()? {
      if member.user_id != crypto_user {
        continue;
      }
      match group.verified_member(member, &directory) {
        Ok(Some(device)) => recipients.push(device),
        Ok(None) => {},
        // One device whose leaf doesn't verify doesn't keep the history from the others.
        Err(error) => {
          tracing::warn!(conversation_id = %conversation, %error, "history not sealed to a device that doesn't verify");
        },
      }
    }
    let chunk_len = self.settings.max_missed_keys.max(1);
    for recipient in &recipients {
      for chunk in keys.chunks(chunk_len) {
        let bundle = ContentKeyBundle::without_current(
          convert::conversation(conversation),
          chunk.iter().map(ContentKey::duplicate).collect(),
        );
        self.send_bundle(conversation, &bundle, Some(recipient)).await?;
      }
    }
    Ok(())
  }

  /// Sends every key of the ring in one bundle to the whole group, as clients before v1.0.3 shared
  /// history with a new member. Only for tests of how this version takes such bundles.
  #[doc(hidden)]
  pub async fn send_history_to_the_group_as_before(
    &mut self,
    conversation: ConversationId,
  ) -> Result<(), CoreError> {
    let ring = self.account.key_bag.ring_mut(convert::conversation(conversation));
    let bundle = ContentKeyBundle::full_history(ring, &mut OsRng);
    self.persist_keys()?;
    self.send_bundle(conversation, &bundle, None).await
  }

  /// Seals to a device added back the keys of the epochs it was a member in (e2ee.md §4.6). Done
  /// when there are none or the device is no longer in the group.
  async fn send_missed_keys(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    device: DeviceId,
  ) -> Result<(), CoreError> {
    let member = GroupMember {
      user_id: convert::user(user),
      device_id: convert::device(device),
    };
    let bundle = self.missed_keys(conversation, member)?;
    if bundle.keys().is_empty() {
      return Ok(());
    }
    let directory = self.member_directory(conversation).await?;
    let Some(recipient) = self
      .group_or_error(conversation)?
      .verified_member(member, &directory)?
    else {
      return Ok(());
    };
    self.send_bundle(conversation, &bundle, Some(&recipient)).await
  }

  /// Applies a content key bundle a member sent to the whole group in `epoch`: only its current
  /// key, which every member may have; history goes sealed to the members it is for (#403). The
  /// key is noted with that epoch (every member then got it). A device that just joined waits for
  /// a member's current key (#178) and takes the first one sent to it; it also takes the history
  /// of a plain bundle, which members on earlier versions send a member they add.
  fn apply_bundle(
    &mut self,
    conversation: ConversationId,
    bundle: ContentKeyBundle,
    epoch: u64,
    sender: GroupMember,
  ) -> Result<(), CoreError> {
    let is_waiting = super::member_device::is_waiting_for_current_key(&self.store, conversation)?;
    // History from a plain bundle only as the new member of a member on an earlier version: from
    // the device whose commit made this device's welcome, in the epoch it joined, while it still
    // waits for its first current key. Anything else gets only the current key.
    let is_own_history = is_waiting
      && super::member_device::welcome_origin(&self.store, conversation)?
        .is_some_and(|origin| origin.is(sender, epoch));
    let maybe_current = bundle.current();
    let keys = plain_bundle_keys(&bundle, is_own_history);
    if keys.len() < bundle.keys().len() {
      tracing::info!(conversation_id = %conversation, ignored = bundle.keys().len() - keys.len(), "ignored history keys sent to the whole group");
    }
    // Only the current key counts as one every member of the epoch got.
    let ids: Vec<ContentKeyId> = maybe_current.into_iter().collect();
    let ring = self.account.key_bag.ring_mut(convert::conversation(conversation));
    let clashing = keys
      .into_iter()
      .filter(|key| ring.insert(key.duplicate()).is_err())
      .count();
    if clashing > 0 {
      tracing::warn!(conversation_id = %conversation, clashing, "skipped content keys that clash with known ones");
    }
    if let Some(current) = maybe_current.filter(|current| ring.get(*current).is_some()) {
      if ring.current().is_none() {
        ring.set_current(current)?;
      }
      if is_waiting {
        ring.set_current(current)?;
        self.store.delete(
          Table::AppliedChanges,
          &super::member_device::current_key_wait_key(conversation),
        )?;
      }
      // The new device has it now; nobody else needs to send it.
      self.drop_current_key_shares(conversation)?;
    }
    self.persist_keys()?;
    self.note_key_epochs(conversation, &ids, epoch)?;
    self.retry_undecrypted(conversation)
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
    // A device that joined by external commit has the current key now (#178).
    self.store.delete(
      Table::AppliedChanges,
      &super::member_device::current_key_wait_key(conversation),
    )?;
    self.drop_current_key_shares(conversation)?;
    // A new device of the user won't join from group information older than this (e2ee.md §4.6).
    let epoch = self.group_or_error(conversation)?.epoch();
    self.note_key_epochs(conversation, &[key.id()], epoch)?;
    self.update_account_data(|data, _| data.note_group_epoch(conversation, epoch))?;
    Ok(key)
  }

  /// Sends content keys to the group as an MLS application message.
  /// With `maybe_recipient`, sealed to that member device only.
  pub(crate) async fn send_bundle(
    &mut self,
    conversation: ConversationId,
    bundle: &ContentKeyBundle,
    maybe_recipient: Option<&VerifiedDevice>,
  ) -> Result<(), CoreError> {
    let attempts = self.settings.max_commit_attempts.max(1);
    for _ in 0..attempts {
      self.resolve_pending_commit(conversation).await?;
      let group = self
        .groups
        .get_mut(&conversation)
        .ok_or(CoreError::NoGroup(conversation))?;
      let payload = match maybe_recipient {
        Some(recipient) => {
          group.send_sealed_content_keys(&self.provider, &self.account.device, recipient, bundle)?
        },
        None => group.send_content_keys(&self.provider, &self.account.device, bundle)?,
      };
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
    // A DM group of one this device founded and nobody joined (setup cut short, or opened on both
    // sides at once): a welcome may be of the group that was founded first. It replaces ours only
    // once it checks out (`join_welcome_group`); without this, it would be dropped as one for a
    // group this device already follows.
    let maybe_founded_for = match self.is_unfinished_dm_founding(conversation)? {
      true => self.dm_setup(conversation)?.map(|setup| setup.others),
      false => None,
    };
    if maybe_founded_for.is_none()
      && self.has_group(conversation)?
      && !self.has_requested_rejoin(conversation)?
    {
      // A welcome to a later epoch of a group this device follows: the queue decides first. A
      // commit that replaced this device's leaf comes before its welcome and drops the group.
      self.sync_mls(conversation).await?;
      if self.is_welcome_outdated(conversation, welcome)? {
        return self.welcome_handled(welcome);
      }
    }
    let is_rejoin = self.has_requested_rejoin(conversation)?;
    if maybe_founded_for.is_none() && self.has_group(conversation)? {
      if !is_rejoin {
        return self.welcome_handled(welcome);
      }
      // This device asked to be added back: the welcome replaces the group it can't follow.
      self.drop_group(conversation)?;
    }
    match self
      .join_welcome_group(conversation, welcome, maybe_founded_for.as_deref())
      .await?
    {
      WelcomeJoin::Joined(joined) => {
        let (group, directory, welcome_sender) = *joined;
        // The joining epoch's key: the members switched to it if they shared history from the
        // join on; the committer's bundle says which key is current.
        let key = group.epoch_content_key(&self.provider)?;
        let key_id = key.id();
        let joined_epoch = group.epoch();
        // What this device had in an earlier membership doesn't count for this one.
        self.forget_key_history(conversation)?;
        self.store.delete(
          Table::AppliedChanges,
          &super::member_device::current_key_wait_key(conversation),
        )?;
        if !is_rejoin {
          // The key bag's current key may be stale (#178): a member's bundle brings the current
          // one (the adder's history share, or the hand-over to a further device).
          let origin = super::member_device::WelcomeOrigin::new(welcome_sender, joined_epoch)?;
          self.wait_for_current_key(conversation, Some(origin))?;
        }
        self.note_key_epochs(conversation, &[key_id], joined_epoch)?;
        let ring = self.account.key_bag.ring_mut(convert::conversation(conversation));
        ring.insert(key)?;
        if is_rejoin {
          // The members switched to this epoch's key with the commit that added this device back;
          // only members of the epoch can derive it (security review A5).
          ring.set_current(key_id)?;
        }
        // The key, the joined group, the welcome cursor and the MLS cursor go to the store
        // together (issue #160): a crash never leaves a group whose welcome counts as unhandled.
        self.atomically(|client| {
          client.persist_keys()?;
          client.groups.insert(conversation, group);
          client.persist_group(conversation)?;
          client
            .store
            .delete(Table::AppliedChanges, &rejoin_key(conversation))?;
          client.store.delete(
            Table::AppliedChanges,
            &super::member_device::device_add_key(conversation),
          )?;
          client.welcome_handled(welcome)?;
          client.remember_member_keys(conversation, &directory, welcome.id.snowflake().unix_millis())?;
          // Everything up to the welcome belongs to epochs before we joined.
          client.advance_cursor(conversation, welcome)
        })?;
        self.note_dm_welcome_joined(conversation)?;
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
      WelcomeJoin::Rejected(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "rejected welcome");
        self.welcome_handled(welcome)
      },
      WelcomeJoin::KeyPackageSpent => {
        // Noted before the welcome counts as handled, so the request is made even if posting it
        // fails now (`join_missing_groups` asks again).
        records::put_json(
          &self.store,
          Table::AppliedChanges,
          &rejoin_key(conversation),
          &RejoinRecord {
            requested_at_ms: 0,
            attempts: 0,
          },
        )?;
        self.welcome_handled(welcome)?;
        self.request_rejoin(conversation, welcome.epoch).await
      },
    }
  }

  /// Verifies and joins the group a welcome brings, with the directory it was verified against.
  /// Identities are pinned for everybody the welcome's tree holds, not only whom the server lists:
  /// somebody removed after the welcome was made is still in its tree, their removal commit next in
  /// the queue, while the server dropped them already (its membership goes after the MLS removal,
  /// e2ee.md §4.6). Every leaf is verified against its user's pinned identity either way.
  /// `maybe_founded_for`: the DM's other participants, when this device holds a group of one it
  /// founded for it, which the welcome's group replaces only if it is that DM's group, made by one
  /// of its participants (e2ee.md §4.6). A welcome that doesn't check out leaves the group as is.
  async fn join_welcome_group(
    &mut self,
    conversation: ConversationId,
    welcome: &MlsMessageResponse,
    maybe_founded_for: Option<&[UserId]>,
  ) -> Result<WelcomeJoin, CoreError> {
    // Pinned before the welcome is opened, which takes this device's key package: a network error
    // here leaves the welcome for the next try. A conversation the server doesn't let this user
    // read (left, removed, or a misrouted welcome) has no group to join.
    let members = match self.api.list_members(conversation).await {
      Ok(members) => members,
      Err(error) if error.is_not_found() || error.is_forbidden() => {
        return Ok(WelcomeJoin::Rejected(MlsError::UnexpectedMessage(
          "welcome for a conversation this user isn't in",
        )));
      },
      Err(error) => return Err(error.into()),
    };
    let me = self.user_id();
    let is_listed = members.members.iter().any(|member| member.user.id == me);
    self
      .pin_identities(members.members.iter().map(|member| member.user.id))
      .await?;
    if let Some(others) = maybe_founded_for {
      // The newest device lists of everybody the DM's group may hold, before the welcome is
      // opened: a revoked device must not be the one that replaces this device's group.
      let me = self.user_id();
      self.trusted_directory(others.iter().copied().chain([me])).await?;
    }
    let stage = match maybe_founded_for {
      Some(_) => ConversationGroup::stage_join_replacing,
      None => ConversationGroup::stage_join,
    };
    let staged = match stage(
      &self.provider,
      welcome.payload.as_slice(),
      convert::conversation(conversation),
    ) {
      Ok(staged) => staged,
      // The key package is gone (deleted after it expired or was replaced, e2ee.md §4.3.1): the
      // members' tree holds this device's leaf, so it asks to be added back, unless the user is no
      // longer in the conversation (a welcome replayed or misrouted after leaving): the members
      // wouldn't answer, and the server would refuse the request (issue #395).
      Err(MlsError::NoMatchingKeyPackage) if !is_listed => {
        tracing::info!(conversation_id = %conversation, "welcome for a spent key package of a conversation this user isn't in; dropped");
        return Ok(WelcomeJoin::Rejected(MlsError::NoMatchingKeyPackage));
      },
      Err(MlsError::NoMatchingKeyPackage) => {
        tracing::warn!(conversation_id = %conversation, "welcome for a key package this device no longer holds; asking to be added back");
        return Ok(WelcomeJoin::KeyPackageSpent);
      },
      Err(error) => return Ok(WelcomeJoin::Rejected(error)),
    };
    let mut users = Vec::new();
    for user in staged.claimed_users()? {
      users.push(convert::user_from_crypto(user)?);
    }
    // Pinned above already, except somebody removed meanwhile.
    let directory = match self.pin_identities(users).await {
      Ok(directory) => directory,
      // openmls takes the key package while it decrypts the welcome, before the tree can be read,
      // so the welcome can't be opened a second time: this device's leaf is in the members' tree
      // but it has no group. It asks to be added back like a device that fell out (e2ee.md §4.6).
      Err(error) if error.is_transient() => {
        tracing::warn!(conversation_id = %conversation, %error, "welcome opened but not joined; asking to be added back");
        return Ok(WelcomeJoin::KeyPackageSpent);
      },
      Err(error) => return Err(error),
    };
    if let Some(others) = maybe_founded_for {
      let me = self.own_member();
      let checked = staged
        .verify(&directory)
        .and_then(|verified| check_dm_welcome(&verified, me, others));
      if let Err(error) = checked {
        return Ok(WelcomeJoin::Rejected(error));
      }
    }
    let welcome_sender = match staged.sender() {
      Ok(sender) => sender,
      Err(error) => return Ok(WelcomeJoin::Rejected(error)),
    };
    let group = match staged.complete(&self.provider, &directory) {
      Ok(group) => group,
      Err(error) => return Ok(WelcomeJoin::Rejected(error)),
    };
    if maybe_founded_for.is_some() {
      // Only now that the join went through: a failed one leaves the founded group as it was.
      tracing::info!(conversation_id = %conversation, "joined the DM group that was founded first");
      self.give_up_founded_group(conversation)?;
    }
    Ok(WelcomeJoin::Joined(Box::new((group, directory, welcome_sender))))
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
    if self.is_welcome_cursor_held {
      // Handled out of order (a catch-up's focused conversation): the in-order pass meets it
      // again and finds it outdated.
      return Ok(());
    }
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
    let maybe_record =
      records::get_json::<RejoinRecord>(&self.store, Table::AppliedChanges, &rejoin_key(conversation))?;
    let attempts = maybe_record.as_ref().map_or(0, |record| record.attempts);
    if let Some(record) = &maybe_record
      && now.saturating_sub(record.requested_at_ms) < self.rejoin_wait_ms(record.attempts)
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
    let has_group = self.has_group(conversation)?;
    let result = self.post_rejoin_request(conversation, current_epoch).await;
    // A device without a group has no queue that would make it ask again: every try counts, also
    // a failed one, so it asks at a growing interval instead of on every sync.
    let attempts = if has_group { 0 } else { attempts.saturating_add(1) };
    if has_group && !matches!(result, Ok(true)) {
      return result.map(|_| ());
    }
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      &rejoin_key(conversation),
      &RejoinRecord {
        requested_at_ms: now,
        attempts,
      },
    )?;
    result.map(|_| ())
  }

  /// How long after a rejoin request this device waits before asking again: `rejoin_retry`, for a
  /// device without a group doubled with every request up to `rejoin_retry_max`.
  fn rejoin_wait_ms(&self, attempts: u32) -> i64 {
    let base = i64::try_from(self.settings.rejoin_retry.as_millis()).unwrap_or(i64::MAX);
    let ceiling = i64::try_from(self.settings.rejoin_retry_max.as_millis())
      .unwrap_or(i64::MAX)
      .max(base);
    let doublings = attempts.saturating_sub(1).min(32);
    base.saturating_mul(1_i64 << doublings).min(ceiling)
  }

  /// Posts the signed rejoin request; `Ok(false)` when the group moved on meanwhile.
  async fn post_rejoin_request(
    &mut self,
    conversation: ConversationId,
    current_epoch: u64,
  ) -> Result<bool, CoreError> {
    let mut epoch = current_epoch;
    for round in 0..2 {
      let signed = RejoinRequest::new(
        convert::conversation(conversation),
        epoch,
        self.own_member(),
        &mut OsRng,
      )
      .sign(&self.account.device, self.account.certificate.clone())?;
      let mut payload = REJOIN_REQUEST_MAGIC.to_vec();
      payload.extend_from_slice(&signed.to_bytes()?);
      let request = PostMlsMessageRequest {
        device_id: self.account.device_id,
        epoch,
        kind: MlsMessageKind::Proposal,
        payload: Base64Bytes::new(payload),
      };
      match self.api.post_mls_message(conversation, &request).await {
        Ok(stored) => {
          self.advance_cursor(conversation, &stored)?;
          return Ok(true);
        },
        // Without a group (a welcome that couldn't be joined) there is no queue to catch up
        // with: the conflict names the epoch to ask for.
        Err(error) if error.is_epoch_conflict() && round == 0 && !self.has_group(conversation)? => {
          match super::workspace::current_epoch_of(&error) {
            Some(current) => epoch = current,
            None => return Err(error.into()),
          }
        },
        // The group moved on meanwhile; the next sync asks for the new epoch.
        Err(error) if error.is_epoch_conflict() => return Ok(false),
        Err(error) => return Err(error.into()),
      }
    }
    Ok(false)
  }

  pub(crate) fn own_member(&self) -> GroupMember {
    GroupMember {
      user_id: convert::user(self.account.user_id),
      device_id: convert::device(self.account.device_id),
    }
  }

  pub(crate) fn has_requested_rejoin(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(
      records::get_json::<RejoinRecord>(&self.store, Table::AppliedChanges, &rejoin_key(conversation))?
        .is_some(),
    )
  }

  /// Queues adding back a device that asked for it, if its request checks out here and the device
  /// wasn't added back within `rejoin_cooldown` before the request (server times, issue #149).
  async fn note_rejoin_request(
    &mut self,
    conversation: ConversationId,
    bytes: &[u8],
    requested_at_ms: i64,
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
    if self.was_readded_recently(conversation, device, requested_at_ms)? {
      tracing::warn!(
        conversation_id = %conversation,
        device_id = %device,
        "ignored a rejoin request for a device added back shortly before"
      );
      return Ok(());
    }
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
    if let Some(share) = self.after_own_commit(conversation, &own.update, own.at_ms)?
      && let Err(error) = self.share_history_or_queue(conversation, share).await
    {
      // Queued when it may work later; the device is back in the group either way.
      tracing::warn!(conversation_id = %conversation, %error, "keys for a device added back not sent yet");
    }
    tracing::info!(conversation_id = %conversation, device_id = %device, "added back a device that fell out of the group");
    Ok(true)
  }

  /// A commit that added `device` back merged at `at_ms` (server time): a waiting request for it is
  /// answered, and further requests are ignored for `rejoin_cooldown`.
  fn note_device_readded(
    &mut self,
    conversation: ConversationId,
    device: GroupMember,
    at_ms: i64,
  ) -> Result<(), CoreError> {
    let device = convert::device_from_crypto(device.device_id)?;
    self.store.delete(
      Table::PendingChanges,
      &PendingChange::rejoin_key(conversation, device),
    )?;
    let key = readded_key(conversation, device);
    let is_newer = records::get_json::<ReaddedRecord>(&self.store, Table::AppliedChanges, &key)?
      .is_none_or(|record| record.at_ms < at_ms);
    if is_newer {
      records::put_json(&self.store, Table::AppliedChanges, &key, &ReaddedRecord { at_ms })?;
    }
    Ok(())
  }

  /// Whether `device` was added back less than `rejoin_cooldown` before `requested_at_ms`, or after
  /// it (an old request). A record past the cooldown is dropped.
  fn was_readded_recently(
    &mut self,
    conversation: ConversationId,
    device: DeviceId,
    requested_at_ms: i64,
  ) -> Result<bool, CoreError> {
    let key = readded_key(conversation, device);
    let Some(record) = records::get_json::<ReaddedRecord>(&self.store, Table::AppliedChanges, &key)? else {
      return Ok(false);
    };
    let cooldown_ms = i64::try_from(self.settings.rejoin_cooldown.as_millis()).unwrap_or(i64::MAX);
    if requested_at_ms >= record.at_ms.saturating_add(cooldown_ms) {
      self.store.delete(Table::AppliedChanges, &key)?;
      return Ok(false);
    }
    Ok(true)
  }

  /// Drops the re-add records whose cooldown is over by the newest server time this device saw
  /// (its MLS queue cursors), so they don't pile up (issue #391); [`Self::was_readded_recently`]
  /// would drop them for the next request anyway. A request older than the re-add (replayed) is
  /// then answered once more, bounded by the request's own freshness (e2ee.md §4.6).
  pub(crate) fn prune_readded_records(&mut self) -> Result<(), CoreError> {
    let Some(server_now_ms) = self
      .sync
      .mls_cursors
      .values()
      .map(|cursor| cursor.snowflake().unix_millis())
      .max()
    else {
      return Ok(());
    };
    let cooldown_ms = i64::try_from(self.settings.rejoin_cooldown.as_millis()).unwrap_or(i64::MAX);
    let mut pruned = 0_usize;
    let records =
      records::list_json_with_prefix::<ReaddedRecord>(&self.store, Table::AppliedChanges, READDED_PREFIX)?;
    for (key, record) in records {
      if server_now_ms >= record.at_ms.saturating_add(cooldown_ms) {
        self.store.delete(Table::AppliedChanges, &key)?;
        pruned += 1;
      }
    }
    if pruned > 0 {
      tracing::debug!(pruned, "dropped re-add records past their cooldown");
    }
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

/// The keys of a bundle sent to the whole group that this device takes: its current key only, or
/// every key when it is this device's own history from a member on an earlier version, which
/// shares history with a new member that way.
fn plain_bundle_keys(bundle: &ContentKeyBundle, is_own_history: bool) -> Vec<ContentKey> {
  bundle
    .keys()
    .iter()
    .filter(|key| is_own_history || bundle.current() == Some(key.id()))
    .map(ContentKey::duplicate)
    .collect()
}

/// Adds the keys sealed to this device; a key whose ID the ring knows with other material is
/// skipped (counted), never replaced.
fn add_sealed_keys(
  ring: &mut teamcraft_crypto::content_key::ContentKeyRing,
  bundle: &ContentKeyBundle,
) -> usize {
  bundle
    .keys()
    .iter()
    .filter(|key| ring.insert(key.duplicate()).is_err())
    .count()
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

/// Whether a verified welcome is of the DM this device founded a group of one for: its tree holds
/// exactly this user and the DM's other participants (this device among them), and one of those
/// participants, or another device of this user, made it.
fn check_dm_welcome(verified: &VerifiedWelcome, me: GroupMember, others: &[UserId]) -> Result<(), MlsError> {
  let in_tree: BTreeSet<_> = verified.members.iter().map(|member| member.user_id).collect();
  let expected: BTreeSet<_> = others
    .iter()
    .map(|user| convert::user(*user))
    .chain([me.user_id])
    .collect();
  if in_tree != expected {
    return Err(MlsError::UnexpectedMessage(
      "the welcome's group isn't the DM's participants",
    ));
  }
  if !verified.members.contains(&me) {
    return Err(MlsError::UnexpectedMessage(
      "the welcome's group doesn't hold this device",
    ));
  }
  let sender = verified.sender;
  let is_participant = sender.user_id != me.user_id && expected.contains(&sender.user_id);
  let is_own_other_device = sender.user_id == me.user_id && sender.device_id != me.device_id;
  if !is_participant && !is_own_other_device {
    return Err(MlsError::UnexpectedMessage(
      "the welcome wasn't made by another participant",
    ));
  }
  Ok(())
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

/// Where this device notes a DM setup it hasn't finished.
fn dm_setup_key(conversation: ConversationId) -> String {
  format!("{DM_SETUP_PREFIX}{conversation}")
}

/// Where this device notes that it asked to be added back to a group.
pub(crate) fn rejoin_key(conversation: ConversationId) -> String {
  format!("rejoin/{conversation}")
}

/// Where a member notes when another device was last added back to the conversation's group.
fn readded_key(conversation: ConversationId, device: DeviceId) -> String {
  format!("{READDED_PREFIX}{conversation}/{device}")
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::Snowflake;

  use super::*;

  const ME: i64 = 10;
  const BOB: i64 = 20;
  const CAROL: i64 = 30;
  const MY_PHONE: i64 = 1;
  const MY_LAPTOP: i64 = 2;
  const BOB_PHONE: i64 = 3;
  const CAROL_PHONE: i64 = 4;

  mod missed_keys {
    use teamcraft_crypto::content_key::ContentKeyRing;
    use teamcraft_crypto::secret::SymmetricKey;

    use super::*;

    #[test]
    fn a_bundle_sent_to_the_whole_group_gives_members_only_its_current_key() {
      // Issue #403: a full-history bundle of an earlier version reaches every member.
      let conversation = teamcraft_crypto::ids::ConversationId::new(5);
      let mut ring = ContentKeyRing::new(conversation);
      let old = ring.rotate(&mut OsRng).id();
      let current = ring.rotate(&mut OsRng).id();
      let bundle = ContentKeyBundle::full_history(&mut ring, &mut OsRng);
      let ids = |keys: Vec<ContentKey>| keys.iter().map(ContentKey::id).collect::<Vec<_>>();

      assert_eq!(ids(plain_bundle_keys(&bundle, false)), [current]);
      assert_eq!(
        ids(plain_bundle_keys(&bundle, true)),
        [old, current],
        "a device that just joined is the new member"
      );
      let without_current =
        ContentKeyBundle::without_current(conversation, vec![ContentKey::generate(&mut OsRng)]);
      assert!(plain_bundle_keys(&without_current, false).is_empty());
    }

    #[test]
    fn a_sealed_key_that_clashes_with_a_known_one_is_skipped_and_the_rest_added() {
      let conversation = teamcraft_crypto::ids::ConversationId::new(5);
      let mut ring = ContentKeyRing::new(conversation);
      let known = ring.rotate(&mut OsRng).duplicate();
      let clashing = ContentKey::from_parts(known.id(), SymmetricKey::generate(&mut OsRng));
      let fresh = ContentKey::generate(&mut OsRng);
      let fresh_id = fresh.id();
      let bundle = ContentKeyBundle::without_current(conversation, vec![clashing, fresh]);

      assert_eq!(add_sealed_keys(&mut ring, &bundle), 1);
      assert!(ring.get(fresh_id).is_some(), "the others still go in");
      assert_eq!(ring.len(), 2);
      assert_eq!(
        ring.current().map(ContentKey::id),
        Some(known.id()),
        "the current key stays"
      );
      // The known key keeps its material: a message under it still opens with the original.
      let mut original = ContentKeyRing::new(conversation);
      original.insert(known).unwrap();
      assert!(ring.insert(original.keys()[0].duplicate()).is_ok());
    }
  }

  mod dm_welcome {
    use super::*;

    #[test]
    fn the_other_participants_welcome_checks_out() {
      let welcome = welcome_of(
        &[member(ME, MY_PHONE), member(BOB, BOB_PHONE)],
        member(BOB, BOB_PHONE),
      );
      assert!(check_dm_welcome(&welcome, member(ME, MY_PHONE), &[user(BOB)]).is_ok());
    }

    #[test]
    fn another_device_of_the_user_may_have_made_it() {
      let welcome = welcome_of(
        &[
          member(ME, MY_PHONE),
          member(ME, MY_LAPTOP),
          member(BOB, BOB_PHONE),
        ],
        member(ME, MY_LAPTOP),
      );
      assert!(check_dm_welcome(&welcome, member(ME, MY_PHONE), &[user(BOB)]).is_ok());
    }

    #[test]
    fn a_tree_with_someone_else_is_refused() {
      let welcome = welcome_of(
        &[
          member(ME, MY_PHONE),
          member(BOB, BOB_PHONE),
          member(CAROL, CAROL_PHONE),
        ],
        member(BOB, BOB_PHONE),
      );
      assert!(check_dm_welcome(&welcome, member(ME, MY_PHONE), &[user(BOB)]).is_err());
    }

    #[test]
    fn a_tree_without_a_participant_is_refused() {
      let welcome = welcome_of(
        &[member(ME, MY_PHONE), member(CAROL, CAROL_PHONE)],
        member(CAROL, CAROL_PHONE),
      );
      assert!(check_dm_welcome(&welcome, member(ME, MY_PHONE), &[user(BOB)]).is_err());
      let group_dm = welcome_of(
        &[member(ME, MY_PHONE), member(BOB, BOB_PHONE)],
        member(BOB, BOB_PHONE),
      );
      assert!(check_dm_welcome(&group_dm, member(ME, MY_PHONE), &[user(BOB), user(CAROL)]).is_err());
    }

    #[test]
    fn a_tree_without_this_device_is_refused() {
      let welcome = welcome_of(
        &[member(ME, MY_LAPTOP), member(BOB, BOB_PHONE)],
        member(BOB, BOB_PHONE),
      );
      assert!(check_dm_welcome(&welcome, member(ME, MY_PHONE), &[user(BOB)]).is_err());
    }

    #[test]
    fn a_welcome_this_device_made_itself_is_refused() {
      let welcome = welcome_of(
        &[member(ME, MY_PHONE), member(BOB, BOB_PHONE)],
        member(ME, MY_PHONE),
      );
      assert!(check_dm_welcome(&welcome, member(ME, MY_PHONE), &[user(BOB)]).is_err());
    }
  }

  fn welcome_of(members: &[GroupMember], sender: GroupMember) -> VerifiedWelcome {
    VerifiedWelcome {
      members: members.to_vec(),
      sender,
    }
  }

  fn member(user_id: i64, device_id: i64) -> GroupMember {
    GroupMember {
      user_id: convert::user(user(user_id)),
      device_id: convert::device(DeviceId::from_snowflake(Snowflake::new(device_id).unwrap())),
    }
  }

  fn user(id: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(id).unwrap())
  }
}
