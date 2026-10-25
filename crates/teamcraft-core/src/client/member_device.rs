//! A further device of a user who is already a member: it joins the user's groups on its own, by
//! MLS external commit from the group information members publish with their commits, without a
//! signed change and without announcing a join (e2ee.md §4.6, §5). Where no group information is
//! there yet, it asks the members (through the group's queue, so members who are offline see it
//! later) and waits. Meanwhile it reads what the key bag's content keys open, checking each sender
//! against the sender's signed device list.

use std::collections::BTreeSet;

use teamcraft_crypto::mls::{ContentKeyBundle, ConversationGroup, GroupMember, MlsError};
use teamcraft_proto::{
  Base64Bytes, ConversationId, ConversationKind, DeviceId, Id, ListMlsMessagesQuery, MessageResponse,
  MlsMessageKind, PostMlsCommitRequest, PostMlsMessageRequest, PutMlsGroupInfoRequest, Snowflake, UserId,
};

use super::Client;
use super::workspace::{PendingChange, PendingOutcome};
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// Where enrollment notes whether the key bag came over the link ([`Client::has_key_bag_from_link`]).
pub(crate) const KEY_BAG_FROM_LINK_KEY: &str = "key_bag_from_link";

/// One day: retention limits are set in days.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Marks an MLS "proposal" entry in which a new device of a member asks to be added.
pub(crate) const DEVICE_ADD_REQUEST_MAGIC: &[u8] = b"TCDA";

/// How this device stands in a conversation's encryption group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GroupAccess {
  /// In the group: reads and receives every key.
  Member,
  /// Asked to be let in (or added back); another device of the user or a colleague has to come
  /// online. History the key bag holds is readable meanwhile.
  WaitingForDevice,
  /// Not in the group and nothing asked for (not a member, or not tried yet).
  NotJoined,
}

/// What became of joining as a further device of a member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemberDeviceJoin {
  Joined,
  /// Asked the members; one adds this device when it comes online.
  Waiting,
  /// The user has no device in the group: this is a user joining, not a further device.
  NotAMember,
}

/// That this device waits for a member's current key, and from whose welcome.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct CurrentKeyWait {
  #[serde(rename = "welcome", default, skip_serializing_if = "Option::is_none")]
  maybe_origin: Option<WelcomeOrigin>,
}

/// The member whose commit made this device's welcome, and the epoch the welcome joined.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct WelcomeOrigin {
  user_id: UserId,
  device_id: DeviceId,
  epoch: u64,
}

impl WelcomeOrigin {
  pub(crate) fn new(sender: GroupMember, epoch: u64) -> Result<Self, CoreError> {
    Ok(Self {
      user_id: convert::user_from_crypto(sender.user_id)?,
      device_id: convert::device_from_crypto(sender.device_id)?,
      epoch,
    })
  }

  /// Whether a message from `sender` in `epoch` comes from the welcome's maker in the joined epoch.
  pub(crate) fn is(&self, sender: GroupMember, epoch: u64) -> bool {
    sender.user_id == convert::user(self.user_id)
      && sender.device_id == convert::device(self.device_id)
      && epoch == self.epoch
  }
}

/// That this device asked the members to add it, and when.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct DeviceAddRecord {
  requested_at_ms: i64,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Whether this device is in the conversation's encryption group, or waiting to be let in.
  pub fn group_access(&mut self, conversation: ConversationId) -> Result<GroupAccess, CoreError> {
    if self.has_group(conversation)? {
      return Ok(GroupAccess::Member);
    }
    Ok(if is_waiting_for_device(&self.store, conversation)? {
      GroupAccess::WaitingForDevice
    } else {
      GroupAccess::NotJoined
    })
  }

  /// Joins the conversation's group as a further device of a member: by external commit from the
  /// group information on the server, or, without one, by asking the members. Tried at most every
  /// `rejoin_retry` per conversation while waiting.
  pub(crate) async fn join_as_member_device(
    &mut self,
    conversation: ConversationId,
  ) -> Result<MemberDeviceJoin, CoreError> {
    let now = self.now_ms();
    let retry_ms = i64::try_from(self.settings.rejoin_retry.as_millis()).unwrap_or(i64::MAX);
    if self
      .member_join_attempts
      .get(&conversation)
      .is_some_and(|tried_at| now.saturating_sub(*tried_at) < retry_ms)
    {
      return Ok(MemberDeviceJoin::Waiting);
    }
    let me = self.account.user_id;
    let members = self.api.list_members(conversation).await?;
    let users: BTreeSet<UserId> = members
      .members
      .iter()
      .map(|member| member.user.id)
      .chain([me])
      .collect();
    let directory = self.trusted_directory(users.iter().copied()).await?;
    // A single device is a user joining, not a further device.
    if !self.has_other_devices(&directory)? {
      return Ok(MemberDeviceJoin::NotAMember);
    }
    self.member_join_attempts.insert(conversation, now);
    // The floor against old group information is only as fresh as this device's key bag: one that
    // came from another device of the user over the link, not one the server served (§4.6).
    let maybe_info = if self.has_key_bag_from_link()? {
      match self.api.mls_group_info(conversation).await {
        Ok(info) => Some(info),
        Err(error) if error.is_not_found() => None,
        Err(error) => return Err(error.into()),
      }
    } else {
      None
    };
    let Some(info) = maybe_info else {
      self.request_device_addition(conversation).await?;
      return Ok(MemberDeviceJoin::Waiting);
    };
    let floor = self.account_data().group_epoch(conversation);
    let joined = ConversationGroup::join_external(
      &self.provider,
      &self.account.device,
      &self.account.certificate,
      info.group_info.as_slice(),
      convert::conversation(conversation),
      floor,
      &directory,
    );
    let (group, output) = match joined {
      Ok(joined) => joined,
      Err(MlsError::MissingSignedChange) => {
        self.member_join_attempts.remove(&conversation);
        return Ok(MemberDeviceJoin::NotAMember);
      },
      Err(MlsError::AlreadyInGroup) => {
        // The server took an earlier join whose answer was lost: be added back with a fresh leaf.
        // The epoch only addresses the request; the server checks it.
        self.request_rejoin(conversation, info.epoch).await?;
        return Ok(MemberDeviceJoin::Waiting);
      },
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "could not join by external commit");
        self.request_device_addition(conversation).await?;
        return Ok(MemberDeviceJoin::Waiting);
      },
    };
    if self.is_founded_by_participants(conversation)? && !holds_everybody(&group, &users)? {
      // A DM's group that doesn't hold all its participants yet may be a founder's group of one
      // that still loses the founding race (e2ee.md §4.6), and a meeting chat's founder doesn't
      // follow the queue until its invitees are in: the members add this device instead.
      group.delete(&self.provider)?;
      self.request_device_addition(conversation).await?;
      return Ok(MemberDeviceJoin::Waiting);
    }
    // The epoch the signed group information was for; the group is one past it.
    let epoch = group.epoch().saturating_sub(1);
    // Kept in memory only until the server accepts: a crash before leaves nothing behind.
    self.groups.insert(conversation, group);
    let maybe_group_info = output
      .maybe_group_info
      .filter(|info| info.len() <= self.settings.max_group_info_bytes)
      .map(Base64Bytes::new);
    let request = PostMlsCommitRequest {
      device_id: self.account.device_id,
      epoch,
      commit: Base64Bytes::new(output.commit),
      welcomes: Vec::new(),
      maybe_group_info,
    };
    match self.api.post_mls_commit(conversation, &request).await {
      Ok(response) => {
        // Waiting first: a crash right after must not leave the device writing with the key
        // bag's current key. One group of writes with the state and the cursor (issue #160).
        self.atomically(|client| {
          client.forget_key_history(conversation)?;
          client.wait_for_current_key(conversation, None)?;
          client.persist_group_at(conversation, &response.commit)
        })?;
        self.remember_member_keys(
          conversation,
          &directory,
          response.commit.id.snowflake().unix_millis(),
        )?;
        self
          .store
          .delete(Table::AppliedChanges, &device_add_key(conversation))?;
        self.member_join_attempts.remove(&conversation);
        self.retry_undecrypted(conversation)?;
        tracing::info!(conversation_id = %conversation, "joined the group as a further device");
        Ok(MemberDeviceJoin::Joined)
      },
      // openmls merged the commit already: without the server's yes the group is not real. A
      // lost answer shows up next time as this device's leaf in the group (added back then).
      Err(error) => {
        self.drop_group(conversation)?;
        if error.is_epoch_conflict() {
          // Another commit came first; the next try uses the newer group information.
          self.member_join_attempts.remove(&conversation);
          return Ok(MemberDeviceJoin::Waiting);
        }
        Err(error.into())
      },
    }
  }

  /// After joining by external commit or from a welcome: the key bag's current key may be from
  /// before a key switch the user's other devices hadn't uploaded yet, one a removed member knows
  /// (#178). It stays for reading; new messages wait until a member sends the current key or the
  /// next epoch key switch, and the conversation shows that this device waits.
  /// `maybe_origin`: for a welcome, who made it and in which epoch (an adder on an earlier version
  /// sends this device its history in a plain bundle then).
  pub(crate) fn wait_for_current_key(
    &mut self,
    conversation: ConversationId,
    maybe_origin: Option<WelcomeOrigin>,
  ) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      &current_key_wait_key(conversation),
      &CurrentKeyWait { maybe_origin },
    )?;
    let ring = self.account.key_bag.ring_mut(convert::conversation(conversation));
    if let Some(current) = ring.current().map(|key| key.id()) {
      ring.unset_current(current);
    }
    self.persist_keys()
  }

  /// Queues handing the current key to `added`, a further device of a member that this device
  /// didn't add: ranked among the group's devices like an addition, so normally one member sends.
  pub(crate) fn queue_current_key_share(
    &mut self,
    conversation: ConversationId,
    added: GroupMember,
  ) -> Result<(), CoreError> {
    let device = convert::device_from_crypto(added.device_id)?;
    if device == self.account.device_id {
      return Ok(());
    }
    let not_before_ms = self
      .now_ms()
      .saturating_add(self.commit_delay_ms(conversation, device)?);
    self.queue_pending(PendingChange::ShareCurrentKey {
      conversation_id: conversation,
      device_id: device,
      not_before_ms,
    })
  }

  /// A member sent a current key, or the group switched keys: no hand-over is needed any more.
  pub(crate) fn drop_current_key_shares(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let prefix = PendingChange::current_key_share_prefix(conversation);
    for (key, _) in self.store.list(Table::PendingChanges)? {
      if key.starts_with(&prefix) {
        self.store.delete(Table::PendingChanges, &key)?;
      }
    }
    Ok(())
  }

  /// Sends the current key to the group for a further device once this device's turn came, unless
  /// another member did or the device left.
  pub(crate) async fn apply_current_key_share(
    &mut self,
    change: &PendingChange,
  ) -> Result<PendingOutcome, CoreError> {
    let PendingChange::ShareCurrentKey {
      conversation_id,
      device_id,
      not_before_ms,
    } = change
    else {
      return Ok(PendingOutcome::Done);
    };
    let conversation = *conversation_id;
    if self.now_ms() < *not_before_ms {
      return Ok(PendingOutcome::Later(change.clone()));
    }
    // Another member's key may have arrived meanwhile, also earlier in this round.
    self.sync_mls(conversation).await?;
    let key = PendingChange::current_key_share_key(conversation, *device_id);
    if self.stored_pending(&key)?.is_none()
      || !self.has_group(conversation)?
      || !self.group_device_ids(conversation)?.contains(device_id)
    {
      return Ok(PendingOutcome::Done);
    }
    // Never a key only the key bag knows: the waiting device would take it (#178).
    let Some(handed) = self.hand_over_key(conversation, false)? else {
      return Ok(PendingOutcome::Done);
    };
    let bundle = ContentKeyBundle::epoch_key(convert::conversation(conversation), handed.key().duplicate());
    self.send_bundle(conversation, &bundle, None).await?;
    self.adopt_handed_over_key(conversation, &handed)?;
    tracing::info!(conversation_id = %conversation, device_id = %device_id, "sent the current content key for a further device");
    Ok(PendingOutcome::Done)
  }

  /// Records the signing keys of the devices that sent messages this device couldn't verify yet,
  /// from their users' signed device lists (e2ee.md §4.4), and decrypts those messages again. A
  /// new device reads the history its key bag holds before it is in the group. Only the sending
  /// device's key, only from a list that names it, and only for current members or devices whose
  /// removal time the user's devices recorded (their later messages are refused).
  pub(crate) async fn learn_sender_keys(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let parked: Vec<MessageResponse> = records::list_json(&self.store, Table::Undecrypted)?;
    let mut senders = BTreeSet::new();
    for message in parked
      .iter()
      .filter(|message| message.conversation_id == conversation)
    {
      if records::device_signing_key(&self.store, message.sender_user_id, message.sender_device_id)?.is_none()
      {
        senders.insert((message.sender_user_id, message.sender_device_id));
      }
    }
    if senders.is_empty() {
      return Ok(());
    }
    let members: BTreeSet<UserId> = self
      .api
      .list_members(conversation)
      .await?
      .members
      .iter()
      .map(|member| member.user.id)
      .collect();
    let users: Vec<UserId> = senders.iter().map(|(user, _)| *user).collect();
    let directory = self.trusted_directory(users.iter().copied()).await?;
    let lists = self.stored_device_lists(&directory, &users);
    for (user, device) in senders {
      let key = format!("{conversation}/{user}/{device}");
      let has_removal = self.store.get(Table::RemovedSenders, &key)?.is_some();
      if !members.contains(&user) && !has_removal {
        // Neither a member nor a device whose leaving time is known: nothing to check it against.
        continue;
      }
      let crypto_device = convert::device(device);
      let maybe_entry = lists
        .iter()
        .filter(|list| list.user_id() == convert::user(user))
        .flat_map(|list| list.devices())
        .find(|entry| entry.device_id == crypto_device);
      if let Some(entry) = maybe_entry {
        records::put_device_signing_key(&self.store, user, device, &entry.signing_key)?;
      }
    }
    self.retry_undecrypted(conversation)
  }

  /// Shares the removal records this device keeps locally (also those of versions before they
  /// travelled in the account data) with the user's other and future devices. Idempotent.
  pub(crate) fn share_removed_senders(&mut self) -> Result<(), CoreError> {
    let mut rows = Vec::new();
    for (key, _) in self.store.list(Table::RemovedSenders)? {
      if let Some(absence) = records::removed_sender(&self.store, &key)? {
        rows.push((key, absence));
      }
    }
    if rows.is_empty() {
      return Ok(());
    }
    self.update_account_data(|data, _| {
      rows.iter().fold(false, |changed, (key, absence)| {
        let left = data.note_removed_sender(key, absence.left_at_ms);
        let back = absence
          .maybe_back_at_ms
          .is_some_and(|back| data.note_sender_back(key, back));
        changed | left | back
      })
    })
  }

  /// Takes the removal records of the user's devices from the account data into the local check,
  /// for every conversation, before anything is decrypted: when the account data is loaded or
  /// merged.
  pub(crate) fn apply_shared_removals(&mut self) -> Result<(), CoreError> {
    let data = self.account_data();
    for (key, absence) in &data.removed_senders {
      records::merge_removed_sender(&self.store, key, absence)?;
    }
    self.forget_pruned_senders(&data)
  }

  /// Drops removal records that can't matter any more (issue #186), here and in the account data:
  /// absences that ended (the device came back) before anything the workspace's retention still
  /// keeps. A device that is still out keeps its record, it refuses what the device signs from now
  /// on; so does an absence of a conversation the user left, whose history a re-join may share
  /// again.
  pub(crate) fn prune_removed_senders(&mut self) -> Result<(), CoreError> {
    let Some(horizon_ms) = self.retention_horizon_ms()? else {
      return Ok(());
    };
    self.update_account_data(|data, now_ms| {
      data.prune_removed_senders(
        |_, absence| {
          absence
            .maybe_back_at_ms
            .is_some_and(|back| back >= absence.left_at_ms && back < horizon_ms)
        },
        now_ms,
      )
    })?;
    let data = self.account_data();
    self.forget_pruned_senders(&data)
  }

  /// Messages older than this (server time) can't be fetched any more: no conversation keeps them
  /// longer than the workspace's retention limit allows. Counted back from the newest server time
  /// this device saw (its MLS queue cursors), not its own clock, with
  /// `removed_sender_retention_leeway` to spare. `None` without such a limit, when pinned messages
  /// survive retention (an old pinned message stays readable), or before any queue was read.
  fn retention_horizon_ms(&self) -> Result<Option<i64>, CoreError> {
    let retention = self.workspace_policy()?.retention;
    if retention.keep_pinned {
      return Ok(None);
    }
    let Some(max_age_days) = retention.limit.maybe_max_age_days else {
      return Ok(None);
    };
    let Some(server_now_ms) = self
      .sync
      .mls_cursors
      .values()
      .map(|cursor| cursor.snowflake().unix_millis())
      .max()
    else {
      return Ok(None);
    };
    let max_age_ms = i64::from(max_age_days).saturating_mul(DAY_MS);
    let leeway_ms =
      i64::try_from(self.settings.removed_sender_retention_leeway.as_millis()).unwrap_or(i64::MAX);
    Ok(Some(
      server_now_ms.saturating_sub(max_age_ms).saturating_sub(leeway_ms),
    ))
  }

  /// Deletes this device's own removal records that the user's devices dropped as spent.
  fn forget_pruned_senders(&mut self, data: &crate::account_data::AccountData) -> Result<(), CoreError> {
    if data.pruned_senders.is_empty() {
      return Ok(());
    }
    for (key, _) in self.store.list(Table::RemovedSenders)? {
      if let Some(absence) = records::removed_sender(&self.store, &key)?
        && data.is_pruned_sender(&key, absence.left_at_ms)
      {
        self.store.delete(Table::RemovedSenders, &key)?;
      }
    }
    Ok(())
  }

  /// A member's device saw a further device of `user` ask, in this conversation's queue, to be
  /// added: if the user's signed device list names that device, it is queued like a colleague's new
  /// device (ranked; checked again when committed), and the group information is published if the
  /// server has none, so the device can join on its own next time. A repeat from the same device
  /// within `rejoin_retry` is skipped before any request (a device asking too often, or a server
  /// replaying entries); the queued addition is deduplicated by its key anyway.
  pub(crate) async fn note_device_add_request(
    &mut self,
    conversation: ConversationId,
    sender_device: DeviceId,
    payload: &[u8],
  ) -> Result<(), CoreError> {
    let Some(user) = <[u8; 8]>::try_from(payload)
      .ok()
      .map(i64::from_be_bytes)
      .and_then(|raw| Snowflake::new(raw).ok())
      .map(UserId::from_snowflake)
    else {
      tracing::warn!(conversation_id = %conversation, "ignored a malformed device add request");
      return Ok(());
    };
    if self.is_device_add_request_repeated(conversation, sender_device) {
      tracing::debug!(conversation_id = %conversation, device_id = %sender_device, "skipped a repeated device add request");
      return Ok(());
    }
    let directory = self.trusted_directory([user]).await?;
    let is_listed = self.stored_device_lists(&directory, &[user]).iter().any(|list| {
      list
        .devices()
        .iter()
        .any(|entry| entry.device_id == convert::device(sender_device))
    });
    if !is_listed {
      tracing::warn!(conversation_id = %conversation, "ignored a device add request for a device its user doesn't list");
      return Ok(());
    }
    // Only a request that checks out counts: one that failed on the network, or came before the
    // device list named the device, is handled again.
    self.note_device_add_request_handled(conversation, sender_device);
    self.queue_device_addition_in(conversation, user, sender_device)?;
    if let Err(error) = self.publish_group_info_if_missing(conversation).await {
      tracing::debug!(conversation_id = %conversation, %error, "group information not published");
    }
    Ok(())
  }

  /// Whether this device handled a request from `device` to be added to the conversation's group
  /// less than `rejoin_retry` ago.
  fn is_device_add_request_repeated(&self, conversation: ConversationId, device: DeviceId) -> bool {
    let (now, retry_ms) = (self.now_ms(), self.rejoin_retry_ms());
    self
      .device_add_requests_handled
      .get(&(conversation, device))
      .is_some_and(|handled_at| is_within(*handled_at, now, retry_ms))
  }

  /// Notes that a request from `device` was handled now, and forgets those past `rejoin_retry`, so
  /// the map holds only the requests of the last interval.
  fn note_device_add_request_handled(&mut self, conversation: ConversationId, device: DeviceId) {
    let (now, retry_ms) = (self.now_ms(), self.rejoin_retry_ms());
    self
      .device_add_requests_handled
      .retain(|_, handled_at| is_within(*handled_at, now, retry_ms));
    self
      .device_add_requests_handled
      .insert((conversation, device), now);
  }

  fn rejoin_retry_ms(&self) -> i64 {
    i64::try_from(self.settings.rejoin_retry.as_millis()).unwrap_or(i64::MAX)
  }

  /// Publishes this device's group information for the current epoch when the server has none
  /// (the last commit came from an older client).
  pub(super) async fn publish_group_info_if_missing(
    &mut self,
    conversation: ConversationId,
  ) -> Result<(), CoreError> {
    match self.api.mls_group_info(conversation).await {
      Ok(_) => return Ok(()),
      Err(error) if error.is_not_found() => {},
      Err(error) => return Err(error.into()),
    }
    if !self.has_group(conversation)? {
      return Ok(());
    }
    let Some(group) = self.groups.get(&conversation) else {
      return Ok(());
    };
    if group.has_pending_commit() {
      return Ok(());
    }
    let epoch = group.epoch();
    let group_info = group.export_group_info(&self.provider, &self.account.device)?;
    if group_info.len() > self.settings.max_group_info_bytes {
      return Ok(());
    }
    let request = PutMlsGroupInfoRequest {
      device_id: self.account.device_id,
      epoch,
      group_info: Base64Bytes::new(group_info),
    };
    match self.api.put_mls_group_info(conversation, &request).await {
      // The group moved on: the next commit carries newer information anyway.
      Err(error) if error.is_epoch_conflict() => Ok(()),
      other => other.map_err(CoreError::from),
    }
  }

  /// Asks the members, through the group's queue, to add this device; at most every
  /// `rejoin_retry`.
  async fn request_device_addition(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let now = self.now_ms();
    let retry_ms = i64::try_from(self.settings.rejoin_retry.as_millis()).unwrap_or(i64::MAX);
    if let Some(record) =
      records::get_json::<DeviceAddRecord>(&self.store, Table::AppliedChanges, &device_add_key(conversation))?
      && now.saturating_sub(record.requested_at_ms) < retry_ms
    {
      return Ok(());
    }
    // A member needs a key package of this device to add it.
    if let Err(error) = self.top_up_key_packages().await {
      tracing::debug!(%error, "key packages not topped up before asking to be added");
    }
    let query = ListMlsMessagesQuery {
      maybe_after: None,
      maybe_limit: Some(1),
    };
    let current_epoch = self
      .api
      .list_mls_messages(conversation, &query)
      .await?
      .current_epoch;
    let mut payload = DEVICE_ADD_REQUEST_MAGIC.to_vec();
    payload.extend_from_slice(&self.account.user_id.snowflake().get().to_be_bytes());
    let request = PostMlsMessageRequest {
      device_id: self.account.device_id,
      epoch: current_epoch,
      kind: MlsMessageKind::Proposal,
      payload: Base64Bytes::new(payload),
    };
    match self.api.post_mls_message(conversation, &request).await {
      Ok(_) => {},
      // The group moved on meanwhile; the next sync asks again.
      Err(error) if error.is_epoch_conflict() => return Ok(()),
      Err(error) => return Err(error.into()),
    }
    tracing::info!(conversation_id = %conversation, "asked the members to add this device");
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      &device_add_key(conversation),
      &DeviceAddRecord { requested_at_ms: now },
    )
  }

  /// Whether the conversation's group is founded with its participants by a setup record (DM,
  /// group DM, meeting chat), by what this device has cached.
  fn is_founded_by_participants(&self, conversation: ConversationId) -> Result<bool, CoreError> {
    Ok(
      self
        .cached_conversations()?
        .get(&conversation)
        .is_some_and(|response| {
          matches!(
            response.kind,
            ConversationKind::Dm | ConversationKind::Group | ConversationKind::Meeting
          )
        }),
    )
  }

  /// Whether this device's key bag came over the link from another device of the user (not from
  /// the server, as on recovery).
  fn has_key_bag_from_link(&self) -> Result<bool, CoreError> {
    Ok(records::get_json::<bool>(&self.store, Table::Account, KEY_BAG_FROM_LINK_KEY)?.unwrap_or(false))
  }

  /// Whether the user's signed device list names another active device.
  fn has_other_devices(&self, directory: &crate::trust::PinnedDirectory) -> Result<bool, CoreError> {
    let me = self.account.user_id;
    let own = convert::device(self.account.device_id);
    Ok(
      self
        .stored_device_lists(directory, &[me])
        .iter()
        .any(|list| list.devices().iter().any(|entry| entry.device_id != own)),
    )
  }
}

/// Whether every user in `users` has a device in the group.
fn holds_everybody(group: &ConversationGroup, users: &BTreeSet<UserId>) -> Result<bool, CoreError> {
  let mut in_group = BTreeSet::new();
  for member in group.members()? {
    in_group.insert(convert::user_from_crypto(member.user_id)?);
  }
  Ok(users.is_subset(&in_group))
}

/// Whether this device asked to be let into (or added back to) the conversation's group and no
/// welcome or own join came yet, or joined and waits for a member's current key. Read by the UI
/// from the store.
pub fn is_waiting_for_device(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<bool, CoreError> {
  Ok(
    store
      .get(Table::AppliedChanges, &device_add_key(conversation))?
      .is_some()
      || store
        .get(Table::AppliedChanges, &super::groups::rejoin_key(conversation))?
        .is_some()
      || is_waiting_for_current_key(store, conversation)?,
  )
}

/// Whether this device joined (by external commit or from a welcome) and no current key came
/// yet (#178).
pub(crate) fn is_waiting_for_current_key(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<bool, CoreError> {
  Ok(
    store
      .get(Table::AppliedChanges, &current_key_wait_key(conversation))?
      .is_some(),
  )
}

/// Who made the welcome this device joined from, and the epoch it joined in, while it waits for
/// a current key ([`Client::wait_for_current_key`]).
pub(crate) fn welcome_origin(
  store: &impl LocalStore,
  conversation: ConversationId,
) -> Result<Option<WelcomeOrigin>, CoreError> {
  // A record of an earlier version (`true`) has no origin.
  let maybe_wait: Option<CurrentKeyWait> = store
    .get(Table::AppliedChanges, &current_key_wait_key(conversation))?
    .and_then(|bytes| serde_json::from_slice(&bytes).ok());
  Ok(maybe_wait.and_then(|wait| wait.maybe_origin))
}

/// Where this device notes that it waits for a member's current key.
pub(crate) fn current_key_wait_key(conversation: ConversationId) -> String {
  format!("current_key_wait/{conversation}")
}

/// Whether `at_ms` lies less than `window_ms` before `now_ms`. A time ahead of now (the clock was
/// set back) counts as long ago.
fn is_within(at_ms: i64, now_ms: i64, window_ms: i64) -> bool {
  (0..window_ms).contains(&now_ms.saturating_sub(at_ms))
}

/// Where this device notes that it asked the members to add it.
pub(crate) fn device_add_key(conversation: ConversationId) -> String {
  format!("device_add/{conversation}")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_a_time_shortly_before_now_is_within_the_window() {
    assert!(is_within(1_000, 1_000, 10));
    assert!(is_within(991, 1_000, 10));
    assert!(!is_within(990, 1_000, 10), "the window ended");
    assert!(
      !is_within(1_001, 1_000, 10),
      "a time ahead of now counts as long ago"
    );
  }
}
