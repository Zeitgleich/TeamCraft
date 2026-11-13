//! The read thread: what the app reads from the local store, and the local half of what it does
//! there (an outbox write, a read marker), doesn't wait for the engine thread, whatever that is busy
//! with: a job on the network, a catch-up, the client opening. The store takes a thread's group of
//! writes as a whole ([`teamcraft_core::store::SealedSqliteStore`]), so a read sees committed records
//! and a write here stays out of the sync loop's group. There is no client and no MLS state on this
//! thread: one core owns the store as before (e2ee.md §9.4), and the notification extension's path
//! (§7.2) stays on the engine thread.
//!
//! There are two read threads, each on a read connection of its own (issue #1476): the open chat's
//! queries ([`ReadQueue::query`]) don't wait behind the chat list, a snapshot or the share sheet's
//! targets on the main one (issue #1583), and neither waits for the engine's groups of writes.
//!
//! An action's local half runs on a third thread (issue #1756): it writes, so it waits for the
//! store's write lock while the engine holds a group of writes, and the reads queued behind it on
//! the main read thread waited with it, for seconds during a catch-up.
//!
//! The chat list a start asks for first is the one the last pass kept (issue #1758): one record,
//! where a pass reads a dozen per conversation (2.6 s for 83 on Android 1.0.30, before the list
//! showed). The main thread runs a pass once nothing else waits, and tells the app if the list
//! changed.
//!
//! A pass and the chat previews run in slices ([`Slice`], issue #1819): each slice is a job of its
//! own and queues the next behind what waits by then, so a read the app queued meanwhile waits one
//! slice, not the whole pass (1.7–2.7 s on phones in 1.0.32).

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::mpsc;
use std::sync::mpsc::TryRecvError;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use teamcraft_client_api::backend::{ChatBackend as _, SearchBackend as _};
use teamcraft_client_api::entry_rules::{channel_leave, space_leave};
use teamcraft_client_api::model::{
  FirstSyncStage, Message as MessageView, MessageKind, SpaceRole, SpaceRoster, TimestampMs, is_pending,
};
use teamcraft_client_api::search::parse_query;
use teamcraft_core::TypingState;
use teamcraft_core::chat_list_pass::{ListPass, SliceLimit};
use teamcraft_core::client::{CatchUpFocus, LinkRequestList};
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_core::notified;
use teamcraft_core::notify_rules::load_mention_preferences;
use teamcraft_core::pending_work::PendingWork;
use teamcraft_core::prefetch::load_segment_prefetch;
use teamcraft_core::push_guard;
use teamcraft_core::push_preview::{device_push_content, push_content_mode};
use teamcraft_core::records;
use teamcraft_core::store::{Table, read_together, try_write_group, write_group};
use teamcraft_core::transfers::Transfers;
use teamcraft_core::ui_backend::{CoreChatBackend, EntryNeed};
use teamcraft_core::ui_snapshot::SnapshotCache;
use teamcraft_proto::{ConversationId, SpaceId, ThreadId, UserId};
use tokio::sync::{mpsc as tokio_mpsc, oneshot};

use crate::EventListener;
use crate::engine::{Job, Store, direct_peer, parse_id};
use crate::error::TeamCraftError;
use crate::logging::{self, JobClock};
use crate::perf_jobs::{self, PerfJobKind};
use crate::types::LinkRequestItem;
use crate::types::{
  AccountState, ChatPreviewItem, ConversationItem, FirstSyncItem, FirstSyncStageItem, GroupMentionReachItem,
  KeyCatchUpItem, LeaveChoice, MentionSettings, MentionableGroupItem, MessageItem,
  NotificationContentSettings, NotifiedConversation, Presence, PushDiagnosticsInfo, SearchHitItem,
  SegmentPrefetchMode, SpaceMenuItem, share_targets,
};

/// How many of a conversation's newest rows a chat list preview looks through for a message people
/// wrote, before it settles for the newest system line; a second look goes this many times further.
const PREVIEW_ROWS: usize = 8;
const PREVIEW_FURTHER: usize = 8;
/// Where a pass keeps the chat list for the next start: the account's table, sealed like every
/// record (issue #1758).
const KEPT_CHAT_LIST_KEY: &str = "chat_list";
/// How often at most a pass keeps a list that changed: it is only what the next start shows before
/// its own pass, and a catch-up changes the list several times a second.
const KEEP_CHAT_LIST_EVERY: Duration = Duration::from_secs(10);

/// Work for the read thread.
type ReadJob = Box<dyn FnOnce(&mut Reads) + Send>;

/// The rest of a sliced read ([`Slice::More`]).
pub(crate) type SliceJob<R> = Box<dyn FnOnce(&mut Reads) -> Slice<R> + Send>;

/// What one slice of a read that hands the thread back between slices did (issue #1819).
pub(crate) enum Slice<R> {
  Done(Result<R, TeamCraftError>),
  /// More to do: queued again behind the reads that wait by then.
  More(SliceJob<R>),
}

/// An action's answer: the engine ran what it queued for the client.
pub(crate) type Dispatched = oneshot::Receiver<Result<(), TeamCraftError>>;

/// Where a read's result goes.
type Reply<R> = oneshot::Sender<Result<R, TeamCraftError>>;

/// The read threads' queues. The threads end once every handle on the queues is gone.
#[derive(Clone)]
pub(crate) struct ReadQueue {
  jobs: mpsc::Sender<ReadJob>,
  /// The open chat's queries: a thread of their own (issue #1583).
  query_jobs: mpsc::Sender<ReadJob>,
  /// Actions' local halves, which wait for the write lock: a thread of their own (issue #1756).
  act_jobs: mpsc::Sender<ReadJob>,
  /// The chat list's read, shared by everyone who asks while one waits to start (issue #1466).
  conversations: Arc<SharedRead<Vec<ConversationItem>>>,
}

/// The store as each read thread sees it ([`teamcraft_core::store::LaneStore`]).
pub(crate) struct ReadLanes {
  /// Everything but the open chat's queries.
  pub(crate) main: Store,
  /// The open chat's queries.
  pub(crate) query: Store,
  /// Actions' local halves: they read and write through the writing connection, in a group.
  pub(crate) act: Store,
}

/// Which read thread runs a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lane {
  Main,
  Query,
  Act,
}

/// What a read does when its caller is gone before it starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WhenCallerGone {
  /// A read: nobody wants the answer, so it isn't worth the thread's time (a cancelled coroutine,
  /// a screen that moved on; issue #1466).
  Skip,
  /// An action's local half: what the user did happens, whether or not anyone waits for it.
  Run,
}

/// A read that callers share while it waits on the queue: asking again before it started adds a
/// caller to it instead of a second pass. Once it starts, the next ask queues a new pass, so nobody
/// gets an answer older than their question.
struct SharedRead<R> {
  /// The callers of the pass that is queued and not started yet; `None` while none is.
  maybe_waiting: Mutex<Option<Vec<Reply<R>>>>,
}

/// Who waits for a sliced read.
enum SliceCallers<R> {
  /// The callers of a shared read, taken when its first slice starts.
  Shared(Arc<SharedRead<R>>),
  /// Callers known already: a read's own, or a shared read's after its first slice.
  Given(Vec<Reply<R>>),
}

/// Where a [`SharedRead`] put a new caller.
enum Joined {
  /// No pass was waiting: the caller queues one.
  First,
  /// A pass waits already and answers this caller too.
  Waiting,
}

/// The read thread's state.
pub(crate) struct Reads {
  pub(crate) store: Store,
  listener: Arc<dyn EventListener>,
  catch_up_focus: CatchUpFocus,
  /// The engine thread's queue, for what an action leaves for the client. Weak: the engine ends
  /// when the app drops its handle, whatever this thread holds.
  engine: tokio_mpsc::WeakUnboundedSender<Job>,
  maybe_account: Option<ReadAccount>,
  /// The client's open link requests, shared with it: reading them needs no engine job.
  link_requests: LinkRequestList,
  /// Set by [`ReadQueue::close`]: the thread ends after this job and answers here.
  maybe_closing: Option<mpsc::Sender<()>>,
}

/// The enrolled account as the read thread has it.
pub(crate) struct ReadAccount {
  backend: CoreChatBackend<Store>,
  /// What a snapshot built last: a change costs what it touched, not the workspace (issue #246).
  pub(crate) snapshots: SnapshotCache,
  pub(crate) identity_fingerprint: String,
  /// The chat list since the account was known on this thread.
  chat_list: ChatListAtStart,
  /// The chat list the store keeps, as far as this thread knows.
  maybe_kept: Option<Vec<ConversationItem>>,
  /// When this thread kept the chat list last.
  maybe_kept_at: Option<Instant>,
  /// The pass after a start that showed the kept list, while it runs a slice at a time between
  /// other reads ([`Reads::when_idle`]).
  maybe_idle_pass: Option<ChatListRun>,
  /// The slices of a chat list pass ([`SliceLimit::default`]; tests make them smaller).
  slice_limit: SliceLimit,
}

/// A chat list pass under way: the core's pass and the app's items of the entries built so far.
pub(crate) struct ChatListRun {
  pass: ListPass,
  /// Each space's name and picture version, for its conversations' items.
  spaces: BTreeMap<SpaceId, (String, Option<String>)>,
  /// The items so far with the space each belongs to, in the store's order.
  items: Vec<(Option<SpaceId>, ConversationItem)>,
}

/// The chat list since the start (issue #1758).
#[derive(Clone, Debug, PartialEq)]
enum ChatListAtStart {
  /// Nobody asked yet: the first ask gets the list the last pass kept, if there is one.
  NotAsked,
  /// The kept list was handed out: a pass runs once the thread has nothing else to do.
  Kept(Vec<ConversationItem>),
  /// A pass ran.
  Passed,
}

/// The chat list as a pass kept it.
#[derive(serde::Serialize, serde::Deserialize)]
struct KeptChatList {
  items: Vec<ConversationItem>,
}

impl ReadAccount {
  pub(crate) fn parts(&mut self) -> (&CoreChatBackend<Store>, &mut SnapshotCache) {
    (&self.backend, &mut self.snapshots)
  }
}

impl ReadQueue {
  pub(crate) fn start(
    lanes: ReadLanes,
    listener: Arc<dyn EventListener>,
    catch_up_focus: CatchUpFocus,
    engine: tokio_mpsc::WeakUnboundedSender<Job>,
  ) -> Result<Self, TeamCraftError> {
    let jobs = spawn_lane(
      "teamcraft-reads",
      lanes.main,
      listener.clone(),
      catch_up_focus.clone(),
      engine.clone(),
    )?;
    let query_jobs = spawn_lane(
      "teamcraft-queries",
      lanes.query,
      listener.clone(),
      catch_up_focus.clone(),
      engine.clone(),
    )?;
    let act_jobs = spawn_lane("teamcraft-actions", lanes.act, listener, catch_up_focus, engine)?;
    Ok(Self {
      jobs,
      query_jobs,
      act_jobs,
      conversations: Arc::new(SharedRead::new()),
    })
  }

  /// Runs `job` on the read thread and waits for its result. Skipped when the caller gave up
  /// before it started.
  pub(crate) async fn read<R, F>(&self, job: F) -> Result<R, TeamCraftError>
  where
    R: Send + 'static,
    F: FnOnce(&mut Reads) -> Result<R, TeamCraftError> + Send + 'static,
  {
    let answer = self.queue(Lane::Main, WhenCallerGone::Skip, job)?;
    answer.await.map_err(|_| TeamCraftError::EngineStopped)?
  }

  /// [`Self::read`] on the open chat's thread: what opening a chat or scrolling it reads, which
  /// doesn't wait behind the other reads (issue #1583).
  pub(crate) async fn query<R, F>(&self, job: F) -> Result<R, TeamCraftError>
  where
    R: Send + 'static,
    F: FnOnce(&mut Reads) -> Result<R, TeamCraftError> + Send + 'static,
  {
    let answer = self.queue(Lane::Query, WhenCallerGone::Skip, job)?;
    answer.await.map_err(|_| TeamCraftError::EngineStopped)?
  }

  /// The chat list ([`Reads::conversations`]): one pass for everyone who asks while one waits.
  /// Each change notification makes several screens ask at once, and a pass reads every
  /// conversation, so separate passes kept the thread busy and opening a chat waited (#1466).
  pub(crate) async fn conversations(&self) -> Result<Vec<ConversationItem>, TeamCraftError> {
    let answer = self.share(&self.conversations, |reads: &mut Reads| reads.chat_list())?;
    answer.await.map_err(|_| TeamCraftError::EngineStopped)?
  }

  /// [`Self::read`] for a read that runs in slices: between two, the reads queued meanwhile run
  /// (issue #1819). Skipped from the next slice on once the caller is gone.
  pub(crate) async fn read_sliced<R, F>(&self, job: F) -> Result<R, TeamCraftError>
  where
    R: Clone + Send + 'static,
    F: FnOnce(&mut Reads) -> Slice<R> + Send + 'static,
  {
    let (reply, answer) = oneshot::channel();
    self.queue_slice(
      std::any::type_name::<F>(),
      SliceCallers::Given(vec![reply]),
      Box::new(job),
    )?;
    answer.await.map_err(|_| TeamCraftError::EngineStopped)?
  }

  /// Runs an action's local half on the actions' thread and hands what it queued to the engine, in
  /// the order of the actions; answers once the engine took it, like before this thread existed.
  /// It waits there for the store's write lock, and the reads don't wait with it (issue #1756).
  pub(crate) async fn act<R, F>(&self, job: F) -> Result<R, TeamCraftError>
  where
    R: Send + 'static,
    F: FnOnce(&mut Reads) -> Result<R, TeamCraftError> + Send + 'static,
  {
    let answer = self.queue(Lane::Act, WhenCallerGone::Run, move |reads| {
      // One group of writes: what the action reads comes from the writing connection and nothing
      // else commits in between, so a change it makes to a record can't undo the engine's.
      let store = reads.store.clone();
      let value = write_group(&store, || job(reads))?;
      Ok((value, reads.dispatch()?))
    })?;
    let (value, dispatched) = answer.await.map_err(|_| TeamCraftError::EngineStopped)??;
    dispatched.await.map_err(|_| TeamCraftError::EngineStopped)??;
    Ok(value)
  }

  /// Ends the thread and returns once it let go of the store. Reads from then on get
  /// [`TeamCraftError::EngineStopped`].
  pub(crate) fn close(&self) {
    let (closed, is_closed) = mpsc::channel();
    let query_closed = closed.clone();
    let act_closed = closed.clone();
    self.post(move |reads| reads.maybe_closing = Some(closed));
    self.post_to(Lane::Query, move |reads| reads.maybe_closing = Some(query_closed));
    self.post_to(Lane::Act, move |reads| reads.maybe_closing = Some(act_closed));
    // Ends once every thread answered or ended: an error means every sender is gone, which is
    // what this waits for.
    while is_closed.recv().is_ok() {}
  }

  /// The account the store holds: from now on the app can read. A new account starts over.
  pub(crate) fn know_account(&self, user: UserId, identity_fingerprint: String) {
    self.post_all(move |reads| {
      let is_known = reads
        .maybe_account
        .as_ref()
        .is_some_and(|account| account.backend.me().id == user);
      if let (true, Some(account)) = (is_known, reads.maybe_account.as_mut()) {
        account.identity_fingerprint = identity_fingerprint.clone();
        return;
      }
      // The apps show no message count or last activity of the snapshot: their counts never read
      // history older than the read position (issue #1466).
      let backend = CoreChatBackend::new(reads.store.clone(), user, Box::new(SystemClock))
        .with_entry_need(EntryNeed::Unread);
      reads.maybe_account = Some(ReadAccount {
        backend,
        snapshots: SnapshotCache::new(u64::try_from(SystemClock.now_ms()).unwrap_or_default()),
        identity_fingerprint: identity_fingerprint.clone(),
        chat_list: ChatListAtStart::NotAsked,
        maybe_kept: None,
        maybe_kept_at: None,
        maybe_idle_pass: None,
        slice_limit: SliceLimit::default(),
      });
    });
  }

  /// Typing and transfers come from the client once it is open, and from its successor when it is
  /// reopened; what the backend keeps stays.
  pub(crate) fn follow_client(&self, typing: TypingState, transfers: Transfers) {
    self.post_all(move |reads| {
      if let Some(account) = reads.maybe_account.take() {
        reads.maybe_account = Some(ReadAccount {
          backend: account
            .backend
            .with_typing(typing.clone())
            .with_transfers(transfers.clone()),
          ..account
        });
      }
    });
  }

  /// The open link requests come from the client's shared list (issue #1900).
  pub(crate) fn follow_link_requests(&self, list: LinkRequestList) {
    self.post_all(move |reads| reads.link_requests = list.clone());
  }

  /// Queues `job` and answers where its result will arrive; queued at once, before anyone awaits.
  fn queue<R, F>(
    &self,
    lane: Lane,
    when_gone: WhenCallerGone,
    job: F,
  ) -> Result<oneshot::Receiver<Result<R, TeamCraftError>>, TeamCraftError>
  where
    R: Send + 'static,
    F: FnOnce(&mut Reads) -> Result<R, TeamCraftError> + Send + 'static,
  {
    let (reply, answer) = oneshot::channel();
    let queued = Instant::now();
    let timing = match lane {
      Lane::Query => perf_jobs::queued_for_query(std::any::type_name::<F>()),
      Lane::Act => perf_jobs::queued_for_action(std::any::type_name::<F>()),
      Lane::Main => perf_jobs::queued(PerfJobKind::ReadQueue, std::any::type_name::<F>()),
    };
    let boxed: ReadJob = Box::new(move |reads| {
      if when_gone == WhenCallerGone::Skip && reply.is_closed() {
        timing.skip();
        return;
      }
      let span = timing.span();
      let running = timing.start();
      let clock = JobClock::start();
      let result = span.in_scope(|| reads.together(job));
      logging::slow_read(std::any::type_name::<F>(), clock.started() - queued, clock.ran());
      running.done();
      // The caller may have given up waiting meanwhile (cancelled coroutine); nothing to tell it.
      let _ = reply.send(result);
    });
    self.send_to(lane, boxed)?;
    Ok(answer)
  }

  /// Joins `shared`'s waiting pass, or queues one running `job`, and answers where the result will
  /// arrive. The pass runs once for all its callers that still wait when it starts, and not at all
  /// when none do.
  fn share<R, F>(
    &self,
    shared: &Arc<SharedRead<R>>,
    job: F,
  ) -> Result<oneshot::Receiver<Result<R, TeamCraftError>>, TeamCraftError>
  where
    R: Clone + Send + 'static,
    F: FnOnce(&mut Reads) -> Slice<R> + Send + 'static,
  {
    let (reply, answer) = oneshot::channel();
    if let Joined::Waiting = shared.join(reply) {
      return Ok(answer);
    }
    let callers = SliceCallers::Shared(shared.clone());
    if let Err(error) = self.queue_slice(std::any::type_name::<F>(), callers, Box::new(job)) {
      // The pass will never run: its callers (this one, and any that joined meanwhile) hear so.
      for caller in shared.start() {
        let _ = caller.send(Err(TeamCraftError::EngineStopped));
      }
      return Err(error);
    }
    Ok(answer)
  }

  /// Queues a slice of a read for `callers`. A slice that isn't the last queues the next behind
  /// the reads that wait by then (issue #1819); once no caller waits, the rest is skipped.
  fn queue_slice<R>(
    &self,
    name: &'static str,
    callers: SliceCallers<R>,
    job: SliceJob<R>,
  ) -> Result<(), TeamCraftError>
  where
    R: Clone + Send + 'static,
  {
    let queued = Instant::now();
    let timing = perf_jobs::queued(PerfJobKind::ReadQueue, name);
    let queue = self.clone();
    let boxed: ReadJob = Box::new(move |reads| {
      let callers = callers.waiting();
      if callers.is_empty() {
        timing.skip();
        return;
      }
      let span = timing.span();
      let running = timing.start();
      let clock = JobClock::start();
      let slice = span.in_scope(|| reads.slice(job));
      logging::slow_read(name, clock.started() - queued, clock.ran());
      running.done();
      match slice {
        Slice::Done(result) => {
          for caller in callers {
            // A caller may have given up waiting meanwhile; nothing to tell it then.
            let _ = caller.send(result.clone());
          }
        },
        // A queue that is closing drops the callers with the job: they hear the engine stopped.
        Slice::More(next) => {
          let _ = queue.queue_slice(name, SliceCallers::Given(callers), next);
        },
      }
    });
    self.send_to(Lane::Main, boxed)
  }

  fn post(&self, job: impl FnOnce(&mut Reads) + Send + 'static) {
    self.post_to(Lane::Main, job);
  }

  fn post_to(&self, lane: Lane, job: impl FnOnce(&mut Reads) + Send + 'static) {
    // Refused only by a thread that ended, which has nobody left to tell.
    let _ = self.lane(lane).send(Box::new(job));
  }

  /// Runs `job` on every thread: what each keeps of the account.
  fn post_all(&self, job: impl Fn(&mut Reads) + Clone + Send + 'static) {
    self.post_to(Lane::Query, job.clone());
    self.post_to(Lane::Act, job.clone());
    self.post_to(Lane::Main, job);
  }

  fn send_to(&self, lane: Lane, job: ReadJob) -> Result<(), TeamCraftError> {
    self
      .lane(lane)
      .send(job)
      .map_err(|_| TeamCraftError::EngineStopped)
  }

  fn lane(&self, lane: Lane) -> &mpsc::Sender<ReadJob> {
    match lane {
      Lane::Main => &self.jobs,
      Lane::Query => &self.query_jobs,
      Lane::Act => &self.act_jobs,
    }
  }
}

/// Whether a slice that started at `started` ran its time ([`SliceLimit::max_ms`]).
fn is_slice_over(started: Instant, limit: SliceLimit) -> bool {
  let max = Duration::from_millis(u64::try_from(limit.max_ms).unwrap_or(0));
  started.elapsed() >= max
}

/// The chat list the last pass kept; `None` without one, or one this version doesn't read.
fn kept_chat_list(store: &Store) -> Option<Vec<ConversationItem>> {
  match records::get_json::<KeptChatList>(store, Table::Account, KEPT_CHAT_LIST_KEY) {
    Ok(maybe_kept) => maybe_kept.map(|kept| kept.items),
    Err(error) => {
      tracing::debug!(%error, "the kept chat list does not read; a pass builds it");
      None
    },
  }
}

/// Starts a read thread on `store` and answers its queue.
fn spawn_lane(
  name: &str,
  store: Store,
  listener: Arc<dyn EventListener>,
  catch_up_focus: CatchUpFocus,
  engine: tokio_mpsc::WeakUnboundedSender<Job>,
) -> Result<mpsc::Sender<ReadJob>, TeamCraftError> {
  let (queue, jobs) = mpsc::channel::<ReadJob>();
  std::thread::Builder::new()
    .name(name.into())
    .stack_size(crate::engine::THREAD_STACK_BYTES)
    .spawn(move || {
      let mut reads = Reads {
        store,
        listener,
        catch_up_focus,
        engine,
        maybe_account: None,
        link_requests: LinkRequestList::default(),
        maybe_closing: None,
      };
      loop {
        let job = match jobs.try_recv() {
          Ok(job) => job,
          Err(TryRecvError::Empty) if reads.is_idle_work_due() => {
            // Like a job's, a panic here leaves the thread working.
            let _ = std::panic::catch_unwind(AssertUnwindSafe(|| reads.when_idle()));
            continue;
          },
          Err(TryRecvError::Empty) => match jobs.recv() {
            Ok(job) => job,
            Err(_) => break,
          },
          Err(TryRecvError::Disconnected) => break,
        };
        // The panic log names it (logging.rs), and the caller's answer is gone: the next read
        // still works instead of every read of a dead thread failing.
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| job(&mut reads)));
        if reads.maybe_closing.is_some() {
          break;
        }
      }
      let maybe_closing = reads.maybe_closing.take();
      drop(reads);
      if let Some(closed) = maybe_closing {
        // Whoever waits may be gone; the store is closed either way.
        let _ = closed.send(());
      }
    })
    .map_err(|error| TeamCraftError::Core(error.to_string()))?;
  Ok(queue)
}

impl ChatListRun {
  /// The items: spaces in ID order, then the direct messages, the store's order within each.
  fn into_items(self) -> Vec<ConversationItem> {
    let mut items = self.items;
    items.sort_by_key(|(maybe_space, _)| (maybe_space.is_none(), *maybe_space));
    items.into_iter().map(|(_, item)| item).collect()
  }
}

impl<R> SliceCallers<R> {
  /// The callers that still wait.
  fn waiting(self) -> Vec<Reply<R>> {
    match self {
      Self::Shared(shared) => shared.start(),
      Self::Given(mut callers) => {
        callers.retain(|caller| !caller.is_closed());
        callers
      },
    }
  }
}

impl<R> SharedRead<R> {
  fn new() -> Self {
    Self {
      maybe_waiting: Mutex::new(None),
    }
  }

  /// Adds a caller to the waiting pass, or starts the list of a new one.
  fn join(&self, reply: Reply<R>) -> Joined {
    let mut maybe_waiting = self.maybe_waiting.lock().unwrap_or_else(PoisonError::into_inner);
    match maybe_waiting.as_mut() {
      Some(callers) => {
        callers.push(reply);
        Joined::Waiting
      },
      None => {
        *maybe_waiting = Some(vec![reply]);
        Joined::First
      },
    }
  }

  /// The pass starts: its callers that still wait. Who asks from now on queues the next pass.
  fn start(&self) -> Vec<Reply<R>> {
    let mut callers = self
      .maybe_waiting
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .take()
      .unwrap_or_default();
    callers.retain(|caller| !caller.is_closed());
    callers
  }
}

impl Reads {
  /// Runs `job` as one look at the store ([`read_together`]): the engine's writes can't slip in
  /// between its reads, each a wait of its own, and the disk is checked once (issue #1466).
  fn together<R>(
    &mut self,
    job: impl FnOnce(&mut Self) -> Result<R, TeamCraftError>,
  ) -> Result<R, TeamCraftError> {
    let store = self.store.clone();
    read_together(&store, || job(self))
  }

  /// Runs one slice of a sliced read as one look at the store, like [`Self::together`].
  fn slice<R>(&mut self, job: SliceJob<R>) -> Slice<R> {
    let store = self.store.clone();
    read_together(&store, || Ok::<_, TeamCraftError>(job(self)))
      .unwrap_or_else(|error| Slice::Done(Err(error)))
  }

  /// The app's first question on every start: answered from what the store says, without waiting
  /// for the client. `None`: this device isn't set up, which takes the engine and maybe the server.
  pub(crate) fn ready_account(&self) -> Option<AccountState> {
    let account = self.maybe_account.as_ref()?;
    let me = account.backend.me();
    Some(AccountState::Ready {
      user_id: me.id.to_string(),
      display_name: me.display_name,
      identity_fingerprint: account.identity_fingerprint.clone(),
    })
  }

  /// The link requests still open, read from the list the client shares.
  pub(crate) fn link_requests(&self) -> Vec<LinkRequestItem> {
    crate::engine::open_link_requests(&self.link_requests)
  }

  pub(crate) fn backend(&self) -> Result<&CoreChatBackend<Store>, TeamCraftError> {
    self
      .maybe_account
      .as_ref()
      .map(|account| &account.backend)
      .ok_or(TeamCraftError::NotEnrolled)
  }

  pub(crate) fn identity_fingerprint(&self) -> Result<String, TeamCraftError> {
    self
      .maybe_account
      .as_ref()
      .map(|account| account.identity_fingerprint.clone())
      .ok_or(TeamCraftError::NotEnrolled)
  }

  pub(crate) fn account_mut(&mut self) -> Result<&mut ReadAccount, TeamCraftError> {
    self.maybe_account.as_mut().ok_or(TeamCraftError::NotEnrolled)
  }

  pub(crate) fn focus(&self, conversation: ConversationId) {
    self.catch_up_focus.set(conversation);
  }

  /// Tells the app what an action wrote locally, before the server hears of it, and queues the
  /// rest for the client on the engine thread.
  fn dispatch(&self) -> Result<Dispatched, TeamCraftError> {
    let commands = self.backend()?.drain_commands();
    self.listener.on_change();
    let engine = self.engine.upgrade().ok_or(TeamCraftError::EngineStopped)?;
    let (reply, answer) = oneshot::channel();
    let job: Job = Box::new(move |engine| {
      Box::pin(async move {
        // The caller may have given up waiting; nothing to tell it then.
        let _ = reply.send(engine.run_commands(commands).await);
      })
    });
    engine.send(job).map_err(|_| TeamCraftError::EngineStopped)?;
    Ok(answer)
  }

  /// The chat list the app shows ([`ReadQueue::conversations`]). The first ask after the start
  /// gets the list the last pass kept, one record (issue #1758); the pass follows once nothing else
  /// waits ([`Self::when_idle`]). Every other ask is a pass, a slice at a time (issue #1819), which
  /// keeps its list now and then.
  pub(crate) fn chat_list(&mut self) -> Slice<Vec<ConversationItem>> {
    let Some(account) = self.maybe_account.as_mut() else {
      return Slice::Done(Err(TeamCraftError::NotEnrolled));
    };
    if account.chat_list == ChatListAtStart::NotAsked
      && let Some(kept) = kept_chat_list(&self.store)
    {
      account.chat_list = ChatListAtStart::Kept(kept.clone());
      account.maybe_kept = Some(kept.clone());
      return Slice::Done(Ok(kept));
    }
    // This pass answers the app, which asked for it: the one after the start has nothing to add.
    account.maybe_idle_pass = None;
    let limit = account.slice_limit;
    // Beginning reads every conversation's record: a job of its own, not one with a slice on top.
    match self.begin_chat_list(limit) {
      Ok(run) => Slice::More(Box::new(move |reads: &mut Reads| reads.chat_list_slice(run))),
      Err(error) => Slice::Done(Err(error)),
    }
  }

  /// The next slice of an asked pass; the last one hands out the list.
  fn chat_list_slice(&mut self, mut run: ChatListRun) -> Slice<Vec<ConversationItem>> {
    if let Err(error) = self.continue_chat_list(&mut run) {
      return Slice::Done(Err(error));
    }
    if !run.pass.is_done() {
      return Slice::More(Box::new(move |reads: &mut Reads| reads.chat_list_slice(run)));
    }
    Slice::Done(self.finish_chat_list(run))
  }

  /// Whether [`Self::when_idle`] has something to do.
  fn is_idle_work_due(&self) -> bool {
    self
      .maybe_account
      .as_ref()
      .is_some_and(|account| matches!(account.chat_list, ChatListAtStart::Kept(_)))
  }

  /// Work for when no job waits: the pass after a start that showed the kept chat list, a slice per
  /// call (issue #1819), which tells the app if the list is another now.
  fn when_idle(&mut self) {
    let Some(account) = self.maybe_account.as_mut() else {
      return;
    };
    let limit = account.slice_limit;
    let maybe_run = account.maybe_idle_pass.take();
    // Beginning is a call of its own, like a slice: it reads every conversation's record.
    let step = self.together(|reads| match maybe_run {
      Some(mut run) => {
        reads.continue_chat_list(&mut run)?;
        Ok(run)
      },
      None => reads.begin_chat_list(limit),
    });
    let run = match step {
      Ok(run) => run,
      Err(error) => {
        // The app asks again on the next change, and that ask is a pass.
        tracing::warn!(%error, "the chat list after the start could not be read");
        if let Some(account) = self.maybe_account.as_mut() {
          account.chat_list = ChatListAtStart::Passed;
        }
        return;
      },
    };
    let Some(account) = self.maybe_account.as_mut() else {
      return;
    };
    if !run.pass.is_done() {
      account.maybe_idle_pass = Some(run);
      return;
    }
    let ChatListAtStart::Kept(shown) = std::mem::replace(&mut account.chat_list, ChatListAtStart::Passed)
    else {
      return;
    };
    match self.finish_chat_list(run) {
      Ok(items) if items != shown => self.listener.on_change(),
      Ok(_) => {},
      Err(error) => tracing::warn!(%error, "the chat list after the start could not be kept"),
    }
  }

  /// Starts a chat list pass: the spaces and what every entry shares, no entry yet.
  fn begin_chat_list(&self, limit: SliceLimit) -> Result<ChatListRun, TeamCraftError> {
    let pass = self.backend()?.begin_list_pass(limit);
    let spaces = pass
      .spaces()
      .iter()
      .map(|space| (space.id, (space.name.clone(), space.maybe_image_version.clone())))
      .collect();
    Ok(ChatListRun {
      pass,
      spaces,
      items: Vec::new(),
    })
  }

  /// Builds the next slice of `run`'s entries and turns them into the app's items.
  fn continue_chat_list(&self, run: &mut ChatListRun) -> Result<(), TeamCraftError> {
    let backend = self.backend()?;
    backend.continue_list_pass(&mut run.pass);
    let me = backend.me();
    for conversation in run.pass.take_listed() {
      let maybe_peer = direct_peer(backend, &conversation, me.id);
      let maybe_space = conversation.maybe_space;
      let (maybe_space_name, maybe_space_image_version) = maybe_space
        .and_then(|space| run.spaces.get(&space).cloned())
        .map_or((None, None), |(name, version)| (Some(name), version));
      let item = ConversationItem {
        maybe_space_image_version,
        ..ConversationItem::from_view(conversation, maybe_space_name, maybe_peer, me.is_guest)
      };
      run.items.push((maybe_space, item));
    }
    Ok(())
  }

  /// The list of a finished pass ([`ChatListRun::into_items`]). Keeps it for the next start when
  /// it changed, now and then.
  fn finish_chat_list(&mut self, run: ChatListRun) -> Result<Vec<ConversationItem>, TeamCraftError> {
    let items = run.into_items();
    let store = self.store.clone();
    let account = self.maybe_account.as_mut().ok_or(TeamCraftError::NotEnrolled)?;
    account.chat_list = ChatListAtStart::Passed;
    let is_due = account.maybe_kept.as_ref() != Some(&items)
      && account
        .maybe_kept_at
        .is_none_or(|kept_at| kept_at.elapsed() >= KEEP_CHAT_LIST_EVERY);
    if !is_due {
      return Ok(items);
    }
    let kept = KeptChatList { items };
    // Only when the write lock is free: the engine's catch-up may hold it, and this is a cache.
    match try_write_group(&store, || {
      records::put_json(&store, Table::Account, KEPT_CHAT_LIST_KEY, &kept)
    }) {
      Ok(Some(())) => {
        account.maybe_kept = Some(kept.items.clone());
        account.maybe_kept_at = Some(Instant::now());
      },
      Ok(None) => {},
      Err(error) => tracing::debug!(%error, "could not keep the chat list"),
    }
    Ok(kept.items)
  }

  /// The chat list in one go, for reads that need it whole (the share sheet's targets). Neither
  /// kept nor the app's list: the start's state stays as it is.
  pub(crate) fn conversations(&self) -> Result<Vec<ConversationItem>, TeamCraftError> {
    let mut run = self.begin_chat_list(SliceLimit::WHOLE)?;
    while !run.pass.is_done() {
      self.continue_chat_list(&mut run)?;
    }
    Ok(run.into_items())
  }

  /// Where a share from another app can go ([`share_targets`]), each with its last activity.
  pub(crate) fn share_targets(&self) -> Result<Vec<ConversationItem>, TeamCraftError> {
    let listed = self.conversations()?;
    let backend = self.backend()?;
    let items = listed
      .into_iter()
      .filter(|item| item.can_post)
      .map(|item| {
        let maybe_last_activity_ms = parse_id::<ConversationId>(&item.id)
          .ok()
          .and_then(|id| last_readable_at(backend, id));
        ConversationItem {
          maybe_last_activity_ms,
          ..item
        }
      })
      .collect();
    Ok(share_targets(items))
  }

  pub(crate) fn first_sync(&self) -> Result<Option<FirstSyncItem>, TeamCraftError> {
    Ok(self.backend()?.first_sync().map(|progress| FirstSyncItem {
      stage: match progress.stage {
        FirstSyncStage::Newest => FirstSyncStageItem::Newest,
        FirstSyncStage::Older => FirstSyncStageItem::Older,
      },
      done: u32::try_from(progress.done).unwrap_or(u32::MAX),
      total: u32::try_from(progress.total).unwrap_or(u32::MAX),
      maybe_fraction: progress.maybe_fraction,
    }))
  }

  pub(crate) fn key_catch_up(&self) -> Result<Option<KeyCatchUpItem>, TeamCraftError> {
    Ok(self.backend()?.key_catch_up().map(|progress| KeyCatchUpItem {
      caught_up: u32::try_from(progress.caught_up).unwrap_or(u32::MAX),
      total: u32::try_from(progress.total).unwrap_or(u32::MAX),
    }))
  }

  /// [`crate::TeamCraft::chat_previews`], a slice at a time (issue #1819): each preview reads a
  /// conversation's newest rows, 2.8 s for 77 conversations on a phone while a catch-up wrote. IDs
  /// that aren't conversation IDs are skipped, so one bad entry doesn't cost the list all its
  /// previews.
  pub(crate) fn chat_previews(
    &mut self,
    mut remaining: std::collections::VecDeque<String>,
    mut previews: Vec<ChatPreviewItem>,
  ) -> Slice<Vec<ChatPreviewItem>> {
    let limit = match self.maybe_account.as_ref() {
      Some(account) => account.slice_limit,
      None => return Slice::Done(Err(TeamCraftError::NotEnrolled)),
    };
    let started = Instant::now();
    let mut done = 0;
    while let Some(id) = remaining.pop_front() {
      match self.chat_preview(&id) {
        Ok(Some(preview)) => previews.push(preview),
        Ok(None) => {},
        Err(error) => return Slice::Done(Err(error)),
      }
      done += 1;
      if done >= limit.max_entries || is_slice_over(started, limit) {
        break;
      }
    }
    if remaining.is_empty() {
      return Slice::Done(Ok(previews));
    }
    Slice::More(Box::new(move |reads: &mut Reads| {
      reads.chat_previews(remaining, previews)
    }))
  }

  /// One conversation's line in the chat list; `None` for an ID that isn't a conversation's or a
  /// conversation without messages.
  fn chat_preview(&self, id: &str) -> Result<Option<ChatPreviewItem>, TeamCraftError> {
    let backend = self.backend()?;
    let Ok(conversation) = parse_id::<ConversationId>(id) else {
      return Ok(None);
    };
    Ok(
      preview_message(backend, conversation).map(|message| ChatPreviewItem {
        conversation_id: id.to_string(),
        message: self.message_item(backend, &message),
      }),
    )
  }

  pub(crate) fn send_message(
    &self,
    conversation_id: String,
    maybe_thread_id: Option<String>,
    body: String,
  ) -> Result<(), TeamCraftError> {
    let conversation: ConversationId = parse_id(&conversation_id)?;
    let maybe_thread: Option<ThreadId> = maybe_thread_id.as_deref().map(parse_id).transpose()?;
    if body.trim().is_empty() {
      return Err(TeamCraftError::InvalidInput("empty message".into()));
    }
    self.backend()?.post_message(conversation, maybe_thread, &body);
    Ok(())
  }

  pub(crate) fn mark_read(&self, conversation_id: String) -> Result<(), TeamCraftError> {
    self.backend()?.mark_read(parse_id(&conversation_id)?);
    Ok(())
  }

  /// Whether the conversation's menu offers "Leave" (#1105). Channel roles are read afresh while
  /// they are not known, so a later ask has the last-admin answer.
  /// The groups the composer offers after `@` here: none without `use_group_mentions` (issue #206).
  pub(crate) fn mentionable_groups(
    &self,
    conversation_id: &str,
  ) -> Result<Vec<MentionableGroupItem>, TeamCraftError> {
    let backend = self.backend()?;
    let id = parse_id(conversation_id)?;
    Ok(backend.mentionable_groups(id).iter().map(Into::into).collect())
  }

  /// Who the group mentions in `body` would notify here, as sending would expand them.
  pub(crate) fn group_mention_reach(
    &self,
    conversation_id: &str,
    body: &str,
  ) -> Result<GroupMentionReachItem, TeamCraftError> {
    if !body.contains('@') {
      return Ok(GroupMentionReachItem::default());
    }
    let backend = self.backend()?;
    let id = parse_id(conversation_id)?;
    let reach = teamcraft_client_api::notify::group_mention_reach(body, &backend.mentionable_groups(id));
    Ok(GroupMentionReachItem {
      notified_count: u32::try_from(reach.notified.len()).unwrap_or(u32::MAX),
      outside_count: reach.outside_count,
    })
  }

  pub(crate) fn conversation_leave_choice(
    &self,
    conversation_id: &str,
  ) -> Result<LeaveChoice, TeamCraftError> {
    let backend = self.backend()?;
    let id = parse_id(conversation_id)?;
    let Some(conversation) = backend.conversation(id) else {
      return Ok(LeaveChoice::Hidden);
    };
    let roles = backend.channel_roles(id);
    if roles.is_empty() {
      backend.refresh_channel_roles(id);
    }
    Ok(channel_leave(&conversation, backend.me().id, &roles).into())
  }

  /// How many conversations the server claimed deleted at once, if suspect; told once.
  pub(crate) fn take_suspect_deletions(&self) -> Result<u32, TeamCraftError> {
    let backend = self.backend()?;
    let count = backend.suspect_deletions();
    if count > 0 {
      backend.dismiss_suspect_deletions();
    }
    Ok(count)
  }

  pub(crate) fn space_menu(&self, space_id: &str) -> Result<Option<SpaceMenuItem>, TeamCraftError> {
    let backend = self.backend()?;
    let id = parse_id(space_id)?;
    let Some(space) = backend.spaces().into_iter().find(|space| space.id == id) else {
      return Ok(None);
    };
    let is_admin = space.maybe_my_role == Some(SpaceRole::SpaceAdmin);
    let roster = if is_admin {
      let roster = backend.space_members(id);
      if matches!(roster, SpaceRoster::Loading) {
        backend.refresh_space_roster(id);
      }
      roster
    } else {
      SpaceRoster::Loading
    };
    let has_unread = backend
      .space_conversations(id)
      .iter()
      .any(|conversation| conversation.unread_count > 0 || conversation.mention_count > 0);
    Ok(Some(SpaceMenuItem {
      name: space.name,
      has_unread,
      leave: space_leave(space.maybe_my_role.as_ref(), backend.me().id, &roster).into(),
      can_delete: space.can_delete_space,
      channel_count: space.deleted_channel_count,
    }))
  }

  pub(crate) fn mark_space_read(&self, space_id: String) -> Result<(), TeamCraftError> {
    let backend = self.backend()?;
    for conversation in backend.space_conversations(parse_id(&space_id)?) {
      if conversation.unread_count > 0 || conversation.mention_count > 0 {
        backend.mark_read(conversation.id);
      }
    }
    Ok(())
  }

  /// Leaves the space and its channels, only where [`Self::space_menu`] offers it: the server's
  /// last-admin refusal is told before the call, not after.
  pub(crate) fn leave_space(&self, space_id: String) -> Result<(), TeamCraftError> {
    let Some(menu) = self.space_menu(&space_id)? else {
      return Err(TeamCraftError::InvalidInput(format!(
        "not a space of this account: {space_id}"
      )));
    };
    if menu.leave != LeaveChoice::Allowed {
      return Err(TeamCraftError::InvalidInput(
        "leaving this space is not allowed".to_owned(),
      ));
    }
    let backend = self.backend()?;
    backend.remove_space_member(parse_id(&space_id)?, backend.me().id);
    Ok(())
  }

  pub(crate) fn set_favorite(
    &self,
    conversation_id: String,
    is_favorite: bool,
  ) -> Result<(), TeamCraftError> {
    self
      .backend()?
      .set_favorite(parse_id(&conversation_id)?, is_favorite);
    Ok(())
  }

  pub(crate) fn set_presence(
    &self,
    presence: Presence,
    maybe_until_ms: Option<i64>,
  ) -> Result<(), TeamCraftError> {
    self
      .backend()?
      .set_presence_until(presence.into(), maybe_until_ms);
    Ok(())
  }

  pub(crate) fn search(&self, query: &str) -> Result<Vec<SearchHitItem>, TeamCraftError> {
    let backend = self.backend()?;
    let results = backend.search(&parse_query(query));
    Ok(
      results
        .hits
        .into_iter()
        .map(|hit| {
          let message = self.message_item(backend, &hit.message);
          SearchHitItem::from_hit(hit, message)
        })
        .collect(),
    )
  }

  /// What the newest notification showed of the conversation, while the store hasn't caught up
  /// to it (see [`notified::notified_conversation`]).
  pub(crate) fn notified_conversation(
    &self,
    conversation_id: &str,
  ) -> Result<Option<NotifiedConversation>, TeamCraftError> {
    let conversation: ConversationId = parse_id(conversation_id)?;
    Ok(notified::notified_conversation(&self.store, conversation)?.map(Into::into))
  }

  pub(crate) fn push_diagnostics(&self) -> Result<PushDiagnosticsInfo, TeamCraftError> {
    Ok(push_guard::push_diagnostics(&self.store)?.into())
  }

  pub(crate) fn mention_settings(&self) -> MentionSettings {
    // A broken record only costs the preferences, not the store.
    load_mention_preferences(&self.store).unwrap_or_default().into()
  }

  /// This device's notification privacy setting and the workspace policy's limit; both stored, so
  /// a notification extension reads them too.
  pub(crate) fn notification_content(&self) -> NotificationContentSettings {
    let store = &self.store;
    NotificationContentSettings {
      device: (&device_push_content(store)).into(),
      workspace: (&push_content_mode(store)).into(),
    }
  }

  pub(crate) fn segment_prefetch(&self) -> Result<SegmentPrefetchMode, TeamCraftError> {
    Ok(load_segment_prefetch(&self.store)?.into())
  }

  /// What still waits for the server: the outbox, uploads and queued actions.
  pub(crate) fn pending_work(&self) -> Result<PendingWork, TeamCraftError> {
    PendingWork::read(&self.store).map_err(TeamCraftError::from)
  }

  fn message_item(&self, backend: &CoreChatBackend<Store>, message: &MessageView) -> MessageItem {
    let maybe_author = backend.user(message.author);
    MessageItem::from_view(message, maybe_author.as_ref(), is_pending(message.id))
  }
}

/// The newest message people wrote among a conversation's newest rows, else the newest row (a
/// system line); `None` for an empty conversation. Joins and renames in bulk (an import, a big
/// channel) must not hide the last thing somebody said.
fn preview_message(backend: &CoreChatBackend<Store>, conversation: ConversationId) -> Option<MessageView> {
  let mut rows = PREVIEW_ROWS;
  loop {
    let tail = backend.feed_tail(conversation, None, rows);
    if let Some(written) = newest_written(&tail.messages) {
      return Some(written.clone());
    }
    if !tail.has_older || rows > PREVIEW_ROWS {
      return tail.messages.last().cloned();
    }
    rows *= PREVIEW_FURTHER;
  }
}

/// When the newest message of `conversation` the user can read was sent, from the newest rows of
/// its feed, as [`preview_message`] reads them. The share picker orders by it for every
/// conversation at once: reading each one whole for it took the picker minutes on a phone with a
/// long history, a spinner without end.
fn last_readable_at(backend: &CoreChatBackend<Store>, conversation: ConversationId) -> Option<TimestampMs> {
  let mut rows = PREVIEW_ROWS;
  loop {
    let tail = backend.feed_tail(conversation, None, rows);
    let maybe_newest = newest_readable_at(&tail.messages);
    if maybe_newest.is_some() || !tail.has_older || rows > PREVIEW_ROWS {
      return maybe_newest;
    }
    rows *= PREVIEW_FURTHER;
  }
}

/// When the newest of `rows` that isn't a placeholder (its key missing) was sent.
fn newest_readable_at(rows: &[MessageView]) -> Option<TimestampMs> {
  rows
    .iter()
    .filter(|message| message.maybe_placeholder.is_none())
    .map(|message| message.sent_at)
    .max()
}

/// The newest row that isn't a system line (a join, a rename), of rows oldest first.
fn newest_written(rows: &[MessageView]) -> Option<&MessageView> {
  rows
    .iter()
    .rev()
    .find(|message| !matches!(message.kind, MessageKind::System(_)))
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicUsize, Ordering};

  use teamcraft_client_api::model::{Placeholder, SystemEvent, id_from_raw};
  use teamcraft_core::store::{LaneStore, SealedSqliteStore, StaticKeySource, TrackedStore};
  use teamcraft_crypto::secret::SymmetricKey;

  use super::*;

  /// Issue #1466: the read thread does no work nobody waits for, and one pass for many callers.
  mod queue {
    use super::*;

    /// Issue #1521 against #1466: a chat open that waited behind forty chat list reads shows,
    /// in its measure's steps, that it waited in a queue forty deep behind `chat_list_pass`.
    #[tokio::test]
    async fn a_chat_open_behind_a_flood_names_what_it_waited_for() {
      let (queue, release) = held_queue();
      let since_ms = perf_jobs::now_ms();
      let floods: Vec<_> = (0..40)
        .map(|_| {
          queue
            .queue(Lane::Main, WhenCallerGone::Run, chat_list_pass())
            .unwrap()
        })
        .collect();
      let open = queue.queue(Lane::Main, WhenCallerGone::Run, open_chat()).unwrap();

      release.send(()).unwrap();
      open.await.unwrap().unwrap();
      for flood in floods {
        flood.await.unwrap().unwrap();
      }
      // Not `perf_job_spans`: its longest few may be other tests' jobs, running meanwhile.
      let spans = perf_jobs::recorded_between(since_ms, perf_jobs::now_ms());
      let opened = spans
        .iter()
        .find(|span| span.name == "open_chat")
        .expect("the chat open is a step of the measure");
      assert_eq!(opened.kind, PerfJobKind::ReadQueue);
      // Other tests share the counter, so at least: forty before it and itself.
      assert!(opened.queue_depth >= 41, "{}", opened.queue_depth);
      assert!(opened.started_at_ms >= opened.enqueued_at_ms);
      assert!(
        spans
          .iter()
          .any(|span| span.name == "chat_list_pass" && span.done_at_ms <= opened.started_at_ms),
        "what it waited behind is named: {spans:?}"
      );
    }

    #[tokio::test]
    async fn a_read_whose_caller_is_gone_is_skipped() {
      let (queue, release) = held_queue();
      let runs = Arc::new(AtomicUsize::new(0));
      drop(
        queue
          .queue(Lane::Main, WhenCallerGone::Skip, counting(&runs, ()))
          .unwrap(),
      );

      release.send(()).unwrap();
      queue.read(|_| Ok(())).await.unwrap();
      assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_action_runs_when_its_caller_is_gone() {
      let (queue, release) = held_queue();
      let runs = Arc::new(AtomicUsize::new(0));
      drop(
        queue
          .queue(Lane::Main, WhenCallerGone::Run, counting(&runs, ()))
          .unwrap(),
      );

      release.send(()).unwrap();
      queue.read(|_| Ok(())).await.unwrap();
      assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn callers_asking_while_a_pass_waits_share_it() {
      let (queue, release) = held_queue();
      let shared = Arc::new(SharedRead::new());
      let runs = Arc::new(AtomicUsize::new(0));
      let first = queue.share(&shared, whole(counting(&runs, vec![1, 2]))).unwrap();
      let second = queue.share(&shared, whole(counting(&runs, vec![3]))).unwrap();

      release.send(()).unwrap();
      assert_eq!(first.await.unwrap().unwrap(), vec![1, 2]);
      assert_eq!(second.await.unwrap().unwrap(), vec![1, 2]);
      assert_eq!(runs.load(Ordering::SeqCst), 1);

      // Asked after the pass started: a pass of its own, never an older answer.
      let later = queue.share(&shared, whole(counting(&runs, vec![4]))).unwrap();
      assert_eq!(later.await.unwrap().unwrap(), vec![4]);
      assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_shared_pass_nobody_waits_for_is_skipped() {
      let (queue, release) = held_queue();
      let shared = Arc::new(SharedRead::new());
      let runs = Arc::new(AtomicUsize::new(0));
      drop(queue.share(&shared, whole(counting(&runs, vec![1]))).unwrap());
      drop(queue.share(&shared, whole(counting(&runs, vec![2]))).unwrap());

      release.send(()).unwrap();
      queue.read(|_| Ok(())).await.unwrap();
      assert_eq!(runs.load(Ordering::SeqCst), 0);
    }
  }

  /// Issue #1819: a long read hands the thread back between its slices.
  mod slices {
    use super::*;

    /// A read queued while a sliced read runs waits for the slice under way, not the rest.
    #[tokio::test]
    async fn a_read_queued_during_a_sliced_read_waits_one_slice() {
      let (queue, release) = held_queue();
      let ran = Arc::new(AtomicUsize::new(0));
      let (reply, answer) = oneshot::channel();
      queue
        .queue_slice(
          "sliced",
          SliceCallers::Given(vec![reply]),
          slices_of(&ran, SLICES),
        )
        .unwrap();
      let seen = ran.clone();
      let behind = queue
        .queue(Lane::Main, WhenCallerGone::Run, move |_| {
          Ok(seen.load(Ordering::SeqCst))
        })
        .unwrap();

      release.send(()).unwrap();
      assert_eq!(behind.await.unwrap().unwrap(), 1, "it ran after the first slice");
      assert_eq!(answer.await.unwrap().unwrap(), SLICES);
      assert_eq!(ran.load(Ordering::SeqCst), SLICES);
    }

    /// Callers of a shared read hear its answer once its last slice ran.
    #[tokio::test]
    async fn a_shared_sliced_read_answers_every_caller() {
      let (queue, release) = held_queue();
      let shared = Arc::new(SharedRead::new());
      let ran = Arc::new(AtomicUsize::new(0));
      let first = queue.share(&shared, slices_of(&ran, SLICES)).unwrap();
      let second = queue.share(&shared, slices_of(&ran, SLICES)).unwrap();

      release.send(()).unwrap();
      assert_eq!(first.await.unwrap().unwrap(), SLICES);
      assert_eq!(second.await.unwrap().unwrap(), SLICES);
      assert_eq!(ran.load(Ordering::SeqCst), SLICES, "one read for both");
    }

    /// Once nobody waits for it, the rest of a sliced read is skipped.
    #[tokio::test]
    async fn a_sliced_read_nobody_waits_for_stops() {
      let (queue, release) = held_queue();
      let ran = Arc::new(AtomicUsize::new(0));
      let (reply, answer) = oneshot::channel();
      queue
        .queue_slice(
          "sliced",
          SliceCallers::Given(vec![reply]),
          slices_of(&ran, SLICES),
        )
        .unwrap();
      let seen = ran.clone();
      let (told, tell) = mpsc::channel();
      let (go, gone) = mpsc::channel::<()>();
      queue.post(move |_| {
        let _ = told.send(seen.load(Ordering::SeqCst));
        // Holds the thread until the caller gave up; ignored, the test ends the wait either way.
        let _ = gone.recv();
      });
      release.send(()).unwrap();
      assert_eq!(tell.recv().unwrap(), 1);
      drop(answer);
      go.send(()).unwrap();

      queue.read(|_| Ok(())).await.unwrap();
      assert_eq!(
        ran.load(Ordering::SeqCst),
        1,
        "the second slice found nobody waiting"
      );
    }

    const SLICES: usize = 4;

    /// A read of `total` slices that counts them in `ran` and answers how many ran.
    fn slices_of(ran: &Arc<AtomicUsize>, total: usize) -> SliceJob<usize> {
      let ran = ran.clone();
      Box::new(move |_reads: &mut Reads| {
        let done = ran.fetch_add(1, Ordering::SeqCst) + 1;
        if done >= total {
          Slice::Done(Ok(done))
        } else {
          Slice::More(slices_of(&ran, total))
        }
      })
    }
  }

  /// Issue #1583: each read thread on a read connection of its own.
  mod lanes {
    use teamcraft_core::store::{CommitJournal, LocalStore as _, SealedStoreLimits, Table};

    use super::*;

    /// A slow read on the main thread, which holds its snapshot, holds up neither the open chat's
    /// query nor the engine's writes; the query sees the engine's commit.
    #[tokio::test]
    async fn a_slow_read_holds_up_neither_a_query_nor_a_write() {
      let files = FileLanes::open();
      let (started, has_started) = mpsc::channel();
      let (release, released) = mpsc::channel::<()>();
      let slow = files
        .queue
        .queue(Lane::Main, WhenCallerGone::Run, move |reads: &mut Reads| {
          reads.store.get(Table::Users, "a")?;
          started.send(()).map_err(|_| TeamCraftError::EngineStopped)?;
          // Deliberately ignored: the test ends the wait either way.
          let _ = released.recv();
          Ok(())
        })
        .unwrap();
      has_started.recv().unwrap();

      // On one connection the engine would wait for the read's group to end.
      files.engine.put(Table::Users, "a", b"new").unwrap();
      let seen = files
        .queue
        .query(|reads| Ok(reads.store.get(Table::Users, "a")?))
        .await
        .unwrap();
      assert_eq!(seen, Some(b"new".to_vec()));

      release.send(()).unwrap();
      slow.await.unwrap().unwrap();
      files.finish();
    }

    /// Issue #1756: an action waits for the store's write lock while the engine holds a group of
    /// writes, and a read asked after it doesn't wait with it; the action's write lands once the
    /// engine's group ended.
    #[tokio::test]
    async fn an_action_waiting_for_the_write_lock_holds_up_no_read() {
      let files = FileLanes::open();
      files.engine.put(Table::Users, "a", b"before").unwrap();
      let engine = files.engine.clone();
      let (held, is_held) = mpsc::channel();
      let (release, released) = mpsc::channel::<()>();
      let catch_up = std::thread::spawn(move || {
        write_group(&engine, || {
          engine.put(Table::Users, "engine", b"1")?;
          held.send(()).map_err(|_| TeamCraftError::EngineStopped)?;
          // Deliberately ignored: the test ends the wait either way.
          let _ = released.recv();
          Ok::<_, TeamCraftError>(())
        })
        .unwrap();
      });
      is_held.recv().unwrap();

      // Queued before the read: on one thread the read would wait for the engine's group too.
      let mut act = Box::pin(files.queue.act(|reads| {
        reads.store.put(Table::Users, "a", b"acted")?;
        Ok(())
      }));
      tokio::select! {
        biased;
        _ = &mut act => panic!("the action can't finish while the engine holds the store"),
        () = std::future::ready(()) => {},
      }
      let seen = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        files.queue.read(|reads| Ok(reads.store.get(Table::Users, "a")?)),
      )
      .await
      .expect("the read doesn't wait for the action")
      .unwrap();
      assert_eq!(seen, Some(b"before".to_vec()));

      release.send(()).unwrap();
      catch_up.join().unwrap();
      // No account and no engine here to take what the action hands on: it fails after its local
      // half, which ran all the same.
      assert!(act.await.is_err());
      assert_eq!(
        files.engine.get(Table::Users, "a").unwrap(),
        Some(b"acted".to_vec())
      );
      files.finish();
    }

    /// The engine's store and read threads on a database file, as `Engine::open` makes them.
    struct FileLanes {
      engine: Store,
      queue: ReadQueue,
      directory: std::path::PathBuf,
    }

    impl FileLanes {
      fn open() -> Self {
        let directory = std::env::temp_dir().join(format!("teamcraft-read-lanes-{}", rand_suffix()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("store.db");
        let key_source = StaticKeySource::new(SymmetricKey::from_bytes([7; 32]));
        let journal = CommitJournal::new();
        let writer = Arc::new(SealedSqliteStore::open(&path, &key_source).unwrap());
        writer.publish_commits(Arc::clone(&journal));
        let engine = TrackedStore::new(LaneStore::writer(Arc::clone(&writer)));
        let lane = || {
          let reader = SealedSqliteStore::open_reader(
            &path,
            &key_source,
            SealedStoreLimits::default(),
            Arc::clone(&journal),
          )
          .unwrap();
          engine.sharing(LaneStore::reader(Arc::clone(&writer), Arc::new(reader)))
        };
        let (jobs, _) = tokio_mpsc::unbounded_channel::<Job>();
        let queue = ReadQueue::start(
          ReadLanes {
            main: lane(),
            query: lane(),
            act: engine.sharing(LaneStore::writer(Arc::clone(&writer))),
          },
          Arc::new(Quiet),
          CatchUpFocus::default(),
          jobs.downgrade(),
        )
        .unwrap();
        Self {
          engine,
          queue,
          directory,
        }
      }

      fn finish(self) {
        self.queue.close();
        drop(self.engine);
        std::fs::remove_dir_all(&self.directory).unwrap();
      }
    }

    fn rand_suffix() -> u64 {
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_nanos()).unwrap_or_default())
        .unwrap_or_default()
    }
  }

  #[test]
  fn a_preview_skips_the_system_lines_after_the_last_written_message() {
    let rows = [
      row(1, MessageKind::User),
      row(2, MessageKind::System(SystemEvent::Joined)),
      row(3, MessageKind::System(SystemEvent::Left)),
    ];
    assert_eq!(
      newest_written(&rows).map(|message| message.id),
      Some(id_from_raw(1))
    );
  }

  #[test]
  fn a_preview_takes_the_newest_written_message() {
    let rows = [
      row(1, MessageKind::System(SystemEvent::Joined)),
      row(2, MessageKind::User),
      row(3, MessageKind::Change),
    ];
    assert_eq!(
      newest_written(&rows).map(|message| message.id),
      Some(id_from_raw(3))
    );
  }

  #[test]
  fn the_last_activity_skips_placeholders() {
    let waiting = MessageView {
      maybe_placeholder: Some(Placeholder::Waiting),
      ..row(3, MessageKind::User)
    };
    let rows = [row(1, MessageKind::User), row(2, MessageKind::User), waiting];
    assert_eq!(newest_readable_at(&rows), Some(2));
  }

  #[test]
  fn placeholders_only_have_no_last_activity() {
    let waiting = MessageView {
      maybe_placeholder: Some(Placeholder::NotAvailable),
      ..row(1, MessageKind::User)
    };
    assert_eq!(newest_readable_at(&[waiting]), None);
  }

  #[test]
  fn rows_of_system_lines_only_have_no_written_message() {
    let rows = [row(1, MessageKind::System(SystemEvent::Joined))];
    assert!(newest_written(&rows).is_none());
    assert!(newest_written(&[]).is_none());
  }

  /// A read queue whose thread waits until the sender is used: what is queued meanwhile waits.
  fn held_queue() -> (ReadQueue, mpsc::Sender<()>) {
    let key_source = StaticKeySource::new(SymmetricKey::from_bytes([7; 32]));
    let store = TrackedStore::new(LaneStore::writer(Arc::new(
      SealedSqliteStore::open_in_memory(&key_source).unwrap(),
    )));
    let (engine, _) = tokio_mpsc::unbounded_channel::<Job>();
    let queue = ReadQueue::start(
      ReadLanes {
        main: store.clone(),
        query: store.clone(),
        act: store,
      },
      Arc::new(Quiet),
      CatchUpFocus::default(),
      engine.downgrade(),
    )
    .unwrap();
    let (release, released) = mpsc::channel();
    queue.post(move |_| {
      let _ = released.recv();
    });
    (queue, release)
  }

  /// The chat list's read in #1466's flood (each a separate pass, as before sharing).
  fn chat_list_pass() -> impl FnOnce(&mut Reads) -> Result<(), TeamCraftError> + Send + 'static {
    |_| Ok(())
  }

  /// The read a chat open waits for.
  fn open_chat() -> impl FnOnce(&mut Reads) -> Result<(), TeamCraftError> + Send + 'static {
    |_| Ok(())
  }

  /// `job` as a read of one slice.
  fn whole<R: Send + 'static>(
    job: impl FnOnce(&mut Reads) -> Result<R, TeamCraftError> + Send + 'static,
  ) -> impl FnOnce(&mut Reads) -> Slice<R> + Send + 'static {
    move |reads| Slice::Done(job(reads))
  }

  /// A job that counts its runs and answers `value`.
  fn counting<R: Send + 'static>(
    runs: &Arc<AtomicUsize>,
    value: R,
  ) -> impl FnOnce(&mut Reads) -> Result<R, TeamCraftError> + Send + 'static {
    let runs = runs.clone();
    move |_| {
      runs.fetch_add(1, Ordering::SeqCst);
      Ok(value)
    }
  }

  struct Quiet;

  impl EventListener for Quiet {
    fn on_change(&self) {}
  }

  fn row(id: u64, kind: MessageKind) -> MessageView {
    MessageView {
      id: id_from_raw(id),
      conversation: id_from_raw(5),
      author: id_from_raw(3),
      kind,
      sent_at: i64::try_from(id).unwrap_or_default(),
      body: "Hello".into(),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread: None,
      maybe_imported: None,
      maybe_source: None,
      maybe_placeholder: None,
      group_mentions: Vec::new(),
      custom_emoji: Vec::new(),
      mentions_me: false,
      maybe_poll: None,
    }
  }
}
