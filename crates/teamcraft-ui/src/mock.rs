//! In-memory mock of [`ChatBackend`] with deterministic, neutral demo data.
//!
//! The integration channels generate their lines lazily from the line index, so a log channel with
//! tens of thousands of entries costs no memory and proves the message list only renders what is visible.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use teamcraft_proto::{BackgroundId, ErrorCode, HistoryPolicy};

use teamcraft_client_api::actions::{
  FileInfo, LinkedMessage, MessageExtras, PermalinkTarget, ShareLink, ShareLinkOptions, WorkspaceEvent,
};
use teamcraft_client_api::activity::{ActivityFeed, ActivityItem, ActivityKind};
use teamcraft_client_api::calls::{CallDirection, CallHistoryItem};
use teamcraft_client_api::privacy::{LastSeenVisibility, PrivacySettings};

use crate::backend::ChatBackend;
use crate::calls::BackgroundImageInfo;
use crate::feedback::{ActionFailure, FailureKind, report_failure};
use crate::model::{
  Bookmark, BookmarkKind, Conversation, ConversationId, ConversationKind, CustomStatus, InviteId, JoinPolicy,
  JoinableSpace, LogLevel, Message, MessageId, MessageKind, PendingInvite, PresenceStatus, Reaction,
  SidebarCategory, Space, SpaceId, SpaceMember, SpaceRole, SpaceRoster, SystemEvent, ThreadId, ThreadSummary,
  TimestampMs, User, UserId, id_from_raw, raw_id, root_message_of, thread_id_of,
};

/// Number of lines in the large log channel.
pub const LOG_CHANNEL_LINES: usize = 50_000;

const MINUTE: i64 = 60_000;

const DM_FIRST: u64 = 30;

fn dm_first() -> u64 {
  DM_FIRST
}

fn me() -> UserId {
  id_from_raw(1)
}

fn bot() -> UserId {
  id_from_raw(100)
}

fn space_a() -> SpaceId {
  id_from_raw(1)
}

fn space_b() -> SpaceId {
  id_from_raw(2)
}

fn ch_general() -> ConversationId {
  id_from_raw(10)
}

fn ch_announcements() -> ConversationId {
  id_from_raw(11)
}

fn ch_engineering() -> ConversationId {
  id_from_raw(12)
}

fn ch_design() -> ConversationId {
  id_from_raw(13)
}

fn ch_leadership() -> ConversationId {
  id_from_raw(14)
}

fn ch_ci() -> ConversationId {
  id_from_raw(15)
}

fn ch_app_logs() -> ConversationId {
  id_from_raw(16)
}

fn ch_b_general() -> ConversationId {
  id_from_raw(20)
}

fn ch_b_planning() -> ConversationId {
  id_from_raw(21)
}

const SENTENCES: &[&str] = &[
  "Morning everyone, the staging deploy finished without errors.",
  "Can someone take a look at the failing integration test? It only fails on the CI runner.",
  "I pushed a fix for the pagination bug, review welcome.",
  "We should move the retro to Thursday, the demo takes longer than planned.",
  "The new onboarding flow looks great, just one note on the empty state copy.",
  "Does anyone know why the nightly export takes twice as long since Monday?",
  "I wrote down the decision in the design doc so we don't have to revisit it.",
  "Reminder: please update your availability for the on-call rotation.",
  "Found it – the cache key did not include the locale. Fixing now.",
  "Thanks for the quick turnaround, that unblocks the release.",
  "Let's keep the scope small for the first iteration and ship behind a flag.",
  "I'll be out tomorrow afternoon, @sam has the context if something comes up.",
  "Draft of the release notes is ready: **three new features**, two fixes, one known issue.",
  "Try `cargo test --workspace` first, it reproduces the failure locally.",
  "Could we get a second pair of eyes on the migration plan before Friday?",
  "The customer confirmed the behaviour is expected, closing the ticket.",
  "Quick poll: standup at 9:30 or 10:00 starting next week?",
  "Latency dropped from 340 ms to 90 ms after the index change. Nice.",
  "I added a _short_ checklist to the template, feedback welcome.",
  "Mockups are in the shared folder, see https://example.com/mockups/v3 for the walkthrough.",
  "Attached the numbers as quarterly-report.pdf, they are final.",
  "Heads up @alex, the staging deploy is waiting for your approval.",
  "Saved the failing screenshot as login-error.png for the ticket.",
];

const CODE_SNIPPET: &str = "Here is the snippet:\n```rust\nfn retry<T>(mut op: impl FnMut() -> Option<T>) -> Option<T> {\n  (0..3).find_map(|_| op())\n}\n```";

const REACTION_EMOJI: &[&str] = &["👍", "🎉", "👀", "✅", "❤️", "🚀"];

const LOG_SERVICES: &[&str] = &["api", "worker", "scheduler", "db", "edge", "auth", "indexer"];
const LOG_INFO: &[&str] = &[
  "request completed status=200 duration_ms=12",
  "request completed status=204 duration_ms=4",
  "job finished queue=default attempts=1",
  "connection pool refreshed size=16",
  "cache hit ratio=0.94 window=60s",
  "scheduled task started name=compaction",
  "token refreshed for session",
  "batch flushed items=250",
];
const LOG_WARN: &[&str] = &[
  "slow query detected duration_ms=1840",
  "retrying upstream call attempt=2",
  "queue depth above threshold depth=1200",
  "certificate expires in 12 days",
];
const LOG_ERROR: &[&str] = &[
  "upstream timeout after 5000 ms",
  "failed to decode payload: unexpected end of input",
  "job failed queue=default attempts=3",
];
const LOG_DEBUG: &[&str] = &[
  "cursor advanced offset=48213",
  "lease renewed ttl=30s",
  "gc pause 3 ms",
];

/// Deterministic xorshift generator so the demo data is identical on every start.
struct Rng(u64);

impl Rng {
  fn new(seed: u64) -> Self {
    Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
  }

  fn next(&mut self) -> u64 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 7;
    self.0 ^= self.0 << 17;
    self.0
  }

  fn below(&mut self, bound: usize) -> usize {
    (self.next() % bound as u64) as usize
  }
}

#[path = "mock_files.rs"]
pub mod files;
#[path = "mock_ops.rs"]
mod ops;
#[path = "mock_search.rs"]
mod search_impl;
#[path = "mock_settings.rs"]
mod settings_data;

pub struct MockBackend {
  now: TimestampMs,
  state: RefCell<State>,
  ops: RefCell<ops::OpsState>,
  search: RefCell<search_impl::SearchState>,
  people: RefCell<DemoPeople>,
}

/// What the demo remembers about the user's activity feed and privacy choice.
struct DemoPeople {
  is_activity_read: bool,
  last_seen: LastSeenVisibility,
}

struct State {
  users: Vec<User>,
  spaces: Vec<Space>,
  conversations: Vec<Conversation>,
  /// Stored top-level messages per conversation (integration logs are generated, not stored).
  messages: HashMap<ConversationId, Vec<Message>>,
  /// Thread replies per conversation, oldest first.
  replies: HashMap<ConversationId, Vec<Message>>,
  next_message_id: u64,
  /// The user's own call backgrounds (only name and size; the mock keeps no pictures).
  backgrounds: Vec<BackgroundImageInfo>,
  next_background_id: u64,
  /// Messages the user saved for later.
  saved: HashSet<MessageId>,
  /// Share links the user created, by file.
  share_links: HashMap<String, ShareLink>,
  /// Open spaces the user isn't in yet.
  joinable: Vec<JoinableSpace>,
  /// Space roles other than member, and people removed from a space (the demo has everyone in
  /// every space otherwise).
  space_roles: HashMap<(SpaceId, UserId), SpaceRole>,
  channel_roles: HashMap<(ConversationId, UserId), teamcraft_client_api::model::ChannelRole>,
  integration_sources: Vec<(ConversationId, teamcraft_client_api::model::IntegrationSource)>,
  space_leavers: HashSet<(SpaceId, UserId)>,
  invites: Vec<(SpaceId, PendingInvite)>,
  /// Changes to a conversation (archived, an admin who joined) that messages report.
  workspace_events: HashMap<MessageId, WorkspaceEvent>,
  /// Who types where (tests set it; the demo has nobody typing).
  typing: Vec<crate::model::TypingNow>,
}

impl MockBackend {
  pub fn new(now: TimestampMs) -> Self {
    let users = demo_users();
    let mut state = State {
      users,
      spaces: vec![
        Space {
          id: space_a(),
          name: "Project A".into(),
          hue: 215,
          maybe_color: None,
          maybe_image_version: None,
          unread_count: 0,
          mention_count: 0,
          maybe_description: Some("Planning and delivery of Project A".into()),
          member_count: 6,
          join_policy: JoinPolicy::Open,
          can_add_members: true,
          can_invite_guests: true,
          can_manage_space: true,
          can_create_channels: true,
          maybe_my_role: Some(SpaceRole::SpaceAdmin),
        },
        Space {
          id: space_b(),
          name: "Project B".into(),
          hue: 28,
          maybe_color: None,
          maybe_image_version: None,
          unread_count: 0,
          mention_count: 0,
          maybe_description: None,
          member_count: 4,
          join_policy: JoinPolicy::InviteOnly,
          can_add_members: false,
          can_invite_guests: false,
          can_manage_space: false,
          can_create_channels: false,
          maybe_my_role: Some(SpaceRole::Member),
        },
      ],
      conversations: demo_conversations(),
      messages: HashMap::new(),
      replies: HashMap::new(),
      next_message_id: 1_000_000,
      backgrounds: Vec::new(),
      next_background_id: 1,
      saved: HashSet::new(),
      share_links: HashMap::new(),
      joinable: vec![JoinableSpace {
        id: id_from_raw(3),
        name: "Sales team".into(),
        maybe_description: Some("Offers, customers and the weekly pipeline".into()),
        hue: 140,
        maybe_image_version: None,
        member_count: 12,
      }],
      space_roles: HashMap::new(),
      channel_roles: HashMap::new(),
      integration_sources: Vec::new(),
      space_leavers: HashSet::new(),
      invites: vec![(
        space_a(),
        PendingInvite {
          id: id_from_raw(900),
          email: "new.colleague@example.com".into(),
          role: SpaceRole::Member,
        },
      )],
      workspace_events: HashMap::new(),
      typing: Vec::new(),
    };
    state.seed_messages(now);
    state.refresh_space_badges();
    let users_snapshot = state.users.clone();
    Self {
      now,
      ops: RefCell::new(ops::OpsState::new(now, &users_snapshot)),
      search: RefCell::new(search_impl::SearchState::new()),
      state: RefCell::new(state),
      people: RefCell::new(DemoPeople {
        is_activity_read: false,
        last_seen: LastSeenVisibility::Everyone,
      }),
    }
  }

  /// A workspace without spaces, channels or DMs, as a new instance's first owner sees it.
  #[cfg(test)]
  pub fn empty_workspace(now: TimestampMs) -> Self {
    let backend = Self::new(now);
    {
      let mut state = backend.state.borrow_mut();
      state.spaces.clear();
      state.conversations.clear();
      state.messages.clear();
      state.replies.clear();
    }
    backend
  }

  /// Lets `message` report `event` in its extras from now on, as a backend whose extras arrive
  /// after the message does.
  #[cfg(test)]
  pub fn set_workspace_event(&self, message: MessageId, event: WorkspaceEvent) {
    self.state.borrow_mut().workspace_events.insert(message, event);
  }

  /// Who types where from now on, as the core would answer after `typing_start` frames.
  #[cfg(test)]
  pub fn set_typing(&self, typing: Vec<crate::model::TypingNow>) {
    self.state.borrow_mut().typing = typing;
  }

  /// A top-level message from someone else arrives in `conversation`, as the core counts it.
  #[cfg(test)]
  pub fn receive(&self, conversation: ConversationId, is_mention: bool) {
    let mut state = self.state.borrow_mut();
    if let Some(conv) = state
      .conversations
      .iter_mut()
      .find(|conv| conv.id == conversation)
    {
      conv.unread_count += 1;
      conv.mention_count += u32::from(is_mention);
    }
    state.refresh_space_badges();
  }
}

impl ChatBackend for MockBackend {
  fn typing_now(&self) -> Vec<crate::model::TypingNow> {
    self.state.borrow().typing.clone()
  }

  /// Like the server: channel members must be in the channel's space (here: in one of its
  /// conversations), or the add is refused with `not_space_member`.
  fn add_member_with_history(&self, conversation: ConversationId, user: UserId, _history: HistoryPolicy) {
    let mut state = self.state.borrow_mut();
    let Some(space) = state
      .conversations
      .iter()
      .find(|conv| conv.id == conversation)
      .and_then(|conv| conv.maybe_space)
    else {
      return;
    };
    let is_in_space = state
      .conversations
      .iter()
      .any(|conv| conv.maybe_space == Some(space) && conv.member_ids.contains(&user));
    if !is_in_space {
      drop(state);
      report_failure(ActionFailure::new(
        FailureKind::Server(ErrorCode::NotSpaceMember),
        "server: server answered 400: the user is not a member of the space",
      ));
      return;
    }
    if let Some(conv) = state
      .conversations
      .iter_mut()
      .find(|conv| conv.id == conversation)
      && !conv.member_ids.contains(&user)
    {
      conv.member_ids.push(user);
    }
  }

  fn activity(&self) -> ActivityFeed {
    let is_read = self.people.borrow().is_activity_read;
    let count = self.message_count(ch_general());
    let recent = self.messages(ch_general(), count.saturating_sub(12), 12);
    let others = recent.iter().rev().filter(|message| message.author != me());
    let items = others
      .take(3)
      .zip([
        ActivityKind::Mention,
        ActivityKind::Reaction { emoji: "🎉".into() },
        ActivityKind::ThreadReply,
      ])
      .enumerate()
      .map(|(index, (message, kind))| ActivityItem {
        id: format!("demo:{index}"),
        kind,
        conversation: message.conversation,
        message: message.id,
        maybe_thread: None,
        actor: message.author,
        at: message.sent_at,
        excerpt: message.body.chars().take(140).collect(),
        maybe_read_through: None,
        is_unread: !is_read && index < 2,
      })
      .collect();
    ActivityFeed::from_items(items)
  }

  fn mark_activity_read(&self) {
    self.people.borrow_mut().is_activity_read = true;
  }

  /// The space's public channels, plus one the demo user hasn't joined (for the preview).
  fn browse_channels(&self, space: SpaceId) -> Vec<teamcraft_client_api::actions::BrowsableChannel> {
    let state = self.state.borrow();
    let mut list: Vec<_> = state
      .conversations
      .iter()
      .filter(|conv| conv.maybe_space == Some(space) && conv.kind == ConversationKind::Public)
      .map(|conv| teamcraft_client_api::actions::BrowsableChannel {
        conversation: conv.id,
        space,
        name: conv.name.clone(),
        topic: conv.topic.clone(),
        is_member: true,
        is_archived: false,
        has_external_people: false,
        maybe_category: None,
        join: teamcraft_client_api::actions::BrowseJoin::Open,
      })
      .collect();
    if space == space_a() {
      list.push(teamcraft_client_api::actions::BrowsableChannel {
        conversation: id_from_raw(90),
        space,
        name: "support".into(),
        topic: "Questions from customers, triaged daily".into(),
        is_member: false,
        is_archived: false,
        has_external_people: true,
        maybe_category: None,
        join: teamcraft_client_api::actions::BrowseJoin::Open,
      });
      // A private channel, as an admin's browser lists it (admin.md §2.4).
      list.push(teamcraft_client_api::actions::BrowsableChannel {
        conversation: id_from_raw(91),
        space,
        name: "leadership".into(),
        topic: String::new(),
        is_member: false,
        is_archived: false,
        has_external_people: false,
        maybe_category: None,
        join: teamcraft_client_api::actions::BrowseJoin::AsAdmin,
      });
    }
    list
  }

  fn last_seen(&self, user: UserId) -> Option<TimestampMs> {
    let state = self.state.borrow();
    let person = state.users.iter().find(|candidate| candidate.id == user)?;
    (person.presence == PresenceStatus::Offline).then(|| self.now - 95 * MINUTE)
  }

  fn privacy(&self) -> PrivacySettings {
    PrivacySettings {
      last_seen: self.people.borrow().last_seen.clone(),
      is_last_seen_available: true,
    }
  }

  fn set_last_seen_visibility(&self, visibility: LastSeenVisibility) {
    self.people.borrow_mut().last_seen = visibility;
  }

  fn call_history(&self) -> Vec<CallHistoryItem> {
    let direct = self.direct_conversations();
    let Some(first) = direct.first() else {
      return Vec::new();
    };
    let call = |index: i64, direction, minutes: i64| CallHistoryItem {
      id: format!("demo-call-{index}"),
      conversation: first.id,
      direction,
      started_at: self.now - index * 180 * MINUTE,
      maybe_ended_at: (direction != CallDirection::Missed)
        .then(|| self.now - index * 180 * MINUTE + minutes * MINUTE),
      maybe_started_by: first.member_ids.first().copied(),
      participants: first.member_ids.clone(),
    };
    vec![
      call(1, CallDirection::Missed, 0),
      call(2, CallDirection::Outgoing, 14),
      call(3, CallDirection::Incoming, 32),
    ]
  }

  fn background_images(&self) -> Vec<BackgroundImageInfo> {
    self.state.borrow().backgrounds.clone()
  }

  fn add_background_image(&self, name: &str, picture: Vec<u8>) {
    let mut state = self.state.borrow_mut();
    let id = id_from_raw(state.next_background_id);
    state.next_background_id += 1;
    state.backgrounds.push(BackgroundImageInfo {
      id,
      name: name.to_owned(),
      size_bytes: picture.len() as u64,
    });
  }

  fn delete_background_image(&self, id: BackgroundId) {
    self.state.borrow_mut().backgrounds.retain(|image| image.id != id);
  }

  fn me(&self) -> User {
    self.state.borrow().users[0].clone()
  }

  fn user(&self, id: UserId) -> Option<User> {
    self
      .state
      .borrow()
      .users
      .iter()
      .find(|user| user.id == id)
      .cloned()
  }

  fn users(&self) -> Vec<User> {
    self.state.borrow().users.clone()
  }

  fn spaces(&self) -> Vec<Space> {
    self.state.borrow().spaces.clone()
  }

  fn space_members(&self, space: SpaceId) -> SpaceRoster {
    let state = self.state.borrow();
    let mut members: Vec<SpaceMember> = state
      .users
      .iter()
      .filter(|user| !user.is_bot && !state.space_leavers.contains(&(space, user.id)))
      .map(|user| SpaceMember {
        user: user.id,
        display_name: user.display_name.clone(),
        is_guest: user.is_guest,
        is_bot: false,
        role: state
          .space_roles
          .get(&(space, user.id))
          .cloned()
          .unwrap_or(if user.is_guest {
            SpaceRole::Guest
          } else {
            SpaceRole::Member
          }),
      })
      .collect();
    members.sort_by_key(|member| member.display_name.to_lowercase());
    SpaceRoster::Loaded(members)
  }

  fn space_invites(&self, space: SpaceId) -> Vec<PendingInvite> {
    let state = self.state.borrow();
    state
      .invites
      .iter()
      .filter(|(of, _)| *of == space)
      .map(|(_, invite)| invite.clone())
      .collect()
  }

  fn integration_sources(
    &self,
    conversation: ConversationId,
  ) -> Vec<teamcraft_client_api::model::IntegrationSource> {
    self
      .state
      .borrow()
      .integration_sources
      .iter()
      .filter(|(id, _)| *id == conversation)
      .map(|(_, source)| source.clone())
      .collect()
  }

  fn can_manage_integration_sources(&self) -> bool {
    true
  }

  fn add_integration_source(
    &self,
    conversation: ConversationId,
    source: teamcraft_client_api::model::NewIntegrationSource,
  ) {
    let listed = teamcraft_client_api::model::IntegrationSource {
      id: source.id,
      name: source.name,
      kind: source.kind,
      created_by: me(),
      created_at_ms: crate::clock::now_ms(),
    };
    self
      .state
      .borrow_mut()
      .integration_sources
      .push((conversation, listed));
  }

  fn revoke_integration_source(&self, conversation: ConversationId, source: teamcraft_proto::SourceId) {
    self
      .state
      .borrow_mut()
      .integration_sources
      .retain(|(id, listed)| !(*id == conversation && listed.id == source));
  }

  fn channel_roles(&self, conversation: ConversationId) -> Vec<teamcraft_client_api::model::ChannelMember> {
    use teamcraft_client_api::model::{ChannelMember, ChannelRole};
    let state = self.state.borrow();
    let Some(found) = state.conversations.iter().find(|conv| conv.id == conversation) else {
      return Vec::new();
    };
    found
      .member_ids
      .iter()
      .map(|user| ChannelMember {
        user: *user,
        role: state
          .channel_roles
          .get(&(conversation, *user))
          .cloned()
          .unwrap_or(if *user == me() {
            ChannelRole::Admin
          } else {
            ChannelRole::Member
          }),
        is_role_signed: true,
      })
      .collect()
  }

  fn set_channel_role(
    &self,
    conversation: ConversationId,
    user: UserId,
    role: teamcraft_client_api::model::ChannelRole,
  ) {
    self
      .state
      .borrow_mut()
      .channel_roles
      .insert((conversation, user), role);
  }

  fn set_space_role(&self, space: SpaceId, user: UserId, role: SpaceRole) {
    self.state.borrow_mut().space_roles.insert((space, user), role);
  }

  fn remove_space_member(&self, space: SpaceId, user: UserId) {
    self.state.borrow_mut().space_leavers.insert((space, user));
  }

  fn revoke_space_invite(&self, space: SpaceId, invite: InviteId) {
    self
      .state
      .borrow_mut()
      .invites
      .retain(|(of, candidate)| !(*of == space && candidate.id == invite));
  }

  fn joinable_spaces(&self) -> Vec<JoinableSpace> {
    self.state.borrow().joinable.clone()
  }

  fn join_space(&self, space: SpaceId) {
    let mut state = self.state.borrow_mut();
    if let Some(index) = state.joinable.iter().position(|candidate| candidate.id == space) {
      let joined = state.joinable.remove(index);
      state.spaces.push(Space {
        member_count: joined.member_count + 1,
        ..joined.tile()
      });
    }
  }

  fn space_conversations(&self, space: SpaceId) -> Vec<Conversation> {
    let state = self.state.borrow();
    state
      .conversations
      .iter()
      .filter(|conv| conv.maybe_space == Some(space))
      .cloned()
      .collect()
  }

  fn direct_conversations(&self) -> Vec<Conversation> {
    let state = self.state.borrow();
    state
      .conversations
      .iter()
      .filter(|conv| conv.kind.is_direct_like())
      .cloned()
      .collect()
  }

  fn conversation(&self, id: ConversationId) -> Option<Conversation> {
    self
      .state
      .borrow()
      .conversations
      .iter()
      .find(|conv| conv.id == id)
      .cloned()
  }

  fn message_count(&self, conversation: ConversationId) -> usize {
    match log_lines(conversation) {
      Some(lines) => lines,
      None => self
        .state
        .borrow()
        .messages
        .get(&conversation)
        .map_or(0, Vec::len),
    }
  }

  fn messages(&self, conversation: ConversationId, start: usize, len: usize) -> Vec<Message> {
    let total = self.message_count(conversation);
    let end = total.min(start.saturating_add(len));
    if start >= end {
      return Vec::new();
    }
    if log_lines(conversation).is_some() {
      return (start..end)
        .map(|index| log_line(conversation, index, total, self.now))
        .collect();
    }
    self
      .state
      .borrow()
      .messages
      .get(&conversation)
      .map_or_else(Vec::new, |all| all[start..end].to_vec())
  }

  fn thread(&self, conversation: ConversationId, thread: ThreadId) -> Vec<Message> {
    let state = self.state.borrow();
    let mut messages: Vec<Message> = state
      .messages
      .get(&conversation)
      .and_then(|all| all.iter().find(|message| message.id == root_message_of(thread)))
      .cloned()
      .into_iter()
      .collect();
    if let Some(replies) = state.replies.get(&conversation) {
      messages.extend(
        replies
          .iter()
          .filter(|reply| reply.maybe_thread == Some(thread))
          .cloned(),
      );
    }
    messages
  }

  fn followed_threads(&self) -> Vec<ThreadSummary> {
    let state = self.state.borrow();
    let mut summaries = Vec::new();
    for (conversation, replies) in &state.replies {
      let Some(roots) = state.messages.get(conversation) else {
        continue;
      };
      for root in roots.iter().filter(|message| message.reply_count > 0) {
        let thread_replies: Vec<&Message> = replies
          .iter()
          .filter(|reply| reply.maybe_thread == Some(thread_id_of(root.id)))
          .collect();
        let Some(last) = thread_replies.last() else {
          continue;
        };
        let mut participants = vec![root.author];
        for reply in &thread_replies {
          if !participants.contains(&reply.author) {
            participants.push(reply.author);
          }
        }
        // Only threads the user takes part in are "followed".
        if !participants.contains(&me()) {
          continue;
        }
        let unread = thread_replies
          .iter()
          .filter(|reply| reply.author != me())
          .count()
          .min(3) as u32;
        summaries.push(ThreadSummary {
          conversation: *conversation,
          root: root.clone(),
          reply_count: thread_replies.len() as u32,
          unread_replies: if last.author == me() { 0 } else { unread },
          last_reply_at: last.sent_at,
          participant_ids: participants,
          has_mention: last.body.contains("@alex"),
        });
      }
    }
    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.last_reply_at));
    summaries
  }

  fn post_message(&self, conversation: ConversationId, maybe_thread: Option<ThreadId>, body: &str) {
    let body = body.trim();
    if body.is_empty() {
      return;
    }
    let mut state = self.state.borrow_mut();
    let id = id_from_raw::<MessageId>(state.next_message_id);
    state.next_message_id += 1;
    let message = Message {
      id,
      conversation,
      author: me(),
      kind: MessageKind::User,
      sent_at: self.now + state.next_message_id as i64 % 1000,
      body: body.to_owned(),
      is_edited: false,
      reactions: Vec::new(),
      reply_count: 0,
      maybe_thread,
      maybe_imported: None,
      maybe_placeholder: None,
    };
    match maybe_thread {
      Some(thread) => {
        if let Some(root_message) = state.messages.get_mut(&conversation).and_then(|all| {
          all
            .iter_mut()
            .find(|message| message.id == root_message_of(thread))
        }) {
          root_message.reply_count += 1;
        }
        state.replies.entry(conversation).or_default().push(message);
      },
      None => state.messages.entry(conversation).or_default().push(message),
    }
  }

  fn toggle_reaction(&self, conversation: ConversationId, message: MessageId, emoji: &str) {
    let mut state = self.state.borrow_mut();
    let found = state
      .messages
      .get_mut(&conversation)
      .and_then(|all| all.iter_mut().find(|candidate| candidate.id == message));
    let target = match found {
      Some(target) => target,
      None => {
        let Some(reply) = state
          .replies
          .get_mut(&conversation)
          .and_then(|all| all.iter_mut().find(|candidate| candidate.id == message))
        else {
          return;
        };
        reply
      },
    };
    match target
      .reactions
      .iter()
      .position(|reaction| reaction.emoji == emoji)
    {
      Some(index) => {
        let reaction = &mut target.reactions[index];
        if reaction.reacted_by_me {
          reaction.reacted_by_me = false;
          reaction.count -= 1;
        } else {
          reaction.reacted_by_me = true;
          reaction.count += 1;
        }
        if reaction.count == 0 {
          target.reactions.remove(index);
        }
      },
      None => target.reactions.push(Reaction {
        emoji: emoji.to_owned(),
        count: 1,
        reacted_by_me: true,
      }),
    }
  }

  fn mark_read(&self, conversation: ConversationId) {
    let mut state = self.state.borrow_mut();
    if let Some(conv) = state
      .conversations
      .iter_mut()
      .find(|conv| conv.id == conversation)
    {
      conv.unread_count = 0;
      conv.mention_count = 0;
    }
    state.refresh_space_badges();
  }

  fn set_favorite(&self, conversation: ConversationId, is_favorite: bool) {
    let mut state = self.state.borrow_mut();
    if let Some(conv) = state
      .conversations
      .iter_mut()
      .find(|conv| conv.id == conversation)
    {
      conv.is_favorite = is_favorite;
    }
  }

  fn message_extras(&self, conversation: ConversationId, messages: &[MessageId]) -> Vec<MessageExtras> {
    let files = self.conversation_files(conversation);
    let state = self.state.borrow();
    messages
      .iter()
      .map(|&message| MessageExtras {
        is_saved: state.saved.contains(&message),
        files: files
          .iter()
          .filter(|file| file.message == message)
          .cloned()
          .collect(),
        maybe_workspace_event: state.workspace_events.get(&message).cloned(),
        ..MessageExtras::new(message)
      })
      .collect()
  }

  fn set_saved(&self, _conversation: ConversationId, message: MessageId, is_saved: bool) {
    let mut state = self.state.borrow_mut();
    match is_saved {
      true => state.saved.insert(message),
      false => state.saved.remove(&message),
    };
  }

  fn permalink(&self, conversation: ConversationId, message: MessageId) -> Option<String> {
    Some(format!("teamcraft:/c/{conversation}/m/{message}"))
  }

  fn resolve_permalink(&self, link: &str) -> Option<PermalinkTarget> {
    self.state.borrow().find_linked(link).map(|(target, _)| target)
  }

  fn linked_message(&self, link: &str) -> Option<LinkedMessage> {
    let Some((target, message)) = self.state.borrow().find_linked(link) else {
      return Some(LinkedMessage::Unavailable);
    };
    let files = self
      .conversation_files(target.conversation)
      .into_iter()
      .filter(|file| file.message == target.message)
      .collect();
    Some(LinkedMessage::Visible {
      target,
      message,
      files,
    })
  }

  fn conversation_files(&self, conversation: ConversationId) -> Vec<FileInfo> {
    if conversation != ch_design() {
      return Vec::new();
    }
    let state = self.state.borrow();
    let messages = state.messages.get(&conversation).map_or(&[][..], Vec::as_slice);
    files::demo_files(conversation, messages)
  }

  fn create_share_link(
    &self,
    _conversation: ConversationId,
    _message: MessageId,
    file_id: &str,
    options: ShareLinkOptions,
  ) {
    let link = ShareLink {
      file_id: file_id.to_owned(),
      share_id: format!("demo{file_id}"),
      download_count: 0,
      url: format!("https://chat.example.com/s/demo{file_id}#demo-key"),
      maybe_expires_at: options
        .maybe_expires_in_seconds
        .map(|seconds| crate::clock::now_ms() + seconds as i64 * 1000),
      maybe_max_downloads: options.maybe_max_downloads,
      has_password: options.maybe_password.is_some(),
    };
    self
      .state
      .borrow_mut()
      .share_links
      .insert(file_id.to_owned(), link);
  }

  fn revoke_share_link(
    &self,
    _conversation: ConversationId,
    _message: MessageId,
    file_id: &str,
    share_id: &str,
  ) {
    let mut state = self.state.borrow_mut();
    if state
      .share_links
      .get(file_id)
      .is_some_and(|link| link.share_id == share_id)
    {
      state.share_links.remove(file_id);
    }
  }

  fn share_links(&self, _conversation: ConversationId, _message: MessageId, file_id: &str) -> Vec<ShareLink> {
    self
      .state
      .borrow()
      .share_links
      .get(file_id)
      .cloned()
      .into_iter()
      .collect()
  }

  fn set_presence(&self, presence: PresenceStatus) {
    self.state.borrow_mut().users[0].presence = presence;
  }

  fn set_custom_status(&self, maybe_status: Option<CustomStatus>) {
    self.state.borrow_mut().users[0].maybe_custom_status =
      maybe_status.map(|status| crate::live::with_local_expiry(status, crate::clock::now_ms()));
  }
}

impl State {
  fn seed_messages(&mut self, now: TimestampMs) {
    let seeds: [(ConversationId, usize); 8] = [
      (ch_general(), 60),
      (ch_announcements(), 6),
      (ch_engineering(), 420),
      (ch_design(), 80),
      (ch_leadership(), 25),
      (ch_b_general(), 40),
      (ch_b_planning(), 30),
      (id_from_raw(DM_FIRST), 18),
    ];
    for (conversation, count) in seeds {
      self.generate_conversation(conversation, count, now);
    }
    for dm in DM_FIRST + 1..DM_FIRST + 6 {
      self.generate_conversation(id_from_raw::<ConversationId>(dm), 12, now);
    }
    self.seed_message_links();
  }

  /// A few recent messages in #general link to other messages: one in another channel, a thread
  /// reply, and one in a conversation the user isn't in (its preview says so).
  fn seed_message_links(&mut self) {
    let Some(linked) = self
      .messages
      .get(&ch_design())
      .and_then(|all| {
        all
          .iter()
          .rev()
          .find(|message| matches!(message.kind, MessageKind::User))
      })
      .map(|message| format!("teamcraft:/c/{}/m/{}", message.conversation, message.id))
    else {
      return;
    };
    let maybe_reply = self
      .replies
      .get(&ch_engineering())
      .and_then(|replies| replies.last())
      .and_then(|reply| {
        reply
          .maybe_thread
          .map(|thread| format!("teamcraft:/c/{}/m/{}/t/{thread}", reply.conversation, reply.id))
      });
    let hidden = "teamcraft:/c/999/m/99900001".to_owned();
    let bodies = [
      Some(format!("Here is the mockup we talked about: {linked}")),
      maybe_reply.map(|link| format!("Answered in the thread: {link}")),
      Some(format!("Context from the other team: {hidden}")),
    ];
    let Some(general) = self.messages.get_mut(&ch_general()) else {
      return;
    };
    let mut users = general
      .iter_mut()
      .rev()
      .skip(1)
      .filter(|message| matches!(message.kind, MessageKind::User));
    for body in bodies.into_iter().flatten() {
      if let Some(message) = users.nth(1) {
        message.body = body;
      }
    }
  }

  /// The message a demo permalink points to, if the demo user can see it.
  fn find_linked(&self, link: &str) -> Option<(PermalinkTarget, Message)> {
    let Some(crate::route::Route::Message {
      conversation,
      message,
      maybe_thread,
    }) = crate::route::Route::from_permalink(link)
    else {
      return None;
    };
    let found = self
      .messages
      .get(&conversation)
      .into_iter()
      .chain(self.replies.get(&conversation))
      .flatten()
      .find(|candidate| candidate.id == message)?
      .clone();
    let target = PermalinkTarget {
      conversation,
      message,
      maybe_thread: maybe_thread.or(found.maybe_thread),
    };
    Some((target, found))
  }

  fn generate_conversation(&mut self, conversation: ConversationId, count: usize, now: TimestampMs) {
    let mut rng = Rng::new(raw_id(conversation) + 7);
    let human_ids: Vec<UserId> = self
      .users
      .iter()
      .filter(|user| !user.is_bot)
      .map(|user| user.id)
      .collect();
    let members: Vec<UserId> = match self.conversations.iter().find(|conv| conv.id == conversation) {
      Some(conv) if conv.kind.is_direct_like() => conv.member_ids.clone(),
      _ => human_ids,
    };
    let mut messages = Vec::with_capacity(count);
    let mut replies = Vec::new();
    // Generate backwards from "now" so the newest message is recent.
    let mut at = now - 3 * MINUTE;
    let mut author = members[rng.below(members.len())];
    for index in (0..count).rev() {
      if rng.below(4) == 0 {
        author = members[rng.below(members.len())];
      }
      let id = id_from_raw::<MessageId>(raw_id(conversation) * 100_000 + index as u64 + 1);
      let kind = if rng.below(40) == 0 {
        MessageKind::System(if rng.below(2) == 0 {
          SystemEvent::Joined
        } else {
          SystemEvent::Left
        })
      } else {
        MessageKind::User
      };
      let body = match kind {
        MessageKind::User if rng.below(25) == 0 => CODE_SNIPPET.to_owned(),
        _ => SENTENCES[rng.below(SENTENCES.len())].to_owned(),
      };
      let mut message = Message {
        id,
        conversation,
        author,
        kind,
        sent_at: at,
        body,
        is_edited: rng.below(12) == 0,
        reactions: Vec::new(),
        reply_count: 0,
        maybe_thread: None,
        maybe_imported: None,
        maybe_placeholder: None,
      };
      if matches!(message.kind, MessageKind::User) {
        for _ in 0..rng.below(3) {
          let emoji = REACTION_EMOJI[rng.below(REACTION_EMOJI.len())];
          if message.reactions.iter().all(|reaction| reaction.emoji != emoji) {
            message.reactions.push(Reaction {
              emoji: emoji.to_owned(),
              count: 1 + rng.below(4) as u32,
              reacted_by_me: rng.below(4) == 0,
            });
          }
        }
        if rng.below(9) == 0
          && !self
            .conversations
            .iter()
            .any(|c| c.id == conversation && c.kind.is_direct_like())
        {
          let reply_count = 1 + rng.below(4);
          message.reply_count = reply_count as u32;
          for reply_index in 0..reply_count {
            let replier = if reply_index % 2 == 0 {
              me()
            } else {
              members[rng.below(members.len())]
            };
            let body = if reply_index == reply_count - 1 && replier != me() && rng.below(3) == 0 {
              "@alex does that match what you saw yesterday?".to_owned()
            } else {
              SENTENCES[rng.below(SENTENCES.len())].to_owned()
            };
            replies.push(Message {
              id: id_from_raw::<MessageId>(raw_id(id) * 100 + reply_index as u64 + 1),
              conversation,
              author: replier,
              kind: MessageKind::User,
              sent_at: at + (reply_index as i64 + 1) * 4 * MINUTE,
              body,
              is_edited: false,
              reactions: Vec::new(),
              reply_count: 0,
              maybe_thread: Some(thread_id_of(id)),
              maybe_imported: None,
              maybe_placeholder: None,
            });
          }
        }
      }
      messages.push(message);
      // Mostly short gaps (grouped messages), sometimes a long pause or a day break.
      at -= match rng.below(10) {
        0 => (6 + rng.below(40) as i64) * 60 * MINUTE,
        1..=3 => (6 + rng.below(20) as i64) * MINUTE,
        _ => (10 + rng.below(100) as i64) * 1000,
      };
    }
    messages.reverse();
    self.messages.insert(conversation, messages);
    replies.sort_by_key(|reply: &Message| reply.sent_at);
    self.replies.insert(conversation, replies);
  }

  fn refresh_space_badges(&mut self) {
    for space in &mut self.spaces {
      let in_space = self
        .conversations
        .iter()
        .filter(|conv| conv.maybe_space == Some(space.id));
      let (unread, mentions) = in_space
        .filter(|conv| !conv.is_muted)
        .fold((0, 0), |(unread, mentions), conv| {
          (unread + conv.unread_count, mentions + conv.mention_count)
        });
      space.unread_count = unread;
      space.mention_count = mentions;
    }
  }
}

fn log_lines(conversation: ConversationId) -> Option<usize> {
  if conversation == ch_ci() {
    Some(2_400)
  } else if conversation == ch_app_logs() {
    Some(LOG_CHANNEL_LINES)
  } else {
    None
  }
}

/// Builds log line `index` without storing it; the newest line has the largest index.
fn log_line(conversation: ConversationId, index: usize, total: usize, now: TimestampMs) -> Message {
  let mut rng = Rng::new(index as u64 ^ (raw_id(conversation) << 32));
  let (level, texts) = match rng.below(100) {
    0..=3 => (LogLevel::Error, LOG_ERROR),
    4..=14 => (LogLevel::Warn, LOG_WARN),
    15..=29 => (LogLevel::Debug, LOG_DEBUG),
    _ => (LogLevel::Info, LOG_INFO),
  };
  let service = LOG_SERVICES[rng.below(LOG_SERVICES.len())];
  let text = texts[rng.below(texts.len())];
  Message {
    id: id_from_raw::<MessageId>(raw_id(conversation) * 1_000_000 + index as u64 + 1),
    conversation,
    author: bot(),
    kind: MessageKind::Log(level),
    sent_at: now - (total - index) as i64 * 1500,
    body: format!("[{service}] {text} seq={index}"),
    is_edited: false,
    reactions: Vec::new(),
    reply_count: 0,
    maybe_thread: None,
    maybe_imported: None,
    maybe_placeholder: None,
  }
}

fn demo_users() -> Vec<User> {
  let people: [(u64, &str, &str, PresenceStatus, u16); 9] = [
    (1, "alex", "Alex Morgan", PresenceStatus::Online, 210),
    (2, "sam", "Sam Rivera", PresenceStatus::Online, 150),
    (3, "jordan", "Jordan Lee", PresenceStatus::Away, 30),
    (4, "robin", "Robin Chen", PresenceStatus::DoNotDisturb, 340),
    (5, "kim", "Kim Novak", PresenceStatus::Offline, 280),
    (6, "taylor", "Taylor Brooks", PresenceStatus::Online, 100),
    (7, "devon", "Devon Park", PresenceStatus::Away, 60),
    (8, "casey", "Casey Diaz", PresenceStatus::Offline, 190),
    (9, "guest", "Riley Guest", PresenceStatus::Online, 0),
  ];
  let mut users: Vec<User> = people
    .into_iter()
    .map(|(id, username, display_name, presence, avatar_hue)| User {
      id: id_from_raw::<UserId>(id),
      username: username.into(),
      display_name: display_name.into(),
      presence,
      maybe_custom_status: None,
      avatar_hue,
      is_guest: id == 9,
      is_bot: false,
      // Two affiliated companies share the demo workspace; the guest belongs to neither.
      maybe_organization: match id {
        1..=4 => Some("Brand A".to_owned()),
        5..=8 => Some("Brand B".to_owned()),
        _ => None,
      },
      // The last colleague hasn't set up the app yet.
      is_deactivated: false,
      is_deleted: false,
      device_setup: if id == 8 {
        crate::model::DeviceSetup::NoDevices
      } else {
        crate::model::DeviceSetup::HasDevices
      },
      maybe_email: Some(format!("{username}@example.com")),
    })
    .collect();
  users[3].maybe_custom_status = Some(CustomStatus {
    emoji: "🎧".into(),
    text: "Focus time".into(),
    expiry: crate::model::StatusExpiry::Today,
    maybe_expires_at: Some(crate::clock::end_of_local_day(crate::clock::now_ms())),
  });
  users[6].maybe_custom_status = Some(CustomStatus {
    emoji: "🌴".into(),
    text: "Vacation until Monday".into(),
    expiry: crate::model::StatusExpiry::ThisWeek,
    maybe_expires_at: Some(crate::clock::end_of_local_week(crate::clock::now_ms())),
  });
  users.push(User {
    id: bot(),
    username: "build-bot".into(),
    display_name: "Build Bot".into(),
    presence: PresenceStatus::Online,
    maybe_custom_status: None,
    avatar_hue: 260,
    is_guest: false,
    is_bot: true,
    maybe_organization: None,
    is_deactivated: false,
    is_deleted: false,
    device_setup: crate::model::DeviceSetup::HasDevices,
    maybe_email: None,
  });
  users
}

/// (id, name, kind, member ids, unread count, mention count)
type DemoDirect = (u64, &'static str, ConversationKind, &'static [u64], u32, u32);

fn demo_conversations() -> Vec<Conversation> {
  let everyone: Vec<UserId> = (1..=8).map(id_from_raw::<UserId>).collect();
  let channel = |id: ConversationId,
                 space: SpaceId,
                 kind: ConversationKind,
                 category: SidebarCategory,
                 name: &str,
                 topic: &str,
                 unread: u32,
                 mentions: u32| Conversation {
    id,
    maybe_space: Some(space),
    kind,
    category,
    name: name.into(),
    topic: topic.into(),
    member_ids: everyone.clone(),
    unread_count: unread,
    mention_count: mentions,
    is_favorite: false,
    is_muted: false,
    can_post: true,
    bookmarks: Vec::new(),
    is_catching_up: false,
    is_notes_to_self: false,
    maybe_permissions: None,
    has_only_deactivated_others: false,
    maybe_image_version: None,
  };
  use ConversationKind::{Integration, Private, Public};
  use SidebarCategory::{Channels, Integrations};
  let mut list = vec![
    channel(
      ch_general(),
      space_a(),
      Public,
      Channels,
      "general",
      "Company-wide announcements and chatter",
      3,
      0,
    ),
    channel(
      ch_announcements(),
      space_a(),
      Public,
      Channels,
      "announcements",
      "Read-only: releases and policies",
      0,
      0,
    ),
    channel(
      ch_engineering(),
      space_a(),
      Public,
      Channels,
      "engineering",
      "Builds, reviews and incident follow-ups",
      12,
      2,
    ),
    channel(
      ch_design(),
      space_a(),
      Public,
      Channels,
      "design",
      "Mockups, critiques and the design system",
      0,
      0,
    ),
    channel(
      ch_leadership(),
      space_a(),
      Private,
      Channels,
      "leadership",
      "Planning and staffing",
      1,
      0,
    ),
    channel(
      ch_ci(),
      space_a(),
      Integration,
      Integrations,
      "ci-builds",
      "Build and test results from the CI pipeline",
      0,
      0,
    ),
    channel(
      ch_app_logs(),
      space_a(),
      Integration,
      Integrations,
      "app-logs",
      "Production application logs (50,000 lines)",
      0,
      0,
    ),
    channel(
      ch_b_general(),
      space_b(),
      Public,
      Channels,
      "general",
      "Everything about Project B",
      0,
      0,
    ),
    channel(
      ch_b_planning(),
      space_b(),
      Public,
      Channels,
      "planning",
      "Roadmap and milestones",
      5,
      1,
    ),
  ];
  list[1].can_post = false;
  list[5].can_post = false;
  list[6].can_post = false;
  list[2].is_favorite = true;
  list[2].bookmarks = vec![
    Bookmark {
      emoji: "📘".into(),
      title: "Runbook".into(),
      kind: BookmarkKind::Link,
      url: "https://example.com/runbook".into(),
      maybe_message: None,
    },
    Bookmark {
      emoji: "📅".into(),
      title: "Release calendar".into(),
      kind: BookmarkKind::Link,
      url: "https://example.com/calendar".into(),
      maybe_message: None,
    },
    Bookmark {
      emoji: "📄".into(),
      title: "architecture-overview.pdf".into(),
      kind: BookmarkKind::File,
      url: String::new(),
      maybe_message: None,
    },
  ];
  list[7].is_muted = false;
  let dms: [DemoDirect; 6] = [
    (DM_FIRST, "Sam Rivera", ConversationKind::Direct, &[1, 2], 2, 2),
    (
      DM_FIRST + 1,
      "Jordan Lee",
      ConversationKind::Direct,
      &[1, 3],
      0,
      0,
    ),
    (
      DM_FIRST + 2,
      "Robin Chen",
      ConversationKind::Direct,
      &[1, 4],
      0,
      0,
    ),
    (DM_FIRST + 3, "Kim Novak", ConversationKind::Direct, &[1, 5], 0, 0),
    (
      DM_FIRST + 4,
      "Taylor Brooks, Devon Park",
      ConversationKind::Group,
      &[1, 6, 7],
      1,
      0,
    ),
    (
      DM_FIRST + 5,
      "Riley Guest",
      ConversationKind::Direct,
      &[1, 9],
      0,
      0,
    ),
  ];
  for (id, name, kind, members, unread, mentions) in dms {
    list.push(Conversation {
      id: id_from_raw::<ConversationId>(id),
      maybe_space: None,
      kind,
      category: SidebarCategory::DirectMessages,
      name: name.into(),
      topic: String::new(),
      member_ids: members.iter().copied().map(id_from_raw::<UserId>).collect(),
      unread_count: unread,
      mention_count: mentions,
      is_favorite: id == DM_FIRST,
      is_muted: false,
      can_post: true,
      bookmarks: Vec::new(),
      is_catching_up: false,
      is_notes_to_self: false,
      maybe_permissions: None,
      has_only_deactivated_others: false,
      maybe_image_version: None,
    });
  }
  list
}

#[cfg(test)]
mod tests {
  use super::*;

  const NOW: TimestampMs = 1_792_584_000_000;

  #[test]
  fn log_channel_is_generated_lazily_and_stable() {
    let backend = MockBackend::new(NOW);
    assert_eq!(backend.message_count(ch_app_logs()), LOG_CHANNEL_LINES);
    let first = backend.messages(ch_app_logs(), 49_990, 10);
    let again = backend.messages(ch_app_logs(), 49_990, 10);
    assert_eq!(first, again);
    assert_eq!(first.len(), 10);
    assert!(first.windows(2).all(|pair| pair[0].sent_at < pair[1].sent_at));
  }

  #[test]
  fn ranges_are_clipped() {
    let backend = MockBackend::new(NOW);
    assert!(backend.messages(ch_general(), 10_000, 5).is_empty());
    assert_eq!(backend.messages(ch_general(), 55, 100).len(), 5);
  }

  #[test]
  fn posting_a_reply_bumps_the_thread_count() {
    let backend = MockBackend::new(NOW);
    let root = backend
      .messages(ch_general(), 0, 60)
      .into_iter()
      .find(|m| matches!(m.kind, MessageKind::User));
    let root = root.expect("demo data contains user messages");
    backend.post_message(ch_general(), Some(thread_id_of(root.id)), "reply");
    let thread = backend.thread(ch_general(), thread_id_of(root.id));
    assert_eq!(thread.last().map(|m| m.body.as_str()), Some("reply"));
    assert_eq!(thread[0].reply_count, root.reply_count + 1);
  }

  #[test]
  fn reaction_toggle_adds_and_removes() {
    let backend = MockBackend::new(NOW);
    let message = backend
      .messages(ch_design(), 0, 80)
      .into_iter()
      .find(|m| m.reactions.is_empty());
    let message = message.expect("some message has no reactions");
    backend.toggle_reaction(ch_design(), message.id, "🎉");
    let after = backend
      .messages(ch_design(), 0, 80)
      .into_iter()
      .find(|m| m.id == message.id);
    assert_eq!(after.map(|m| m.reactions.len()), Some(1));
    backend.toggle_reaction(ch_design(), message.id, "🎉");
    let after = backend
      .messages(ch_design(), 0, 80)
      .into_iter()
      .find(|m| m.id == message.id);
    assert_eq!(after.map(|m| m.reactions.len()), Some(0));
  }
}
