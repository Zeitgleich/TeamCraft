//! What a device that was added back missed (e2ee.md §4.6, "a device added back"): the content
//! keys this device got in a group, each with the first epoch it had it in, and since which epoch
//! each other member device has been in the group. A member that adds a device back sends it the
//! keys of the epochs that device was a member in, sealed to it alone, never older ones, unless
//! the device's leaf came in with a signed addition that shared the full history (#400).

use std::collections::BTreeMap;

use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::content_key::{ContentKey, ContentKeyId};
use teamcraft_crypto::ids::UserId as UserIdCrypto;
use teamcraft_crypto::membership::{HistoryShare, MembershipAction, MembershipChange};
use teamcraft_crypto::mls::{GroupMember, MembershipUpdate};
use teamcraft_proto::{ConversationId, DeviceId, Id, UserId};

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
  /// User ID → the last epoch in which the user's last device left the group, while this device
  /// was in it: a history share pending from before must not reach the user's devices (#403).
  #[serde(default)]
  user_left: BTreeMap<String, u64>,
  /// "user/device" → the epoch it was last added back in, while this device was in the group:
  /// its own record of keys starts there afresh (#389). Decides whether it may lack older keys,
  /// not what it may be sent.
  #[serde(default)]
  member_restarted: BTreeMap<String, u64>,
  /// "user/device" → the epoch its current leaf was added in by a signed change that shared the
  /// full history with its user, which this device verified and merged (#400). A device that
  /// can't open that welcome is added back with a new leaf but stays the same member, so it keeps
  /// the entry; the entry goes with the leaf (a removal), and a leaf added any other way has none.
  #[serde(default)]
  full_history_since: BTreeMap<String, u64>,
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

/// The membership effect of a merged commit, as [`KeyHistory::note_member_change`] records it.
struct MemberChange<'a> {
  /// The epoch the commit started.
  epoch: u64,
  before: &'a [GroupMember],
  after: &'a [GroupMember],
  /// The user its verified signed change added with the full history ([`full_history_user`]).
  maybe_full_history_user: Option<UserIdCrypto>,
  /// The device it added back.
  maybe_rejoined: Option<GroupMember>,
}

/// What a share sealed to one device rests on ([`Client::share_guard`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SealedShare {
  /// The history of a member added with it in `added_epoch` (e2ee.md §4.3).
  HistoryOf { added_epoch: u64 },
  /// The history of the full-history addition that put the device's current leaf in (#400).
  AdditionHistory,
  /// The keys of the epochs the device was a member in.
  MissedKeys,
}

/// A share sealed to `device`, with the epoch its leaf was added in when the share was decided.
/// Checked again before every bundle of it goes out ([`Client::is_share_still_due`]): the group
/// may move on meanwhile (an epoch conflict catches the sender up), and a removal, maybe followed
/// by a new addition of the same device from the join on, stops the rest (#403, #400).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ShareGuard {
  device: GroupMember,
  share: SealedShare,
  maybe_leaf_since: Option<u64>,
}

impl KeyHistory {
  /// Records `change`: added devices with the epoch of their leaf, and whether a signed change
  /// shared the full history with them; removed devices lose every entry (and their user is
  /// noted as gone when it was their last device); a device added back keeps its entries, but its
  /// own record starts afresh. Returns whether anything changed.
  fn note_member_change(&mut self, change: &MemberChange<'_>) -> Result<bool, CoreError> {
    let added: Vec<&GroupMember> = change
      .after
      .iter()
      .filter(|member| !change.before.contains(member))
      .collect();
    let removed: Vec<&GroupMember> = change
      .before
      .iter()
      .filter(|member| !change.after.contains(member))
      .collect();
    if added.is_empty() && removed.is_empty() && change.maybe_rejoined.is_none() {
      return Ok(false);
    }
    for member in added {
      let key = member_key(*member)?;
      self.member_since.insert(key.clone(), change.epoch);
      if change.maybe_full_history_user == Some(member.user_id) {
        self.full_history_since.insert(key, change.epoch);
      } else {
        self.full_history_since.remove(&key);
      }
    }
    for member in removed {
      let key = member_key(*member)?;
      self.member_since.remove(&key);
      self.member_restarted.remove(&key);
      self.full_history_since.remove(&key);
      if !change.after.iter().any(|other| other.user_id == member.user_id) {
        let user = convert::user_from_crypto(member.user_id)?;
        self.user_left.insert(user.to_string(), change.epoch);
      }
    }
    if let Some(rejoined) = change.maybe_rejoined {
      self.member_restarted.insert(member_key(rejoined)?, change.epoch);
    }
    Ok(true)
  }

  /// Whether a share decided while `device`'s leaf was the one added in `maybe_leaf_since` may
  /// still go out: the leaf is the same (not removed, and maybe added again, since), and what the
  /// share rests on still holds. The caller checks that the device is in the group at all.
  fn is_share_still_due(
    &self,
    device: &str,
    user: &str,
    joined: u64,
    share: SealedShare,
    maybe_leaf_since: Option<u64>,
  ) -> bool {
    if self.member_since.get(device).copied() != maybe_leaf_since {
      return false;
    }
    match share {
      SealedShare::HistoryOf { added_epoch } => self.has_stayed(user, added_epoch, joined),
      SealedShare::AdditionHistory => self.is_owed_full_history(device, user, joined),
      SealedShare::MissedKeys => true,
    }
  }

  /// Whether `user` stayed a member from `epoch` on, as far as a device that joined in `joined`
  /// could see.
  fn has_stayed(&self, user: &str, epoch: u64, joined: u64) -> bool {
    epoch >= joined && self.user_left.get(user).is_none_or(|left| *left <= epoch)
  }

  /// Whether `device` of `user`, added back, is owed the history a signed full-history addition
  /// shared with it (#400): its current leaf came in with that addition, which this device saw
  /// committed, and the user stayed a member since, as far as this device could see. What the
  /// device may have missed of it (it may never have opened that welcome) can't be told here: a
  /// device that only reads leaves no trace in the group.
  fn is_owed_full_history(&self, device: &str, user: &str, joined: u64) -> bool {
    let Some(added) = self.full_history_since.get(device) else {
      return false;
    };
    self.member_since.get(device) == Some(added) && self.has_stayed(user, *added, joined)
  }

  /// Whether `member`'s own record of keys starts after `joined` (this device's join): its leaf
  /// was added later, or it was added back later and started afresh (#389). It may then lack
  /// keys this device has.
  fn has_record_starting_after(&self, member: &str, joined: u64) -> bool {
    self.record_start(member).is_some_and(|start| start > joined)
  }

  /// Where `member`'s own record of keys starts, as far as this device saw: its leaf added or
  /// it added back, the later one. `None`: before this device joined.
  fn record_start(&self, member: &str) -> Option<u64> {
    let since = self.member_since.get(member).copied();
    let restarted = self.member_restarted.get(member).copied();
    since.max(restarted)
  }

  /// Keeps the `max` newest key entries (and `keep`, the current key): older ones are never sent
  /// anyway (at most `max_missed_keys`, newest first), and the record stays bounded.
  fn prune_key_epochs(&mut self, max: usize, maybe_keep: Option<&str>) {
    if self.key_epochs.len() <= max {
      return;
    }
    let mut entries: Vec<(String, u64)> = self
      .key_epochs
      .iter()
      .map(|(key, epoch)| (key.clone(), *epoch))
      .collect();
    entries.sort_by_key(|(_, epoch)| std::cmp::Reverse(*epoch));
    let kept: std::collections::BTreeSet<String> =
      entries.into_iter().take(max).map(|(key, _)| key).collect();
    self
      .key_epochs
      .retain(|key, _| kept.contains(key) || maybe_keep == Some(key.as_str()));
  }

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
      let maybe_current = self
        .account
        .key_bag
        .ring(convert::conversation(conversation))
        .and_then(|ring| ring.current())
        .map(|key| key_hex(key.id()));
      history.prune_key_epochs(self.settings.max_missed_keys, maybe_current.as_deref());
      self.put_key_history(conversation, &history)?;
    }
    Ok(())
  }

  /// After the merged commit `update`: devices in the group now and not `before` were added in
  /// the current epoch, with the full history if its signed change shared that with their user;
  /// devices gone left it. A device added back is in both and keeps its entries; its own record
  /// starts afresh.
  pub(crate) fn note_member_changes(
    &mut self,
    conversation: ConversationId,
    before: &[GroupMember],
    update: &MembershipUpdate,
  ) -> Result<(), CoreError> {
    let group = self.group_or_error(conversation)?;
    let epoch = group.epoch();
    let after = group.members()?;
    let mut history = self.key_history(conversation)?;
    let change = MemberChange {
      epoch,
      before,
      after: &after,
      maybe_full_history_user: full_history_user(update),
      maybe_rejoined: update.maybe_rejoined,
    };
    if history.note_member_change(&change)? {
      self.put_key_history(conversation, &history)?;
    }
    Ok(())
  }

  /// Whether this device saw `user` stay in the group since `epoch`: it was in the group itself
  /// then, and the user's last device didn't leave since.
  pub(crate) fn stayed_member_since(
    &mut self,
    conversation: ConversationId,
    user: UserId,
    epoch: u64,
  ) -> Result<bool, CoreError> {
    let joined = self.group_or_error(conversation)?.joined_epoch();
    let history = self.key_history(conversation)?;
    Ok(history.has_stayed(&user.to_string(), epoch, joined))
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
  ) -> Result<Vec<ContentKey>, CoreError> {
    let history = self.key_history(conversation)?;
    let joined = self.group_or_error(conversation)?.joined_epoch();
    let device_key = member_key(device)?;
    let ring_keys = self
      .account
      .key_bag
      .ring(convert::conversation(conversation))
      .map(|ring| ring.keys())
      .unwrap_or_default();
    Ok(history.entitled_keys(ring_keys, joined, &device_key, self.settings.max_missed_keys))
  }

  /// Whether `device`, added back, is owed the full history its leaf's signed addition shared
  /// with its user, by what this device recorded (#400). The caller still checks that the user
  /// may get the full history at all (a guest never does).
  pub(crate) fn is_owed_full_history(
    &mut self,
    conversation: ConversationId,
    device: GroupMember,
  ) -> Result<bool, CoreError> {
    let joined = self.group_or_error(conversation)?.joined_epoch();
    let history = self.key_history(conversation)?;
    let user = convert::user_from_crypto(device.user_id)?;
    Ok(history.is_owed_full_history(&member_key(device)?, &user.to_string(), joined))
  }

  /// The guard of `share` sealed to `device`, decided now ([`Self::is_share_still_due`]).
  pub(crate) fn share_guard(
    &self,
    conversation: ConversationId,
    device: GroupMember,
    share: SealedShare,
  ) -> Result<ShareGuard, CoreError> {
    let history = self.key_history(conversation)?;
    Ok(ShareGuard {
      device,
      share,
      maybe_leaf_since: history.member_since.get(&member_key(device)?).copied(),
    })
  }

  /// Whether the share `guard` stands for may still go out: its device is in the group with the
  /// same leaf as when it was decided, and what the share rests on still holds (#403, #400).
  pub(crate) fn is_share_still_due(
    &mut self,
    conversation: ConversationId,
    guard: &ShareGuard,
  ) -> Result<bool, CoreError> {
    let group = self.group_or_error(conversation)?;
    if !group.members()?.contains(&guard.device) {
      return Ok(false);
    }
    let joined = group.joined_epoch();
    let history = self.key_history(conversation)?;
    let user = convert::user_from_crypto(guard.device.user_id)?;
    Ok(history.is_share_still_due(
      &member_key(guard.device)?,
      &user.to_string(),
      joined,
      guard.share,
      guard.maybe_leaf_since,
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
    // Ranked only among the members this device sees as older than the committer (who may have
    // the keys it lacks), the oldest records first; one stagger per rank, uncapped, so they don't
    // send at once and the first sealed keys from a member no newer cancel the rest.
    let rank = self.follow_up_rank(conversation, rejoined, committer)?;
    let stagger_ms = i64::try_from(self.settings.device_add_stagger.as_millis()).unwrap_or(i64::MAX);
    let not_before_ms = self
      .now_ms()
      .saturating_add(stagger_ms.saturating_mul(i64::from(rank)));
    self.queue_pending(PendingChange::ShareMissedKeys {
      conversation_id: conversation,
      user_id: convert::user_from_crypto(rejoined.user_id)?,
      device_id: device,
      not_before_ms,
    })
  }

  /// This device's place among the group's devices that may follow up a rejoin `committer`
  /// answered: those whose record of keys starts before the committer's, online users first,
  /// then the oldest record (who has the most of what the device missed). Devices agree on the
  /// order as far as they saw the same additions and rejoins; where they don't, two may send at
  /// the same time, which only duplicates keys.
  fn follow_up_rank(
    &mut self,
    conversation: ConversationId,
    rejoined: GroupMember,
    committer: GroupMember,
  ) -> Result<u32, CoreError> {
    let history = self.key_history(conversation)?;
    let joined = self.group_or_error(conversation)?.joined_epoch();
    let committer_start = history.record_start(&member_key(committer)?).unwrap_or(joined);
    let members = self.group_or_error(conversation)?.members()?;
    let mut candidates = Vec::with_capacity(members.len());
    for member in members {
      if member == rejoined || member == committer {
        continue;
      }
      // No entry: in the group since before this device joined, so before the committer.
      let start = history.record_start(&member_key(member)?).unwrap_or(0);
      if start < committer_start {
        let user = convert::user_from_crypto(member.user_id)?;
        candidates.push(FollowUpCandidate {
          device: convert::device_from_crypto(member.device_id)?,
          is_online: self.is_probably_online(user)?,
          record_start: start,
        });
      }
    }
    let own = FollowUpCandidate {
      device: self.account.device_id,
      is_online: true,
      record_start: joined,
    };
    Ok(follow_up_rank_among(
      &candidates,
      own,
      convert::device_from_crypto(rejoined.device_id)?,
    ))
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
    Ok(history.has_record_starting_after(&member_key(member)?, joined))
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

/// A device that may follow up a rejoin ([`Client::follow_up_rank`]).
#[derive(Clone, Copy, Debug)]
struct FollowUpCandidate {
  device: DeviceId,
  is_online: bool,
  record_start: u64,
}

/// How many `candidates` go before `own`: online first, then the oldest record, then an order that
/// changes with the device added back (so the same member doesn't always go first).
fn follow_up_rank_among(candidates: &[FollowUpCandidate], own: FollowUpCandidate, rejoined: DeviceId) -> u32 {
  let order = |candidate: &FollowUpCandidate| {
    (
      !candidate.is_online,
      candidate.record_start,
      candidate.device.snowflake().get() ^ rejoined.snowflake().get(),
    )
  };
  let own_order = order(&own);
  let ahead = candidates
    .iter()
    .filter(|candidate| candidate.device != own.device && order(candidate) < own_order)
    .count();
  u32::try_from(ahead).unwrap_or(u32::MAX)
}

/// Where a conversation's [`KeyHistory`] is kept.
pub(crate) fn key_history_key(conversation: ConversationId) -> String {
  format!("key_history/{conversation}")
}

/// The user a merged commit's verified signed change added with the full history, if it did. Not
/// a user who signed that for themselves: only a member who chooses to share the history adds
/// somebody with it (e2ee.md §4.7), so members don't commit such a change, and don't count it
/// when another member did. This device's own commits went through that check before signing.
fn full_history_user(update: &MembershipUpdate) -> Option<UserIdCrypto> {
  match update.maybe_change.as_ref().map(MembershipChange::action) {
    Some(MembershipAction::AddUser {
      user_id,
      history: HistoryShare::Full,
    }) if update.maybe_signer != Some(*user_id) => Some(*user_id),
    _ => None,
  }
}

fn member_key(member: GroupMember) -> Result<String, CoreError> {
  let user: UserId = convert::user_from_crypto(member.user_id)?;
  let device: DeviceId = convert::device_from_crypto(member.device_id)?;
  Ok(format!("{user}/{device}"))
}

pub(crate) fn key_hex(id: ContentKeyId) -> String {
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
  fn a_history_share_is_valid_only_while_its_user_stayed() {
    let mut history = KeyHistory::default();
    assert!(history.has_stayed("9", 5, 0));
    assert!(
      !history.has_stayed("9", 5, 6),
      "added before this device joined: can't tell"
    );
    history.user_left.insert("9".into(), 7);
    assert!(
      !history.has_stayed("9", 5, 0),
      "left in epoch 7 after the addition in 5"
    );
    assert!(history.has_stayed("9", 8, 0), "a later addition");
    assert!(history.has_stayed("3", 5, 0), "another user");
  }

  #[test]
  fn a_member_added_back_after_this_device_joined_may_lack_older_keys() {
    // Issue #389: a founder-era member (no entry) fell out and was added back in epoch 50.
    let mut history = KeyHistory::default();
    assert!(
      !history.has_record_starting_after(DEVICE, 4),
      "there before this device"
    );
    history.member_restarted.insert(DEVICE.into(), 50);
    assert!(history.has_record_starting_after(DEVICE, 4));
    assert!(
      !history.has_record_starting_after(DEVICE, 60),
      "added back before this device joined"
    );
    history.member_since.insert(DEVICE.into(), 70);
    assert!(
      history.has_record_starting_after(DEVICE, 60),
      "the later start counts"
    );
    // What it may be sent doesn't change: it was a member all along.
    let keys = generate(1);
    let mut tagged = history_of(&keys, &[10], None);
    tagged.member_restarted.insert(DEVICE.into(), 50);
    assert_eq!(ids(&tagged.entitled_keys(&keys, 0, DEVICE, 10)), ids(&keys));
  }

  #[test]
  fn an_entry_from_an_earlier_version_reads_without_restarts() {
    let decoded: KeyHistory =
      serde_json::from_str(r#"{"key_epochs":{"00":3},"member_since":{"7/8":2}}"#).unwrap();
    assert!(decoded.member_restarted.is_empty());
    assert!(decoded.full_history_since.is_empty());
    assert_eq!(decoded.member_since.get(DEVICE), Some(&2));
  }

  /// Issue #400: who is owed the full history again when it is added back.
  mod full_history {
    use teamcraft_crypto::ids::ConversationId as ConversationIdCrypto;

    use super::*;

    const USER: &str = "7";

    #[test]
    fn a_device_its_full_history_addition_put_in_is_owed_it() {
      assert!(added_with_full_history(5).is_owed_full_history(DEVICE, USER, 0));
      assert!(
        added_with_full_history(5).is_owed_full_history(DEVICE, USER, 4),
        "this device joined in the epoch before: it merged the addition"
      );
    }

    #[test]
    fn a_device_added_any_other_way_is_owed_nothing() {
      // From the join on, a further device, or a record from before this was noted.
      let mut history = KeyHistory::default();
      history.member_since.insert(DEVICE.into(), 5);
      assert!(!history.is_owed_full_history(DEVICE, USER, 0));
    }

    #[test]
    fn a_leaf_added_again_since_is_owed_nothing() {
      let mut history = added_with_full_history(5);
      history.member_since.insert(DEVICE.into(), 9);
      assert!(!history.is_owed_full_history(DEVICE, USER, 0));
    }

    #[test]
    fn a_user_who_left_since_is_owed_nothing() {
      let mut history = added_with_full_history(5);
      history.user_left.insert(USER.into(), 7);
      assert!(!history.is_owed_full_history(DEVICE, USER, 0));
    }

    #[test]
    fn an_addition_before_this_device_joined_is_not_vouched_for() {
      assert!(!added_with_full_history(5).is_owed_full_history(DEVICE, USER, 6));
    }

    #[test]
    fn only_a_signed_full_history_addition_by_somebody_else_counts() {
      let (user, admin) = (UserIdCrypto::new(7), UserIdCrypto::new(1));
      let full = add(user, HistoryShare::Full);
      assert_eq!(full_history_user(&update(Some(full), Some(admin))), Some(user));
      assert_eq!(
        full_history_user(&update(Some(full), None)),
        Some(user),
        "this device's own commit"
      );
      assert_eq!(
        full_history_user(&update(Some(full), Some(user))),
        None,
        "signed by the user added"
      );
      let from_join = add(user, HistoryShare::FromJoin);
      assert_eq!(full_history_user(&update(Some(from_join), Some(admin))), None);
      let removal = MembershipAction::RemoveUser { user_id: user };
      assert_eq!(full_history_user(&update(Some(removal), Some(admin))), None);
      assert_eq!(
        full_history_user(&update(None, None)),
        None,
        "a rejoin or a further device"
      );
    }

    fn add(user_id: UserIdCrypto, history: HistoryShare) -> MembershipAction {
      MembershipAction::AddUser { user_id, history }
    }

    fn update(
      maybe_action: Option<MembershipAction>,
      maybe_signer: Option<UserIdCrypto>,
    ) -> MembershipUpdate {
      MembershipUpdate {
        maybe_change: maybe_action
          .map(|action| MembershipChange::new(ConversationIdCrypto::new(3), 4, action, &mut OsRng)),
        maybe_signer,
        ..MembershipUpdate::default()
      }
    }
  }

  /// What a merged commit leaves in the record ([`KeyHistory::note_member_change`]).
  mod member_changes {
    use super::*;

    const EPOCH: u64 = 9;

    #[test]
    fn a_full_history_addition_notes_the_new_leaves_of_its_user_only() {
      let (alice, phone, laptop, bob) = (member(1, 2), member(7, 8), member(7, 9), member(6, 5));
      let mut history = KeyHistory::default();
      let after = [alice, phone, laptop, bob];
      assert!(note(&mut history, &[alice], &after, Some(7), None));
      for device in ["7/8", "7/9", "6/5"] {
        assert_eq!(history.member_since.get(device), Some(&EPOCH), "{device}");
      }
      assert_eq!(history.full_history_since.get("7/8"), Some(&EPOCH));
      assert_eq!(history.full_history_since.get("7/9"), Some(&EPOCH));
      assert!(!history.full_history_since.contains_key("6/5"), "another user");
      assert!(!history.full_history_since.contains_key("1/2"), "not added");
    }

    #[test]
    fn a_leaf_added_any_other_way_has_no_full_history_entry() {
      // A stale entry for a device that comes in again from the join on (or as a further device).
      let mut history = KeyHistory::default();
      history.full_history_since.insert(DEVICE.into(), 3);
      assert!(note(&mut history, &[], &[member(7, 8)], None, None));
      assert_eq!(history.member_since.get(DEVICE), Some(&EPOCH));
      assert!(!history.full_history_since.contains_key(DEVICE));
    }

    #[test]
    fn a_removed_leaf_loses_every_entry() {
      let mut history = KeyHistory::default();
      history.member_since.insert(DEVICE.into(), 3);
      history.member_restarted.insert(DEVICE.into(), 4);
      history.full_history_since.insert(DEVICE.into(), 3);
      let laptop = member(7, 9);
      assert!(note(&mut history, &[member(7, 8), laptop], &[laptop], None, None));
      assert!(!history.member_since.contains_key(DEVICE));
      assert!(!history.member_restarted.contains_key(DEVICE));
      assert!(!history.full_history_since.contains_key(DEVICE));
      assert!(history.user_left.is_empty(), "the user's laptop stays");
    }

    #[test]
    fn the_last_leaf_of_a_user_leaving_notes_the_user_gone() {
      let mut history = KeyHistory::default();
      assert!(note(&mut history, &[member(7, 8)], &[], None, None));
      assert_eq!(history.user_left.get("7"), Some(&EPOCH));
    }

    #[test]
    fn a_device_added_back_keeps_its_entries_and_starts_its_record_afresh() {
      let mut history = KeyHistory::default();
      history.member_since.insert(DEVICE.into(), 3);
      history.full_history_since.insert(DEVICE.into(), 3);
      let device = member(7, 8);
      assert!(note(&mut history, &[device], &[device], None, Some(device)));
      assert_eq!(history.member_since.get(DEVICE), Some(&3));
      assert_eq!(history.full_history_since.get(DEVICE), Some(&3));
      assert_eq!(history.member_restarted.get(DEVICE), Some(&EPOCH));
    }

    #[test]
    fn a_commit_that_changes_no_member_writes_nothing() {
      let mut history = KeyHistory::default();
      let device = member(7, 8);
      assert!(!note(&mut history, &[device], &[device], Some(7), None));
      assert_eq!(history, KeyHistory::default());
    }

    fn note(
      history: &mut KeyHistory,
      before: &[GroupMember],
      after: &[GroupMember],
      maybe_full_history_user: Option<u64>,
      maybe_rejoined: Option<GroupMember>,
    ) -> bool {
      let change = MemberChange {
        epoch: EPOCH,
        before,
        after,
        maybe_full_history_user: maybe_full_history_user.map(UserIdCrypto::new),
        maybe_rejoined,
      };
      history.note_member_change(&change).unwrap()
    }
  }

  /// Whether the rest of a sealed share may still go out ([`KeyHistory::is_share_still_due`]).
  mod share_guard {
    use super::*;

    const USER: &str = "7";
    const SHARES: [SealedShare; 3] = [
      SealedShare::HistoryOf { added_epoch: 5 },
      SealedShare::AdditionHistory,
      SealedShare::MissedKeys,
    ];

    #[test]
    fn a_share_for_the_same_leaf_is_still_due() {
      let history = added_with_full_history(5);
      for share in SHARES {
        assert!(
          history.is_share_still_due(DEVICE, USER, 0, share, Some(5)),
          "{share:?}"
        );
      }
    }

    #[test]
    fn a_leaf_removed_or_added_again_since_stops_every_share() {
      let removed = KeyHistory::default();
      let mut added_again = KeyHistory::default();
      added_again.member_since.insert(DEVICE.into(), 9);
      for share in SHARES {
        assert!(
          !removed.is_share_still_due(DEVICE, USER, 0, share, Some(5)),
          "{share:?}"
        );
        assert!(
          !added_again.is_share_still_due(DEVICE, USER, 0, share, Some(5)),
          "{share:?}"
        );
      }
    }

    #[test]
    fn a_device_there_before_this_one_joined_keeps_its_share() {
      let history = KeyHistory::default();
      assert!(history.is_share_still_due(DEVICE, USER, 4, SealedShare::MissedKeys, None));
    }

    #[test]
    fn the_history_of_an_addition_stops_once_it_is_no_longer_owed() {
      let mut history = added_with_full_history(5);
      history.full_history_since.clear();
      assert!(!history.is_share_still_due(DEVICE, USER, 0, SealedShare::AdditionHistory, Some(5)));
      assert!(history.is_share_still_due(DEVICE, USER, 0, SealedShare::MissedKeys, Some(5)));
    }

    #[test]
    fn a_history_share_stops_once_its_user_left() {
      let mut history = added_with_full_history(5);
      history.user_left.insert(USER.into(), 7);
      let share = SealedShare::HistoryOf { added_epoch: 5 };
      assert!(!history.is_share_still_due(DEVICE, USER, 0, share, Some(5)));
    }
  }

  #[test]
  fn only_the_newest_key_entries_and_the_current_key_are_kept() {
    let keys = generate(5);
    let mut history = history_of(&keys, &[1, 5, 2, 9, 3], None);
    let current = key_hex(keys[0].id());
    history.prune_key_epochs(2, Some(&current));
    let mut kept: Vec<u64> = history.key_epochs.values().copied().collect();
    kept.sort_unstable();
    assert_eq!(kept, [1, 5, 9], "epochs 9 and 5, and the current key");
    history.prune_key_epochs(10, None);
    assert_eq!(history.key_epochs.len(), 3, "nothing to prune");
  }

  mod follow_up_rank {
    use teamcraft_proto::Snowflake;

    use super::*;

    #[test]
    fn the_oldest_online_record_goes_first_and_nobody_is_left_out() {
      // Issue #390 review: B joined in epoch 4, D, E and F later; B must not be capped out.
      let (b, d, e, f) = (
        candidate(2, 4),
        candidate(4, 12),
        candidate(5, 15),
        candidate(6, 20),
      );
      let all = [b, d, e, f];
      let rejoined = device(9);
      let ranks: Vec<u32> = all
        .iter()
        .map(|own| follow_up_rank_among(&all, *own, rejoined))
        .collect();
      assert_eq!(ranks, [0, 1, 2, 3], "oldest record first, every rank distinct");
    }

    #[test]
    fn online_members_go_before_older_offline_ones() {
      let offline_oldest = FollowUpCandidate {
        is_online: false,
        ..candidate(2, 0)
      };
      let online = candidate(3, 10);
      assert_eq!(
        follow_up_rank_among(&[offline_oldest, online], online, device(9)),
        0
      );
    }

    #[test]
    fn equal_records_are_ordered_by_the_device_added_back() {
      let (a, b) = (candidate(1, 0), candidate(2, 0));
      let first = follow_up_rank_among(&[a, b], a, device(8));
      assert_eq!(first + follow_up_rank_among(&[a, b], b, device(8)), 1, "one each");
    }

    fn candidate(raw: i64, record_start: u64) -> FollowUpCandidate {
      FollowUpCandidate {
        device: device(raw),
        is_online: true,
        record_start,
      }
    }

    fn device(raw: i64) -> DeviceId {
      DeviceId::from_snowflake(Snowflake::new(raw).unwrap())
    }
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

  fn added_with_full_history(epoch: u64) -> KeyHistory {
    let mut history = KeyHistory::default();
    history.member_since.insert(DEVICE.into(), epoch);
    history.full_history_since.insert(DEVICE.into(), epoch);
    history
  }

  fn member(user: u64, device: u64) -> GroupMember {
    GroupMember {
      user_id: UserIdCrypto::new(user),
      device_id: teamcraft_crypto::ids::DeviceId::new(device),
    }
  }

  fn ids(keys: &[ContentKey]) -> Vec<ContentKeyId> {
    keys.iter().map(ContentKey::id).collect()
  }
}
