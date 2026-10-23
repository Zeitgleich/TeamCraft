//! A further device of a user who is already a member: it joins the user's groups on its own, by
//! MLS external commit from the group information members publish with their commits, without a
//! signed change and without announcing a join (e2ee.md §4.6, §5). Where no group information is
//! there yet, it asks the members (through the group's queue, so members who are offline see it
//! later) and waits. Meanwhile it reads what the key bag's content keys open, checking each sender
//! against the sender's signed device list.

use std::collections::BTreeSet;

use teamcraft_crypto::mls::{ConversationGroup, MlsError};
use teamcraft_proto::{
  Base64Bytes, ConversationId, DeviceId, Id, ListMlsMessagesQuery, MessageResponse, MlsMessageKind,
  PostMlsCommitRequest, PostMlsMessageRequest, PutMlsGroupInfoRequest, Snowflake, UserId,
};

use super::Client;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// Where enrollment notes whether the key bag came over the link ([`Client::has_key_bag_from_link`]).
pub(crate) const KEY_BAG_FROM_LINK_KEY: &str = "key_bag_from_link";

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
    let directory = self.trusted_directory(users).await?;
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
        self.persist_group(conversation)?;
        self.advance_cursor(conversation, &response.commit)?;
        self.remember_member_keys(conversation, &directory)?;
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

  /// Records the signing keys of the devices that sent messages this device couldn't verify yet,
  /// from their users' signed device lists (e2ee.md §4.4), and decrypts those messages again. A
  /// new device reads the history its key bag holds before it is in the group. Only the sending
  /// device's key, only from a list that names it, and only for current members or devices whose
  /// removal time the user's devices recorded (their later messages are refused).
  pub(crate) async fn learn_sender_keys(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self.apply_shared_removals(conversation)?;
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

  /// Takes the removal times the user's devices recorded in the account data (for devices this one
  /// never saw leave) into the local check of removed senders.
  fn apply_shared_removals(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let prefix = format!("{conversation}/");
    let data = self.account_data();
    for (key, entry) in data
      .removed_senders
      .iter()
      .filter(|(key, entry)| key.starts_with(&prefix) && !entry.is_removed)
    {
      if self.store.get(Table::RemovedSenders, key)?.is_none() {
        records::put_json(&self.store, Table::RemovedSenders, key, &entry.value)?;
      }
    }
    Ok(())
  }

  /// A member's device saw a further device of `user` ask, in this conversation's queue, to be
  /// added: if the user's signed device list names that device, it is queued like a colleague's new
  /// device (ranked; checked again when committed), and the group information is published if the
  /// server has none, so the device can join on its own next time.
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
    self.queue_device_addition_in(conversation, user, sender_device)?;
    if let Err(error) = self.publish_group_info_if_missing(conversation).await {
      tracing::debug!(conversation_id = %conversation, %error, "group information not published");
    }
    Ok(())
  }

  /// Publishes this device's group information for the current epoch when the server has none
  /// (the last commit came from an older client).
  async fn publish_group_info_if_missing(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
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

/// Whether this device asked to be let into (or added back to) the conversation's group and no
/// welcome or own join came yet. Read by the UI from the store.
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
        .is_some(),
  )
}

/// Where this device notes that it asked the members to add it.
pub(crate) fn device_add_key(conversation: ConversationId) -> String {
  format!("device_add/{conversation}")
}
