//! A sorted index kept as pages of records (issue #155): what lets the sealed store read one
//! conversation's page of messages, or one table's keys, without decrypting the rest.
//!
//! An index is a root and its pages. Each page holds up to [`MAX_PAGE_ENTRIES`] entries in key
//! order; the root lists the pages in order with each one's first key and length. A lookup reads
//! the root and one page; a range reads the root and the pages it covers. Pages split when they
//! grow past the limit (an entry added at either end of the index starts a page of its own, so
//! history written in order fills its pages) and go away when they are empty.
//!
//! Where the nodes live is the caller's ([`IndexNodes`]): the sealed store keeps them as sealed
//! records of a reserved table, behind a cache, and writes the nodes a group of writes touched once
//! when it ends.

use std::cmp::Ordering;
use std::ops::Bound;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::StoreError;

/// Entries per page before it splits: a page of message entries seals to about 25 KB, and a root
/// of a channel with 100,000 messages lists about 400 pages.
pub(crate) const MAX_PAGE_ENTRIES: usize = 256;

/// The pages of an index in key order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct IndexRoot<K> {
  #[serde(rename = "n")]
  next_page: u64,
  #[serde(rename = "p")]
  pages: Vec<PageRef<K>>,
}

/// One page as the root lists it.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct PageRef<K> {
  #[serde(rename = "i")]
  id: u64,
  #[serde(rename = "f")]
  first: K,
  #[serde(rename = "l")]
  len: u32,
}

/// Entries in key order, each key once.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct IndexPage<K, V> {
  #[serde(rename = "e")]
  entries: Vec<(K, V)>,
}

/// Which way [`scan`] walks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
  Forward,
  Backward,
}

/// Where an index reads and writes its nodes.
pub(crate) trait IndexNodes<K, V> {
  /// The root; an index never written has no pages.
  fn root(&mut self) -> Result<Arc<IndexRoot<K>>, StoreError>;
  /// A page the root lists.
  fn page(&mut self, id: u64) -> Result<Arc<IndexPage<K, V>>, StoreError>;
  /// Replaces the root; one without pages removes the index.
  fn put_root(&mut self, root: IndexRoot<K>);
  /// Replaces a page; `None` removes it.
  fn put_page(&mut self, id: u64, maybe_page: Option<IndexPage<K, V>>);
}

impl<K> IndexRoot<K> {
  pub(crate) fn empty() -> Self {
    Self {
      next_page: 0,
      pages: Vec::new(),
    }
  }

  pub(crate) fn is_empty(&self) -> bool {
    self.pages.is_empty()
  }

  /// The ids of every page, for removing the index.
  pub(crate) fn page_ids(&self) -> impl Iterator<Item = u64> + '_ {
    self.pages.iter().map(|page| page.id)
  }

  /// The entries of all pages together, from the root alone.
  pub(crate) fn len(&self) -> usize {
    self
      .pages
      .iter()
      .map(|page| usize::try_from(page.len).unwrap_or(usize::MAX))
      .sum()
  }
}

impl<K: Ord> IndexRoot<K> {
  /// The page that holds `key`, or would: the last one starting at or before it (the first page
  /// for a key before every page).
  fn locate(&self, key: &K) -> usize {
    self
      .pages
      .partition_point(|page| page.first <= *key)
      .saturating_sub(1)
  }
}

/// The page the root lists at `at`, checked against what the root says of it: a page rolled back
/// or swapped for another is reported, not read.
fn checked_page<K: Ord, V>(
  nodes: &mut impl IndexNodes<K, V>,
  root: &IndexRoot<K>,
  at: usize,
) -> Result<Arc<IndexPage<K, V>>, StoreError> {
  let listed = &root.pages[at];
  let page = nodes.page(listed.id)?;
  let is_as_listed = usize::try_from(listed.len).is_ok_and(|len| len == page.entries.len())
    && page
      .entries
      .first()
      .is_some_and(|(first, _)| *first == listed.first);
  if !is_as_listed {
    return Err(StoreError::Corrupt(
      "an index page disagrees with its root".into(),
    ));
  }
  Ok(page)
}

/// The value of `key`.
#[cfg(test)]
pub(crate) fn get<K: Ord, V: Clone>(
  nodes: &mut impl IndexNodes<K, V>,
  key: &K,
) -> Result<Option<V>, StoreError> {
  let root = nodes.root()?;
  if root.is_empty() {
    return Ok(None);
  }
  let page = checked_page(nodes, &root, root.locate(key))?;
  Ok(
    page
      .entries
      .binary_search_by(|(entry, _)| entry.cmp(key))
      .ok()
      .map(|at| page.entries[at].1.clone()),
  )
}

/// Adds `key` or replaces its value. Writes nothing when the value is the same.
pub(crate) fn upsert<K: Ord + Clone, V: Clone + PartialEq>(
  nodes: &mut impl IndexNodes<K, V>,
  key: K,
  value: V,
) -> Result<(), StoreError> {
  let root = nodes.root()?;
  if root.is_empty() {
    let mut root = IndexRoot::empty();
    add_page(&mut root, nodes, 0, vec![(key, value)]);
    nodes.put_root(root);
    return Ok(());
  }
  let at = root.locate(&key);
  let id = root.pages[at].id;
  let page = checked_page(nodes, &root, at)?;
  let mut entries = page.entries.clone();
  match entries.binary_search_by(|(entry, _)| entry.cmp(&key)) {
    Ok(found) => {
      if entries[found].1 != value {
        entries[found].1 = value;
        nodes.put_page(id, Some(IndexPage { entries }));
      }
      return Ok(());
    },
    Err(slot) => entries.insert(slot, (key, value)),
  }
  let mut root = IndexRoot::clone(&root);
  if entries.len() <= MAX_PAGE_ENTRIES {
    set_page(&mut root, nodes, at, entries);
  } else {
    let is_append =
      at + 1 == root.pages.len() && entries.last().is_some_and(|(last, _)| is_last(last, &page));
    let is_prepend = at == 0 && entries.first().is_some_and(|(first, _)| is_first(first, &page));
    let split = if is_append {
      entries.len() - 1
    } else if is_prepend {
      1
    } else {
      entries.len() / 2
    };
    let right = entries.split_off(split);
    set_page(&mut root, nodes, at, entries);
    add_page(&mut root, nodes, at + 1, right);
  }
  nodes.put_root(root);
  Ok(())
}

/// Removes `key`; its value, if it was there.
pub(crate) fn remove<K: Ord + Clone, V: Clone>(
  nodes: &mut impl IndexNodes<K, V>,
  key: &K,
) -> Result<Option<V>, StoreError> {
  let root = nodes.root()?;
  if root.is_empty() {
    return Ok(None);
  }
  let at = root.locate(key);
  let id = root.pages[at].id;
  let page = checked_page(nodes, &root, at)?;
  let Ok(found) = page.entries.binary_search_by(|(entry, _)| entry.cmp(key)) else {
    return Ok(None);
  };
  let mut entries = page.entries.clone();
  let (_, value) = entries.remove(found);
  let mut root = IndexRoot::clone(&root);
  if entries.is_empty() {
    root.pages.remove(at);
    nodes.put_page(id, None);
  } else {
    set_page(&mut root, nodes, at, entries);
  }
  nodes.put_root(root);
  Ok(Some(value))
}

/// Hands entries to `visit` in key order (`Forward`) or against it, starting at `from`, until
/// `visit` returns `false` or the index ends. Reads only the pages it walks through.
pub(crate) fn scan<K: Ord, V>(
  nodes: &mut impl IndexNodes<K, V>,
  from: Bound<&K>,
  direction: Direction,
  visit: &mut dyn FnMut(&K, &V) -> bool,
) -> Result<(), StoreError> {
  let root = nodes.root()?;
  if root.is_empty() {
    return Ok(());
  }
  let start = match from {
    Bound::Unbounded => match direction {
      Direction::Forward => 0,
      Direction::Backward => root.pages.len() - 1,
    },
    Bound::Included(key) | Bound::Excluded(key) => root.locate(key),
  };
  let is_inside = |key: &K| match (from, direction) {
    (Bound::Unbounded, _) => true,
    (Bound::Included(bound), Direction::Forward) => key >= bound,
    (Bound::Excluded(bound), Direction::Forward) => key > bound,
    (Bound::Included(bound), Direction::Backward) => key <= bound,
    (Bound::Excluded(bound), Direction::Backward) => key < bound,
  };
  let order: Box<dyn Iterator<Item = usize>> = match direction {
    Direction::Forward => Box::new(start..root.pages.len()),
    Direction::Backward => Box::new((0..=start).rev()),
  };
  for at in order {
    let page = checked_page(nodes, &root, at)?;
    let entries: Box<dyn Iterator<Item = &(K, V)>> = match direction {
      Direction::Forward => Box::new(page.entries.iter()),
      Direction::Backward => Box::new(page.entries.iter().rev()),
    };
    for (key, value) in entries.filter(|(key, _)| is_inside(key)) {
      if !visit(key, value) {
        return Ok(());
      }
    }
  }
  Ok(())
}

/// The keys at positions `start..start + len` in key order. Reads the root and only the pages
/// that hold them: the root knows each page's length.
pub(crate) fn keys_at<K: Ord + Clone, V>(
  nodes: &mut impl IndexNodes<K, V>,
  start: usize,
  len: usize,
) -> Result<Vec<K>, StoreError> {
  let root = nodes.root()?;
  let mut keys = Vec::with_capacity(len.min(MAX_PAGE_ENTRIES));
  let mut page_start = 0usize;
  let end = start.saturating_add(len);
  for page_ref in &root.pages {
    if page_start >= end {
      break;
    }
    let page_len = usize::try_from(page_ref.len).unwrap_or(usize::MAX);
    let page_end = page_start.saturating_add(page_len);
    if page_end > start {
      let page = nodes.page(page_ref.id)?;
      let from = start.saturating_sub(page_start);
      let to = (end - page_start).min(page.entries.len());
      keys.extend(
        page
          .entries
          .get(from..to)
          .into_iter()
          .flatten()
          .map(|(key, _)| key.clone()),
      );
    }
    page_start = page_end;
  }
  Ok(keys)
}

/// How many keys sort before `key`: the lengths of the pages before its page, from the root, and
/// its place in that page.
pub(crate) fn rank<K: Ord, V>(nodes: &mut impl IndexNodes<K, V>, key: &K) -> Result<usize, StoreError> {
  let root = nodes.root()?;
  if root.is_empty() {
    return Ok(0);
  }
  let at = root.locate(key);
  let before: usize = root.pages[..at]
    .iter()
    .map(|page| usize::try_from(page.len).unwrap_or(usize::MAX))
    .sum();
  let page = nodes.page(root.pages[at].id)?;
  Ok(before + page.entries.partition_point(|(entry, _)| entry < key))
}

/// Removes every page and the root.
pub(crate) fn clear<K, V>(nodes: &mut impl IndexNodes<K, V>) -> Result<(), StoreError> {
  let root = nodes.root()?;
  for id in root.page_ids() {
    nodes.put_page(id, None);
  }
  nodes.put_root(IndexRoot::empty());
  Ok(())
}

/// Writes `entries` (not empty) as the page at `at` of `root`.
fn set_page<K: Clone, V>(
  root: &mut IndexRoot<K>,
  nodes: &mut impl IndexNodes<K, V>,
  at: usize,
  entries: Vec<(K, V)>,
) {
  let page = &mut root.pages[at];
  page.first = entries[0].0.clone();
  page.len = u32::try_from(entries.len()).unwrap_or(u32::MAX);
  nodes.put_page(page.id, Some(IndexPage { entries }));
}

/// Writes `entries` (not empty) as a new page at `at` of `root`.
fn add_page<K: Clone, V>(
  root: &mut IndexRoot<K>,
  nodes: &mut impl IndexNodes<K, V>,
  at: usize,
  entries: Vec<(K, V)>,
) {
  let id = root.next_page;
  root.next_page += 1;
  root.pages.insert(
    at,
    PageRef {
      id,
      first: entries[0].0.clone(),
      len: u32::try_from(entries.len()).unwrap_or(u32::MAX),
    },
  );
  nodes.put_page(id, Some(IndexPage { entries }));
}

/// Whether `key` comes after every entry of `page` (before the insert).
fn is_last<K: Ord, V>(key: &K, page: &IndexPage<K, V>) -> bool {
  page
    .entries
    .last()
    .is_none_or(|(last, _)| key.cmp(last) == Ordering::Greater)
}

/// Whether `key` comes before every entry of `page` (before the insert).
fn is_first<K: Ord, V>(key: &K, page: &IndexPage<K, V>) -> bool {
  page
    .entries
    .first()
    .is_none_or(|(first, _)| key.cmp(first) == Ordering::Less)
}

#[cfg(test)]
mod tests {
  use std::collections::{BTreeMap, HashMap};

  use super::*;

  #[test]
  fn entries_read_back_in_order_through_many_splits() {
    let mut nodes = MemoryNodes::<u64>::default();
    let mut expected = BTreeMap::new();
    // A spread-out order: appends, prepends and inserts in the middle.
    for step in 0..2_000u64 {
      let key = (step * 7_919) % 2_003;
      upsert(&mut nodes, key, step).unwrap();
      expected.insert(key, step);
    }
    assert_eq!(all(&mut nodes), expected.into_iter().collect::<Vec<_>>());
    assert!(nodes.root.as_ref().unwrap().pages.len() > 7);
    assert!(
      nodes
        .pages
        .values()
        .all(|page| page.entries.len() <= MAX_PAGE_ENTRIES)
    );
    assert_eq!(nodes.root.as_ref().unwrap().len(), 2_000);
  }

  #[test]
  fn history_written_in_order_fills_its_pages() {
    let mut nodes = MemoryNodes::default();
    for key in 1_000..2_000u64 {
      upsert(&mut nodes, key, ()).unwrap();
    }
    for key in (0..1_000u64).rev() {
      upsert(&mut nodes, key, ()).unwrap();
    }
    let pages = nodes.root.as_ref().unwrap().pages.len();
    assert!(pages <= 2_000 / MAX_PAGE_ENTRIES + 3, "{pages} pages");
  }

  #[test]
  fn lookups_replacements_and_removals() {
    let mut nodes = MemoryNodes::default();
    for key in 0..600u64 {
      upsert(&mut nodes, key, key * 2).unwrap();
    }
    assert_eq!(get(&mut nodes, &300).unwrap(), Some(600));
    assert_eq!(get(&mut nodes, &601).unwrap(), None);
    let writes = nodes.writes;
    upsert(&mut nodes, 300, 600).unwrap();
    assert_eq!(nodes.writes, writes, "an unchanged value writes nothing");
    upsert(&mut nodes, 300, 1).unwrap();
    assert_eq!(get(&mut nodes, &300).unwrap(), Some(1));
    for key in 0..600u64 {
      assert!(remove(&mut nodes, &key).unwrap().is_some());
    }
    assert_eq!(remove(&mut nodes, &3).unwrap(), None);
    assert!(nodes.root.as_ref().is_none_or(IndexRoot::is_empty));
    assert!(nodes.pages.is_empty(), "empty pages go");
  }

  #[test]
  fn scans_start_at_their_bound_and_stop_when_asked() {
    let mut nodes = MemoryNodes::default();
    for key in (0..1_000u64).step_by(2) {
      upsert(&mut nodes, key, ()).unwrap();
    }
    let walk = |nodes: &mut MemoryNodes<()>, from: Bound<&u64>, direction, limit: usize| {
      let mut keys = Vec::new();
      scan(nodes, from, direction, &mut |key, _| {
        keys.push(*key);
        keys.len() < limit
      })
      .unwrap();
      keys
    };
    assert_eq!(
      walk(&mut nodes, Bound::Unbounded, Direction::Backward, 3),
      [998, 996, 994]
    );
    assert_eq!(
      walk(&mut nodes, Bound::Excluded(&600), Direction::Backward, 2),
      [598, 596]
    );
    assert_eq!(
      walk(&mut nodes, Bound::Included(&600), Direction::Backward, 2),
      [600, 598]
    );
    assert_eq!(
      walk(&mut nodes, Bound::Excluded(&511), Direction::Forward, 2),
      [512, 514]
    );
    assert_eq!(
      walk(&mut nodes, Bound::Included(&2), Direction::Forward, 2),
      [2, 4]
    );
    assert_eq!(
      walk(&mut nodes, Bound::Excluded(&0), Direction::Backward, 2),
      Vec::<u64>::new()
    );
    assert_eq!(
      walk(&mut nodes, Bound::Unbounded, Direction::Forward, usize::MAX).len(),
      500
    );
  }

  #[test]
  fn positions_and_ranks_come_from_the_page_lengths() {
    let mut nodes = MemoryNodes::default();
    for key in (0..2_000u64).map(|n| n * 3) {
      upsert(&mut nodes, key, ()).unwrap();
    }
    for key in (0..300u64).map(|n| n * 3 + 1) {
      upsert(&mut nodes, key, ()).unwrap();
    }
    let keys: Vec<u64> = all(&mut nodes).into_iter().map(|(key, ())| key).collect();
    for (start, len) in [
      (0, 0),
      (0, 5),
      (250, 20),
      (1_000, 600),
      (2_295, 10),
      (2_300, 4),
      (5_000, 1),
    ] {
      let expected: Vec<u64> = keys.iter().skip(start).take(len).copied().collect();
      assert_eq!(
        keys_at(&mut nodes, start, len).unwrap(),
        expected,
        "{start}+{len}"
      );
    }
    for probe in [0, 1, 2, 899, 900, 3_000, 5_997, 6_000] {
      assert_eq!(
        rank(&mut nodes, &probe).unwrap(),
        keys.partition_point(|key| *key < probe),
        "{probe}"
      );
    }
    assert_eq!(rank(&mut MemoryNodes::<()>::default(), &5).unwrap(), 0);
  }

  #[test]
  fn a_cleared_index_holds_nothing() {
    let mut nodes = MemoryNodes::default();
    for key in 0..700u64 {
      upsert(&mut nodes, key, ()).unwrap();
    }
    clear(&mut nodes).unwrap();
    assert!(nodes.pages.is_empty());
    assert!(all(&mut nodes).is_empty());
  }

  /// Nodes in maps, counting writes.
  struct MemoryNodes<V> {
    root: Option<IndexRoot<u64>>,
    pages: HashMap<u64, IndexPage<u64, V>>,
    writes: usize,
  }

  impl<V> Default for MemoryNodes<V> {
    fn default() -> Self {
      Self {
        root: None,
        pages: HashMap::new(),
        writes: 0,
      }
    }
  }

  impl<V: Clone> IndexNodes<u64, V> for MemoryNodes<V> {
    fn root(&mut self) -> Result<Arc<IndexRoot<u64>>, StoreError> {
      Ok(Arc::new(self.root.clone().unwrap_or_else(IndexRoot::empty)))
    }

    fn page(&mut self, id: u64) -> Result<Arc<IndexPage<u64, V>>, StoreError> {
      self
        .pages
        .get(&id)
        .cloned()
        .map(Arc::new)
        .ok_or_else(|| StoreError::Corrupt(format!("page {id}")))
    }

    fn put_root(&mut self, root: IndexRoot<u64>) {
      self.writes += 1;
      self.root = Some(root);
    }

    fn put_page(&mut self, id: u64, maybe_page: Option<IndexPage<u64, V>>) {
      self.writes += 1;
      match maybe_page {
        Some(page) => self.pages.insert(id, page),
        None => self.pages.remove(&id),
      };
    }
  }

  fn all<V: Clone>(nodes: &mut MemoryNodes<V>) -> Vec<(u64, V)> {
    let mut entries = Vec::new();
    scan(nodes, Bound::Unbounded, Direction::Forward, &mut |key, value| {
      entries.push((*key, value.clone()));
      true
    })
    .unwrap();
    entries
  }
}
