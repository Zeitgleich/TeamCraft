//! What a device that was added back missed (e2ee.md §4.6, "a device added back"): the content
//! keys this device got in a group, each with the first epoch it had it in, and since which epoch
//! each other member device has been in the group. A member that adds a device back sends it the
//! keys of the epochs that device was a member in, sealed to it alone, never older ones.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use teamcraft_crypto::content_key::{ContentKey, ContentKeyId};
use teamcraft_crypto::mls::{ContentKeyBundle, GroupMember};
use teamcraft_proto::{ConversationId, DeviceId, UserId};

use super::Client;
use crate::api::ServerApi;
use crate::convert;
use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};

/// Per conversation, next to the MLS state; forgotten with the group.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct KeyHistory {
  /// Content key ID (hex) → the first epoch this device had the key in, in this group: derived
  /// as the epoch's key or received from a member in an MLS message of that epoch. Every device
  /// that was a member then had (or was sent) the same key. Keys from the user's key bag and keys
  /// sealed to this device have no entry: this device can't tell which epoch they belong to.
  key_epochs: BTreeMap<String, u64>,
  /// "user/device" → the epoch its current leaf was added in, for devices added while this
  /// device was in the group. A device added back keeps its entry: it was a member all along.
  member_since: BTreeMap<String, u64>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Notes that this device has had the keys since `epoch` of the conversation's group (keeps an
  /// earlier epoch).
  pub(crate) fn note_key_epochs(
    &mut self,
    conversation: ConversationId,
    keys: &[ContentKeyId],
    epoch: u64,
  ) -> Result<(), CoreError> {
    let mut history = self.key_history(conversation)?;
    let mut is_changed = false;
    for key in keys {
      let hex = key_hex(*key);
      if history.key_epochs.get(&hex).is_none_or(|known| *known > epoch) {
        history.key_epochs.insert(hex, epoch);
        is_changed = true;
      }
    }
    if is_changed {
      self.put_key_history(conversation, &history)?;
    }
    Ok(())
  }

  /// After a merged commit: devices in the group now and not `before` were added in the current
  /// epoch; devices gone left it. A device added back is in both and keeps its entry.
  pub(crate) fn note_member_changes(
    &mut self,
    conversation: ConversationId,
    before: &[GroupMember],
  ) -> Result<(), CoreError> {
    let group = self.group_or_error(conversation)?;
    let epoch = group.epoch();
    let after = group.members()?;
    let mut history = self.key_history(conversation)?;
    for added in after.iter().filter(|member| !before.contains(member)) {
      history.member_since.insert(member_key(*added)?, epoch);
    }
    for removed in before.iter().filter(|member| !after.contains(member)) {
      history.member_since.remove(&member_key(*removed)?);
    }
    self.put_key_history(conversation, &history)
  }

  /// Starts the record afresh for a group this device just joined.
  pub(crate) fn forget_key_history(&mut self, conversation: ConversationId) -> Result<(), CoreError> {
    self
      .store
      .delete(Table::AppliedChanges, &key_history_key(conversation))?;
    Ok(())
  }

  /// The keys `device` was entitled to as a member and may have missed, newest first, at most
  /// `max_missed_keys`: those this device had from an epoch since which both were in the group.
  pub(crate) fn missed_keys(
    &mut self,
    conversation: ConversationId,
    device: GroupMember,
  ) -> Result<ContentKeyBundle, CoreError> {
    let history = self.key_history(conversation)?;
    let joined = self.group_or_error(conversation)?.joined_epoch();
    let since = history
      .member_since
      .get(&member_key(device)?)
      .copied()
      .unwrap_or(0);
    let floor = joined.max(since);
    let maybe_ring = self.account.key_bag.ring(convert::conversation(conversation));
    let mut tagged: Vec<(u64, &ContentKey)> = maybe_ring
      .map(|ring| {
        ring
          .keys()
          .iter()
          .filter_map(|key| {
            let epoch = *history.key_epochs.get(&key_hex(key.id()))?;
            (epoch >= floor).then_some((epoch, key))
          })
          .collect()
      })
      .unwrap_or_default();
    tagged.sort_by_key(|(epoch, _)| std::cmp::Reverse(*epoch));
    let keys = tagged
      .into_iter()
      .take(self.settings.max_missed_keys)
      .map(|(_, key)| key.duplicate())
      .collect();
    Ok(ContentKeyBundle::missed_keys(
      convert::conversation(conversation),
      keys,
    ))
  }

  fn key_history(&self, conversation: ConversationId) -> Result<KeyHistory, CoreError> {
    Ok(
      records::get_json(&self.store, Table::AppliedChanges, &key_history_key(conversation))?
        .unwrap_or_default(),
    )
  }

  fn put_key_history(&self, conversation: ConversationId, history: &KeyHistory) -> Result<(), CoreError> {
    records::put_json(
      &self.store,
      Table::AppliedChanges,
      &key_history_key(conversation),
      history,
    )
  }
}

/// Where a conversation's [`KeyHistory`] is kept.
pub(crate) fn key_history_key(conversation: ConversationId) -> String {
  format!("key_history/{conversation}")
}

fn member_key(member: GroupMember) -> Result<String, CoreError> {
  let user: UserId = convert::user_from_crypto(member.user_id)?;
  let device: DeviceId = convert::device_from_crypto(member.device_id)?;
  Ok(format!("{user}/{device}"))
}

fn key_hex(id: ContentKeyId) -> String {
  id.as_bytes().iter().map(|byte| format!("{byte:02x}")).collect()
}
