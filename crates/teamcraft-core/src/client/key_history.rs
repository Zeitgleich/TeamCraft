//! What a device that was added back missed (e2ee.md §4.6, "a device added back"): the content
//! keys this device got in a group, each with the first epoch it had it in, and since which epoch
//! each other member device has been in the group. A member that adds a device back sends it the
//! keys of the epochs that device was a member in, sealed to it alone, never older ones.

use std::collections::BTreeMap;

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::content_key::{ContentKey, ContentKeyId};
use teamcraft_crypto::mls::{ContentKeyBundle, GroupMember};
use teamcraft_proto::{ConversationId, DeviceId, UserId};

use super::Client;
use super::workspace::{PendingChange, PendingOutcome};
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

/// The current key a bundle hands over ([`Client::hand_over_key`]).
pub(crate) enum HandOverKey {
  /// This device's current key, which it had in the group.
  Known(ContentKey),
  /// Made for the hand-over: in the ring, current once the bundle went out.
  Fresh(ContentKey),
}

impl HandOverKey {
  pub(crate) fn key(&self) -> &ContentKey {
    match self {
      Self::Known(key) | Self::Fresh(key) => key,
    }
  }
}

impl KeyHistory {
  /// The keys of `ring_keys` the member device `device` was entitled to: those this device had
  /// from an epoch in which both were in the group (from `joined`, this device's join, and the
  /// device's own addition on, if this device saw it), newest first, at most `max`.
  fn entitled_keys(
    &self,
    ring_keys: &[ContentKey],
    joined: u64,
    device: &str,
    max: usize,
  ) -> Vec<ContentKey> {
    let since = self.member_since.get(device).copied().unwrap_or(0);
    let floor = joined.max(since);
    let mut tagged: Vec<(u64, &ContentKey)> = ring_keys
      .iter()
      .filter_map(|key| {
        let epoch = *self.key_epochs.get(&key_hex(key.id()))?;
        (epoch >= floor).then_some((epoch, key))
      })
      .collect();
    tagged.sort_by_key(|(epoch, _)| std::cmp::Reverse(*epoch));
    tagged
      .into_iter()
      .take(max)
      .map(|(_, key)| key.duplicate())
      .collect()
  }
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
    let added: Vec<&GroupMember> = after.iter().filter(|member| !before.contains(member)).collect();
    let removed: Vec<&GroupMember> = before.iter().filter(|member| !after.contains(member)).collect();
    if added.is_empty() && removed.is_empty() {
      return Ok(());
    }
    let mut history = self.key_history(conversation)?;
    for member in added {
      history.member_since.insert(member_key(*member)?, epoch);
    }
    for member in removed {
      history.member_since.remove(&member_key(*member)?);
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
    let device_key = member_key(device)?;
    let ring_keys = self
      .account
      .key_bag
      .ring(convert::conversation(conversation))
      .map(|ring| ring.keys())
      .unwrap_or_default();
    let keys = history.entitled_keys(ring_keys, joined, &device_key, self.settings.max_missed_keys);
    Ok(ContentKeyBundle::without_current(
      convert::conversation(conversation),
      keys,
    ))
  }

  /// Whether this device had the key in the group since its join (derived or received there, not
  /// only from the key bag).
  pub(crate) fn has_key_epoch(
    &self,
    conversation: ConversationId,
    key: ContentKeyId,
  ) -> Result<bool, CoreError> {
    Ok(
      self
        .key_history(conversation)?
        .key_epochs
        .contains_key(&key_hex(key)),
    )
  }

  /// The current key to hand to others in a bundle: one this device had in the group since its
  /// join. A current key only the key bag knows (also every key from before this was recorded)
  /// may be stale, one a removed member knows (#178): then a fresh random key is made and handed
  /// over instead; it is new and reaches only the group's members of this epoch, over MLS. The
  /// fresh key goes into the ring but becomes current only with
  /// [`Self::adopt_handed_over_key`] once the bundle went out, so a failed send never leaves this
  /// device writing with a key nobody has. Without a current key (or while this device waits for
  /// one) `None`, unless `is_new_key_allowed`: a device sharing history with a new member makes
  /// one then (also while waiting; a fresh key is safe).
  pub(crate) fn hand_over_key(
    &mut self,
    conversation: ConversationId,
    is_new_key_allowed: bool,
  ) -> Result<Option<HandOverKey>, CoreError> {
    let is_waiting = super::member_device::is_waiting_for_current_key(&self.store, conversation)?;
    let maybe_current = self
      .account
      .key_bag
      .ring(convert::conversation(conversation))
      .and_then(|ring| ring.current())
      .map(ContentKey::duplicate)
      .filter(|_| !is_waiting);
    match maybe_current {
      Some(current) if self.has_key_epoch(conversation, current.id())? => {
        return Ok(Some(HandOverKey::Known(current)));
      },
      Some(_) => {
        tracing::info!(conversation_id = %conversation, "a current key only the key bag knew is replaced by a fresh one");
      },
      None if !is_new_key_allowed => return Ok(None),
      None => {},
    }
    let fresh = ContentKey::generate(&mut OsRng);
    self
      .account
      .key_bag
      .ring_mut(convert::conversation(conversation))
      .insert(fresh.duplicate())?;
    self.persist_keys()?;
    Ok(Some(HandOverKey::Fresh(fresh)))
  }

  /// After the bundle with `key` went out: a fresh key becomes this device's current key, noted
  /// for the epoch it was sent in (a device that waited for one has it now).
  pub(crate) fn adopt_handed_over_key(
    &mut self,
    conversation: ConversationId,
    key: &HandOverKey,
  ) -> Result<(), CoreError> {
    let HandOverKey::Fresh(fresh) = key else {
      return Ok(());
    };
    self
      .account
      .key_bag
      .ring_mut(convert::conversation(conversation))
      .set_current(fresh.id())?;
    self.persist_keys()?;
    let epoch = self.group_or_error(conversation)?.epoch();
    self.note_key_epochs(conversation, &[fresh.id()], epoch)?;
    self.store.delete(
      Table::AppliedChanges,
      &super::member_device::current_key_wait_key(conversation),
    )?;
    Ok(())
  }

  /// A member added `rejoined` back. If that member joined the group after this device, it may
  /// lack keys of earlier epochs the device missed: this device sends its own, at its rank.
  pub(crate) fn follow_up_rejoin(
    &mut self,
    conversation: ConversationId,
    rejoined: GroupMember,
    committer: GroupMember,
  ) -> Result<(), CoreError> {
    if rejoined == self.own_member() || !self.joined_after_this_device(conversation, committer)? {
      return Ok(());
    }
    let device = convert::device_from_crypto(rejoined.device_id)?;
    let not_before_ms = self
      .now_ms()
      .saturating_add(self.commit_delay_ms(conversation, device)?);
    self.queue_pending(PendingChange::ShareMissedKeys {
      conversation_id: conversation,
      user_id: convert::user_from_crypto(rejoined.user_id)?,
      device_id: device,
      not_before_ms,
    })
  }

  /// Another member sealed keys to `recipient`: if it joined no later than this device, it had
  /// every key this device would send, so this device's follow-up isn't needed.
  pub(crate) fn note_sealed_keys_sent(
    &mut self,
    conversation: ConversationId,
    sender: GroupMember,
    recipient: GroupMember,
  ) -> Result<(), CoreError> {
    if self.joined_after_this_device(conversation, sender)? {
      return Ok(());
    }
    let device = convert::device_from_crypto(recipient.device_id)?;
    self.store.delete(
      Table::PendingChanges,
      &PendingChange::missed_keys_share_key(conversation, device),
    )?;
    Ok(())
  }

  /// Sends the follow-up keys once this device's turn came, unless a member that joined no later
  /// did meanwhile.
  pub(crate) async fn apply_missed_keys_share(
    &mut self,
    change: &PendingChange,
  ) -> Result<PendingOutcome, CoreError> {
    let PendingChange::ShareMissedKeys {
      conversation_id,
      user_id,
      device_id,
      not_before_ms,
    } = change
    else {
      return Ok(PendingOutcome::Done);
    };
    if self.now_ms() < *not_before_ms {
      return Ok(PendingOutcome::Later(change.clone()));
    }
    self.sync_mls(*conversation_id).await?;
    let key = PendingChange::missed_keys_share_key(*conversation_id, *device_id);
    if self.stored_pending(&key)?.is_none() || !self.has_group(*conversation_id)? {
      return Ok(PendingOutcome::Done);
    }
    self
      .share_history(
        *conversation_id,
        super::workspace::SharedKeys::MissedKeys {
          user_id: *user_id,
          device_id: *device_id,
        },
      )
      .await?;
    Ok(PendingOutcome::Done)
  }

  /// Whether `member`'s leaf was added after this device joined (this device saw it come).
  fn joined_after_this_device(
    &mut self,
    conversation: ConversationId,
    member: GroupMember,
  ) -> Result<bool, CoreError> {
    let joined = self.group_or_error(conversation)?.joined_epoch();
    let history = self.key_history(conversation)?;
    Ok(
      history
        .member_since
        .get(&member_key(member)?)
        .is_some_and(|since| *since > joined),
    )
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

#[cfg(test)]
mod tests {
  use rand_core::OsRng;

  use super::*;

  const DEVICE: &str = "7/8";

  #[test]
  fn keys_from_before_the_device_was_added_are_never_sent() {
    let keys = generate(4);
    let history = history_of(&keys, &[0, 2, 5, 6], Some(5));
    assert_eq!(
      ids(&history.entitled_keys(&keys, 0, DEVICE, 10)),
      ids(&[keys[3].duplicate(), keys[2].duplicate()]),
      "from epoch 5 on, newest first"
    );
  }

  #[test]
  fn keys_from_before_this_device_joined_are_never_sent() {
    // This device joined in epoch 4: what it noted earlier (an earlier membership) doesn't count.
    let keys = generate(3);
    let history = history_of(&keys, &[1, 4, 9], None);
    assert_eq!(
      ids(&history.entitled_keys(&keys, 4, DEVICE, 10)),
      ids(&[keys[2].duplicate(), keys[1].duplicate()])
    );
  }

  #[test]
  fn keys_without_an_epoch_are_never_sent() {
    // From the key bag or sealed to this device: no epoch to check them against.
    let keys = generate(2);
    let mut history = history_of(&keys[..1], &[3], None);
    history.member_since.clear();
    assert_eq!(ids(&history.entitled_keys(&keys, 0, DEVICE, 10)), ids(&keys[..1]));
  }

  #[test]
  fn at_most_the_newest_are_sent() {
    let keys = generate(5);
    let history = history_of(&keys, &[1, 2, 3, 4, 5], None);
    assert_eq!(
      ids(&history.entitled_keys(&keys, 0, DEVICE, 2)),
      ids(&[keys[4].duplicate(), keys[3].duplicate()])
    );
    assert!(history.entitled_keys(&keys, 0, DEVICE, 0).is_empty());
  }

  #[test]
  fn the_record_survives_encoding() {
    let keys = generate(2);
    let history = history_of(&keys, &[6, 3], Some(2));
    let decoded: KeyHistory = serde_json::from_slice(&serde_json::to_vec(&history).unwrap()).unwrap();
    assert_eq!(decoded, history);
  }

  fn generate(count: usize) -> Vec<ContentKey> {
    (0..count).map(|_| ContentKey::generate(&mut OsRng)).collect()
  }

  fn history_of(keys: &[ContentKey], epochs: &[u64], maybe_since: Option<u64>) -> KeyHistory {
    let mut history = KeyHistory::default();
    for (key, epoch) in keys.iter().zip(epochs) {
      history.key_epochs.insert(key_hex(key.id()), *epoch);
    }
    if let Some(since) = maybe_since {
      history.member_since.insert(DEVICE.into(), since);
    }
    history
  }

  fn ids(keys: &[ContentKey]) -> Vec<ContentKeyId> {
    keys.iter().map(ContentKey::id).collect()
  }
}
