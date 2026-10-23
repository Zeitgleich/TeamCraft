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
  PostMlsCommitRequest, PostMlsMessageRequest, Snowflake, UserId,
};

use super::Client;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

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
  /// group information on the server, or, without one, by asking the members.
  pub(crate) async fn join_as_member_device(
    &mut self,
    conversation: ConversationId,
  ) -> Result<MemberDeviceJoin, CoreError> {
    let maybe_info = match self.api.mls_group_info(conversation).await {
      Ok(info) => Some(info),
      Err(error) if error.is_not_found() => None,
      Err(error) => return Err(error.into()),
    };
    let me = self.account.user_id;
    let members = self.api.list_members(conversation).await?;
    let users: BTreeSet<UserId> = members
      .members
      .iter()
      .map(|member| member.user.id)
      .chain([me])
      .collect();
    let directory = self.trusted_directory(users).await?;
    let Some(info) = maybe_info else {
      // No group information yet (the last commit came from an older client): only a device of a
      // user with other devices asks; a single device is a user joining.
      if !self.has_other_devices(&directory)? {
        return Ok(MemberDeviceJoin::NotAMember);
      }
      self.request_device_addition(conversation).await?;
      return Ok(MemberDeviceJoin::Waiting);
    };
    let floor = self.account_data().group_epoch(conversation);
    if info.epoch < floor {
      // The user's own devices saw a newer epoch: the server serves old group information.
      tracing::warn!(conversation_id = %conversation, epoch = info.epoch, floor, "refused outdated group information");
      self.request_device_addition(conversation).await?;
      return Ok(MemberDeviceJoin::Waiting);
    }
    let joined = ConversationGroup::join_external(
      &self.provider,
      &self.account.device,
      &self.account.certificate,
      info.group_info.as_slice(),
      convert::conversation(conversation),
      &directory,
    );
    let (group, output) = match joined {
      Ok(joined) => joined,
      Err(MlsError::MissingSignedChange) => return Ok(MemberDeviceJoin::NotAMember),
      Err(MlsError::AlreadyInGroup) => {
        // The server took an earlier join whose answer was lost: be added back with a fresh leaf.
        self.request_rejoin(conversation, info.epoch).await?;
        return Ok(MemberDeviceJoin::Waiting);
      },
      Err(error) => {
        tracing::warn!(conversation_id = %conversation, %error, "could not join by external commit");
        self.request_device_addition(conversation).await?;
        return Ok(MemberDeviceJoin::Waiting);
      },
    };
    self.groups.insert(conversation, group);
    self.persist_group(conversation)?;
    let request = PostMlsCommitRequest {
      device_id: self.account.device_id,
      epoch: info.epoch,
      commit: Base64Bytes::new(output.commit),
      welcomes: Vec::new(),
      maybe_group_info: output.maybe_group_info.map(Base64Bytes::new),
    };
    match self.api.post_mls_commit(conversation, &request).await {
      Ok(response) => {
        self.advance_cursor(conversation, &response.commit)?;
        self.remember_member_keys(conversation, &directory)?;
        self
          .store
          .delete(Table::AppliedChanges, &device_add_key(conversation))?;
        self.retry_undecrypted(conversation)?;
        tracing::info!(conversation_id = %conversation, "joined the group as a further device");
        Ok(MemberDeviceJoin::Joined)
      },
      // openmls merged the commit already: without the server's yes the group is not real. A
      // lost answer shows up next time as this device's leaf in the group (added back then).
      Err(error) => {
        self.drop_group(conversation)?;
        if error.is_epoch_conflict() {
          // Another commit came first; the next sync tries with the newer group information.
          return Ok(MemberDeviceJoin::Waiting);
        }
        Err(error.into())
      },
    }
  }

  /// Records the signing keys of the devices that sent messages this device couldn't verify yet,
  /// from their users' signed device lists (e2ee.md §4.4), and decrypts those messages again. A
  /// new device reads the history its key bag holds before it is in the group.
  pub(crate) async fn learn_sender_keys(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    let parked: Vec<MessageResponse> = records::list_json(&self.store, Table::Undecrypted)?;
    let senders: BTreeSet<UserId> = parked
      .iter()
      .filter(|message| message.conversation_id == conversation)
      .filter(|message| {
        records::device_signing_key(&self.store, message.sender_user_id, message.sender_device_id)
          .ok()
          .flatten()
          .is_none()
      })
      .map(|message| message.sender_user_id)
      .collect();
    if senders.is_empty() {
      return Ok(());
    }
    let users: Vec<UserId> = senders.into_iter().collect();
    let directory = self.trusted_directory(users.iter().copied()).await?;
    for list in self.stored_device_lists(&directory, &users) {
      let user = convert::user_from_crypto(list.user_id())?;
      for entry in list.devices() {
        let device = convert::device_from_crypto(entry.device_id)?;
        if records::device_signing_key(&self.store, user, device)?.is_none() {
          records::put_device_signing_key(&self.store, user, device, &entry.signing_key)?;
        }
      }
    }
    self.retry_undecrypted(conversation)
  }

  /// A member's device saw a further device of `user` ask to be added: queued like a colleague's
  /// new device (ranked, checked against the user's signed device list when committed).
  pub(crate) fn note_device_add_request(
    &mut self,
    sender_device: DeviceId,
    payload: &[u8],
  ) -> Result<(), CoreError> {
    let Some(raw) = <[u8; 8]>::try_from(payload).ok().map(i64::from_be_bytes) else {
      tracing::warn!("ignored a malformed device add request");
      return Ok(());
    };
    let Ok(snowflake) = Snowflake::new(raw) else {
      tracing::warn!("ignored a malformed device add request");
      return Ok(());
    };
    self.queue_device_addition(UserId::from_snowflake(snowflake), sender_device)
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
