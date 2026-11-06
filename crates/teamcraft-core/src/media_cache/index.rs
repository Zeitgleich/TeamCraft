//! Which entries the cache holds, how big they are and when each was last used: what eviction
//! decides on. Platform-neutral; the backends rebuild it from their files or records on opening.

use std::collections::HashMap;

use teamcraft_crypto::local_store::LookupId;

/// How long after a conversation's purge its group takes no new entries: a picture whose download
/// started before the purge and ends after it isn't cached again (#1161). One finishing later
/// still is, and goes with the age limit.
const PURGE_FENCE_MS: u64 = 10 * 60 * 1000;

/// Entries by ID with their size and last use, within a byte budget.
#[derive(Debug, Default)]
pub struct CacheIndex {
  entries: HashMap<EntryId, IndexEntry>,
  used_bytes: u64,
  /// Groups purged in the last [`PURGE_FENCE_MS`], with when (device clock).
  fenced: HashMap<LookupId, u64>,
}

/// Where an entry is stored: its group (the conversation it belongs to, or the group of pictures
/// of people and spaces) and its own lookup ID, both pseudorandom ([`super::MediaSealer`]). A
/// conversation's entries share the group, so its purge drops them all at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId {
  pub group: LookupId,
  pub entry: LookupId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IndexEntry {
  size_bytes: u64,
  /// Milliseconds since the Unix epoch of the last read or write (the device clock: it only orders
  /// entries).
  last_used_ms: u64,
}

impl CacheIndex {
  /// Notes an entry found on opening (or written now).
  pub fn insert(&mut self, id: EntryId, size_bytes: u64, now_ms: u64) {
    self.remove(&id);
    self.entries.insert(
      id,
      IndexEntry {
        size_bytes,
        last_used_ms: now_ms,
      },
    );
    self.used_bytes = self.used_bytes.saturating_add(size_bytes);
  }

  /// The entry was just read: it moves to the back of the eviction line.
  pub fn touch(&mut self, id: &EntryId, now_ms: u64) {
    if let Some(entry) = self.entries.get_mut(id) {
      entry.last_used_ms = entry.last_used_ms.max(now_ms);
    }
  }

  pub fn remove(&mut self, id: &EntryId) -> bool {
    match self.entries.remove(id) {
      Some(entry) => {
        self.used_bytes = self.used_bytes.saturating_sub(entry.size_bytes);
        true
      },
      None => false,
    }
  }

  /// Takes the least recently used entries out until the rest fits `budget_bytes`, except `keep`
  /// (the entry just written); returns them, oldest first, for the backend to delete.
  pub fn evict_to(&mut self, budget_bytes: u64, keep: Option<&EntryId>) -> Vec<EntryId> {
    if self.used_bytes <= budget_bytes {
      return Vec::new();
    }
    let mut by_age: Vec<(u64, EntryId)> = self
      .entries
      .iter()
      .filter(|(id, _)| Some(*id) != keep)
      .map(|(id, entry)| (entry.last_used_ms, *id))
      .collect();
    by_age.sort_unstable();
    let mut evicted = Vec::new();
    for (_, id) in by_age {
      if self.used_bytes <= budget_bytes {
        break;
      }
      self.remove(&id);
      evicted.push(id);
    }
    evicted
  }

  /// Takes out the entries not used for `max_age_ms` (so also written longer ago); returns them for
  /// the backend to delete.
  pub fn take_expired(&mut self, now_ms: u64, max_age_ms: u64) -> Vec<EntryId> {
    let oldest_kept = now_ms.saturating_sub(max_age_ms);
    let expired: Vec<EntryId> = self
      .entries
      .iter()
      .filter(|(_, entry)| entry.last_used_ms < oldest_kept)
      .map(|(id, _)| *id)
      .collect();
    for id in &expired {
      self.remove(id);
    }
    expired
  }

  /// `group` is being purged: for a while it takes no entries ([`Self::is_fenced`]). Set before the
  /// deletion, so a write that ends during it is caught too.
  pub fn fence(&mut self, group: LookupId, now_ms: u64) {
    self.fenced.retain(|_, at_ms| is_recent(*at_ms, now_ms));
    self.fenced.insert(group, now_ms);
  }

  /// Whether `group` was purged so recently that an entry written now is dropped instead.
  pub fn is_fenced(&self, group: &LookupId, now_ms: u64) -> bool {
    self
      .fenced
      .get(group)
      .is_some_and(|at_ms| is_recent(*at_ms, now_ms))
  }

  /// Takes out every entry of `group` (a conversation was purged); returns how many there were.
  pub fn remove_group(&mut self, group: &LookupId) -> usize {
    let before = self.entries.len();
    let mut freed_bytes = 0u64;
    self.entries.retain(|id, entry| {
      let is_kept = id.group != *group;
      if !is_kept {
        freed_bytes = freed_bytes.saturating_add(entry.size_bytes);
      }
      is_kept
    });
    self.used_bytes = self.used_bytes.saturating_sub(freed_bytes);
    before - self.entries.len()
  }

  pub fn contains(&self, id: &EntryId) -> bool {
    self.entries.contains_key(id)
  }

  pub fn used_bytes(&self) -> u64 {
    self.used_bytes
  }

  pub fn len(&self) -> usize {
    self.entries.len()
  }

  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }
}

/// Within the fence of a purge at `at_ms`; a clock that went back counts the same distance.
fn is_recent(at_ms: u64, now_ms: u64) -> bool {
  at_ms.abs_diff(now_ms) < PURGE_FENCE_MS
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_least_recently_used_entries_go_first() {
    let mut index = CacheIndex::default();
    index.insert(id(1), 40, 1);
    index.insert(id(2), 40, 2);
    index.insert(id(3), 40, 3);
    index.touch(&id(1), 4);
    assert_eq!(index.evict_to(80, None), vec![id(2)], "2 was used least recently");
    assert_eq!(index.used_bytes(), 80);
    assert!(index.contains(&id(1)) && index.contains(&id(3)));
    assert_eq!(index.evict_to(80, None), Vec::<EntryId>::new(), "within budget");
  }

  #[test]
  fn the_entry_just_written_is_never_evicted_for_itself() {
    let mut index = CacheIndex::default();
    index.insert(id(1), 50, 9);
    index.insert(id(2), 60, 1);
    assert_eq!(index.evict_to(60, Some(&id(2))), vec![id(1)]);
    assert!(index.contains(&id(2)));
  }

  #[test]
  fn replacing_an_entry_counts_its_new_size_only() {
    let mut index = CacheIndex::default();
    index.insert(id(1), 60, 1);
    index.insert(id(1), 30, 2);
    assert_eq!((index.len(), index.used_bytes()), (1, 30));
    assert!(index.remove(&id(1)));
    assert!(!index.remove(&id(1)));
    assert_eq!(index.used_bytes(), 0);
  }

  #[test]
  fn a_lower_budget_evicts_down_to_it() {
    let mut index = CacheIndex::default();
    for n in 0..10u8 {
      index.insert(id(n), 10, u64::from(n));
    }
    let evicted = index.evict_to(25, None);
    assert_eq!(evicted, (0..8).map(id).collect::<Vec<_>>(), "oldest first");
    assert_eq!(index.used_bytes(), 20);
  }

  #[test]
  fn entries_unused_for_the_max_age_expire() {
    let mut index = CacheIndex::default();
    index.insert(id(1), 10, 1_000);
    index.insert(id(2), 10, 5_000);
    assert_eq!(index.take_expired(6_000, 2_000), vec![id(1)]);
    assert!(index.contains(&id(2)));
    assert_eq!(index.used_bytes(), 10);
  }

  #[test]
  fn a_group_goes_as_a_whole_and_others_stay() {
    let mut index = CacheIndex::default();
    for n in 0..5u8 {
      index.insert(in_group(7, n), 10, 1);
    }
    index.insert(id(9), 25, 1);
    assert_eq!(index.remove_group(&LookupId([7; 32])), 5);
    assert_eq!((index.len(), index.used_bytes()), (1, 25));
    assert!(index.contains(&id(9)));
    assert_eq!(index.remove_group(&LookupId([7; 32])), 0, "nothing left of it");
  }

  #[test]
  fn a_purged_group_is_fenced_for_a_while_and_others_are_not() {
    let mut index = CacheIndex::default();
    let group = LookupId([7; 32]);
    index.fence(group, 1_000);
    assert!(index.is_fenced(&group, 1_000 + PURGE_FENCE_MS - 1));
    assert!(!index.is_fenced(&LookupId([8; 32]), 1_000));
    assert!(!index.is_fenced(&group, 1_000 + PURGE_FENCE_MS), "over");
    index.fence(LookupId([8; 32]), 1_000 + PURGE_FENCE_MS);
    assert_eq!(index.fenced.len(), 1, "expired fences are swept");
  }

  fn id(n: u8) -> EntryId {
    in_group(0, n)
  }

  fn in_group(group: u8, n: u8) -> EntryId {
    EntryId {
      group: LookupId([group; 32]),
      entry: LookupId([n; 32]),
    }
  }
}
