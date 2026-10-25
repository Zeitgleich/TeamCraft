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
//! - A file is downloaded and decrypted once at a time ([`InFlight`]): the lightbox opened while the
//!   inline preview still loads, or the same picture asked for twice, waits for the load under way
//!   instead of starting another one.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::hash::Hash;
use std::rc::Rc;
use std::thread::LocalKey;

use futures_channel::oneshot;

use crate::lightbox::MediaKey;
use crate::model::ConversationId;
use crate::object_url::{FetchError, ObjectUrl, fetch_decrypted, url_for};

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
  /// Renditions being made, so a second ask for one gets the same URL.
  static LOADING_URLS: RefCell<InFlight<(MediaKey, Rendition), UrlResult>> = RefCell::new(InFlight::default());
  /// Files being downloaded and decrypted, shared by the renditions made from them.
  static LOADING_BYTES: RefCell<InFlight<MediaKey, BytesResult>> = RefCell::new(InFlight::default());
}

type UrlResult = Result<Rc<ObjectUrl>, FetchError>;
type BytesResult = Result<js_sys::Uint8Array, FetchError>;

/// Which version of a picture is wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

/// Loads under way by key, with whoever waits for each. The first to ask leads the load; later ones
/// get its result.
#[derive(Debug)]
pub struct InFlight<K, V> {
  waiting: HashMap<K, Vec<oneshot::Sender<V>>>,
}

/// What asking for a load under way gives.
#[derive(Debug)]
pub enum Joined<V> {
  /// Nothing under way: this caller loads and [`InFlight::finish`]es (or [`InFlight::abandon`]s).
  Lead,
  /// Another caller loads; its result arrives here, or a cancellation when it gave up.
  Wait(oneshot::Receiver<V>),
}

/// Takes a led load off the table if its future is dropped before it finished, so waiters don't
/// wait forever: they are cancelled and load themselves.
struct Leading<K: Eq + Hash + 'static, V: Clone + 'static> {
  table: &'static LocalKey<RefCell<InFlight<K, V>>>,
  maybe_key: Option<K>,
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
  let url = shared(&LOADING_URLS, (key, rendition), || async move {
    let bytes = shared(&LOADING_BYTES, key, || {
      fetch_decrypted(conversation, key.message, key.attachment.to_string())
    })
    .await?;
    let url = Rc::new(url_for(&bytes, media_type, maybe_max_side).await?);
    CACHE.with_borrow_mut(|cache| cache.insert((key, rendition), url.clone(), url.byte_len()));
    Ok(url)
  })
  .await?;
  Ok(Some(url))
}

/// The result of the load of `key` under way in `table`, or of `load` when none is.
async fn shared<K, V, F, Fut>(table: &'static LocalKey<RefCell<InFlight<K, V>>>, key: K, load: F) -> V
where
  K: Clone + Eq + Hash + 'static,
  V: Clone + 'static,
  F: FnOnce() -> Fut,
  Fut: Future<Output = V>,
{
  loop {
    match table.with_borrow_mut(|loads| loads.join(key.clone())) {
      Joined::Wait(receiver) => {
        if let Ok(value) = receiver.await {
          return value;
        }
        // The leader gave up before it finished: ask again, and lead if nobody else does.
      },
      Joined::Lead => {
        let mut leading = Leading {
          table,
          maybe_key: Some(key.clone()),
        };
        let value = load().await;
        leading.maybe_key = None;
        table.with_borrow_mut(|loads| loads.finish(&key, &value));
        return value;
      },
    }
  }
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

impl<K: Eq + Hash, V: Clone> InFlight<K, V> {
  /// Leads the load of `key`, or waits for the one under way.
  pub fn join(&mut self, key: K) -> Joined<V> {
    match self.waiting.get_mut(&key) {
      Some(waiters) => {
        let (sender, receiver) = oneshot::channel();
        waiters.push(sender);
        Joined::Wait(receiver)
      },
      None => {
        self.waiting.insert(key, Vec::new());
        Joined::Lead
      },
    }
  }

  /// The led load is done: everyone waiting for it gets `value`.
  pub fn finish(&mut self, key: &K, value: &V) {
    for waiter in self.waiting.remove(key).into_iter().flatten() {
      // A waiter that went away no longer needs it.
      drop(waiter.send(value.clone()));
    }
  }

  /// The led load stopped without a result: the waiters are cancelled.
  pub fn abandon(&mut self, key: &K) {
    self.waiting.remove(key);
  }

  pub fn is_loading(&self, key: &K) -> bool {
    self.waiting.contains_key(key)
  }
}

impl<K, V> Default for InFlight<K, V> {
  fn default() -> Self {
    Self {
      waiting: HashMap::new(),
    }
  }
}

impl<K: Eq + Hash + 'static, V: Clone + 'static> Drop for Leading<K, V> {
  fn drop(&mut self) {
    let Some(key) = self.maybe_key.take() else {
      return;
    };
    // The thread-local may already be gone while the page unloads; nobody waits then.
    self.table.try_with(|loads| loads.borrow_mut().abandon(&key)).ok();
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
  use std::cell::Cell;
  use std::pin::pin;
  use std::task::{Context, Poll, Waker};

  use futures_util::future::join;

  use super::*;

  thread_local! {
    static TEST_LOADS: RefCell<InFlight<u32, u32>> = RefCell::new(InFlight::default());
  }

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

  mod in_flight {
    use super::*;

    #[test]
    fn a_second_ask_waits_for_the_load_under_way() {
      let mut loads = InFlight::<&str, u32>::default();
      assert!(matches!(loads.join("a"), Joined::Lead));
      let Joined::Wait(mut second) = loads.join("a") else {
        panic!("the second ask must wait");
      };
      let Joined::Wait(mut third) = loads.join("a") else {
        panic!("the third ask must wait");
      };
      assert!(
        matches!(loads.join("b"), Joined::Lead),
        "another key loads by itself"
      );
      loads.finish(&"a", &7);
      assert_eq!((second.try_recv(), third.try_recv()), (Ok(Some(7)), Ok(Some(7))));
      assert!(!loads.is_loading(&"a"));
      assert!(
        matches!(loads.join("a"), Joined::Lead),
        "a finished load is not joined again"
      );
    }

    #[test]
    fn an_abandoned_load_cancels_its_waiters() {
      let mut loads = InFlight::<&str, u32>::default();
      assert!(matches!(loads.join("a"), Joined::Lead));
      let Joined::Wait(mut waiter) = loads.join("a") else {
        panic!("the second ask must wait");
      };
      loads.abandon(&"a");
      assert!(waiter.try_recv().is_err(), "cancelled, so it loads itself");
      assert!(matches!(loads.join("a"), Joined::Lead));
    }

    #[test]
    fn two_asks_for_the_same_file_load_it_once() {
      let loads = Cell::new(0);
      let (release, gate) = oneshot::channel::<u32>();
      let first = shared(&TEST_LOADS, 1, || async {
        loads.set(loads.get() + 1);
        gate.await.unwrap_or(0)
      });
      let second = shared(&TEST_LOADS, 1, || async {
        loads.set(loads.get() + 1);
        99
      });
      let mut both = pin!(join(first, second));
      let mut cx = Context::from_waker(Waker::noop());
      assert!(both.as_mut().poll(&mut cx).is_pending());
      release.send(7).expect("the load waits for it");
      assert_eq!(both.as_mut().poll(&mut cx), Poll::Ready((7, 7)));
      assert_eq!(loads.get(), 1, "the second ask waited for the first load");
      assert!(!TEST_LOADS.with_borrow(|loads| loads.is_loading(&1)));
    }

    #[test]
    fn a_waiter_loads_itself_when_the_leader_is_dropped() {
      let (_never, gate) = oneshot::channel::<u32>();
      let mut first = Box::pin(shared(&TEST_LOADS, 2, || async { gate.await.unwrap_or(0) }));
      let mut second = Box::pin(shared(&TEST_LOADS, 2, || async { 5 }));
      let mut cx = Context::from_waker(Waker::noop());
      assert!(first.as_mut().poll(&mut cx).is_pending());
      assert!(second.as_mut().poll(&mut cx).is_pending(), "waits for the first");
      drop(first);
      assert_eq!(second.as_mut().poll(&mut cx), Poll::Ready(5));
    }

    #[test]
    fn a_waiter_that_went_away_does_not_stop_the_others() {
      let mut loads = InFlight::<&str, u32>::default();
      assert!(matches!(loads.join("a"), Joined::Lead));
      let Joined::Wait(gone) = loads.join("a") else {
        panic!("must wait");
      };
      let Joined::Wait(mut waiting) = loads.join("a") else {
        panic!("must wait");
      };
      drop(gone);
      loads.finish(&"a", &1);
      assert_eq!(waiting.try_recv(), Ok(Some(1)));
    }
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
