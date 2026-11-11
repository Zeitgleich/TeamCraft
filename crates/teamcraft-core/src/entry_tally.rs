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
//! | Since the tally was written                     | Read                                   |
//! |-------------------------------------------------|----------------------------------------|
//! | Nothing                                         | The tally, the feed index's positions  |
//! | New messages at the end                         | Those rows                             |
//! | A read position, a mark, mention rules, an edit | The rows from the read position        |
//! | A row left or moved, older rows came in         | All rows; the rows from the read       |
//! |                                                 | position for the unread counts alone   |
//!
//! A chat list needs the unread counts alone ([`EntryNeed::Unread`]): history older than the read
//! position is never read for it, also when a sync rewrote it (issue #1466).
//!
//! Stores without a feed version (the in-memory one) count all rows every time.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use teamcraft_client_api::model::TimestampMs;
use teamcraft_client_api::notify::MentionRules;
use teamcraft_proto::{ClientMessageId, ConversationId, Id, MessageId, Snowflake, UserId};

use crate::imported::ImportAdditions;
use crate::store::{
  FeedVersion, LocalStore, MessageFilter, MessageOrder, StoreError, StoredMessage, Table, Written,
};
use crate::ui_backend::{EntryCounts, MentionMemo, UnreadCounts, is_deleted};

/// Rows of the feed read at a time.
const ROWS_PER_READ: usize = 256;
/// The sort group of messages with a place on the server's clock ([`StoredMessage::sort_key`]);
/// pending ones come after them, change without notice and are never kept.
const PLACED_GROUP: u8 = 0;
/// What [`EntryTally::to_bytes`] writes first. Another one reads as no tally, so it is counted again.
const TALLY_FORMAT: u64 = 1;
/// The same record for a tally of the unread counts alone ([`EntryTally::is_unread_only`]): an
/// older app reads it as none and counts again.
const UNREAD_ONLY_FORMAT: u64 = 2;
/// The newest rows the unread counts alone are read from at most: beyond them a badge says "99+"
/// anyway, and a conversation never read (an import) holds tens of thousands (issue #1466).
#[cfg(not(test))]
const UNREAD_WINDOW_ROWS: usize = 2_000;
#[cfg(test)]
const UNREAD_WINDOW_ROWS: usize = 4;
/// The numbers of a tally's record.
const TALLY_WORDS: usize = 13;
/// A tally's record: the numbers and the last counted row's client message ID.
const TALLY_BYTES: usize = TALLY_WORDS * 8 + 16;

/// The counts of a conversation's rows up to [`Self::maybe_through`], as they were at
/// [`Self::version`]. Sealed like every record; says nothing the messages don't. Stored as plain
/// numbers: the web app's download budget has no room for what deriving a JSON form adds, and a
/// guest's meeting carries this code too.
#[derive(Clone, Debug, PartialEq, Eq)]
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
  /// Only the unread counts hold, read from the newest rows ([`EntryNeed::Unread`]); a caller that
  /// needs everything counts again.
  is_unread_only: bool,
  unread_messages: u32,
  unread_mentions: u32,
}

/// The last row a tally counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// What a caller needs of a conversation's counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryNeed {
  /// Message count, last activity and unread counts (a snapshot whose feed knows its length).
  Everything,
  /// The unread counts alone (a chat list): older history than the read position is never read
  /// for them, also when it was rewritten (issue #1466).
  Unread,
}

/// The unread rows a scan counted, and whether each mentions the user: kept by a reader that then
/// follows the store's journal of writes instead of scanning again ([`LocalStore::written_since`]).
pub(crate) type UnreadRows = HashMap<ClientMessageId, bool>;

/// What [`counted`] found.
pub(crate) enum Counted {
  /// All counts, and the tally that keeps them.
  Kept(EntryCounts, EntryTally),
  /// The unread counts alone, read from the newest rows, and a tally of them to keep (none when
  /// the feed holds pending rows only).
  Scanned(UnreadCounts, Option<EntryTally>),
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
    let _ = self.take_unread(message);
  }

  /// Counts `message` if it is unread; whether it mentions the user, then.
  fn take_unread(&mut self, message: &StoredMessage) -> Option<bool> {
    if !(self.is_unread)(message) {
      return None;
    }
    self.counts.unread.messages = self.counts.unread.messages.saturating_add(1);
    let is_mention = self.memo.is_mention(message, self.me, self.rules);
    if is_mention {
      self.counts.unread.mentions = self.counts.unread.mentions.saturating_add(1);
    }
    Some(is_mention)
  }

  /// Before an event in the middle of a conversation: the additions told of before it.
  fn note_earlier_events(
    &mut self,
    store: &dyn LocalStore,
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
  fn holds(
    &self,
    store: &dyn LocalStore,
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

  pub(crate) fn to_bytes(&self) -> Vec<u8> {
    let (through_value, through_id) = self
      .maybe_through
      .map_or((0, [0; 16]), |through| (through.value, through.client_message_id));
    let words: [u64; TALLY_WORDS] = [
      if self.is_unread_only {
        UNREAD_ONLY_FORMAT
      } else {
        TALLY_FORMAT
      },
      self.version.generation,
      self.version.reshaped,
      self.version.edited,
      self
        .maybe_importer
        .map_or(0, |user| user.snowflake().get().cast_unsigned()),
      u64::from(self.maybe_through.is_some()),
      through_value.cast_unsigned(),
      self.rows as u64,
      self.messages as u64,
      u64::from(self.maybe_last_activity.is_some()),
      self.maybe_last_activity.unwrap_or(0).cast_unsigned(),
      self.unread_key,
      u64::from(self.unread_messages) << 32 | u64::from(self.unread_mentions),
    ];
    let mut bytes = Vec::with_capacity(TALLY_BYTES);
    for word in words {
      bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.extend_from_slice(&through_id);
    bytes
  }

  /// `None` for anything [`Self::to_bytes`] didn't write.
  fn from_bytes(bytes: &[u8]) -> Option<Self> {
    let bytes: &[u8; TALLY_BYTES] = bytes.try_into().ok()?;
    let (numbers, through_id) = bytes.split_at(TALLY_WORDS * 8);
    let mut words = [0u64; TALLY_WORDS];
    for (word, number) in words.iter_mut().zip(numbers.as_chunks::<8>().0) {
      *word = u64::from_le_bytes(*number);
    }
    let is_unread_only = match words[0] {
      TALLY_FORMAT => false,
      UNREAD_ONLY_FORMAT => true,
      _ => return None,
    };
    let maybe_importer = match words[4] {
      0 => None,
      raw => Some(UserId::from_snowflake(Snowflake::new(raw.cast_signed()).ok()?)),
    };
    Some(Self {
      version: FeedVersion {
        generation: words[1],
        reshaped: words[2],
        edited: words[3],
      },
      maybe_importer,
      maybe_through: (words[5] != 0).then_some(Through {
        value: words[6].cast_signed(),
        client_message_id: through_id.try_into().ok()?,
      }),
      rows: words[7] as usize,
      messages: words[8] as usize,
      maybe_last_activity: (words[9] != 0).then_some(words[10].cast_signed()),
      unread_key: words[11],
      unread_messages: (words[12] >> 32) as u32,
      unread_mentions: words[12] as u32,
      is_unread_only,
    })
  }
}

/// The counts of `conversation`, from `maybe_kept` where it holds. Where it doesn't, the unread
/// counts alone ([`EntryNeed::Unread`]) are taken from the rows from the read position on, so a
/// rewrite of older history (older messages coming in, a placeholder decrypted, a deletion) reads
/// none of it, and a tally of them alone is kept.
pub(crate) fn counted(
  store: &dyn LocalStore,
  conversation: ConversationId,
  inputs: &TallyInputs,
  maybe_kept: Option<EntryTally>,
  need: EntryNeed,
  mut pass: EntryPass<'_>,
  maybe_rows: Option<&mut UnreadRows>,
) -> Result<Counted, StoreError> {
  let rows_now = store.feed_len(conversation)?;
  let maybe_holding = match maybe_kept {
    Some(kept) if kept.holds(store, conversation, inputs, rows_now)? => Some(kept),
    _ => None,
  };
  // The unread counts alone, decided anew (another read position): the scan that keeps their rows
  // for the journal reads the same rows a kept tally would.
  let maybe_holding = maybe_holding.filter(|kept| match need {
    EntryNeed::Everything => !kept.is_unread_only,
    EntryNeed::Unread => kept.unread_key == inputs.unread_key,
  });
  if let Some(kept) = maybe_holding {
    let (counts, tally) = counted_from(store, conversation, inputs, kept, rows_now, pass)?;
    return Ok(Counted::Kept(counts, tally));
  }
  match need {
    EntryNeed::Everything => {
      let mut progress = Progress::default();
      counted_whole(store, conversation, &mut pass, &mut progress)?;
      let (counts, tally) = progress.finish(inputs, pass.counts);
      Ok(Counted::Kept(counts, tally))
    },
    EntryNeed::Unread => counted_unread(store, conversation, inputs, rows_now, pass, maybe_rows),
  }
}

/// The unread counts alone from the newest rows, and a tally that keeps just them. Without a placed
/// row to keep them up to (only pending ones), nothing is kept.
fn counted_unread(
  store: &dyn LocalStore,
  conversation: ConversationId,
  inputs: &TallyInputs,
  rows_now: usize,
  mut pass: EntryPass<'_>,
  maybe_rows: Option<&mut UnreadRows>,
) -> Result<Counted, StoreError> {
  let (unread, maybe_read_last) =
    unread_between(store, conversation, inputs, rows_now, true, &mut pass, maybe_rows)?;
  let maybe_last_placed = match maybe_read_last {
    Some(last) => Some(last),
    None => last_placed(store, conversation, rows_now)?,
  };
  Ok(Counted::Scanned(
    unread,
    unread_only_tally(inputs, unread, rows_now, maybe_last_placed),
  ))
}

/// A tally of the unread counts alone as they are now ([`EntryNeed::Unread`]), kept up to the
/// newest placed row; `None` without one to keep them by (only pending rows).
pub(crate) fn unread_kept_now(
  store: &dyn LocalStore,
  conversation: ConversationId,
  inputs: &TallyInputs,
  unread: UnreadCounts,
) -> Result<Option<EntryTally>, StoreError> {
  let rows_now = store.feed_len(conversation)?;
  let maybe_last_placed = last_placed(store, conversation, rows_now)?;
  Ok(unread_only_tally(inputs, unread, rows_now, maybe_last_placed))
}

fn unread_only_tally(
  inputs: &TallyInputs,
  unread: UnreadCounts,
  rows_now: usize,
  maybe_last_placed: Option<(Through, usize)>,
) -> Option<EntryTally> {
  if rows_now > 0 && maybe_last_placed.is_none() {
    return None;
  }
  let (maybe_through, rows) = maybe_last_placed.map_or((None, 0), |(through, rows)| (Some(through), rows));
  Some(EntryTally {
    version: inputs.version,
    maybe_importer: inputs.maybe_importer,
    maybe_through,
    rows,
    messages: 0,
    maybe_last_activity: None,
    unread_key: inputs.unread_key,
    unread_messages: unread.messages,
    unread_mentions: unread.mentions,
    is_unread_only: true,
  })
}

/// The unread counts of `rows` (the unread rows a scan counted) after `written`, what the store's
/// journal says was written since: each message's part is decided again from what it is now, so
/// a rewrite costs a read of that message, whatever the conversation's size. `None` where a
/// message alone can't tell (an event, which can hide the import's additions; all of it gone):
/// the caller scans again.
pub(crate) fn followed_unread(
  store: &dyn LocalStore,
  conversation: ConversationId,
  rows: &mut UnreadRows,
  written: &[Written],
  pass: &mut EntryPass<'_>,
) -> Result<Option<UnreadCounts>, StoreError> {
  for change in written {
    let Written::Message(id) = change else {
      return Ok(None);
    };
    rows.remove(id);
    let Some(message) = store.message(conversation, *id)? else {
      continue;
    };
    if !message.is_feed_row() {
      continue;
    }
    if message.is_event() {
      return Ok(None);
    }
    if let Some(is_mention) = pass.take_unread(&message) {
      rows.insert(*id, is_mention);
    }
  }
  let mentions = rows.values().filter(|is_mention| **is_mention).count();
  Ok(Some(UnreadCounts {
    messages: u32::try_from(rows.len()).unwrap_or(u32::MAX),
    mentions: u32::try_from(mentions).unwrap_or(u32::MAX),
  }))
}

/// The newest placed row of the feed's first `rows_now` rows and its place, among the last few:
/// pending rows sort after every placed one.
fn last_placed(
  store: &dyn LocalStore,
  conversation: ConversationId,
  rows_now: usize,
) -> Result<Option<(Through, usize)>, StoreError> {
  // The newest row first: it is placed unless a message is on its way.
  for len in [1, ROWS_PER_READ] {
    let start = rows_now.saturating_sub(len);
    let rows = store.feed_rows(conversation, start, rows_now - start)?;
    let maybe_found = rows
      .iter()
      .enumerate()
      .rev()
      .find(|(_, row)| row.order().group == PLACED_GROUP)
      .map(|(offset, row)| (Through::of(row.order()), start + offset + 1));
    if maybe_found.is_some() || start == 0 {
      return Ok(maybe_found);
    }
  }
  Ok(None)
}

/// The counts of `conversation` in a store that keeps no version: all its rows, every time.
pub(crate) fn counted_unkept(
  store: &dyn LocalStore,
  conversation: ConversationId,
  mut pass: EntryPass<'_>,
) -> Result<EntryCounts, StoreError> {
  counted_whole(store, conversation, &mut pass, &mut Progress::default())?;
  Ok(pass.counts)
}

fn counted_whole(
  store: &dyn LocalStore,
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
      is_unread_only: false,
    };
    (counts, tally)
  }
}

/// `kept` holds: its counts, with the unread ones decided again if what decides them changed, and
/// the rows after it.
fn counted_from(
  store: &dyn LocalStore,
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
    unread_between(
      store,
      conversation,
      inputs,
      kept.rows,
      kept.is_unread_only,
      &mut pass,
      None,
    )?
    .0
  };
  let is_unread_only = kept.is_unread_only;
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
  let (counts, tally) = progress.finish(inputs, pass.counts);
  Ok((
    counts,
    EntryTally {
      is_unread_only,
      ..tally
    },
  ))
}

/// The unread counts of the feed's first `end` rows: only those from the read position on are read,
/// and `is_windowed` only the newest [`UNREAD_WINDOW_ROWS`] of them. Also the last placed row read
/// and its place, where one was.
fn unread_between(
  store: &dyn LocalStore,
  conversation: ConversationId,
  inputs: &TallyInputs,
  end: usize,
  is_windowed: bool,
  pass: &mut EntryPass<'_>,
  mut maybe_rows: Option<&mut UnreadRows>,
) -> Result<(UnreadCounts, Option<(Through, usize)>), StoreError> {
  let from_boundary = match inputs.maybe_boundary {
    Some(boundary) => store
      .feed_ranks(conversation, &[boundary])?
      .first()
      .copied()
      .unwrap_or(0),
    None => 0,
  };
  let mut at = if is_windowed {
    from_boundary.max(end.saturating_sub(UNREAD_WINDOW_ROWS))
  } else {
    from_boundary
  };
  let mut maybe_last_placed = None;
  pass.counts = EntryCounts::default();
  while at < end {
    let rows = store.feed_rows(conversation, at, ROWS_PER_READ.min(end - at))?;
    if rows.is_empty() {
      break;
    }
    let first = at;
    at += rows.len();
    #[cfg(test)]
    UNREAD_ROWS.with(|read| read.set(read.get() + rows.len()));
    for (offset, row) in rows.iter().enumerate() {
      let order = row.order();
      if order.group == PLACED_GROUP {
        maybe_last_placed = Some((Through::of(order), first + offset + 1));
      }
      if row.is_event() {
        pass.note_earlier_events(store, conversation, row.order())?;
      }
      if !pass.additions.note(row)
        && let Some(is_mention) = pass.take_unread(row)
        && let Some(rows) = maybe_rows.as_deref_mut()
      {
        rows.insert(row.client_message_id, is_mention);
      }
    }
  }
  Ok((pass.counts.unread, maybe_last_placed))
}

/// The oldest unread row of the feed, where the "new messages" line goes. Only rows from the read
/// position on are read, up to the first unread one, and of a conversation never read (an import)
/// only the newest [`UNREAD_WINDOW_ROWS`], like the chat list's counts: opening a conversation no
/// longer reads (and decrypts) its whole history to place the line (TEAMCRAFT-1D).
pub(crate) fn first_unread_row(
  store: &dyn LocalStore,
  conversation: ConversationId,
  maybe_boundary: Option<MessageOrder>,
  maybe_importer: Option<UserId>,
  is_unread: &dyn Fn(&StoredMessage) -> bool,
) -> Result<Option<StoredMessage>, StoreError> {
  let end = store.feed_len(conversation)?;
  let mut at = match maybe_boundary {
    Some(boundary) => store
      .feed_ranks(conversation, &[boundary])?
      .first()
      .copied()
      .unwrap_or(0),
    None => end.saturating_sub(UNREAD_WINDOW_ROWS),
  };
  let mut additions = ImportAdditions::new(maybe_importer);
  // Without an import account there are no additions to know of.
  let mut has_earlier_events = maybe_importer.is_none();
  while at < end {
    let rows = store.feed_rows(conversation, at, ROWS_PER_READ.min(end - at))?;
    if rows.is_empty() {
      break;
    }
    at += rows.len();
    #[cfg(test)]
    UNREAD_ROWS.with(|read| read.set(read.get() + rows.len()));
    for row in rows {
      if row.is_event() && !has_earlier_events {
        has_earlier_events = true;
        for event in store.event_messages(conversation)? {
          if event.is_feed_row() && event.order() < row.order() {
            additions.note(&event);
          }
        }
      }
      if !additions.note(&row) && is_unread(&row) {
        return Ok(Some(row));
      }
    }
  }
  Ok(None)
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
pub(crate) fn kept_tally(store: &dyn LocalStore, conversation: ConversationId) -> Option<EntryTally> {
  let bytes = store.get(Table::EntryTallies, &conversation.to_string()).ok()??;
  EntryTally::from_bytes(&bytes)
}

#[cfg(test)]
thread_local! {
  /// Passes over a conversation's whole feed on this thread.
  static WHOLE_PASSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
  /// Rows read for the unread counts from the read position on, on this thread.
  static UNREAD_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
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
    NotificationLevel, UserKind, UserSummary,
  };

  use super::*;
  use crate::clock::ManualClock;
  use crate::imported::{ImportSource, ImportedAuthor, ImportedOrigin};
  use crate::notify_rules::MentionPreferences;
  use crate::records;
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
      maybe_custom: None,
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

  /// TEAMCRAFT-1D: the "new messages" line read and decrypted the whole conversation.
  mod first_unread {
    use super::*;

    #[test]
    fn reads_from_the_read_position_up_to_the_first_unread_row() {
      let (store, _dir) = sealed_store();
      let mut fixture = Fixture::new(store);
      for n in 7..=20 {
        let sender = if n == 16 { ME } else { PEER };
        fixture.write(&text(n, sender, "plain"));
      }
      fixture.set_last_read(Some(15));
      let before = UNREAD_ROWS.with(std::cell::Cell::get);
      let first = fixture.backend.first_unread_message(&fixture.response);
      assert_eq!(
        UNREAD_ROWS.with(std::cell::Cell::get) - before,
        5,
        "only the rows after the read position, in one page"
      );
      let expected = fixture.mirror.first_unread_message(&fixture.response);
      assert_eq!(first.map(|row| row.client_message_id), Some(client_id(17)));
      assert_eq!(expected.map(|row| row.client_message_id), Some(client_id(17)));
    }

    #[test]
    fn everything_read_has_none() {
      let (store, _dir) = sealed_store();
      let mut fixture = Fixture::new(store);
      fixture.set_last_read(Some(FIRST_ROWS));
      assert!(fixture.backend.first_unread_message(&fixture.response).is_none());
      assert!(fixture.mirror.first_unread_message(&fixture.response).is_none());
    }

    #[test]
    fn a_conversation_never_read_reads_only_its_newest_rows() {
      let (store, _dir) = sealed_store();
      let mut fixture = Fixture::new(store);
      for n in 7..=20 {
        fixture.write(&text(n, PEER, "plain"));
      }
      let first = fixture.backend.first_unread_message(&fixture.response);
      assert_eq!(
        first.map(|row| row.client_message_id),
        Some(client_id(21 - UNREAD_WINDOW_ROWS as i64)),
        "the line goes at the oldest of the newest rows, like the badge's count"
      );
    }
  }

  /// TEAMCRAFT-F: the pinned bar read and decrypted the whole conversation on every change.
  mod pinned {
    use super::*;
    use crate::ui_backend::PIN_SCANS;

    #[test]
    fn pins_are_scanned_once_and_then_followed() {
      let (store, _dir) = tracked_sealed_store();
      let mut fixture = Fixture::new(store);
      fixture.write(&pinned(2, true));
      let scans = || PIN_SCANS.with(std::cell::Cell::get);
      let before = scans();
      assert_eq!(fixture.pinned_ids(), vec![server_id(2)]);
      assert_eq!(scans() - before, 1);

      fixture.write(&pinned(4, true));
      assert_eq!(fixture.pinned_ids(), vec![server_id(2), server_id(4)]);
      fixture.write(&pinned(2, false));
      assert_eq!(fixture.pinned_ids(), vec![server_id(4)]);
      fixture.write(&text(30, PEER, "new"));
      assert_eq!(fixture.pinned_ids(), vec![server_id(4)]);
      assert_eq!(scans() - before, 1, "later changes are followed, not scanned");

      fixture.remove(4);
      assert!(fixture.pinned_ids().is_empty(), "a removed message is no pin");
      fixture.remove_all();
      assert!(fixture.pinned_ids().is_empty());
    }

    /// Issue #1735: the panel, read before the pin, lists a pin made through the UI afterwards.
    #[test]
    fn a_pin_made_after_the_scan_is_listed() {
      let (store, _dir) = cached_sealed_store();
      let fixture = Fixture::new(store);
      let _ = teamcraft_client_api::backend::ChatBackend::messages(&fixture.backend, conversation(), 0, 50);
      assert!(fixture.pinned_ids().is_empty());
      fixture.backend.ui_set_pinned(conversation(), server_id(2), true);
      fixture.mirror.ui_set_pinned(conversation(), server_id(2), true);
      assert_eq!(fixture.pinned_ids(), vec![server_id(2)]);
      fixture.backend.ui_set_pinned(conversation(), server_id(2), false);
      fixture.mirror.ui_set_pinned(conversation(), server_id(2), false);
      assert!(fixture.pinned_ids().is_empty());
    }

    fn pinned(n: i64, is_pinned: bool) -> StoredMessage {
      let mut message = text(n, PEER, "plain");
      message.is_pinned = is_pinned;
      message
    }

    impl<S: LocalStore> Fixture<S> {
      fn pinned_ids(&self) -> Vec<MessageId> {
        let ids: Vec<MessageId> = self
          .backend
          .pinned_messages_of(conversation())
          .into_iter()
          .map(|message| message.id)
          .collect();
        let expected: Vec<MessageId> = self
          .mirror
          .pinned_messages_of(conversation())
          .into_iter()
          .map(|message| message.id)
          .collect();
        assert_eq!(ids, expected, "as a whole read finds them");
        ids
      }
    }
  }

  /// Issue #1466: a chat list shows the unread counts alone, which never read older history than
  /// the read position, whatever was rewritten there.
  mod unread_only {
    use super::*;

    #[test]
    fn older_history_reads_only_the_unread_rows() {
      let (store, _dir) = sealed_store();
      let mut fixture = Fixture::new(store);
      fixture.set_last_read(Some(4));
      fixture.check();
      fixture.write(&text(0, PEER, "older, loaded later"));
      // Rows 5 and 6 are after the read position.
      assert_eq!(fixture.check_unread(), (2, 0));
      assert_eq!(fixture.check_unread(), (0, 0), "nothing changed: the scan holds");
      fixture.write(&text(100, PEER, "new"));
      assert_eq!(
        fixture.check_unread(),
        (0, 0),
        "a new row is added to the kept unread counts"
      );
      fixture.reopen();
      assert_eq!(fixture.check_unread(), (0, 0), "they are kept across a restart");
      fixture.need = EntryNeed::Everything;
      assert_eq!(
        fixture.check(),
        1,
        "a caller that needs everything counts everything"
      );
    }

    #[test]
    fn a_conversation_never_read_reads_only_its_newest_rows() {
      let (store, _dir) = sealed_store();
      let mut fixture = Fixture::new(store);
      for n in 7..=20 {
        fixture.write(&text(n, PEER, "plain"));
      }
      fixture.need = EntryNeed::Unread;
      let before = UNREAD_ROWS.with(std::cell::Cell::get);
      let counts = fixture.counts();
      assert_eq!(
        UNREAD_ROWS.with(std::cell::Cell::get) - before,
        UNREAD_WINDOW_ROWS
      );
      assert_eq!(counts.unread.messages, UNREAD_WINDOW_ROWS as u32);
      fixture.write(&text(21, PEER, "@alice new"));
      let counts = fixture.counts();
      assert_eq!(counts.unread.messages, UNREAD_WINDOW_ROWS as u32 + 1);
      assert_eq!(counts.unread.mentions, 1);
    }

    #[test]
    fn a_rewrite_before_the_read_position_reads_no_row_before_it() {
      let (store, _dir) = sealed_store();
      let mut fixture = Fixture::new(store);
      fixture.set_last_read(Some(4));
      fixture.check();
      let mut placeholder = text(2, PEER, "");
      placeholder.content = MessageContent::Unknown;
      placeholder.maybe_unreadable = Some(Unreadable::Waiting { since_ms: 1 });
      fixture.write(&placeholder);
      assert_eq!(fixture.check_unread(), (2, 0));
      fixture.write(&text(2, PEER, "decrypted"));
      assert_eq!(fixture.check_unread(), (2, 0));
      fixture.remove(1);
      assert_eq!(fixture.check_unread(), (2, 0));
    }

    #[test]
    fn a_rewrite_after_the_read_position_counts_again() {
      let (store, _dir) = sealed_store();
      let mut fixture = Fixture::new(store);
      fixture.set_last_read(Some(4));
      fixture.check();
      let mut deleted = text(5, PEER, "");
      deleted.content = MessageContent::Deleted;
      fixture.write(&deleted);
      // A deleted message is no row any more: row 6 is left after the read position.
      assert_eq!(fixture.check_unread(), (1, 0));
      fixture.write(&edited(6, "now it says @alice"));
      assert_eq!(fixture.check_unread(), (1, 0));
      assert_eq!(fixture.counts().unread.mentions, 1);
    }

    #[test]
    fn a_holding_tally_answers_without_a_scan() {
      let (store, _dir) = sealed_store();
      let mut fixture = Fixture::new(store);
      fixture.check();
      fixture.write(&text(100, PEER, "new"));
      assert_eq!(fixture.check_unread(), (0, 0));
    }
  }

  /// After one scan, the unread counts follow the store's journal: no rewrite reads more than the
  /// messages it wrote.
  mod followed {
    use super::*;

    #[test]
    fn every_rewrite_after_a_scan_reads_no_more_rows() {
      let (store, _dir) = tracked_sealed_store();
      let mut fixture = Fixture::new(store);
      fixture.set_last_read(Some(4));
      fixture.check();
      fixture.write(&text(0, PEER, "older, loaded later"));
      assert_eq!(fixture.check_unread(), (2, 0), "the first rewrite scans once");
      let mut placeholder = text(101, PEER, "");
      placeholder.content = MessageContent::Unknown;
      placeholder.maybe_unreadable = Some(Unreadable::Waiting { since_ms: 1 });
      fixture.write(&placeholder);
      assert_eq!(fixture.check_unread(), (0, 0));
      fixture.write(&text(101, PEER, "decrypted, @alice"));
      assert_eq!(fixture.check_unread(), (0, 0));
      assert_eq!(fixture.counts().unread.mentions, 1);
      fixture.write(&text(-1, PEER, "even older"));
      fixture.write(&edited(6, "now it says @alice"));
      assert_eq!(fixture.check_unread(), (0, 0));
      let mut deleted = text(5, PEER, "");
      deleted.content = MessageContent::Deleted;
      fixture.write(&deleted);
      fixture.remove(101);
      assert_eq!(fixture.check_unread(), (0, 0));
      fixture.write(&text(102, PEER, "new"));
      assert_eq!(fixture.check_unread(), (0, 0));
      assert_eq!(fixture.counts().unread.messages, 2);
    }

    #[test]
    fn a_new_read_position_or_an_event_scans_again() {
      let (store, _dir) = tracked_sealed_store();
      let mut fixture = Fixture::new(store);
      fixture.set_last_read(Some(2));
      fixture.check();
      fixture.write(&text(0, PEER, "older, loaded later"));
      fixture.check_unread();
      fixture.set_last_read(Some(5));
      assert_eq!(fixture.check_unread(), (1, 0));
      fixture.write(&text(-1, PEER, "even older"));
      assert_eq!(
        fixture.check_unread(),
        (0, 0),
        "followed again after the new position"
      );
      // An event can hide what an import added: the journal can't tell, the kept tally counts it.
      fixture.write(&event(103, PEER, ConversationEvent::Joined));
      fixture.check_unread();
    }

    #[test]
    fn all_messages_gone_scans_again() {
      let (store, _dir) = tracked_sealed_store();
      let mut fixture = Fixture::new(store);
      fixture.set_last_read(Some(4));
      fixture.check();
      fixture.write(&text(0, PEER, "older"));
      fixture.check_unread();
      fixture.remove_all();
      fixture.check_unread();
      assert_eq!(fixture.counts().unread.messages, 0);
    }
  }

  mod record {
    use super::*;

    #[test]
    fn a_tally_reads_back_as_written() {
      let full = EntryTally {
        version: FeedVersion {
          generation: u64::MAX,
          reshaped: 2,
          edited: 3,
        },
        maybe_importer: Some(id(IMPORTER)),
        maybe_through: Some(Through {
          value: i64::MAX,
          client_message_id: *client_id(7).as_bytes(),
        }),
        rows: 11,
        messages: 10,
        maybe_last_activity: Some(0),
        unread_key: u64::MAX - 1,
        unread_messages: u32::MAX,
        unread_mentions: 5,
        is_unread_only: false,
      };
      assert_eq!(EntryTally::from_bytes(&full.to_bytes()), Some(full));
      let empty = EntryTally {
        version: FeedVersion::default(),
        maybe_importer: None,
        maybe_through: None,
        rows: 0,
        messages: 0,
        maybe_last_activity: None,
        unread_key: 0,
        unread_messages: 0,
        unread_mentions: 0,
        is_unread_only: true,
      };
      assert_eq!(EntryTally::from_bytes(&empty.to_bytes()), Some(empty));
    }

    #[test]
    fn anything_else_reads_as_none() {
      let bytes = EntryTally {
        version: FeedVersion::default(),
        maybe_importer: None,
        maybe_through: None,
        rows: 0,
        messages: 0,
        maybe_last_activity: None,
        unread_key: 0,
        unread_messages: 0,
        unread_mentions: 0,
        is_unread_only: false,
      }
      .to_bytes();
      assert_eq!(EntryTally::from_bytes(&[]), None);
      assert_eq!(EntryTally::from_bytes(&bytes[..bytes.len() - 1]), None);
      let mut other_format = bytes;
      other_format[0] = 3;
      assert_eq!(EntryTally::from_bytes(&other_format), None);
    }
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
      assert!(kept_tally(fixture.store.as_ref(), conversation()).is_some());
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
    need: EntryNeed,
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
        need: EntryNeed::Everything,
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
      let expected = self.mirror.entry_counts(
        &self.response,
        self.maybe_mark,
        Some(&me()),
        &self.preferences,
        self.need,
      );
      if self.need == EntryNeed::Everything {
        assert_eq!(counts.messages, expected.messages);
        assert_eq!(counts.maybe_last_activity, expected.maybe_last_activity);
      }
      assert_eq!(counts.unread.messages, expected.unread.messages);
      assert_eq!(counts.unread.mentions, expected.unread.mentions);
      passes
    }

    /// Like [`Self::check`], for the unread counts alone: the rows read for them, and whether the
    /// whole feed was read.
    fn check_unread(&mut self) -> (usize, usize) {
      self.need = EntryNeed::Unread;
      let rows_before = UNREAD_ROWS.with(std::cell::Cell::get);
      let whole = self.check();
      (UNREAD_ROWS.with(std::cell::Cell::get) - rows_before, whole)
    }

    fn counts(&self) -> EntryCounts {
      let counts = self.kept_counts();
      self.backend.flush_entry_tallies();
      counts
    }

    fn kept_counts(&self) -> EntryCounts {
      self.backend.entry_counts(
        &self.response,
        self.maybe_mark,
        Some(&me()),
        &self.preferences,
        self.need,
      )
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

  fn cached_sealed_store() -> (Arc<TrackedStore<SealedSqliteStore>>, ScratchDir) {
    let (store, dir) = sealed_store();
    let store = Arc::try_unwrap(store).ok().unwrap();
    (Arc::new(TrackedStore::with_message_cache(store, 1 << 20)), dir)
  }

  fn tracked_sealed_store() -> (Arc<TrackedStore<SealedSqliteStore>>, ScratchDir) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
      "teamcraft-entry-tally-tracked-{}-{}",
      std::process::id(),
      NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    (Arc::new(TrackedStore::new(open_sealed(&dir))), ScratchDir(dir))
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
      is_awaiting_first_sign_in: false,
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
