//! What a conversation's sidebar entry counts, kept in the store so a start doesn't read every
//! message (docs/performance.md, "Mobile core").
//!
//! Message count, last activity and unread counts are summed over the feed's rows, and the rows
//! are sealed. An [`EntryTally`] keeps the sums and how far they went. It is a cache, never the
//! truth: every read checks it against what the store wrote with the messages
//! ([`LocalStore::feed_version`] and the place of the last counted row), and nothing updates it
//! when a message is written. So a crash between a message and its tally, a lost or damaged tally
//! and a rebuilt catalog all end the same way: the rows it misses are counted, or all of them.
//!
//! What a read costs:
//!
//! | Since the tally was written                 | Read                                 |
//! |---------------------------------------------|--------------------------------------|
//! | Nothing                                     | The tally, the feed index's positions |
//! | New messages at the end                     | Those rows                           |
//! | A read position, a mark, mention rules, an edit | The rows from the read position  |
//! | A row left or moved, older rows came in     | All of the conversation's rows       |
//!
//! Stores without a feed version (the in-memory one) count all rows every time.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use teamcraft_client_api::model::TimestampMs;
use teamcraft_client_api::notify::MentionRules;
use teamcraft_proto::{ConversationId, Id, MessageId, UserId};

use crate::imported::ImportAdditions;
use crate::records;
use crate::store::{FeedVersion, LocalStore, MessageFilter, MessageOrder, StoreError, StoredMessage, Table};
use crate::ui_backend::{EntryCounts, MentionMemo, UnreadCounts, is_deleted};

/// Rows of the feed read at a time.
const ROWS_PER_READ: usize = 256;
/// The sort group of messages with a place on the server's clock ([`StoredMessage::sort_key`]);
/// pending ones come after them, change without notice and are never kept.
const PLACED_GROUP: u8 = 0;

/// The counts of a conversation's rows up to [`Self::maybe_through`], as they were at
/// [`Self::version`]. Sealed like every record; says nothing the messages don't.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EntryTally {
  version: FeedVersion,
  /// Decides which rows are hidden as the import's additions.
  maybe_importer: Option<UserId>,
  maybe_through: Option<Through>,
  /// The feed index's rows up to and including that row: a row that came in before it shows here.
  rows: usize,
  messages: usize,
  maybe_last_activity: Option<TimestampMs>,
  /// [`unread_key`] of what the unread counts were decided by.
  unread_key: u64,
  unread_messages: u32,
  unread_mentions: u32,
}

/// The last row a tally counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Through {
  value: i64,
  client_message_id: [u8; 16],
}

/// What a tally must have been counted under to hold.
pub(crate) struct TallyInputs {
  pub(crate) version: FeedVersion,
  pub(crate) maybe_importer: Option<UserId>,
  pub(crate) unread_key: u64,
  /// No unread row sorts before it ([`read_boundary`]).
  pub(crate) maybe_boundary: Option<MessageOrder>,
}

/// One pass over rows in display order, adding them up.
pub(crate) struct EntryPass<'a> {
  me: UserId,
  rules: &'a MentionRules<'a>,
  memo: &'a mut MentionMemo,
  is_unread: &'a dyn Fn(&StoredMessage) -> bool,
  additions: ImportAdditions,
  /// The events before this pass started were noted; a pass over whole conversation sees them.
  has_earlier_events: bool,
  counts: EntryCounts,
}

impl Through {
  fn of(order: MessageOrder) -> Self {
    Self {
      value: order.value,
      client_message_id: order.client_message_id,
    }
  }

  fn order(self) -> MessageOrder {
    MessageOrder {
      group: PLACED_GROUP,
      value: self.value,
      client_message_id: self.client_message_id,
    }
  }
}

impl<'a> EntryPass<'a> {
  pub(crate) fn new(
    me: UserId,
    rules: &'a MentionRules<'a>,
    memo: &'a mut MentionMemo,
    is_unread: &'a dyn Fn(&StoredMessage) -> bool,
    maybe_importer: Option<UserId>,
  ) -> Self {
    Self {
      me,
      rules,
      memo,
      is_unread,
      additions: ImportAdditions::new(maybe_importer),
      // Without an import account there are no additions to know of.
      has_earlier_events: maybe_importer.is_none(),
      counts: EntryCounts::default(),
    }
  }

  /// All of a top-level message's counts; tombstones (not rows) have none.
  fn take(&mut self, message: &StoredMessage) {
    if self.additions.note(message) || is_deleted(message) {
      return;
    }
    self.counts.messages += 1;
    if message.maybe_unreadable.is_none() {
      self.counts.maybe_last_activity = self.counts.maybe_last_activity.max(Some(message.sent_at_ms));
    }
    self.take_unread(message);
  }

  fn take_unread(&mut self, message: &StoredMessage) {
    if (self.is_unread)(message) {
      self.counts.unread.messages = self.counts.unread.messages.saturating_add(1);
      if self.memo.is_mention(message, self.me, self.rules) {
        self.counts.unread.mentions = self.counts.unread.mentions.saturating_add(1);
      }
    }
  }

  /// Before an event in the middle of a conversation: the additions told of before it.
  fn note_earlier_events<S: LocalStore>(
    &mut self,
    store: &S,
    conversation: ConversationId,
    before: MessageOrder,
  ) -> Result<(), StoreError> {
    if self.has_earlier_events {
      return Ok(());
    }
    self.has_earlier_events = true;
    for event in store.event_messages(conversation)? {
      if event.is_feed_row() && event.order() < before {
        self.additions.note(&event);
      }
    }
    Ok(())
  }
}

impl EntryTally {
  /// Whether this tally holds under `inputs`, its place checked against the store's feed index.
  fn holds<S: LocalStore>(
    &self,
    store: &S,
    conversation: ConversationId,
    inputs: &TallyInputs,
    rows_now: usize,
  ) -> Result<bool, StoreError> {
    if self.version.generation != inputs.version.generation
      || self.version.reshaped != inputs.version.reshaped
      || self.maybe_importer != inputs.maybe_importer
      || self.rows > rows_now
    {
      return Ok(false);
    }
    let Some(through) = self.maybe_through else {
      return Ok(self.rows == 0);
    };
    let ranks = store.feed_ranks(conversation, &[through.order()])?;
    Ok(ranks.first().is_some_and(|rank| rank + 1 == self.rows))
  }

  fn unread(&self) -> UnreadCounts {
    UnreadCounts {
      messages: self.unread_messages,
      mentions: self.unread_mentions,
    }
  }
}

/// The counts of `conversation`, from `maybe_kept` where it holds, and the tally to keep.
pub(crate) fn counted<S: LocalStore>(
  store: &S,
  conversation: ConversationId,
  inputs: &TallyInputs,
  maybe_kept: Option<EntryTally>,
  mut pass: EntryPass<'_>,
) -> Result<(EntryCounts, EntryTally), StoreError> {
  let rows_now = store.feed_len(conversation)?;
  let maybe_holding = match maybe_kept {
    Some(kept) if kept.holds(store, conversation, inputs, rows_now)? => Some(kept),
    _ => None,
  };
  match maybe_holding {
    Some(kept) => counted_from(store, conversation, inputs, kept, rows_now, pass),
    None => {
      let mut progress = Progress::default();
      counted_whole(store, conversation, &mut pass, &mut progress)?;
      Ok(progress.finish(inputs, pass.counts))
    },
  }
}

/// The counts of `conversation` in a store that keeps no version: all its rows, every time.
pub(crate) fn counted_unkept<S: LocalStore>(
  store: &S,
  conversation: ConversationId,
  mut pass: EntryPass<'_>,
) -> Result<EntryCounts, StoreError> {
  counted_whole(store, conversation, &mut pass, &mut Progress::default())?;
  Ok(pass.counts)
}

fn counted_whole<S: LocalStore>(
  store: &S,
  conversation: ConversationId,
  pass: &mut EntryPass<'_>,
  progress: &mut Progress,
) -> Result<(), StoreError> {
  #[cfg(test)]
  WHOLE_PASSES.with(|passes| passes.set(passes.get() + 1));
  pass.has_earlier_events = true;
  store.visit_messages(conversation, MessageFilter::TopLevel, &mut |message| {
    progress.note_row(message, &pass.counts);
    pass.take(message);
  })
}

/// How far a pass got, and what of it may be kept: the counts before the first pending row.
#[derive(Default)]
struct Progress {
  maybe_through: Option<Through>,
  rows: usize,
  maybe_kept: Option<EntryCounts>,
}

impl Progress {
  /// `counts` are the pass's before it takes `message`.
  fn note_row(&mut self, message: &StoredMessage, counts: &EntryCounts) {
    if !message.is_feed_row() || self.maybe_kept.is_some() {
      return;
    }
    let order = message.order();
    if order.group == PLACED_GROUP {
      self.maybe_through = Some(Through::of(order));
      self.rows += 1;
    } else {
      self.maybe_kept = Some(*counts);
    }
  }

  fn finish(self, inputs: &TallyInputs, counts: EntryCounts) -> (EntryCounts, EntryTally) {
    let kept = self.maybe_kept.unwrap_or(counts);
    let tally = EntryTally {
      version: inputs.version,
      maybe_importer: inputs.maybe_importer,
      maybe_through: self.maybe_through,
      rows: self.rows,
      messages: kept.messages,
      maybe_last_activity: kept.maybe_last_activity,
      unread_key: inputs.unread_key,
      unread_messages: kept.unread.messages,
      unread_mentions: kept.unread.mentions,
    };
    (counts, tally)
  }
}

/// `kept` holds: its counts, with the unread ones decided again if what decides them changed, and
/// the rows after it.
fn counted_from<S: LocalStore>(
  store: &S,
  conversation: ConversationId,
  inputs: &TallyInputs,
  kept: EntryTally,
  rows_now: usize,
  mut pass: EntryPass<'_>,
) -> Result<(EntryCounts, EntryTally), StoreError> {
  let is_unread_kept = kept.unread_key == inputs.unread_key && kept.version.edited == inputs.version.edited;
  let unread = if is_unread_kept {
    kept.unread()
  } else {
    unread_up_to(store, conversation, inputs, &kept, &mut pass)?
  };
  pass.counts = EntryCounts {
    messages: kept.messages,
    maybe_last_activity: kept.maybe_last_activity,
    unread,
  };
  pass.additions = ImportAdditions::new(inputs.maybe_importer);
  pass.has_earlier_events = inputs.maybe_importer.is_none();
  let mut progress = Progress {
    maybe_through: kept.maybe_through,
    rows: kept.rows,
    maybe_kept: None,
  };
  let mut at = kept.rows;
  while at < rows_now {
    let rows = store.feed_rows(conversation, at, ROWS_PER_READ)?;
    if rows.is_empty() {
      break;
    }
    at += rows.len();
    for row in &rows {
      // A row came in before these since the place was checked, so one is read twice. The rows
      // now disagree with the place, and the next read counts them all.
      if kept
        .maybe_through
        .is_some_and(|through| row.order() <= through.order())
      {
        continue;
      }
      if row.is_event() {
        pass.note_earlier_events(store, conversation, row.order())?;
      }
      progress.note_row(row, &pass.counts);
      pass.take(row);
    }
  }
  Ok(progress.finish(inputs, pass.counts))
}

/// The unread counts of the rows `kept` covers: those from the read position on.
fn unread_up_to<S: LocalStore>(
  store: &S,
  conversation: ConversationId,
  inputs: &TallyInputs,
  kept: &EntryTally,
  pass: &mut EntryPass<'_>,
) -> Result<UnreadCounts, StoreError> {
  let mut at = match inputs.maybe_boundary {
    Some(boundary) => store
      .feed_ranks(conversation, &[boundary])?
      .first()
      .copied()
      .unwrap_or(0),
    None => 0,
  };
  pass.counts = EntryCounts::default();
  while at < kept.rows {
    let rows = store.feed_rows(conversation, at, ROWS_PER_READ.min(kept.rows - at))?;
    if rows.is_empty() {
      break;
    }
    at += rows.len();
    for row in &rows {
      if row.is_event() {
        pass.note_earlier_events(store, conversation, row.order())?;
      }
      if !pass.additions.note(row) {
        pass.take_unread(row);
      }
    }
  }
  Ok(pass.counts.unread)
}

/// The first place an unread row can be: at the "mark unread" mark, or after the read position.
pub(crate) fn read_boundary(
  maybe_mark: Option<MessageId>,
  maybe_last_read: Option<MessageId>,
) -> Option<MessageOrder> {
  let at = |id: MessageId, fill: u8| MessageOrder {
    group: PLACED_GROUP,
    value: id.snowflake().get(),
    client_message_id: [fill; 16],
  };
  match (maybe_mark, maybe_last_read) {
    (Some(mark), _) => Some(at(mark, 0)),
    (None, Some(last_read)) => Some(at(last_read, u8::MAX)),
    (None, None) => None,
  }
}

/// What decides which rows are unread and which of them mention the user. A hash because only
/// equality counts; one that differs after an update only means deciding again.
pub(crate) fn unread_key(
  maybe_mark: Option<MessageId>,
  maybe_last_read: Option<MessageId>,
  maybe_lines_read: Option<MessageId>,
  maybe_importer: Option<UserId>,
  rules: &MentionRules<'_>,
) -> u64 {
  let mut hasher = DefaultHasher::new();
  for maybe_id in [maybe_mark, maybe_last_read, maybe_lines_read] {
    maybe_id.map(|id| id.snowflake().get()).hash(&mut hasher);
  }
  maybe_importer.map(|id| id.snowflake().get()).hash(&mut hasher);
  rules.username.hash(&mut hasher);
  rules.display_name.hash(&mut hasher);
  rules.first_name_is_mention.hash(&mut hasher);
  rules.channel_mentions_notify.hash(&mut hasher);
  rules.keywords.hash(&mut hasher);
  hasher.finish()
}

/// The kept tally of `conversation`. One that doesn't read counts as none: it is counted again.
pub(crate) fn kept_tally<S: LocalStore>(store: &S, conversation: ConversationId) -> Option<EntryTally> {
  records::get_json(store, Table::EntryTallies, &conversation.to_string())
    .ok()
    .flatten()
}

#[cfg(test)]
thread_local! {
  /// Passes over a conversation's whole feed on this thread.
  static WHOLE_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
#[cfg(all(feature = "sealed-sqlite", feature = "sqlcipher", not(target_arch = "wasm32")))]
mod tests {
  use std::path::PathBuf;
  use std::sync::Arc;
  use std::sync::atomic::{AtomicUsize, Ordering};

  use teamcraft_crypto::secret::SymmetricKey;
  use teamcraft_proto::{
    ChannelRole, ClientMessageId, ConversationResponse, ConversationVisibility, HistoryPolicy, MyMembership,
    NotificationLevel, Snowflake, UserKind, UserSummary,
  };

  use super::*;
  use crate::clock::ManualClock;
  use crate::imported::{ImportSource, ImportedAuthor, ImportedOrigin};
  use crate::notify_rules::MentionPreferences;
  use crate::store::{
    ConversationEvent, EditRecord, FeedChange, MemoryStore, MessageContent, MessageMeta, SealedSqliteStore,
    SqlCipherStore, StaticKeySource, StoreChanges, TrackedStore, Unreadable,
  };
  use crate::ui_backend::CoreChatBackend;
  use crate::ui_snapshot::SnapshotCache;
  use crate::workspace::WorkspaceState;

  const CONVERSATION: i64 = 10;
  const ME: i64 = 1;
  const PEER: i64 = 2;
  const IMPORTER: i64 = 9;
  const FIRST_ROWS: i64 = 6;

  /// One test on both stores that keep a version.
  macro_rules! on_each_store {
    ($($name:ident),+ $(,)?) => {
      mod on_each_store {
        use super::*;
        $(mod $name {
          use super::*;
          #[test]
          fn sealed() {
            let (store, _dir) = sealed_store();
            super::super::$name(Fixture::new(store));
          }
          #[test]
          fn sqlcipher() {
            super::super::$name(Fixture::new(sqlcipher_store()));
          }
        })+
      }
    };
  }

  on_each_store!(
    nothing_changed_reads_no_row,
    a_new_message_is_added_to_the_tally,
    a_thread_reply_or_reaction_changes_no_count,
    an_edit_decides_the_unread_rows_again,
    a_deleted_message_leaves_the_counts,
    a_removed_message_leaves_the_counts,
    a_decrypted_placeholder_starts_to_count,
    imported_history_counts_as_read_and_its_additions_stay_hidden,
    the_read_position_moves_either_way,
    mark_unread_counts_from_the_mark,
    mention_rules_decide_again,
    older_history_is_counted_in,
    a_pending_message_counts_but_is_not_kept,
    deleting_all_messages_starts_over,
    a_lost_tally_is_counted_again,
    a_damaged_tally_is_counted_again,
    a_tally_older_than_the_new_rows_catches_up,
    a_tally_from_before_a_rewrite_is_not_believed,
  );

  fn nothing_changed_reads_no_row<S: LocalStore>(mut fixture: Fixture<S>) {
    assert_eq!(fixture.check(), 1, "the first read counts every row");
    assert_eq!(fixture.check(), 0);
    fixture.reopen();
    assert_eq!(fixture.check(), 0, "a restart keeps the tally");
    assert_eq!(fixture.counts().messages, FIRST_ROWS as usize);
  }

  fn a_new_message_is_added_to_the_tally<S: LocalStore>(mut fixture: Fixture<S>) {
    fixture.check();
    fixture.write(&text(100, PEER, "new"));
    assert_eq!(fixture.check(), 0);
    assert_eq!(fixture.counts().messages, FIRST_ROWS as usize + 1);
    assert_eq!(fixture.counts().maybe_last_activity, Some(sent_at(100)));
    fixture.write(&text(101, PEER, "ping @alice"));
    assert_eq!(fixture.check(), 0);
    assert_eq!(fixture.counts().unread.mentions, 2);
  }

  fn a_thread_reply_or_reaction_changes_no_count<S: LocalStore>(mut fixture: Fixture<S>) {
    let before = fixture.counts();
    let mut reply = text(100, PEER, "@alice in a thread");
    reply.maybe_thread_id = Some(id(1001));
    fixture.write(&reply);
    let mut reaction = text(101, PEER, "");
    reaction.content = MessageContent::Reaction {
      target: client_id(1),
      emoji: "x".into(),
      is_added: true,
    };
    fixture.write(&reaction);
    assert_eq!(fixture.check(), 0);
    assert_eq!(fixture.counts().messages, before.messages);
    assert_eq!(fixture.counts().unread.messages, before.unread.messages);
  }

  fn an_edit_decides_the_unread_rows_again<S: LocalStore>(mut fixture: Fixture<S>) {
    let before = fixture.counts();
    fixture.write(&edited(5, "now it says @alice"));
    assert_eq!(
      fixture.check(),
      0,
      "an edit changes what a row says, not its place"
    );
    assert_eq!(fixture.counts().unread.mentions, before.unread.mentions + 1);
  }

  fn a_deleted_message_leaves_the_counts<S: LocalStore>(mut fixture: Fixture<S>) {
    let before = fixture.counts();
    let mut deleted = text(FIRST_ROWS, PEER, "");
    deleted.content = MessageContent::Deleted;
    fixture.write(&deleted);
    assert_eq!(fixture.check(), 1);
    let after = fixture.counts();
    assert_eq!(after.messages, before.messages - 1);
    assert_eq!(after.maybe_last_activity, Some(sent_at(FIRST_ROWS - 1)));
    assert_eq!(after.unread.messages, before.unread.messages - 1);
    assert_eq!(fixture.check(), 0);
  }

  fn a_removed_message_leaves_the_counts<S: LocalStore>(mut fixture: Fixture<S>) {
    let before = fixture.counts();
    fixture.remove(FIRST_ROWS);
    assert_eq!(fixture.check(), 1);
    assert_eq!(fixture.counts().messages, before.messages - 1);
    assert_eq!(fixture.check(), 0);
  }

  fn a_decrypted_placeholder_starts_to_count<S: LocalStore>(mut fixture: Fixture<S>) {
    fixture.check();
    let mut placeholder = text(100, PEER, "");
    placeholder.content = MessageContent::Unknown;
    placeholder.maybe_unreadable = Some(Unreadable::Waiting { since_ms: 1 });
    fixture.write(&placeholder);
    assert_eq!(fixture.check(), 0);
    let waiting = fixture.counts();
    assert_eq!(waiting.maybe_last_activity, Some(sent_at(FIRST_ROWS)));
    fixture.write(&text(100, PEER, "readable"));
    assert_eq!(fixture.check(), 1);
    assert_eq!(fixture.counts().messages, waiting.messages);
    assert_eq!(fixture.counts().unread.messages, waiting.unread.messages + 1);
    assert_eq!(fixture.counts().maybe_last_activity, Some(sent_at(100)));
  }

  fn imported_history_counts_as_read_and_its_additions_stay_hidden<S: LocalStore>(mut fixture: Fixture<S>) {
    fixture.set_importer();
    assert_eq!(fixture.check(), 1, "another import account decides anew");
    let before = fixture.counts();
    // After newer messages, the imported rows carry their older origin time.
    fixture.write(&imported(100));
    fixture.write(&imported(101));
    let added = event(
      102,
      IMPORTER,
      ConversationEvent::MemberAdded {
        user: id(PEER),
        history: HistoryPolicy::Full,
      },
    );
    fixture.write(&added);
    fixture.write(&event(103, PEER, ConversationEvent::Joined));
    assert_eq!(fixture.check(), 0);
    let after = fixture.counts();
    assert_eq!(after.messages, before.messages + 2);
    assert_eq!(after.unread.messages, before.unread.messages);
    assert_eq!(after.maybe_last_activity, before.maybe_last_activity);
    // Told after the additions it can hide, also when they were counted on an earlier read.
    fixture.write(&event(104, PEER, ConversationEvent::Joined));
    assert_eq!(fixture.check(), 0);
    assert_eq!(fixture.counts().messages, after.messages);
  }

  fn the_read_position_moves_either_way<S: LocalStore>(mut fixture: Fixture<S>) {
    fixture.check();
    fixture.set_last_read(Some(4));
    assert_eq!(fixture.check(), 0);
    assert_eq!(fixture.counts().unread.messages, 2);
    fixture.set_last_read(Some(FIRST_ROWS));
    assert_eq!(fixture.check(), 0);
    assert_eq!(fixture.counts().unread.messages, 0);
    fixture.write(&text(100, PEER, "new"));
    assert_eq!(fixture.check(), 0);
    assert_eq!(fixture.counts().unread.messages, 1);
    // A late, older position (another device's) still counts as told.
    fixture.set_last_read(Some(2));
    assert_eq!(fixture.check(), 0);
    assert_eq!(fixture.counts().unread.messages, 4);
  }

  fn mark_unread_counts_from_the_mark<S: LocalStore>(mut fixture: Fixture<S>) {
    fixture.set_last_read(Some(FIRST_ROWS));
    fixture.check();
    fixture.set_mark(Some(3));
    assert_eq!(fixture.check(), 0);
    // Its own rows count too once marked (row 3 is the user's).
    assert_eq!(fixture.counts().unread.messages, 4);
    fixture.set_mark(None);
    assert_eq!(fixture.check(), 0);
    assert_eq!(fixture.counts().unread.messages, 0);
  }

  fn mention_rules_decide_again<S: LocalStore>(mut fixture: Fixture<S>) {
    let before = fixture.counts();
    fixture.preferences.keywords = vec!["plain".into()];
    assert_eq!(fixture.check(), 0);
    assert!(fixture.counts().unread.mentions > before.unread.mentions);
  }

  fn older_history_is_counted_in<S: LocalStore>(mut fixture: Fixture<S>) {
    let before = fixture.counts();
    fixture.write(&text(0, PEER, "older, loaded later"));
    assert_eq!(fixture.check(), 1);
    assert_eq!(fixture.counts().messages, before.messages + 1);
    assert_eq!(fixture.check(), 0);
  }

  fn a_pending_message_counts_but_is_not_kept<S: LocalStore>(mut fixture: Fixture<S>) {
    let before = fixture.counts();
    let mut pending = text(100, ME, "on its way");
    pending.maybe_server_id = None;
    fixture.write(&pending);
    assert_eq!(fixture.check(), 0);
    assert_eq!(fixture.counts().messages, before.messages + 1);
    assert_eq!(
      fixture.check(),
      0,
      "still pending, and nothing was kept that it could change"
    );
    fixture.write(&text(100, ME, "on its way"));
    assert_eq!(fixture.check(), 0, "sent: its place is after the tally");
    assert_eq!(fixture.counts().messages, before.messages + 1);
    assert_eq!(fixture.counts().unread.messages, before.unread.messages);
    fixture.write(&text(101, PEER, "after it"));
    let mut failed = text(102, ME, "never sent");
    failed.maybe_server_id = None;
    fixture.write(&failed);
    fixture.check();
    fixture.remove(102);
    assert_eq!(
      fixture.check(),
      0,
      "dropping a pending message changes nothing kept"
    );
    assert_eq!(fixture.counts().messages, before.messages + 2);
  }

  fn deleting_all_messages_starts_over<S: LocalStore>(mut fixture: Fixture<S>) {
    fixture.check();
    fixture.remove_all();
    assert_eq!(fixture.check(), 1);
    assert_eq!(fixture.counts().messages, 0);
    assert_eq!(fixture.counts().maybe_last_activity, None);
    fixture.write(&text(1, PEER, "the same place as before"));
    assert_eq!(fixture.check(), 0);
    assert_eq!(fixture.counts().messages, 1);
  }

  fn a_lost_tally_is_counted_again<S: LocalStore>(mut fixture: Fixture<S>) {
    fixture.check();
    fixture
      .store
      .delete(Table::EntryTallies, &conversation().to_string())
      .unwrap();
    assert_eq!(fixture.check(), 1);
    assert_eq!(fixture.check(), 0, "and kept again");
  }

  fn a_damaged_tally_is_counted_again<S: LocalStore>(mut fixture: Fixture<S>) {
    fixture.check();
    fixture
      .store
      .put(Table::EntryTallies, &conversation().to_string(), b"not a tally")
      .unwrap();
    assert_eq!(fixture.check(), 1);
    assert_eq!(fixture.check(), 0);
  }

  /// A crash after the messages' group and before the tally's.
  fn a_tally_older_than_the_new_rows_catches_up<S: LocalStore>(mut fixture: Fixture<S>) {
    fixture.check();
    let tally = fixture.tally_bytes();
    fixture.write(&text(100, PEER, "written, then the crash"));
    fixture.check();
    fixture.restore_tally(&tally);
    assert_eq!(fixture.check(), 0, "the row after it is added");
    assert_eq!(fixture.counts().messages, FIRST_ROWS as usize + 1);
  }

  fn a_tally_from_before_a_rewrite_is_not_believed<S: LocalStore>(mut fixture: Fixture<S>) {
    fixture.check();
    let tally = fixture.tally_bytes();
    let mut deleted = text(2, PEER, "");
    deleted.content = MessageContent::Deleted;
    fixture.write(&deleted);
    fixture.write(&text(100, PEER, "keeps the number of rows the same"));
    fixture.check();
    fixture.restore_tally(&tally);
    assert_eq!(fixture.check(), 1);
    fixture.write(&edited(5, "@alice edited after the tally"));
    fixture.check();
    let edited_tally = fixture.tally_bytes();
    let mut again = edited(5, "edited again, no mention any more");
    again.edits.push(EditRecord {
      body: "@alice edited after the tally".into(),
      replaced_at_ms: 2,
    });
    fixture.write(&again);
    fixture.check();
    fixture.restore_tally(&edited_tally);
    assert_eq!(fixture.check(), 0, "only the unread counts are decided again");
  }

  mod kept_in_the_store {
    use super::*;

    #[test]
    fn a_snapshot_keeps_what_it_counted() {
      let (store, _dir) = sealed_store();
      let fixture = Fixture::new(store);
      let direct = ConversationResponse {
        kind: teamcraft_proto::ConversationKind::Dm,
        ..response()
      };
      records::put_json(
        fixture.store.as_ref(),
        Table::Conversations,
        &conversation().to_string(),
        &direct,
      )
      .unwrap();
      let before = WHOLE_PASSES.with(std::cell::Cell::get);
      let mut cache = SnapshotCache::new(1);
      let snapshot = cache.snapshot(&fixture.backend, StoreChanges::default(), || None);
      assert_eq!(snapshot.message_counts, [(conversation(), FIRST_ROWS as usize)]);
      assert_eq!(WHOLE_PASSES.with(std::cell::Cell::get) - before, 1);
      assert!(kept_tally(&fixture.store, conversation()).is_some());
      // The cache of the next start.
      let mut next = SnapshotCache::new(2);
      let again = next.snapshot(&fixture.backend, StoreChanges::default(), || None);
      assert_eq!(again.message_counts, [(conversation(), FIRST_ROWS as usize)]);
      assert_eq!(WHOLE_PASSES.with(std::cell::Cell::get) - before, 1);
    }

    #[test]
    fn a_conversation_that_goes_away_takes_its_tally_along() {
      for remove in [
        |store: &TrackedStore<MemoryStore>| store.delete_messages(conversation()),
        |store: &TrackedStore<MemoryStore>| store.delete(Table::Conversations, &conversation().to_string()),
      ] {
        let store = TrackedStore::new(MemoryStore::new());
        store
          .put(Table::EntryTallies, &conversation().to_string(), b"{}")
          .unwrap();
        remove(&store).unwrap();
        assert_eq!(
          store
            .get(Table::EntryTallies, &conversation().to_string())
            .unwrap(),
          None
        );
      }
    }

    #[test]
    fn a_tally_write_tells_no_reader_of_a_change() {
      let store = TrackedStore::new(MemoryStore::new());
      store.take_changes();
      store
        .put(Table::EntryTallies, &conversation().to_string(), b"{}")
        .unwrap();
      assert_eq!(store.take_changes(), StoreChanges::default());
    }
  }

  mod sealed_only {
    use super::*;

    #[test]
    fn the_version_outlives_a_restart() {
      let (store, dir) = sealed_store();
      let mut fixture = Fixture::new(store);
      fixture.write(&edited(5, "edited"));
      let version = fixture.store.feed_version(conversation()).unwrap().unwrap();
      assert_eq!(version.edited, 1);
      drop(fixture);
      let reopened = open_sealed(&dir.0);
      assert_eq!(reopened.feed_version(conversation()).unwrap(), Some(version));
    }
  }

  mod feed_change {
    use super::*;

    #[test]
    fn says_what_a_rewrite_changes() {
      let row = text(5, PEER, "text");
      let mut edited = row.clone();
      edited.content = text(5, PEER, "other").content;
      assert_eq!(FeedChange::of(&row, Some(&row)), None);
      assert_eq!(FeedChange::of(&row, Some(&edited)), Some(FeedChange::Edited));
      assert_eq!(FeedChange::of(&row, None), Some(FeedChange::Reshaped));
      let mut tombstone = row.clone();
      tombstone.content = MessageContent::Deleted;
      assert_eq!(FeedChange::of(&row, Some(&tombstone)), Some(FeedChange::Reshaped));
      assert_eq!(FeedChange::of(&tombstone, Some(&row)), Some(FeedChange::Reshaped));
      assert_eq!(FeedChange::of(&tombstone, None), None);
      let mut moved = row.clone();
      moved.maybe_server_id = Some(id(99_999));
      assert_eq!(FeedChange::of(&row, Some(&moved)), Some(FeedChange::Reshaped));
      let mut retimed = row.clone();
      retimed.sent_at_ms += 1;
      assert_eq!(FeedChange::of(&row, Some(&retimed)), Some(FeedChange::Reshaped));
      let mut placeholder = row.clone();
      placeholder.maybe_unreadable = Some(Unreadable::NotAvailable);
      assert_eq!(
        FeedChange::of(&placeholder, Some(&row)),
        Some(FeedChange::Reshaped)
      );
    }

    #[test]
    fn a_pending_message_and_a_row_of_no_feed_change_nothing() {
      let mut pending = text(5, ME, "text");
      pending.maybe_server_id = None;
      assert_eq!(FeedChange::of(&pending, None), None);
      assert_eq!(FeedChange::of(&pending, Some(&text(5, ME, "text"))), None);
      let mut reply = text(6, PEER, "reply");
      reply.maybe_thread_id = Some(id(1001));
      let mut changed = reply.clone();
      changed.content = text(6, PEER, "other").content;
      assert_eq!(FeedChange::of(&reply, Some(&changed)), None);
    }

    #[test]
    fn an_event_is_never_just_an_edit() {
      let line = event(5, PEER, ConversationEvent::Joined);
      let mut changed = line.clone();
      changed.is_pinned = true;
      assert_eq!(FeedChange::of(&line, Some(&changed)), Some(FeedChange::Reshaped));
    }
  }

  /// The same messages in a store under test and in memory, where every read still counts all rows:
  /// what the two say must not differ, whatever happened.
  struct Fixture<S: LocalStore> {
    store: Arc<S>,
    backend: CoreChatBackend<Arc<S>>,
    mirror: CoreChatBackend<Arc<MemoryStore>>,
    mirror_store: Arc<MemoryStore>,
    response: ConversationResponse,
    maybe_mark: Option<MessageId>,
    preferences: MentionPreferences,
  }

  impl<S: LocalStore> Fixture<S> {
    /// Six rows, the third the user's, the fifth with no mention yet and the first mentioning them.
    fn new(store: Arc<S>) -> Self {
      let mirror_store = Arc::new(MemoryStore::new());
      let mut fixture = Self {
        backend: backend_on(store.clone()),
        mirror: backend_on(mirror_store.clone()),
        store,
        mirror_store,
        response: response(),
        maybe_mark: None,
        preferences: MentionPreferences::default(),
      };
      fixture.write(&text(1, PEER, "ping @alice"));
      for n in 2..=FIRST_ROWS {
        let sender = if n == 3 { ME } else { PEER };
        fixture.write(&text(n, sender, "plain"));
      }
      fixture.maybe_mark = None;
      fixture
    }

    /// Reads the counts and compares them with the mirror's. Returns the whole passes it took.
    fn check(&mut self) -> usize {
      let before = WHOLE_PASSES.with(std::cell::Cell::get);
      let counts = self.kept_counts();
      self.backend.flush_entry_tallies();
      let passes = WHOLE_PASSES.with(std::cell::Cell::get) - before;
      let expected =
        self
          .mirror
          .entry_counts(&self.response, self.maybe_mark, Some(&me()), &self.preferences);
      assert_eq!(counts.messages, expected.messages);
      assert_eq!(counts.maybe_last_activity, expected.maybe_last_activity);
      assert_eq!(counts.unread.messages, expected.unread.messages);
      assert_eq!(counts.unread.mentions, expected.unread.mentions);
      passes
    }

    fn counts(&self) -> EntryCounts {
      let counts = self.kept_counts();
      self.backend.flush_entry_tallies();
      counts
    }

    fn kept_counts(&self) -> EntryCounts {
      self
        .backend
        .entry_counts(&self.response, self.maybe_mark, Some(&me()), &self.preferences)
    }

    fn reopen(&mut self) {
      self.backend = backend_on(self.store.clone());
    }

    fn write(&mut self, message: &StoredMessage) {
      self.store.upsert_message(message).unwrap();
      self.mirror_store.upsert_message(message).unwrap();
    }

    fn remove(&mut self, n: i64) {
      self.store.delete_message(conversation(), client_id(n)).unwrap();
      self
        .mirror_store
        .delete_message(conversation(), client_id(n))
        .unwrap();
    }

    fn remove_all(&mut self) {
      self.store.delete_messages(conversation()).unwrap();
      self.mirror_store.delete_messages(conversation()).unwrap();
    }

    fn set_importer(&mut self) {
      let mut state = WorkspaceState::default();
      state.policy.import.maybe_importer = Some(id(IMPORTER));
      for result in [
        records::put_json(
          self.store.as_ref(),
          Table::Workspace,
          crate::client::WORKSPACE_KEY,
          &state,
        ),
        records::put_json(
          self.mirror_store.as_ref(),
          Table::Workspace,
          crate::client::WORKSPACE_KEY,
          &state,
        ),
      ] {
        result.unwrap();
      }
    }

    fn set_last_read(&mut self, maybe_n: Option<i64>) {
      self.response.maybe_my_membership = Some(MyMembership {
        maybe_last_read_message_id: maybe_n.map(server_id),
        ..membership()
      });
    }

    fn set_mark(&mut self, maybe_n: Option<i64>) {
      self.maybe_mark = maybe_n.map(server_id);
    }

    fn tally_bytes(&self) -> Vec<u8> {
      self
        .store
        .get(Table::EntryTallies, &conversation().to_string())
        .unwrap()
        .unwrap()
    }

    fn restore_tally(&self, bytes: &[u8]) {
      self
        .store
        .put(Table::EntryTallies, &conversation().to_string(), bytes)
        .unwrap();
    }
  }

  fn backend_on<T: LocalStore>(store: T) -> CoreChatBackend<T> {
    CoreChatBackend::new(store, id(ME), Box::new(ManualClock::new(0)))
  }

  /// Keeps a store's folder until the test ends.
  struct ScratchDir(PathBuf);

  impl Drop for ScratchDir {
    fn drop(&mut self) {
      // Left behind in the system's temporary folder at worst.
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }

  fn sealed_store() -> (Arc<SealedSqliteStore>, ScratchDir) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "teamcraft-entry-tally-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    (Arc::new(open_sealed(&dir)), ScratchDir(dir))
  }

  fn open_sealed(dir: &std::path::Path) -> SealedSqliteStore {
    SealedSqliteStore::open(
      &dir.join("store.db"),
      &StaticKeySource::new(SymmetricKey::from_bytes([3; 32])),
    )
    .unwrap()
  }

  fn sqlcipher_store() -> Arc<SqlCipherStore> {
    Arc::new(
      SqlCipherStore::open_in_memory(&StaticKeySource::new(SymmetricKey::from_bytes([4; 32]))).unwrap(),
    )
  }

  fn text(n: i64, sender: i64, body: &str) -> StoredMessage {
    StoredMessage {
      conversation_id: conversation(),
      client_message_id: client_id(n),
      maybe_server_id: Some(server_id(n)),
      sender_user_id: id(sender),
      maybe_thread_id: None,
      sent_at_ms: sent_at(n),
      content: MessageContent::Text {
        body: body.into(),
        mentions: Vec::new(),
        meta: MessageMeta::default(),
      },
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
      maybe_unreadable: None,
    }
  }

  /// A peer's row `n` with new text.
  fn edited(n: i64, body: &str) -> StoredMessage {
    let mut message = text(n, PEER, body);
    message.is_edited = true;
    message.edits.push(EditRecord {
      body: "plain".into(),
      replaced_at_ms: 1,
    });
    message
  }

  fn imported(n: i64) -> StoredMessage {
    let mut message = text(n, IMPORTER, "from the old system");
    // Stored at its origin's time, long before the newest message's.
    message.sent_at_ms = 1;
    message.content = MessageContent::Text {
      body: "from the old system".into(),
      mentions: Vec::new(),
      meta: MessageMeta {
        maybe_imported: Some(Box::new(ImportedOrigin {
          source: ImportSource::Synology,
          source_id: n.to_string(),
          author: ImportedAuthor::User { user_id: id(PEER) },
          sent_at_ms: 1,
          maybe_edited_at_ms: None,
          reactions: Vec::new(),
          missing_files: Vec::new(),
        })),
        ..MessageMeta::default()
      },
    };
    message
  }

  fn event(n: i64, sender: i64, event: ConversationEvent) -> StoredMessage {
    StoredMessage {
      content: MessageContent::Event { event },
      ..text(n, sender, "")
    }
  }

  fn response() -> ConversationResponse {
    ConversationResponse {
      id: conversation(),
      maybe_space_id: None,
      kind: teamcraft_proto::ConversationKind::Channel,
      maybe_name: Some("general".into()),
      maybe_topic: None,
      maybe_description: None,
      visibility: ConversationVisibility::Public,
      history_policy: HistoryPolicy::Full,
      maybe_category_id: None,
      is_archived: false,
      moderation: Default::default(),
      is_integration: false,
      contains_guests: false,
      maybe_organizer_user_id: None,
      maybe_my_permissions: None,
      maybe_importer_user_id: None,
      maybe_imported_through: None,
      maybe_my_membership: None,
      participants: Vec::new(),
      maybe_image: None,
    }
  }

  fn membership() -> MyMembership {
    serde_json::from_value(serde_json::json!({
      "role": ChannelRole::Member,
      "notification_level": NotificationLevel::All,
      "is_favorite": false,
    }))
    .unwrap()
  }

  fn me() -> UserSummary {
    UserSummary {
      id: id(ME),
      maybe_username: Some("alice".into()),
      display_name: "Alice Example".into(),
      kind: UserKind::Member,
      maybe_email: None,
      maybe_organization: None,
      maybe_has_devices: None,
      is_deactivated: false,
      is_deleted: false,
    }
  }

  fn conversation() -> ConversationId {
    id(CONVERSATION)
  }

  fn client_id(n: i64) -> ClientMessageId {
    let mut bytes = [0xab; 16];
    bytes[..8].copy_from_slice(&n.to_be_bytes());
    ClientMessageId::from_bytes(bytes)
  }

  /// Ascending with `n`; `n` 0 is older than the rows a fixture starts with.
  fn server_id(n: i64) -> MessageId {
    id(1_000 + n)
  }

  fn sent_at(n: i64) -> TimestampMs {
    1_000 * (n + 1)
  }

  fn id<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
