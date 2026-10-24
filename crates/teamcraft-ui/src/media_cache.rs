//! Decrypted images of this session, shared by the inline previews in the conversation and the
//! lightbox, so an image opens instantly the second time.
//!
//! - The cache holds `blob:` URLs only (e2ee.md: decrypted data stays in memory), least recently
//!   used first out, within [`MEDIA_CACHE_BUDGET_BYTES`]. A view still showing an evicted image keeps
//!   its own reference; the URL is revoked once nobody holds it.
//! - Inline previews load in the background, at most [`MAX_BACKGROUND_LOADS`] at a time, and skip
//!   the download when their row left the screen while they waited. They keep a rendition of at
//!   most [`INLINE_RENDITION_SIDE`] pixels, not the decoded original. The lightbox loads the full
//!   file at once.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use futures_channel::oneshot;

use crate::lightbox::MediaKey;
use crate::model::ConversationId;
use crate::object_url::{FetchError, ObjectUrl, fetch_attachment};

/// Decrypted bytes the cache keeps at most. A large workspace stays within the memory budget of
/// docs/performance.md with this on top; one image bigger than this is shown but not kept.
pub const MEDIA_CACHE_BUDGET_BYTES: u64 = 160 * 1024 * 1024;
/// Longest side of an inline rendition: the largest inline box (480 CSS pixels) on a 2x screen.
pub const INLINE_RENDITION_SIDE: u32 = 960;
/// Inline previews downloading at the same time; opening a channel full of photos doesn't start
/// dozens of downloads at once.
const MAX_BACKGROUND_LOADS: usize = 3;

thread_local! {
  static CACHE: RefCell<Lru<(MediaKey, Rendition), Rc<ObjectUrl>>> = RefCell::new(Lru::new(MEDIA_CACHE_BUDGET_BYTES));
  static PERMITS: RefCell<Permits> = RefCell::new(Permits::new(MAX_BACKGROUND_LOADS));
}

/// Which version of a picture is wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rendition {
  /// The conversation: scaled down to [`INLINE_RENDITION_SIDE`], loaded in the background behind
  /// other previews. Animations stay as they are.
  Inline,
  /// The lightbox: the original, loaded at once.
  Full,
}

/// Least recently used entries with a size each, within a byte budget.
#[derive(Debug)]
pub struct Lru<K, V> {
  /// Oldest first.
  entries: VecDeque<(K, V, u64)>,
  used_bytes: u64,
  budget_bytes: u64,
}

/// Waiting background loads and the free slots.
struct Permits {
  available: usize,
  waiters: VecDeque<oneshot::Sender<()>>,
}

/// A slot for one background load, given back when dropped.
struct Permit;

/// The image, from the cache or downloaded and decrypted. An inline load first waits for a slot and
/// gives up (`Ok(None)`) when `is_wanted` says the preview is gone by then.
pub async fn load(
  conversation: ConversationId,
  key: MediaKey,
  media_type: &str,
  rendition: Rendition,
  is_wanted: impl Fn() -> bool,
) -> Result<Option<Rc<ObjectUrl>>, FetchError> {
  if let Some(hit) = cached(key, rendition) {
    return Ok(Some(hit));
  }
  let (_permit, maybe_max_side) = match rendition {
    Rendition::Full => (None, None),
    Rendition::Inline => (Some(Permit::acquire().await), Some(INLINE_RENDITION_SIDE)),
  };
  if let Some(hit) = cached(key, rendition) {
    return Ok(Some(hit));
  }
  if !is_wanted() {
    return Ok(None);
  }
  let file_id = key.attachment.to_string();
  let url = Rc::new(fetch_attachment(conversation, key.message, file_id, media_type, maybe_max_side).await?);
  CACHE.with_borrow_mut(|cache| cache.insert((key, rendition), url.clone(), url.byte_len()));
  Ok(Some(url))
}

/// The image if it is in the cache (and marks it as just used).
pub fn cached(key: MediaKey, rendition: Rendition) -> Option<Rc<ObjectUrl>> {
  CACHE.with_borrow_mut(|cache| cache.get(&(key, rendition)))
}

impl<K: PartialEq, V: Clone> Lru<K, V> {
  pub fn new(budget_bytes: u64) -> Self {
    Self {
      entries: VecDeque::new(),
      used_bytes: 0,
      budget_bytes,
    }
  }

  /// The value, now the most recently used one.
  pub fn get(&mut self, key: &K) -> Option<V> {
    let index = self.entries.iter().position(|(entry, _, _)| entry == key)?;
    let entry = self.entries.remove(index)?;
    let value = entry.1.clone();
    self.entries.push_back(entry);
    Some(value)
  }

  /// Keeps `value` and drops the least recently used entries until the budget holds. A value larger
  /// than the whole budget is not kept.
  pub fn insert(&mut self, key: K, value: V, size_bytes: u64) {
    if let Some(index) = self.entries.iter().position(|(entry, _, _)| *entry == key)
      && let Some((_, _, old_size)) = self.entries.remove(index)
    {
      self.used_bytes -= old_size;
    }
    if size_bytes > self.budget_bytes {
      return;
    }
    self.entries.push_back((key, value, size_bytes));
    self.used_bytes += size_bytes;
    while self.used_bytes > self.budget_bytes {
      let Some((_, _, evicted)) = self.entries.pop_front() else {
        break;
      };
      self.used_bytes -= evicted;
    }
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

impl Permits {
  fn new(available: usize) -> Self {
    Self {
      available,
      waiters: VecDeque::new(),
    }
  }

  /// Hands the slot to the next waiter that is still there, or frees it.
  fn release(&mut self) {
    while let Some(waiter) = self.waiters.pop_front() {
      if waiter.send(()).is_ok() {
        return;
      }
    }
    self.available += 1;
  }
}

impl Permit {
  async fn acquire() -> Self {
    let maybe_waiting = PERMITS.with_borrow_mut(|permits| {
      if permits.available > 0 {
        permits.available -= 1;
        return None;
      }
      let (sender, receiver) = oneshot::channel();
      permits.waiters.push_back(sender);
      Some(receiver)
    });
    if let Some(receiver) = maybe_waiting {
      // The sender is only dropped unsent when the permits go away with the thread: run anyway.
      receiver.await.ok();
    }
    Self
  }
}

impl Drop for Permit {
  fn drop(&mut self) {
    // The thread-local may already be gone while the page unloads; nothing waits then.
    PERMITS.try_with(|permits| permits.borrow_mut().release()).ok();
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_least_recently_used_entries_go_first_when_over_budget() {
    let mut lru = Lru::new(100);
    lru.insert("a", 1, 40);
    lru.insert("b", 2, 40);
    assert_eq!(lru.get(&"a"), Some(1), "a is now the most recent");
    lru.insert("c", 3, 40);
    assert_eq!(lru.get(&"b"), None, "b was the least recently used");
    assert_eq!((lru.get(&"a"), lru.get(&"c")), (Some(1), Some(3)));
    assert_eq!(lru.used_bytes(), 80);
  }

  #[test]
  fn replacing_an_entry_counts_its_new_size_only() {
    let mut lru = Lru::new(100);
    lru.insert("a", 1, 60);
    lru.insert("a", 2, 30);
    assert_eq!((lru.len(), lru.used_bytes(), lru.get(&"a")), (1, 30, Some(2)));
  }

  #[test]
  fn a_value_larger_than_the_budget_is_not_kept() {
    let mut lru = Lru::new(100);
    lru.insert("small", 1, 10);
    lru.insert("huge", 2, 101);
    assert_eq!(lru.get(&"huge"), None);
    assert_eq!(lru.get(&"small"), Some(1), "nothing was evicted for it");
  }

  #[test]
  fn a_freed_slot_goes_to_the_next_waiter_still_waiting() {
    let mut permits = Permits::new(0);
    let (gone, gone_receiver) = oneshot::channel();
    let (waiting, mut waiting_receiver) = oneshot::channel();
    permits.waiters.extend([gone, waiting]);
    drop(gone_receiver);
    permits.release();
    assert_eq!(waiting_receiver.try_recv(), Ok(Some(())));
    assert_eq!(permits.available, 0);
    permits.release();
    assert_eq!(permits.available, 1, "nobody waits: the slot is free");
  }
}
