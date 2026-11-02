//! A map that keeps what was used last, up to a weight (issue #155): the bounded caches of the
//! sealed store (decrypted entries by bytes, decoded messages and index pages by count).

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

/// Values by key, the least recently used dropped first once their weights sum above the
/// capacity. A value heavier than the whole capacity is not kept.
#[derive(Debug)]
pub(crate) struct Lru<K, V> {
  capacity: usize,
  held: usize,
  next_tick: u64,
  values: HashMap<K, Slot<V>>,
  /// Keys by the tick of their last use: the first is the next to go.
  recency: BTreeMap<u64, K>,
}

#[derive(Debug)]
struct Slot<V> {
  value: V,
  weight: usize,
  tick: u64,
}

impl<K: Clone + Eq + Hash, V> Lru<K, V> {
  pub(crate) fn new(capacity: usize) -> Self {
    Self {
      capacity,
      held: 0,
      next_tick: 0,
      values: HashMap::new(),
      recency: BTreeMap::new(),
    }
  }

  /// The value of `key`, now the most recently used.
  pub(crate) fn get(&mut self, key: &K) -> Option<&V> {
    let tick = self.tick();
    let slot = self.values.get_mut(key)?;
    self.recency.remove(&slot.tick);
    slot.tick = tick;
    self.recency.insert(tick, key.clone());
    Some(&slot.value)
  }

  /// Keeps `value` under `key` (replacing what was there), then drops the least recently used
  /// values until the rest fit.
  pub(crate) fn insert(&mut self, key: K, value: V, weight: usize) {
    self.remove(&key);
    if weight > self.capacity {
      return;
    }
    let tick = self.tick();
    self.held += weight;
    self.recency.insert(tick, key.clone());
    self.values.insert(key, Slot { value, weight, tick });
    while self.held > self.capacity
      && let Some((_, oldest)) = self.recency.pop_first()
      && let Some(slot) = self.values.remove(&oldest)
    {
      self.held -= slot.weight;
    }
  }

  pub(crate) fn remove(&mut self, key: &K) -> Option<V> {
    let slot = self.values.remove(key)?;
    self.recency.remove(&slot.tick);
    self.held -= slot.weight;
    Some(slot.value)
  }

  /// Drops every value whose key `is_dropped` says so.
  pub(crate) fn retain(&mut self, mut is_kept: impl FnMut(&K) -> bool) {
    let dropped: Vec<K> = self.values.keys().filter(|key| !is_kept(key)).cloned().collect();
    for key in dropped {
      self.remove(&key);
    }
  }

  pub(crate) fn clear(&mut self) {
    self.values.clear();
    self.recency.clear();
    self.held = 0;
  }

  /// The summed weight of what is kept.
  #[cfg(test)]
  pub(crate) fn held(&self) -> usize {
    self.held
  }

  fn tick(&mut self) -> u64 {
    self.next_tick += 1;
    self.next_tick
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_least_recently_used_goes_first() {
    let mut lru = Lru::new(3);
    lru.insert(1, "one", 1);
    lru.insert(2, "two", 1);
    lru.insert(3, "three", 1);
    assert_eq!(lru.get(&1), Some(&"one"));
    lru.insert(4, "four", 1);
    assert_eq!(lru.get(&2), None, "the oldest use goes");
    assert_eq!(lru.get(&1), Some(&"one"), "used again, so kept");
    assert_eq!(lru.held(), 3);
  }

  #[test]
  fn weights_count_and_a_value_too_heavy_is_not_kept() {
    let mut lru = Lru::new(10);
    lru.insert("a", (), 6);
    lru.insert("b", (), 6);
    assert_eq!(lru.get(&"a"), None);
    assert_eq!(lru.held(), 6);
    lru.insert("c", (), 11);
    assert_eq!(lru.get(&"c"), None);
    assert_eq!(lru.get(&"b"), Some(&()), "an oversized value drops nothing");
  }

  #[test]
  fn replacing_and_removing_keep_the_weight_right() {
    let mut lru = Lru::new(10);
    lru.insert("a", 1, 4);
    lru.insert("a", 2, 5);
    assert_eq!(lru.held(), 5);
    assert_eq!(lru.remove(&"a"), Some(2));
    assert_eq!(lru.held(), 0);
    lru.insert("b", 3, 1);
    lru.retain(|key| *key != "b");
    assert_eq!(lru.held(), 0);
  }
}
