//! The read thread: what the app reads from the local store, and the local half of what it does
//! there (an outbox write, a read marker), doesn't wait for the engine thread, whatever that is busy
//! with: a job on the network, a catch-up, the client opening. The store takes a thread's group of
//! writes as a whole ([`teamcraft_core::store::SealedSqliteStore`]), so a read sees committed records
//! and a write here stays out of the sync loop's group. There is no client and no MLS state on this
//! thread: one core owns the store as before (e2ee.md §9.4), and the notification extension's path
//! (§7.2) stays on the engine thread.

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

use teamcraft_client_api::backend::{ChatBackend as _, SearchBackend as _};
use teamcraft_client_api::entry_rules::{channel_leave, space_leave};
use teamcraft_client_api::model::{
  FirstSyncStage, Message as MessageView, SpaceRole, SpaceRoster, is_pending,
};
use teamcraft_client_api::search::parse_query;
use teamcraft_core::TypingState;
use teamcraft_core::client::CatchUpFocus;
use teamcraft_core::clock::{Clock as _, SystemClock};
use teamcraft_core::notified;
use teamcraft_core::notify_rules::load_mention_preferences;
use teamcraft_core::pending_work::PendingWork;
use teamcraft_core::prefetch::load_segment_prefetch;
use teamcraft_core::push_guard;
use teamcraft_core::push_preview::{device_push_content, push_content_mode};
use teamcraft_core::transfers::Transfers;
use teamcraft_core::ui_backend::CoreChatBackend;
use teamcraft_core::ui_snapshot::SnapshotCache;
use teamcraft_proto::{ConversationId, ThreadId, UserId};
use tokio::sync::{mpsc as tokio_mpsc, oneshot};

use crate::EventListener;
use crate::engine::{Job, Store, direct_peer, feed_anchor, older_len, parse_id};
use crate::error::TeamCraftError;
use crate::logging;
use crate::types::{
  AccountState, ConversationItem, FirstSyncItem, FirstSyncStageItem, KeyCatchUpItem, LeaveChoice,
  MentionSettings, MessageItem, MessagePage, NotificationContentSettings, NotifiedConversation, Presence,
  PushDiagnosticsInfo, SearchHitItem, SegmentPrefetchMode, SpaceMenuItem, share_targets,
};

/// Work for the read thread.
type ReadJob = Box<dyn FnOnce(&mut Reads) + Send>;

/// An action's answer: the engine ran what it queued for the client.
pub(crate) type Dispatched = oneshot::Receiver<Result<(), TeamCraftError>>;

/// The read thread's queue. The thread ends once every handle on the queue is gone.
#[derive(Clone)]
pub(crate) struct ReadQueue(mpsc::Sender<ReadJob>);

/// The read thread's state.
pub(crate) struct Reads {
  pub(crate) store: Store,
  listener: Arc<dyn EventListener>,
  catch_up_focus: CatchUpFocus,
  /// The engine thread's queue, for what an action leaves for the client. Weak: the engine ends
  /// when the app drops its handle, whatever this thread holds.
  engine: tokio_mpsc::WeakUnboundedSender<Job>,
  maybe_account: Option<ReadAccount>,
  /// Set by [`ReadQueue::close`]: the thread ends after this job and answers here.
  maybe_closing: Option<mpsc::Sender<()>>,
}

/// The enrolled account as the read thread has it.
pub(crate) struct ReadAccount {
  backend: CoreChatBackend<Store>,
  /// What a snapshot built last: a change costs what it touched, not the workspace (issue #246).
  pub(crate) snapshots: SnapshotCache,
  pub(crate) identity_fingerprint: String,
}

impl ReadAccount {
  pub(crate) fn parts(&mut self) -> (&CoreChatBackend<Store>, &mut SnapshotCache) {
    (&self.backend, &mut self.snapshots)
  }
}

impl ReadQueue {
  pub(crate) fn start(
    store: Store,
    listener: Arc<dyn EventListener>,
    catch_up_focus: CatchUpFocus,
    engine: tokio_mpsc::WeakUnboundedSender<Job>,
  ) -> Result<Self, TeamCraftError> {
    let (queue, jobs) = mpsc::channel::<ReadJob>();
    std::thread::Builder::new()
      .name("teamcraft-reads".into())
      .spawn(move || {
        let mut reads = Reads {
          store,
          listener,
          catch_up_focus,
          engine,
          maybe_account: None,
          maybe_closing: None,
        };
        for job in jobs {
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
    Ok(Self(queue))
  }

  /// Runs `job` on the read thread and waits for its result.
  pub(crate) async fn read<R, F>(&self, job: F) -> Result<R, TeamCraftError>
  where
    R: Send + 'static,
    F: FnOnce(&mut Reads) -> Result<R, TeamCraftError> + Send + 'static,
  {
    let (reply, answer) = oneshot::channel();
    let queued = Instant::now();
    let boxed: ReadJob = Box::new(move |reads| {
      let started = Instant::now();
      let result = job(reads);
      logging::slow_read(std::any::type_name::<F>(), started - queued, started.elapsed());
      // The caller may have given up waiting (cancelled coroutine); nothing to tell it then.
      let _ = reply.send(result);
    });
    self.send(boxed)?;
    answer.await.map_err(|_| TeamCraftError::EngineStopped)?
  }

  /// Runs an action's local half on the read thread and hands what it queued to the engine, in
  /// the order of the actions; answers once the engine took it, like before this thread existed.
  pub(crate) async fn act<R, F>(&self, job: F) -> Result<R, TeamCraftError>
  where
    R: Send + 'static,
    F: FnOnce(&mut Reads) -> Result<R, TeamCraftError> + Send + 'static,
  {
    let (value, dispatched) = self
      .read(move |reads| {
        let value = job(reads)?;
        Ok((value, reads.dispatch()?))
      })
      .await?;
    dispatched.await.map_err(|_| TeamCraftError::EngineStopped)??;
    Ok(value)
  }

  /// Ends the thread and returns once it let go of the store. Reads from then on get
  /// [`TeamCraftError::EngineStopped`].
  pub(crate) fn close(&self) {
    let (closed, is_closed) = mpsc::channel();
    self.post(move |reads| reads.maybe_closing = Some(closed));
    // An error here means the thread had ended already, which is what this waits for.
    let _ = is_closed.recv();
  }

  /// The account the store holds: from now on the app can read. A new account starts over.
  pub(crate) fn know_account(&self, user: UserId, identity_fingerprint: String) {
    self.post(move |reads| {
      let is_known = reads
        .maybe_account
        .as_ref()
        .is_some_and(|account| account.backend.me().id == user);
      if let (true, Some(account)) = (is_known, reads.maybe_account.as_mut()) {
        account.identity_fingerprint = identity_fingerprint;
        return;
      }
      let backend = CoreChatBackend::new(reads.store.clone(), user, Box::new(SystemClock));
      reads.maybe_account = Some(ReadAccount {
        backend,
        snapshots: SnapshotCache::new(u64::try_from(SystemClock.now_ms()).unwrap_or_default()),
        identity_fingerprint,
      });
    });
  }

  /// Typing and transfers come from the client once it is open, and from its successor when it is
  /// reopened; what the backend keeps stays.
  pub(crate) fn follow_client(&self, typing: TypingState, transfers: Transfers) {
    self.post(move |reads| {
      if let Some(account) = reads.maybe_account.take() {
        reads.maybe_account = Some(ReadAccount {
          backend: account.backend.with_typing(typing).with_transfers(transfers),
          ..account
        });
      }
    });
  }

  /// The store turned out unreadable: nothing reads it any more.
  fn post(&self, job: impl FnOnce(&mut Reads) + Send + 'static) {
    // Refused only by a thread that ended, which has nobody left to tell.
    let _ = self.0.send(Box::new(job));
  }

  fn send(&self, job: ReadJob) -> Result<(), TeamCraftError> {
    self.0.send(job).map_err(|_| TeamCraftError::EngineStopped)
  }
}

impl Reads {
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

  pub(crate) fn conversations(&self) -> Result<Vec<ConversationItem>, TeamCraftError> {
    let backend = self.backend()?;
    let spaces: BTreeMap<_, _> = backend
      .spaces()
      .into_iter()
      .map(|space| (space.id, space.name))
      .collect();
    let mut conversations = Vec::new();
    for space in spaces.keys() {
      conversations.extend(backend.space_conversations(*space));
    }
    conversations.extend(backend.direct_conversations());
    let me = backend.me();
    Ok(
      conversations
        .into_iter()
        .map(|conversation| {
          let maybe_peer = direct_peer(backend, &conversation, me.id);
          let maybe_space_name = conversation
            .maybe_space
            .and_then(|space| spaces.get(&space).cloned());
          ConversationItem::from_view(conversation, maybe_space_name, maybe_peer, me.is_guest)
        })
        .collect(),
    )
  }

  /// Where a share from another app can go ([`share_targets`]), each with its last activity.
  pub(crate) fn share_targets(&self) -> Result<Vec<ConversationItem>, TeamCraftError> {
    let backend = self.backend()?;
    let items = self
      .conversations()?
      .into_iter()
      .filter(|item| item.can_post)
      .map(|item| {
        let maybe_last_activity_ms = parse_id::<ConversationId>(&item.id)
          .ok()
          .and_then(|id| backend.last_activity(id));
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

  pub(crate) fn messages(
    &self,
    conversation_id: &str,
    limit: u32,
  ) -> Result<Vec<MessageItem>, TeamCraftError> {
    let backend = self.backend()?;
    let conversation: ConversationId = parse_id(conversation_id)?;
    self.focus(conversation);
    // One read for count and page: a separate count could change before the page is read and
    // shift the window (issue #1060).
    let tail = backend.feed_tail(conversation, None, usize::try_from(limit).unwrap_or(usize::MAX));
    Ok(self.message_items(backend, &tail.messages))
  }

  pub(crate) fn messages_from(
    &self,
    conversation_id: &str,
    maybe_oldest_id: Option<&str>,
    oldest_sent_at_ms: i64,
    older: u32,
  ) -> Result<MessagePage, TeamCraftError> {
    let backend = self.backend()?;
    let conversation: ConversationId = parse_id(conversation_id)?;
    self.focus(conversation);
    let tail = backend.feed_tail(
      conversation,
      feed_anchor(maybe_oldest_id, oldest_sent_at_ms)?,
      older_len(older),
    );
    Ok(MessagePage {
      messages: self.message_items(backend, &tail.messages),
      has_older: tail.has_older,
    })
  }

  pub(crate) fn thread(
    &self,
    conversation_id: &str,
    thread_id: &str,
  ) -> Result<Vec<MessageItem>, TeamCraftError> {
    let backend = self.backend()?;
    let conversation = parse_id(conversation_id)?;
    self.focus(conversation);
    let messages = backend.thread(conversation, parse_id(thread_id)?);
    Ok(self.message_items(backend, &messages))
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

  pub(crate) fn set_presence(&self, presence: Presence) -> Result<(), TeamCraftError> {
    self.backend()?.set_presence(presence.into());
    Ok(())
  }

  pub(crate) fn search(&self, query: &str) -> Result<Vec<SearchHitItem>, TeamCraftError> {
    let backend = self.backend()?;
    let results = backend.search(&parse_query(query));
    Ok(
      results
        .hits
        .into_iter()
        .map(|hit| SearchHitItem {
          message: self.message_item(backend, &hit.message),
          conversation_name: hit.conversation_name,
          maybe_space_name: hit.maybe_space_name,
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

  fn message_items(&self, backend: &CoreChatBackend<Store>, messages: &[MessageView]) -> Vec<MessageItem> {
    messages
      .iter()
      .map(|message| self.message_item(backend, message))
      .collect()
  }

  fn message_item(&self, backend: &CoreChatBackend<Store>, message: &MessageView) -> MessageItem {
    let maybe_author = backend.user(message.author);
    MessageItem::from_view(message, maybe_author.as_ref(), is_pending(message.id))
  }
}
