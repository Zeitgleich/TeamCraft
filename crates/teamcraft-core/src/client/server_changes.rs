//! Lines about changes the server records of a channel (issue #818): archived, restored, made
//! private or public, moved to another space.
//!
//! These changes have no MLS commit: whether a channel is archived, public or listed in a space is
//! the server's record of it, unsigned until channel properties are signed (a follow-up of the
//! signed channel roles, #380, #801). So every
//! member's device writes the line itself, from the server's record as it sees it change (the
//! refetch after a gateway `conversation_update`, a sync's conversation list, the answer to its
//! own change), and the line says what the server says: "This channel was archived". Nothing a
//! member sends makes such a line or keeps it away.
//!
//! Whoever makes the change still sends an encrypted announcement of it
//! ([`ConversationEvent::Archived`] and the like), which older versions show. This version keeps it
//! hidden ([`StoredMessage::is_hidden`]); it only names who made the change, on the line it
//! matches ([`note_announcement`]). One that matches nothing names nobody and shows nothing.

use std::time::Duration;

use teamcraft_proto::{
  ClientMessageId, ConversationId, ConversationKind, ConversationResponse, ConversationVisibility, Id,
  MessageId, Snowflake, SpaceId, SpaceResponse, UpdateConversationRequest,
};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records;
use crate::store::{
  self, ChangeActor, ConversationEvent, LocalStore, MessageContent, MessageFilter, ServerChange,
  StoredMessage, Table, nobody,
};

/// The second half of a line's ID (the first is where the line was first put). Its top bit is set,
/// so it is never the second half of an admin-join line's ID, a user ID (issue #811).
const LINE_ID_TAG: u64 = 0x8000_0000_0000_0818;

/// How this device learned of a record of a conversation it keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Observed {
  /// The server's answer to this device's own request, which `asked` for some changes: the user
  /// made those. Anything else that differs from the record held before, somebody else changed
  /// meanwhile.
  OwnChange { asked: Asked },
  /// The server's record, read on its own (a refetch, a sync). `maybe_head` is the newest message
  /// of the conversation the sync's heads named, if any.
  FromServer { maybe_head: Option<MessageId> },
}

/// What a request of this device asked to change of a channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Asked {
  pub maybe_is_archived: Option<bool>,
  pub maybe_is_public: Option<bool>,
  pub is_move: bool,
}

impl Asked {
  /// A move to another space (`PUT …/space`).
  pub(crate) const MOVE: Self = Self {
    maybe_is_archived: None,
    maybe_is_public: None,
    is_move: true,
  };

  /// What an update (`PATCH …`) asks for.
  pub(crate) fn update(request: &UpdateConversationRequest) -> Self {
    Self {
      maybe_is_archived: request.maybe_is_archived,
      maybe_is_public: request
        .maybe_visibility
        .as_ref()
        .map(|visibility| *visibility == ConversationVisibility::Public),
      is_move: false,
    }
  }

  /// Whether the request asked for the change `event` tells of.
  fn covers(&self, event: &ConversationEvent) -> bool {
    match event {
      ConversationEvent::Archived => self.maybe_is_archived == Some(true),
      ConversationEvent::Unarchived => self.maybe_is_archived == Some(false),
      ConversationEvent::MadePublic => self.maybe_is_public == Some(true),
      ConversationEvent::MadePrivate => self.maybe_is_public == Some(false),
      ConversationEvent::Moved { .. } => self.is_move,
      _ => false,
    }
  }
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Keeps `after`, the server's record of a conversation, with a line for every change from the
  /// record this device held ([`changes`]), in one group of writes: a crash keeps both, and the
  /// change was told once, or neither, and the change is seen again.
  pub(crate) fn store_conversation(
    &self,
    after: &ConversationResponse,
    observed: Observed,
  ) -> Result<(), CoreError> {
    let maybe_before: Option<ConversationResponse> =
      records::get_json(&self.store, Table::Conversations, &after.id.to_string())?;
    self.store_conversation_from(maybe_before.as_ref(), after, observed)
  }

  /// [`Self::store_conversation`] with the record this device held already read.
  pub(crate) fn store_conversation_from(
    &self,
    maybe_before: Option<&ConversationResponse>,
    after: &ConversationResponse,
    observed: Observed,
  ) -> Result<(), CoreError> {
    let key = after.id.to_string();
    let events = match maybe_before {
      Some(before) => changes(before, after, |space| self.space_name(space))?,
      // Nothing to compare with (a conversation new to this device): nothing it saw change.
      None => Vec::new(),
    };
    if events.is_empty() {
      return records::put_json(&self.store, Table::Conversations, &key, after);
    }
    let lines = self.change_lines(after.id, events, observed)?;
    store::write_group(&self.store, || -> Result<(), CoreError> {
      records::put_json(&self.store, Table::Conversations, &key, after)?;
      for line in &lines {
        self.store.upsert_message(line)?;
      }
      Ok(())
    })?;
    tracing::info!(conversation_id = %after.id, lines = lines.len(), "the server reports changes to the channel");
    Ok(())
  }

  /// The lines for `events`, which this device just saw in the server's record of `conversation`:
  /// after everything it knows of the conversation, in order.
  fn change_lines(
    &self,
    conversation: ConversationId,
    events: Vec<ConversationEvent>,
    observed: Observed,
  ) -> Result<Vec<StoredMessage>, CoreError> {
    // Read only when a change is seen, which is rare: archiving, converting and moving channels.
    let messages = self.store.messages(conversation, MessageFilter::All)?;
    let seen_after = messages
      .iter()
      .filter_map(|message| message.maybe_server_id)
      .max()
      // No message yet: the conversation's ID is older than all of them.
      .unwrap_or_else(|| MessageId::from_snowflake(conversation.snowflake()));
    let maybe_head = match observed {
      Observed::FromServer { maybe_head } => maybe_head,
      Observed::OwnChange { .. } => None,
    };
    let seen_until = maybe_head.map_or(seen_after, |head| head.max(seen_after));
    let mut last = messages
      .iter()
      .filter_map(StoredMessage::line_position)
      .fold(seen_until, MessageId::max);
    let now_ms = self.now_ms();
    let mut lines = Vec::with_capacity(events.len());
    for event in events {
      let at = next_line_position(last);
      last = at;
      let (sender, actor) = match observed {
        Observed::OwnChange { asked } if asked.covers(&event) => (self.user_id(), ChangeActor::ThisDevice),
        _ => (nobody(), ChangeActor::Nobody),
      };
      lines.push(StoredMessage {
        conversation_id: conversation,
        client_message_id: line_id(at),
        maybe_server_id: None,
        sender_user_id: sender,
        maybe_thread_id: None,
        // When this device learned of it, shown only: its place is `at`, on the server's clock.
        sent_at_ms: now_ms.max(at.snowflake().unix_millis()),
        content: MessageContent::Event { event },
        is_edited: false,
        edits: Vec::new(),
        maybe_edit_id: None,
        is_pinned: false,
        maybe_verified_commit: None,
        maybe_server_change: Some(ServerChange {
          at,
          seen_after,
          seen_until,
          actor,
        }),
      });
    }
    Ok(lines)
  }

  /// The name of `space` as this device knows it; empty when it doesn't.
  fn space_name(&self, space: SpaceId) -> Result<String, CoreError> {
    let maybe_space: Option<SpaceResponse> =
      records::get_json(&self.store, Table::Spaces, &space.to_string())?;
    Ok(maybe_space.map(|space| space.name).unwrap_or_default())
  }
}

/// Whether `message` is a member's announcement of a change the server reports, which
/// [`note_announcement`] matches against the lines.
pub(crate) fn is_announcement(message: &StoredMessage) -> bool {
  matches!(&message.content, MessageContent::Event { event } if event.is_server_change())
    && message.maybe_server_id.is_some()
    && !message.is_local_line()
}

/// `announcement`, a member's message just stored, names who made a change the server reports on
/// the line it matches: a line of the same change ([`is_same_change`]) that names nobody yet, the
/// earliest if several do, provided the announcement was sent after everything this device held
/// when it saw the change and at most `window` after the server's newest message then (a member
/// can't take a change seen long before or after their message). The line moves back to the
/// announcement if that is earlier. An announcement that finds no such line but fits lines another member's
/// announcement named leaves them naming nobody ([`ChangeActor::Contested`]): two members claimed
/// the change. Lines this device made itself keep naming the user. Nothing else comes of it.
pub(crate) fn note_announcement(
  store: &impl LocalStore,
  announcement: &StoredMessage,
  window: Duration,
) -> Result<(), CoreError> {
  if !is_announcement(announcement) {
    return Ok(());
  }
  let (MessageContent::Event { event }, Some(sent)) = (&announcement.content, announcement.maybe_server_id)
  else {
    return Ok(());
  };
  let window_ms = i64::try_from(window.as_millis()).unwrap_or(i64::MAX);
  let fitting: Vec<StoredMessage> = store
    .messages(announcement.conversation_id, MessageFilter::All)?
    .into_iter()
    .filter(|line| fits(line, event, sent, window_ms))
    .collect();
  let maybe_unnamed = fitting
    .iter()
    .filter(|line| actor_of(line) == Some(&ChangeActor::Nobody))
    .min_by_key(|line| line.line_position());
  if let Some(line) = maybe_unnamed {
    let mut named = line.clone();
    named.sender_user_id = announcement.sender_user_id;
    named.sent_at_ms = sent.snowflake().unix_millis();
    if let Some(change) = &mut named.maybe_server_change {
      // Back to the announcement when it is older (a change caught up after a while away), so the
      // line sits where the change happened; never later, so its row keeps its place on screen.
      change.at = change.at.min(sent);
      change.actor = ChangeActor::Announced {
        announcement: announcement.client_message_id,
      };
    }
    return store.upsert_message(&named).map_err(CoreError::from);
  }
  for line in fitting.iter().filter(|line| {
    matches!(actor_of(line), Some(ChangeActor::Announced { announcement: by }) if *by != announcement.client_message_id)
      && line.sender_user_id != announcement.sender_user_id
  }) {
    tracing::info!(conversation_id = %line.conversation_id, "two members announced the same change; the line names nobody");
    let mut contested = line.clone();
    contested.sender_user_id = nobody();
    if let Some(change) = &mut contested.maybe_server_change {
      change.actor = ChangeActor::Contested;
    }
    store.upsert_message(&contested)?;
  }
  Ok(())
}

/// Whether an announcement of `event` sent as `sent` fits `line`, a line about a change the server
/// reports. Whom the line names already decides what the announcement does to it
/// ([`note_announcement`]).
fn fits(line: &StoredMessage, event: &ConversationEvent, sent: MessageId, window_ms: i64) -> bool {
  let (Some(change), MessageContent::Event { event: shown }) = (&line.maybe_server_change, &line.content)
  else {
    return false;
  };
  is_same_change(shown, event)
    && sent > change.seen_after
    && sent.snowflake().unix_millis()
      <= change
        .seen_until
        .snowflake()
        .unix_millis()
        .saturating_add(window_ms)
}

fn actor_of(line: &StoredMessage) -> Option<&ChangeActor> {
  line.maybe_server_change.as_ref().map(|change| &change.actor)
}

/// Whether an announcement of `announced` tells of the change `shown` on a line: the same kind,
/// and for a move the same space, unless one side doesn't know its name.
fn is_same_change(shown: &ConversationEvent, announced: &ConversationEvent) -> bool {
  match (shown, announced) {
    (ConversationEvent::Moved { space: known }, ConversationEvent::Moved { space: claimed }) => {
      known.is_empty() || claimed.is_empty() || known == claimed
    },
    _ => shown.is_server_change() && shown == announced,
  }
}

/// The lines a new record of a channel calls for, in the order of its settings: archived or
/// restored, made private or public, moved (to the space `space_name` names). DMs, group DMs and
/// meeting chats have none of these.
fn changes(
  before: &ConversationResponse,
  after: &ConversationResponse,
  space_name: impl FnOnce(SpaceId) -> Result<String, CoreError>,
) -> Result<Vec<ConversationEvent>, CoreError> {
  let mut events = Vec::new();
  if before.kind != ConversationKind::Channel || after.kind != ConversationKind::Channel {
    return Ok(events);
  }
  if before.is_archived != after.is_archived {
    events.push(if after.is_archived {
      ConversationEvent::Archived
    } else {
      ConversationEvent::Unarchived
    });
  }
  let is_public = |response: &ConversationResponse| response.visibility == ConversationVisibility::Public;
  if is_public(before) != is_public(after) {
    events.push(if is_public(after) {
      ConversationEvent::MadePublic
    } else {
      ConversationEvent::MadePrivate
    });
  }
  if let (Some(from), Some(to)) = (before.maybe_space_id, after.maybe_space_id)
    && from != to
  {
    events.push(ConversationEvent::Moved {
      space: space_name(to)?,
    });
  }
  Ok(events)
}

/// The place for a line after `after`: the last ID of `after`'s millisecond, or of the next one
/// when `after` is that already (another line). A message has such an ID only if a server instance
/// numbered 1023 hands out 4096 IDs in one millisecond, so a line doesn't share a message's row.
fn next_line_position(after: MessageId) -> MessageId {
  let last_of = |unix_millis: i64| Snowflake::cursor_before(unix_millis.saturating_add(1));
  let millis = after.snowflake().unix_millis();
  let mut position = last_of(millis);
  if position <= after.snowflake() {
    position = last_of(millis.saturating_add(1));
  }
  MessageId::from_snowflake(position)
}

/// The line's ID: where it was first put, which no other line of the conversation shares, so
/// writing it again (a change seen again after a crash) writes the same line. Known to whoever
/// can guess that place, which is why no message replaces or changes a line
/// ([`StoredMessage::is_local_line`]).
fn line_id(first_at: MessageId) -> ClientMessageId {
  let mut bytes = [0u8; 16];
  bytes[..8].copy_from_slice(&first_at.snowflake().get().to_be_bytes());
  bytes[8..].copy_from_slice(&LINE_ID_TAG.to_be_bytes());
  ClientMessageId::from_bytes(bytes)
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{ChannelModeration, HistoryPolicy, UserId};

  use super::*;
  use crate::store::MemoryStore;

  const WINDOW: Duration = Duration::from_secs(10 * 60);
  const MINUTE_MS: i64 = 60 * 1000;

  mod changes {
    use super::*;

    #[test]
    fn each_setting_the_server_reports_changed_is_a_line_in_order() {
      let before = channel(false, ConversationVisibility::Public, Some(space(1)));
      let after = channel(true, ConversationVisibility::Private, Some(space(2)));
      assert_eq!(
        changes(&before, &after, |_| Ok("Project B".into())).unwrap(),
        vec![
          ConversationEvent::Archived,
          ConversationEvent::MadePrivate,
          ConversationEvent::Moved {
            space: "Project B".into()
          },
        ]
      );
      assert_eq!(
        changes(&after, &before, |_| Ok(String::new())).unwrap(),
        vec![
          ConversationEvent::Unarchived,
          ConversationEvent::MadePublic,
          ConversationEvent::Moved { space: String::new() },
        ]
      );
    }

    #[test]
    fn nothing_changed_is_no_line() {
      let record = channel(true, ConversationVisibility::Private, Some(space(1)));
      let mut renamed = record.clone();
      renamed.maybe_name = Some("other".into());
      renamed.maybe_category_id = Some(teamcraft_proto::CategoryId::from_snowflake(
        Snowflake::new(4).unwrap(),
      ));
      assert!(
        changes(&record, &renamed, |_| unreachable_name())
          .unwrap()
          .is_empty()
      );
    }

    /// DMs, group DMs and meeting chats aren't archived, converted or moved: a record saying so
    /// tells nothing.
    #[test]
    fn only_channels_have_these_lines() {
      for kind in [
        ConversationKind::Dm,
        ConversationKind::Group,
        ConversationKind::Meeting,
      ] {
        let mut before = channel(false, ConversationVisibility::Private, None);
        before.kind = kind.clone();
        let mut after = before.clone();
        after.is_archived = true;
        after.visibility = ConversationVisibility::Public;
        assert!(
          changes(&before, &after, |_| unreachable_name())
            .unwrap()
            .is_empty(),
          "{kind:?}"
        );
      }
    }

    fn unreachable_name() -> Result<String, CoreError> {
      panic!("no move, no space name")
    }
  }

  mod positions {
    use super::*;

    /// A line sits after `after`, at the last ID of its millisecond, which no message the server
    /// numbers there gets; a second line after it takes the next millisecond's.
    #[test]
    fn a_line_sits_after_everything_known_and_after_the_line_before() {
      let message = id_at(5, 3);
      let first = next_line_position(message);
      assert!(first > message);
      assert_eq!(first.snowflake().unix_millis(), message.snowflake().unix_millis());
      assert!(
        id_at(5, 4095) < first,
        "every message of that millisecond is before it"
      );
      let second = next_line_position(first);
      assert!(second > first);
      assert_eq!(
        second.snowflake().unix_millis(),
        first.snowflake().unix_millis() + 1
      );
      assert!(id_at(6, 0) < second);
    }

    #[test]
    fn line_ids_differ_by_place_and_from_admin_lines() {
      let first = line_id(id_at(5, 3));
      assert_eq!(first, line_id(id_at(5, 3)));
      assert_ne!(first, line_id(id_at(6, 3)));
      assert!(first.as_bytes()[8] & 0x80 != 0, "no user ID has the top bit set");
    }
  }

  mod announcements {
    use super::*;

    #[test]
    fn a_matching_announcement_names_its_sender_and_moves_the_line_back_there() {
      let store = MemoryStore::default();
      // Put after the newest message the server had, later than the announcement.
      let line = unnamed_line(1, ConversationEvent::Archived, id_at(10, 0), id_at(10, 0));
      store.upsert_message(&line).unwrap();
      let sent = id_at(11, 0);
      assert!(sent < line.line_position().unwrap());

      note_announcement(
        &store,
        &announcement(2, ConversationEvent::Archived, sent, user(7)),
        WINDOW,
      )
      .unwrap();

      let named = store
        .message(conversation(), line.client_message_id)
        .unwrap()
        .unwrap();
      assert_eq!(named.maybe_actor(), Some(user(7)));
      assert_eq!(named.line_position(), Some(sent));
      assert_eq!(named.sent_at_ms, sent.snowflake().unix_millis());
    }

    /// An announcement after the line names who made the change and leaves the line where it is.
    #[test]
    fn a_later_announcement_leaves_the_line_in_place() {
      let store = MemoryStore::default();
      let line = unnamed_line(1, ConversationEvent::Archived, id_at(10, 0), id_at(10, 0));
      store.upsert_message(&line).unwrap();
      let sent = id_at(12, 0);
      note_announcement(
        &store,
        &announcement(2, ConversationEvent::Archived, sent, user(7)),
        WINDOW,
      )
      .unwrap();
      let named = store
        .message(conversation(), line.client_message_id)
        .unwrap()
        .unwrap();
      assert_eq!(named.maybe_actor(), Some(user(7)));
      assert_eq!(named.line_position(), line.line_position());
    }

    /// Same kind, after what the device held, within the window: anything else names nobody.
    #[test]
    fn an_announcement_names_nobody_unless_it_fits() {
      let seen = id_at(10, 5);
      let cases = [
        (ConversationEvent::Unarchived, id_at(11, 0), "another kind"),
        (
          ConversationEvent::Archived,
          id_at(10, 4),
          "sent before the device saw the change",
        ),
        (
          ConversationEvent::Archived,
          seen,
          "the newest message the device held",
        ),
        (
          ConversationEvent::Archived,
          id_at(10 + 11 * MINUTE_MS, 0),
          "sent after the window",
        ),
      ];
      for (event, sent, case) in cases {
        let store = MemoryStore::default();
        let line = unnamed_line(1, ConversationEvent::Archived, seen, seen);
        store.upsert_message(&line).unwrap();
        note_announcement(&store, &announcement(2, event, sent, user(7)), WINDOW).unwrap();
        let kept = store
          .message(conversation(), line.client_message_id)
          .unwrap()
          .unwrap();
        assert_eq!(kept, line, "{case}");
      }
    }

    /// The sync's head ends the window: an announcement in the gap the device caught up names
    /// who made the change.
    #[test]
    fn the_window_runs_from_the_servers_newest_message() {
      let store = MemoryStore::default();
      let line = unnamed_line(
        1,
        ConversationEvent::MadePrivate,
        id_at(10, 0),
        id_at(10 + 60 * MINUTE_MS, 0),
      );
      store.upsert_message(&line).unwrap();
      let sent = id_at(10 + 30 * MINUTE_MS, 0);
      note_announcement(
        &store,
        &announcement(2, ConversationEvent::MadePrivate, sent, user(7)),
        WINDOW,
      )
      .unwrap();
      let named = store
        .message(conversation(), line.client_message_id)
        .unwrap()
        .unwrap();
      assert_eq!(named.maybe_actor(), Some(user(7)));
    }

    #[test]
    fn a_move_names_the_mover_only_for_the_same_space() {
      let line = unnamed_line(
        1,
        ConversationEvent::Moved {
          space: "Project B".into(),
        },
        id_at(10, 0),
        id_at(10, 0),
      );
      for (claimed, is_named) in [("Project C", false), ("Project B", true), ("", true)] {
        let store = MemoryStore::default();
        store.upsert_message(&line).unwrap();
        let moved = ConversationEvent::Moved {
          space: claimed.into(),
        };
        note_announcement(&store, &announcement(2, moved, id_at(11, 0), user(7)), WINDOW).unwrap();
        let kept = store
          .message(conversation(), line.client_message_id)
          .unwrap()
          .unwrap();
        assert_eq!(kept.maybe_actor().is_some(), is_named, "{claimed:?}");
      }
    }

    /// Two members claiming the same change: the line names neither, also not after a third one.
    #[test]
    fn a_second_claim_names_nobody() {
      let store = MemoryStore::default();
      let line = unnamed_line(1, ConversationEvent::Archived, id_at(10, 0), id_at(10, 0));
      store.upsert_message(&line).unwrap();
      let first = announcement(2, ConversationEvent::Archived, id_at(11, 0), user(7));
      note_announcement(&store, &first, WINDOW).unwrap();
      // The same member again changes nothing.
      note_announcement(
        &store,
        &announcement(3, ConversationEvent::Archived, id_at(12, 0), user(7)),
        WINDOW,
      )
      .unwrap();
      assert_eq!(
        store
          .message(conversation(), line.client_message_id)
          .unwrap()
          .unwrap()
          .maybe_actor(),
        Some(user(7))
      );

      note_announcement(
        &store,
        &announcement(4, ConversationEvent::Archived, id_at(13, 0), user(8)),
        WINDOW,
      )
      .unwrap();
      note_announcement(
        &store,
        &announcement(5, ConversationEvent::Archived, id_at(14, 0), user(9)),
        WINDOW,
      )
      .unwrap();

      let contested = store
        .message(conversation(), line.client_message_id)
        .unwrap()
        .unwrap();
      assert_eq!(contested.maybe_actor(), None);
      assert_eq!(contested.sender_user_id, nobody());
      assert_eq!(actor_of(&contested), Some(&ChangeActor::Contested));
    }

    /// Two changes of the same kind: each announcement names the earliest line still unnamed.
    #[test]
    fn announcements_name_lines_in_order() {
      let store = MemoryStore::default();
      let first = unnamed_line(1, ConversationEvent::Archived, id_at(10, 0), id_at(10, 0));
      let second = unnamed_line(2, ConversationEvent::Archived, id_at(10, 0), id_at(10, 0));
      store.upsert_message(&first).unwrap();
      store.upsert_message(&second).unwrap();

      note_announcement(
        &store,
        &announcement(3, ConversationEvent::Archived, id_at(11, 0), user(7)),
        WINDOW,
      )
      .unwrap();
      note_announcement(
        &store,
        &announcement(4, ConversationEvent::Archived, id_at(12, 0), user(8)),
        WINDOW,
      )
      .unwrap();

      let actor = |line: &StoredMessage| {
        store
          .message(conversation(), line.client_message_id)
          .unwrap()
          .unwrap()
          .maybe_actor()
      };
      assert_eq!((actor(&first), actor(&second)), (Some(user(7)), Some(user(8))));
    }

    /// The user's own change stays theirs, whatever others claim.
    #[test]
    fn a_line_of_this_devices_own_change_keeps_naming_the_user() {
      let store = MemoryStore::default();
      let mut line = unnamed_line(1, ConversationEvent::Archived, id_at(10, 0), id_at(10, 0));
      line.sender_user_id = user(1);
      if let Some(change) = &mut line.maybe_server_change {
        change.actor = ChangeActor::ThisDevice;
      }
      store.upsert_message(&line).unwrap();
      note_announcement(
        &store,
        &announcement(2, ConversationEvent::Archived, id_at(11, 0), user(8)),
        WINDOW,
      )
      .unwrap();
      assert_eq!(
        store
          .message(conversation(), line.client_message_id)
          .unwrap()
          .unwrap(),
        line
      );
    }

    fn announcement(client: u8, event: ConversationEvent, sent: MessageId, sender: UserId) -> StoredMessage {
      StoredMessage {
        conversation_id: conversation(),
        client_message_id: ClientMessageId::from_bytes([client; 16]),
        maybe_server_id: Some(sent),
        sender_user_id: sender,
        maybe_thread_id: None,
        sent_at_ms: sent.snowflake().unix_millis(),
        content: MessageContent::Event { event },
        is_edited: false,
        edits: Vec::new(),
        maybe_edit_id: None,
        is_pinned: false,
        maybe_verified_commit: None,
        maybe_server_change: None,
      }
    }

    fn unnamed_line(
      client: u8,
      event: ConversationEvent,
      seen_after: MessageId,
      seen_until: MessageId,
    ) -> StoredMessage {
      // Each line in a millisecond of its own, as the lines of one record are.
      let at = next_line_position(seen_until.max(id_at(10 + i64::from(client), 0)));
      StoredMessage {
        client_message_id: line_id(at),
        maybe_server_id: None,
        sender_user_id: nobody(),
        maybe_server_change: Some(ServerChange {
          at,
          seen_after,
          seen_until,
          actor: ChangeActor::Nobody,
        }),
        ..announcement(client, event, at, nobody())
      }
    }
  }

  fn channel(
    is_archived: bool,
    visibility: ConversationVisibility,
    maybe_space_id: Option<SpaceId>,
  ) -> ConversationResponse {
    ConversationResponse {
      id: conversation(),
      maybe_space_id,
      kind: ConversationKind::Channel,
      maybe_name: Some("ops".into()),
      maybe_topic: None,
      maybe_description: None,
      visibility,
      history_policy: HistoryPolicy::FromJoin,
      maybe_category_id: None,
      is_archived,
      moderation: ChannelModeration::default(),
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

  /// A message ID made at `millis` after the epoch, with `sequence`.
  fn id_at(millis: i64, sequence: u16) -> MessageId {
    let raw = (millis << 22) | i64::from(sequence);
    MessageId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn conversation() -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(9).unwrap())
  }

  fn space(raw: i64) -> SpaceId {
    SpaceId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn user(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
