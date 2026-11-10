//! The purge of the sealed SQLite store's write-ahead log on a connection and a thread of its own
//! (issue #1684, docs/e2ee.md §4.3.1, §9.5).
//!
//! A truncating checkpoint can only finish when no connection holds a snapshot that still reads
//! from the log. The app's read threads read through connections of their own (issue #1476), so it
//! used to fail most of the time, and each try ran on the writing connection, under its lock: every
//! write, and every read that needed the writer, waited for its syncs (1.0.29, #1657, #1678).
//!
//! Now the writer, the readers and the purge share a [`LogPurge`]. Every connection of the store in
//! this process reports when its transactions begin and end, so the purge knows when they are
//! idle, which is the earliest moment a truncation can succeed. The purge thread then truncates on
//! its own connection, which never waits for a lock: nothing else waits for it either, apart from
//! SQLite's own lock on the log for the moment of the truncation.
//!
//! The bound: a purge that is due runs at most [`LogPurgeLimits::max_delay`] late. When readers
//! never go idle for that long, new groups of writes are held back: with no new commit, every read
//! that begins after the log was written back reads from the database file alone (SQLite gives it
//! read mark 0), so the readers that pin the log only get fewer, and the truncation follows as soon
//! as the reads open at that moment end. Reads are never held back.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};

use super::StoreError;
use super::sql_group::sqlite_error;
use super::sqlite_log::{LOG_PRAGMAS, PURGE_INTERVAL, checkpoint_truncate_now};

/// The default of [`LogPurgeLimits::max_delay`]: how late a due purge may run while readers keep
/// the log. Short against the time somebody needs to take a phone and copy its files, long against
/// a read group's length, so writes are rarely held back.
pub const DEFAULT_MAX_PURGE_DELAY: Duration = Duration::from_secs(10);
/// After a try that could not truncate although no connection of this process had a transaction
/// open (another process reads: the iOS notification extension), the next try waits this long.
const IDLE_RETRY_PAUSE: Duration = Duration::from_millis(500);
/// The longest a group of writes is held back for an overdue purge. A safety valve, not the
/// normal case: a read that waits for a held-back write (through a channel the store can't see)
/// would otherwise never end. Reaching it is logged.
const MAX_WRITE_HOLD: Duration = Duration::from_secs(5);

/// The purge's timing (docs/e2ee.md §4.3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogPurgeLimits {
  /// The most often a debounced purge ([`super::LocalStore::purge_freed_soon`]) runs.
  pub debounce: Duration,
  /// How late a due purge may run while readers keep the log, before new groups of writes are
  /// held back for it.
  pub max_delay: Duration,
}

/// The purge thread of a writing connection; dropping it runs a last purge that is due and stops
/// the thread.
pub(crate) struct LogPurger {
  purge: Arc<LogPurge>,
  maybe_thread: Option<JoinHandle<()>>,
}

/// What the store's connections in this process share about the log: their open transactions and
/// the purge that is due.
pub(crate) struct LogPurge {
  schedule: Mutex<Schedule>,
  /// Raised whenever the schedule changes in a way a waiting thread may care about.
  changed: Condvar,
}

/// An open transaction of one of the store's connections, from [`LogPurge::transaction_began`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Ticket(u64);

/// The state of the purge, under [`LogPurge::schedule`].
struct Schedule {
  limits: LogPurgeLimits,
  /// Open transactions by ticket, in the order they began, with the thread that began them.
  open: BTreeMap<u64, ThreadId>,
  next_ticket: u64,
  maybe_due: Option<Due>,
  /// Asked for while a checkpoint ran: it may not cover writes committed after it began.
  maybe_due_next: Option<Due>,
  is_checkpointing: bool,
  /// Tickets below this were open when the last try began.
  attempt_ticket: u64,
  /// A transaction that was open when the last try began has ended since.
  has_ended_since_attempt: bool,
  /// When the last try that failed began.
  maybe_last_attempt: Option<Instant>,
  maybe_last_purge: Option<Instant>,
  /// The due purge is overdue: groups of writes wait for it ([`LogPurge::wait_before_writes`]).
  is_holding_writes: bool,
  is_closing: bool,
  has_tried_closing: bool,
}

/// When a due purge may run, and when it must.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Due {
  earliest: Instant,
  deadline: Instant,
}

/// What the purge thread does next.
#[derive(Debug, PartialEq, Eq)]
enum Step {
  Checkpoint,
  /// Until the schedule changes, or at most this long.
  Wait(Option<Duration>),
  Stop,
}

/// The purges of this process, by database path: a reading connection, opened after the writing
/// one, reports its transactions to the writer's purge.
static PURGES: Mutex<Vec<(String, Weak<LogPurge>)>> = Mutex::new(Vec::new());

impl Default for LogPurgeLimits {
  fn default() -> Self {
    Self {
      debounce: PURGE_INTERVAL,
      max_delay: DEFAULT_MAX_PURGE_DELAY,
    }
  }
}

impl LogPurger {
  /// Opens the purge's connection to the database at `path` (as the writer opened it, a `file:`
  /// URI included) and starts its thread. A purge is due at once: a crash may have left a log
  /// that still holds a deleted key's pages.
  pub(crate) fn start(path: &Path) -> Result<Self, StoreError> {
    let connection = open_connection(path)?;
    let purge = Arc::new(LogPurge::new(LogPurgeLimits::default()));
    purge.request_now();
    let thread_purge = Arc::clone(&purge);
    let thread = thread::Builder::new()
      .name("store-log-purge".into())
      .spawn(move || run(&thread_purge, &connection))
      .map_err(|error| StoreError::Database(format!("log purge thread: {error}")))?;
    register(path, &purge);
    Ok(Self {
      purge,
      maybe_thread: Some(thread),
    })
  }

  pub(crate) fn purge(&self) -> &Arc<LogPurge> {
    &self.purge
  }
}

impl Drop for LogPurger {
  fn drop(&mut self) {
    {
      let mut schedule = self.purge.lock();
      schedule.is_closing = true;
    }
    self.purge.changed.notify_all();
    if let Some(thread) = self.maybe_thread.take()
      && thread.join().is_err()
    {
      tracing::warn!("the log purge thread panicked");
    }
  }
}

impl LogPurge {
  fn new(limits: LogPurgeLimits) -> Self {
    Self {
      schedule: Mutex::new(Schedule::new(limits)),
      changed: Condvar::new(),
    }
  }

  /// The purge of the writer opened at `path` in this process, for a connection that only reads.
  pub(crate) fn of_database(path: &Path) -> Option<Arc<Self>> {
    let key = path.to_string_lossy();
    lock_ignoring_poison(&PURGES)
      .iter()
      .rev()
      .filter(|(registered, _)| *registered == key)
      .find_map(|(_, purge)| purge.upgrade())
  }

  pub(crate) fn set_limits(&self, limits: LogPurgeLimits) {
    self.lock().limits = limits;
    self.changed.notify_all();
  }

  /// A purge as soon as the readers allow (a deleted key).
  pub(crate) fn request_now(&self) {
    let now = Instant::now();
    let mut schedule = self.lock();
    let due = Due {
      earliest: now,
      deadline: now + schedule.limits.max_delay,
    };
    schedule.add_due(due);
    drop(schedule);
    self.changed.notify_all();
  }

  /// A purge at most every [`LogPurgeLimits::debounce`] (the MLS state, saved for every message).
  pub(crate) fn request_soon(&self) {
    let now = Instant::now();
    let mut schedule = self.lock();
    let earliest = schedule
      .maybe_last_purge
      .map_or(now, |last| (last + schedule.limits.debounce).max(now));
    let due = Due {
      earliest,
      deadline: earliest + schedule.limits.max_delay,
    };
    schedule.add_due(due);
    drop(schedule);
    self.changed.notify_all();
  }

  /// A connection began a transaction (a read snapshot or a group of writes).
  pub(crate) fn transaction_began(&self) -> Ticket {
    let mut schedule = self.lock();
    let ticket = schedule.next_ticket;
    schedule.next_ticket += 1;
    schedule.open.insert(ticket, thread::current().id());
    Ticket(ticket)
  }

  /// The transaction of `ticket` ended; the purge may run now.
  pub(crate) fn transaction_ended(&self, ticket: Ticket) {
    let mut schedule = self.lock();
    schedule.open.remove(&ticket.0);
    if ticket.0 < schedule.attempt_ticket {
      schedule.has_ended_since_attempt = true;
    }
    let may_run = schedule.maybe_due.is_some() && (schedule.open.is_empty() || schedule.is_holding_writes);
    drop(schedule);
    if may_run {
      self.changed.notify_all();
    }
  }

  /// Before a group of writes begins: waits while an overdue purge holds writes back, unless this
  /// thread has a transaction open itself (its read would keep the purge waiting for this write).
  pub(crate) fn wait_before_writes(&self) {
    let schedule = self.lock();
    if !schedule.is_holding_writes || schedule.is_held_by(thread::current().id()) {
      return;
    }
    let (schedule, waited) = self
      .changed
      .wait_timeout_while(schedule, MAX_WRITE_HOLD, |schedule| {
        schedule.is_holding_writes && !schedule.is_closing
      })
      .unwrap_or_else(|poisoned| poisoned.into_inner());
    if waited.timed_out() {
      tracing::warn!(
        open_transactions = schedule.open.len(),
        "a write waited the longest for the overdue log purge and goes ahead"
      );
    }
  }

  /// Waits until no purge is due, at most `max_wait`; whether none is.
  pub(crate) fn wait_until_purged(&self, max_wait: Duration) -> bool {
    let (schedule, _) = self
      .changed
      .wait_timeout_while(self.lock(), max_wait, |schedule| schedule.is_pending())
      .unwrap_or_else(|poisoned| poisoned.into_inner());
    !schedule.is_pending()
  }

  fn lock(&self) -> MutexGuard<'_, Schedule> {
    lock_ignoring_poison(&self.schedule)
  }
}

impl Schedule {
  fn new(limits: LogPurgeLimits) -> Self {
    Self {
      limits,
      open: BTreeMap::new(),
      next_ticket: 0,
      maybe_due: None,
      maybe_due_next: None,
      is_checkpointing: false,
      attempt_ticket: 0,
      has_ended_since_attempt: true,
      maybe_last_attempt: None,
      maybe_last_purge: None,
      is_holding_writes: false,
      is_closing: false,
      has_tried_closing: false,
    }
  }

  fn add_due(&mut self, due: Due) {
    let slot = if self.is_checkpointing {
      &mut self.maybe_due_next
    } else {
      &mut self.maybe_due
    };
    *slot = Some(slot.map_or(due, |earlier| earlier.merge(due)));
  }

  fn is_pending(&self) -> bool {
    self.maybe_due.is_some() || self.maybe_due_next.is_some() || self.is_checkpointing
  }

  fn is_held_by(&self, thread: ThreadId) -> bool {
    self.open.values().any(|owner| *owner == thread)
  }

  /// What the purge thread does at `now`.
  fn next_step(&mut self, now: Instant) -> Step {
    if self.is_closing {
      // One last try of a due purge; SQLite's own checkpoint when the last connection closes and
      // the next start of the app cover what it can't finish.
      if self.maybe_due.is_some() && !self.has_tried_closing {
        self.has_tried_closing = true;
        return Step::Checkpoint;
      }
      return Step::Stop;
    }
    let Some(due) = self.maybe_due else {
      return Step::Wait(None);
    };
    if now < due.earliest {
      return Step::Wait(Some(due.earliest - now));
    }
    if now >= due.deadline && !self.is_holding_writes {
      tracing::debug!("readers kept the write-ahead log past the purge's deadline; holding writes back");
      self.is_holding_writes = true;
    }
    if self.is_holding_writes {
      return self.next_step_holding(now);
    }
    if !self.open.is_empty() {
      return Step::Wait(Some(due.deadline - now));
    }
    match self.retry_pause_left(now) {
      Some(left) => Step::Wait(Some(left.min(due.deadline - now))),
      None => Step::Checkpoint,
    }
  }

  /// With writes held back: a try is worth it once every transaction that was open at the last
  /// try has ended, since reads begun after the log was written back don't keep it.
  fn next_step_holding(&self, now: Instant) -> Step {
    let is_older_open = self
      .open
      .keys()
      .next()
      .is_some_and(|oldest| *oldest < self.attempt_ticket);
    if is_older_open {
      return Step::Wait(None);
    }
    if self.has_ended_since_attempt {
      return Step::Checkpoint;
    }
    // Nothing of this process changed since the last try (another process reads).
    match self.retry_pause_left(now) {
      Some(left) => Step::Wait(Some(left)),
      None => Step::Checkpoint,
    }
  }

  fn retry_pause_left(&self, now: Instant) -> Option<Duration> {
    let next = self.maybe_last_attempt? + IDLE_RETRY_PAUSE;
    (now < next).then(|| next - now)
  }

  fn begin_attempt(&mut self, now: Instant) {
    self.is_checkpointing = true;
    self.attempt_ticket = self.next_ticket;
    self.has_ended_since_attempt = false;
    self.maybe_last_attempt = Some(now);
  }

  fn end_attempt(&mut self, outcome: &Result<bool, StoreError>, started: Instant) {
    self.is_checkpointing = false;
    let maybe_next = self.maybe_due_next.take();
    match outcome {
      Ok(true) => {
        self.maybe_due = maybe_next;
        self.maybe_last_purge = Some(started);
        // The pause is for tries that failed: the next purge runs as soon as the readers allow.
        self.maybe_last_attempt = None;
        self.is_holding_writes = false;
      },
      Ok(false) => {
        tracing::debug!("the write-ahead log could not be truncated yet; trying again when readers allow");
        self.maybe_due = merge(self.maybe_due, maybe_next);
      },
      Err(error) => {
        tracing::warn!(%error, "could not truncate the write-ahead log; trying again later");
        self.maybe_due = merge(self.maybe_due, maybe_next);
        // A failing disk must not hold writes back for good; the next step holds them again once
        // the pause is over.
        self.is_holding_writes = false;
        self.has_ended_since_attempt = false;
      },
    }
  }
}

impl Due {
  fn merge(self, other: Self) -> Self {
    Self {
      earliest: self.earliest.min(other.earliest),
      deadline: self.deadline.min(other.deadline),
    }
  }
}

fn merge(maybe_due: Option<Due>, maybe_other: Option<Due>) -> Option<Due> {
  match (maybe_due, maybe_other) {
    (Some(due), Some(other)) => Some(due.merge(other)),
    (maybe_due, maybe_other) => maybe_due.or(maybe_other),
  }
}

/// The purge thread: runs the checkpoint whenever [`Schedule::next_step`] says so, until closed.
fn run(purge: &LogPurge, connection: &Connection) {
  let mut schedule = purge.lock();
  loop {
    schedule = match schedule.next_step(Instant::now()) {
      Step::Stop => return,
      Step::Wait(None) => purge
        .changed
        .wait(schedule)
        .unwrap_or_else(|poisoned| poisoned.into_inner()),
      Step::Wait(Some(duration)) => {
        purge
          .changed
          .wait_timeout(schedule, duration)
          .unwrap_or_else(|poisoned| poisoned.into_inner())
          .0
      },
      Step::Checkpoint => {
        let started = Instant::now();
        schedule.begin_attempt(started);
        drop(schedule);
        let outcome = checkpoint_truncate_now(connection);
        let mut schedule = purge.lock();
        schedule.end_attempt(&outcome, started);
        purge.changed.notify_all();
        schedule
      },
    };
  }
}

/// The purge's own connection: it never waits for a lock, so a checkpoint that can't finish
/// returns at once instead of holding anything up.
fn open_connection(path: &Path) -> Result<Connection, StoreError> {
  let connection = Connection::open_with_flags(
    path,
    OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX | OpenFlags::SQLITE_OPEN_URI,
  )
  .map_err(sqlite_error)?;
  connection.execute_batch(LOG_PRAGMAS).map_err(sqlite_error)?;
  connection.busy_timeout(Duration::ZERO).map_err(sqlite_error)?;
  Ok(connection)
}

fn register(path: &Path, purge: &Arc<LogPurge>) {
  let mut purges = lock_ignoring_poison(&PURGES);
  purges.retain(|(_, registered)| registered.strong_count() > 0);
  purges.push((path.to_string_lossy().into_owned(), Arc::downgrade(purge)));
}

fn lock_ignoring_poison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
  // A poisoned lock only means another thread panicked; the schedule stays consistent.
  mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
  use super::*;

  const LIMITS: LogPurgeLimits = LogPurgeLimits {
    debounce: Duration::from_secs(30),
    max_delay: Duration::from_secs(10),
  };

  mod idle_readers {
    use super::*;

    #[test]
    fn a_due_purge_waits_for_open_transactions_and_runs_once_they_end() {
      let (mut schedule, now) = due_now();
      schedule.open.insert(0, thread::current().id());
      assert!(matches!(schedule.next_step(now), Step::Wait(Some(_))));
      schedule.open.clear();
      assert_eq!(schedule.next_step(now), Step::Checkpoint);
    }

    #[test]
    fn a_try_that_failed_while_idle_waits_before_the_next() {
      let (mut schedule, now) = due_now();
      schedule.begin_attempt(now);
      schedule.end_attempt(&Ok(false), now);
      assert!(matches!(schedule.next_step(now), Step::Wait(Some(_))));
      assert_eq!(schedule.next_step(now + IDLE_RETRY_PAUSE), Step::Checkpoint);
    }

    #[test]
    fn a_purge_after_one_that_finished_runs_at_once() {
      let (mut schedule, now) = due_now();
      schedule.begin_attempt(now);
      schedule.end_attempt(&Ok(true), now);
      schedule.add_due(Due {
        earliest: now,
        deadline: now + LIMITS.max_delay,
      });
      assert_eq!(schedule.next_step(now), Step::Checkpoint);
    }

    #[test]
    fn a_purge_asked_for_during_a_checkpoint_stays_due_after_it() {
      let (mut schedule, now) = due_now();
      schedule.begin_attempt(now);
      schedule.add_due(Due {
        earliest: now,
        deadline: now + LIMITS.max_delay,
      });
      schedule.end_attempt(&Ok(true), now);
      assert!(
        schedule.maybe_due.is_some(),
        "its writes may be after the checkpoint"
      );
    }

    #[test]
    fn a_debounced_purge_waits_for_the_interval() {
      let purge = LogPurge::new(LIMITS);
      purge.lock().maybe_last_purge = Some(Instant::now());
      purge.request_soon();
      let mut schedule = purge.lock();
      let due = schedule.maybe_due.unwrap();
      assert!(due.earliest > Instant::now() + LIMITS.debounce / 2);
      assert!(matches!(schedule.next_step(Instant::now()), Step::Wait(Some(_))));
    }
  }

  mod deadline {
    use super::*;

    #[test]
    fn an_overdue_purge_holds_writes_and_runs_beside_open_reads() {
      let (mut schedule, now) = due_now();
      schedule.open.insert(0, thread::current().id());
      let late = now + LIMITS.max_delay;
      assert_eq!(
        schedule.next_step(late),
        Step::Checkpoint,
        "it writes the log back while the read goes on"
      );
      assert!(schedule.is_holding_writes);
    }

    #[test]
    fn after_a_try_it_waits_for_the_reads_that_were_open_then() {
      let (mut schedule, now) = due_now();
      let late = now + LIMITS.max_delay;
      schedule.open.insert(0, thread::current().id());
      schedule.next_ticket = 1;
      schedule.next_step(late);
      schedule.begin_attempt(late);
      schedule.end_attempt(&Ok(false), late);
      // A read begun after the try, while the older one still runs.
      schedule.next_ticket = 2;
      schedule.open.insert(1, thread::current().id());
      assert_eq!(schedule.next_step(late), Step::Wait(None));
      schedule.open.remove(&0);
      schedule.has_ended_since_attempt = true;
      assert_eq!(
        schedule.next_step(late),
        Step::Checkpoint,
        "a read begun after the log was written back doesn't keep it"
      );
    }

    #[test]
    fn a_finished_purge_lets_writes_go() {
      let (mut schedule, now) = due_now();
      let late = now + LIMITS.max_delay;
      schedule.next_step(late);
      schedule.begin_attempt(late);
      schedule.end_attempt(&Ok(true), late);
      assert!(!schedule.is_holding_writes);
      assert!(!schedule.is_pending());
    }

    #[test]
    fn a_failing_disk_does_not_hold_writes_for_good() {
      let (mut schedule, now) = due_now();
      let late = now + LIMITS.max_delay;
      schedule.next_step(late);
      schedule.begin_attempt(late);
      schedule.end_attempt(&Err(StoreError::Database("disk".into())), late);
      assert!(!schedule.is_holding_writes);
      assert!(schedule.is_pending());
    }

    #[test]
    fn a_thread_with_an_open_read_is_not_held() {
      let purge = LogPurge::new(LIMITS);
      let ticket = purge.transaction_began();
      purge.lock().is_holding_writes = true;
      let started = Instant::now();
      purge.wait_before_writes();
      assert!(started.elapsed() < MAX_WRITE_HOLD / 2, "it would wait for itself");
      purge.transaction_ended(ticket);
    }
  }

  mod closing {
    use super::*;

    #[test]
    fn closing_tries_a_due_purge_once_and_stops() {
      let (mut schedule, now) = due_now();
      schedule.open.insert(0, thread::current().id());
      schedule.is_closing = true;
      assert_eq!(schedule.next_step(now), Step::Checkpoint);
      schedule.begin_attempt(now);
      schedule.end_attempt(&Ok(false), now);
      assert_eq!(schedule.next_step(now), Step::Stop);
    }
  }

  fn due_now() -> (Schedule, Instant) {
    let now = Instant::now();
    let mut schedule = Schedule::new(LIMITS);
    schedule.add_due(Due {
      earliest: now,
      deadline: now + LIMITS.max_delay,
    });
    (schedule, now)
  }
}
