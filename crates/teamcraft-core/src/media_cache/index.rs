//! Which entries the cache holds, how big they are and when each was last used: what eviction
//! decides on. Platform-neutral; the backends rebuild it from their files or records on opening.

use std::collections::HashMap;

use teamcraft_crypto::local_store::LookupId;

/// Entries by lookup ID with their size and last use, within a byte budget.
#[derive(Debug, Default)]
pub struct CacheIndex {
  entries: HashMap<LookupId, IndexEntry>,
  used_bytes: u64,
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
  pub fn insert(&mut self, id: LookupId, size_bytes: u64, now_ms: u64) {
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
  pub fn touch(&mut self, id: &LookupId, now_ms: u64) {
    if let Some(entry) = self.entries.get_mut(id) {
      entry.last_used_ms = entry.last_used_ms.max(now_ms);
    }
  }

  pub fn remove(&mut self, id: &LookupId) -> bool {
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
  pub fn evict_to(&mut self, budget_bytes: u64, keep: Option<&LookupId>) -> Vec<LookupId> {
    if self.used_bytes <= budget_bytes {
      return Vec::new();
    }
    let mut by_age: Vec<(u64, LookupId)> = self
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

  pub fn contains(&self, id: &LookupId) -> bool {
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
    assert_eq!(index.evict_to(80, None), Vec::<LookupId>::new(), "within budget");
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

  fn id(n: u8) -> LookupId {
    LookupId([n; 32])
  }
}
