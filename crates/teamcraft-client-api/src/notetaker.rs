//! The notetaker as the call screen sees it (e2ee.md §8.2): who it is comes from the verified
//! policy, whether it is in the call from the participant list; never from what the server or the
//! notetaker say about themselves. What the user may do is only what the screen offers: the
//! notetaker decides on every request from its own copy of the signed data. Desktop, web and the
//! mobile apps (through teamcraft-ffi) show the same.

use teamcraft_proto::{MeetingId, NotetakerInviters, NotetakerPolicy};

use crate::model::{Conversation, ConversationId, Space, TimestampMs, UserId};

/// What the call screen shows and offers about the notetaker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotetakerInCall {
  pub bot: UserId,
  /// Where the audio goes, for the notices.
  pub transcription_model: String,
  /// The notetaker is a participant of the call now.
  pub is_present: bool,
  /// The notetaker is a member of the conversation. "Add notetaker" adds it first otherwise, and
  /// asks it in once its membership commit is in (e2ee.md §8.2).
  pub is_member: bool,
  /// "Add notetaker": on, allowed, not in the call yet, and a member of the conversation or one
  /// the user can bring in (add there, or move the call into a room with it).
  pub can_invite: bool,
  /// "Remove notetaker".
  pub can_stop: bool,
  /// Why "Add notetaker" shows, but disabled (issue #1290): the policy doesn't let this user ask
  /// it in, where somebody who may would be offered the button. `None` while the button is
  /// offered, and where it isn't shown at all (the notetaker is off, in the call already, or
  /// can't be brought into this call).
  pub maybe_refusal: Option<NotetakerRefusal>,
}

/// Why the policy doesn't let the user ask the notetaker in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotetakerRefusal {
  /// Guests (and bots) never ask it in.
  Guest,
  /// Only workspace admins may ask it in (`inviters: admins`).
  AdminsOnly,
}

/// The notetaker of `policy` in a call with `participants` (the others), for the user in a
/// conversation with `member_ids`. `is_person`: the user is neither a guest nor a bot.
/// `can_bring_in`: the user can bring the notetaker into the call where it is no member (a
/// channel where the user manages members, a meeting the user organizes, or any other call, which
/// moves into a room with it; e2ee.md §8.2, "In every call"). `None` without a notetaker account in
/// the policy.
pub fn notetaker_in_call(
  policy: &NotetakerPolicy,
  is_person: bool,
  is_roster_admin: bool,
  member_ids: &[UserId],
  participants: &[UserId],
  can_bring_in: bool,
) -> Option<NotetakerInCall> {
  // Its account counts as the notetaker even while the policy turns it off: it is still in the
  // call until it has left, and the notices stay until then.
  let bot = policy.maybe_bot?;
  let is_present = participants.contains(&bot);
  let is_member = member_ids.contains(&bot);
  let may_invite = may_ask_in(policy, is_person, is_roster_admin);
  let can_stop = is_present && (policy.can_anyone_stop || may_invite || (is_roster_admin && is_person));
  let would_offer = policy.active_bot().is_some() && !is_present && (is_member || can_bring_in);
  let maybe_refusal = if !would_offer || may_invite {
    None
  } else if !is_person {
    Some(NotetakerRefusal::Guest)
  } else if policy.inviters == NotetakerInviters::Admins {
    Some(NotetakerRefusal::AdminsOnly)
  } else {
    None
  };
  Some(NotetakerInCall {
    bot,
    transcription_model: policy.transcription_model.clone(),
    is_present,
    is_member,
    can_invite: may_invite && !is_present && (is_member || can_bring_in),
    can_stop,
    maybe_refusal,
  })
}

/// Whether the user may ask the notetaker in, as the policy says (the notetaker checks again).
pub fn may_ask_in(policy: &NotetakerPolicy, is_person: bool, is_roster_admin: bool) -> bool {
  policy.active_bot().is_some()
    && is_person
    && match policy.inviters {
      NotetakerInviters::Members => true,
      NotetakerInviters::Admins => is_roster_admin,
      NotetakerInviters::Unknown(_) => false,
    }
}

/// How the user brings the notetaker into the call of a conversation where it is no member yet
/// (e2ee.md §8.2, "In every call").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddRoute {
  /// A channel of a space where the user manages members: the bot goes in as from the channel's
  /// integrations, into the space first (as a guest) where the user may put it there.
  Channel { joins_space: bool },
  /// A meeting the user organizes: the bot becomes one of its invitees, which puts it into the
  /// meeting chat's group (e2ee.md §4.6, "Meeting chats").
  Meeting(MeetingId),
  /// A conversation the user can't add the bot to: the call moves into a new meeting room with
  /// everybody in it and the bot (docs/meetings.md, "A call becomes a meeting"), where the core
  /// asks it in. The conversation keeps its members and history; the bot never sees them.
  MoveToRoom,
}

/// What the user can do in the conversation of the call, which decides the [`AddRoute`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConversationRights {
  /// A meeting the user organizes.
  OrganizesMeeting(MeetingId),
  /// A channel of a space where the user manages members.
  ManagesChannel { joins_space: bool },
  /// A DM or group DM (which takes nobody more), a channel whose members the user doesn't manage,
  /// or a meeting somebody else organizes: the user can't add anybody there.
  CannotAdd,
}

/// Where the user can bring the notetaker in: a meeting the user organizes (as an invitee), a
/// channel of a space where the user manages members (as from its integrations). Anywhere else
/// (a DM or group DM, a channel whose members somebody else manages, a meeting somebody else
/// organizes) its call moves into a meeting room with the notetaker, `can_move`: the app can move
/// calls; the call stays where it is for those who don't follow (e2ee.md §8.2, "In every call").
pub fn add_route(rights: ConversationRights, can_move: bool) -> Option<AddRoute> {
  match rights {
    ConversationRights::OrganizesMeeting(meeting) => Some(AddRoute::Meeting(meeting)),
    ConversationRights::ManagesChannel { joins_space } => Some(AddRoute::Channel { joins_space }),
    ConversationRights::CannotAdd => can_move.then_some(AddRoute::MoveToRoom),
  }
}

/// The user's rights in `conversation` for bringing the notetaker in: `maybe_organized`, the
/// meeting the user organizes whose chat it is; `can_add_members`, the user manages the channel's
/// members; `spaces`, the user's, for whether the bot may be put into the channel's space first.
pub fn conversation_rights(
  conversation: &Conversation,
  maybe_organized: Option<MeetingId>,
  can_add_members: bool,
  spaces: &[Space],
) -> ConversationRights {
  if let Some(meeting) = maybe_organized {
    return ConversationRights::OrganizesMeeting(meeting);
  }
  if !conversation.has_channel_settings() || !can_add_members {
    return ConversationRights::CannotAdd;
  }
  let joins_space = spaces
    .iter()
    .find(|space| Some(space.id) == conversation.maybe_space)
    .is_some_and(|space| space.can_add_members || space.can_invite_guests);
  ConversationRights::ManagesChannel { joins_space }
}

/// How long after the announcement "The call moved into a room" shows to latecomers (issue #1321).
/// Calls rarely run longer; the hint also needs the room's call to run, so this only keeps an old
/// announcement from pointing at a room whose call is a different one by now.
pub const MOVED_CALL_HINT_MAX_AGE_MS: i64 = 3 * 60 * 60_000;

/// A conversation's call that moved into a meeting room, as announced in the conversation (a
/// `CallMoved` event), for the hint that sends latecomers there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MovedCall {
  /// Who moved the call.
  pub by: UserId,
  /// The room, which the user is a member of: the core names no other (nobody is pointed at a
  /// room they can't open).
  pub room: ConversationId,
  /// The room's name as the sidebar shows it.
  pub room_name: String,
  /// When the announcement was sent.
  pub moved_at: TimestampMs,
}

/// The hint for somebody who comes to a conversation's call after it moved: `newest`, the newest
/// move announced in the conversation, if its room's call is running with other people in it
/// (`room_has_others`) and the announcement is younger than [`MOVED_CALL_HINT_MAX_AGE_MS`].
///
/// The announcement comes from a member of the conversation, who can point it at any room, but
/// the hint offers only a room the user is a member of already (the core names no other), shows
/// only while that room's call runs, and only opens the room: it joins nothing by itself.
pub fn moved_call_hint(
  newest: Option<MovedCall>,
  room_has_others: bool,
  now_ms: TimestampMs,
) -> Option<MovedCall> {
  newest.filter(|moved| {
    room_has_others
      && !moved.room_name.is_empty()
      && now_ms.saturating_sub(moved.moved_at) <= MOVED_CALL_HINT_MAX_AGE_MS
  })
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  fn moved(room_name: &str, moved_at: i64) -> MovedCall {
    MovedCall {
      by: id(1),
      room: id_of(5),
      room_name: room_name.into(),
      moved_at,
    }
  }

  #[test]
  fn latecomers_are_pointed_at_the_room_while_its_call_runs() {
    let hint = moved_call_hint(Some(moved("Call with notetaker", 1_000)), true, 60_000);
    assert_eq!(hint, Some(moved("Call with notetaker", 1_000)));
  }

  #[test]
  fn no_hint_without_a_move_or_once_the_rooms_call_is_over() {
    assert_eq!(moved_call_hint(None, true, 0), None);
    assert_eq!(moved_call_hint(Some(moved("Room", 0)), false, 1_000), None);
  }

  #[test]
  fn no_hint_for_a_room_the_user_is_not_in() {
    assert_eq!(moved_call_hint(Some(moved("", 0)), true, 1_000), None);
  }

  #[test]
  fn the_hint_goes_stale() {
    let at = 10_000;
    assert!(moved_call_hint(Some(moved("Room", at)), true, at + MOVED_CALL_HINT_MAX_AGE_MS).is_some());
    assert_eq!(
      moved_call_hint(Some(moved("Room", at)), true, at + MOVED_CALL_HINT_MAX_AGE_MS + 1),
      None
    );
    // A clock a little behind the sender's doesn't hide it.
    assert!(moved_call_hint(Some(moved("Room", at)), true, at - 5_000).is_some());
  }

  #[test]
  fn a_member_asks_it_in_where_it_is_a_member_and_not_yet_in_the_call() {
    let policy = policy(NotetakerInviters::Members);
    let alice = true;
    let in_call = notetaker_in_call(&policy, alice, false, &[id(1), id(9)], &[id(2)], false).unwrap();
    assert!(in_call.can_invite && !in_call.is_present && !in_call.can_stop);
    let elsewhere = notetaker_in_call(&policy, alice, false, &[id(1)], &[], false).unwrap();
    assert!(!elsewhere.can_invite, "not a member, and alice can't add it");
    assert!(!elsewhere.is_member);
    let present = notetaker_in_call(&policy, alice, false, &[id(1), id(9)], &[id(9)], false).unwrap();
    assert!(present.is_present && !present.can_invite && present.can_stop);
  }

  #[test]
  fn who_can_add_members_asks_it_in_where_it_is_no_member_yet() {
    let policy = policy(NotetakerInviters::Members);
    let in_call = notetaker_in_call(&policy, true, false, &[id(1)], &[id(1)], true).unwrap();
    assert!(in_call.can_invite && !in_call.is_member && !in_call.is_present);
    let present = notetaker_in_call(&policy, true, false, &[id(1), id(9)], &[id(9)], true).unwrap();
    assert!(!present.can_invite, "in the call already");
    let guest = notetaker_in_call(&policy, false, false, &[id(1)], &[], true).unwrap();
    assert!(
      !guest.can_invite,
      "the policy decides, not the right to add members"
    );
    let mut off = policy.clone();
    off.is_enabled = false;
    let disabled = notetaker_in_call(&off, true, false, &[id(1)], &[], true).unwrap();
    assert!(!disabled.can_invite);
  }

  #[test]
  fn with_admins_only_a_member_who_can_add_people_still_does_not_ask_it_in() {
    let policy = policy(NotetakerInviters::Admins);
    let member = notetaker_in_call(&policy, true, false, &[id(1)], &[], true).unwrap();
    assert!(!member.can_invite);
    let admin = notetaker_in_call(&policy, true, true, &[id(1)], &[], true).unwrap();
    assert!(admin.can_invite && !admin.is_member);
  }

  #[test]
  fn guests_never_ask_it_in_but_may_send_it_away_when_anyone_may() {
    let mut policy = policy(NotetakerInviters::Members);
    let guest = false;
    let present = notetaker_in_call(&policy, guest, false, &[id(9)], &[id(9)], false).unwrap();
    assert!(!present.can_invite && present.can_stop);
    policy.can_anyone_stop = false;
    let strict = notetaker_in_call(&policy, guest, false, &[id(9)], &[id(9)], false).unwrap();
    assert!(!strict.can_stop);
  }

  #[test]
  fn with_admins_only_members_neither_ask_nor_stop_unless_anyone_may() {
    let mut policy = policy(NotetakerInviters::Admins);
    policy.can_anyone_stop = false;
    let member = true;
    let absent = notetaker_in_call(&policy, member, false, &[id(9)], &[], false).unwrap();
    assert!(!absent.can_invite);
    let admin = notetaker_in_call(&policy, member, true, &[id(9)], &[], false).unwrap();
    assert!(admin.can_invite);
    let present = notetaker_in_call(&policy, member, false, &[id(9)], &[id(9)], false).unwrap();
    assert!(!present.can_stop);
  }

  /// #1290: who the policy keeps from asking it in sees the button disabled, with the reason;
  /// nobody sees it where it isn't on offer at all.
  #[test]
  fn the_button_shows_disabled_with_the_reason_the_policy_gives() {
    let admins_only = policy(NotetakerInviters::Admins);
    let member = notetaker_in_call(&admins_only, true, false, &[id(1), id(9)], &[], false).unwrap();
    assert_eq!(member.maybe_refusal, Some(NotetakerRefusal::AdminsOnly));
    let admin = notetaker_in_call(&admins_only, true, true, &[id(1), id(9)], &[], false).unwrap();
    assert_eq!(admin.maybe_refusal, None);
    let anyone = policy(NotetakerInviters::Members);
    let guest = notetaker_in_call(&anyone, false, false, &[id(1)], &[], true).unwrap();
    assert_eq!(guest.maybe_refusal, Some(NotetakerRefusal::Guest));
    let present = notetaker_in_call(&admins_only, true, false, &[id(9)], &[id(9)], false).unwrap();
    assert_eq!(present.maybe_refusal, None, "in the call already");
    let unreachable = notetaker_in_call(&admins_only, true, false, &[id(1)], &[], false).unwrap();
    assert_eq!(
      unreachable.maybe_refusal, None,
      "can't be brought into this call anyway"
    );
    let mut off = anyone.clone();
    off.is_enabled = false;
    let disabled = notetaker_in_call(&off, false, false, &[id(9)], &[], true).unwrap();
    assert_eq!(disabled.maybe_refusal, None, "turned off: no button");
  }

  #[test]
  fn a_disabled_notetaker_still_shows_while_it_is_in_the_call() {
    let mut policy = policy(NotetakerInviters::Members);
    policy.is_enabled = false;
    let in_call = notetaker_in_call(&policy, true, false, &[id(9)], &[id(9)], false).unwrap();
    assert!(in_call.is_present && !in_call.can_invite);
    policy.maybe_bot = None;
    assert_eq!(notetaker_in_call(&policy, true, false, &[], &[], true), None);
  }

  /// #1303: desktop, web and the apps bring the notetaker in on the same route: added in a meeting
  /// the user organizes and a channel whose members the user manages (into the space first where
  /// the user may put it there), moved into a room anywhere else.
  #[test]
  fn the_route_follows_what_the_user_can_do_in_the_conversation() {
    use crate::model::{ConversationKind, JoinPolicy, SidebarCategory};

    let space_id = id_of(2);
    let channel = |kind, maybe_space| Conversation {
      maybe_draft_preview: None,
      id: id_of(5),
      maybe_space,
      kind,
      category: SidebarCategory::Channels,
      name: "news".into(),
      topic: String::new(),
      member_ids: Vec::new(),
      unread_count: 0,
      mention_count: 0,
      is_favorite: false,
      is_muted: false,
      can_post: true,
      bookmarks: Vec::new(),
      is_catching_up: false,
      maybe_image_version: None,
      is_notes_to_self: false,
      is_archived: false,
      is_imported: false,
      maybe_permissions: Some(vec![teamcraft_proto::Permission::ManageMembers]),
      has_only_deactivated_others: false,
    };
    let space = |can_add_members, can_invite_guests| Space {
      id: space_id,
      name: "Project".into(),
      hue: 0,
      maybe_color: None,
      maybe_image_version: None,
      unread_count: 0,
      mention_count: 0,
      maybe_description: None,
      member_count: 3,
      join_policy: JoinPolicy::Open,
      can_add_members,
      can_invite_guests,
      can_manage_space: false,
      can_delete_space: false,
      deleted_channel_count: 0,
      can_create_channels: false,
      maybe_my_role: None,
    };
    let managed = channel(ConversationKind::Private, Some(space_id));
    assert!(managed.can_add_members());
    let rights = conversation_rights(&managed, None, managed.can_add_members(), &[space(false, true)]);
    assert_eq!(
      add_route(rights, true),
      Some(AddRoute::Channel { joins_space: true })
    );
    let rights = conversation_rights(&managed, None, true, &[space(false, false)]);
    assert_eq!(
      add_route(rights, true),
      Some(AddRoute::Channel { joins_space: false }),
      "the bot can't be put into the space: added to the channel only"
    );
    let rights = conversation_rights(&managed, None, false, &[space(true, true)]);
    assert_eq!(
      add_route(rights, true),
      Some(AddRoute::MoveToRoom),
      "members managed by others"
    );

    let meeting: MeetingId = id_of(7);
    let meeting_chat = channel(ConversationKind::Private, None);
    let rights = conversation_rights(&meeting_chat, Some(meeting), false, &[]);
    assert_eq!(add_route(rights, true), Some(AddRoute::Meeting(meeting)));
    let rights = conversation_rights(&meeting_chat, None, true, &[]);
    assert_eq!(
      add_route(rights, true),
      Some(AddRoute::MoveToRoom),
      "somebody else's meeting"
    );

    let mut group = channel(ConversationKind::Group, None);
    assert!(!group.can_add_members(), "a group DM takes nobody more");
    group.maybe_permissions = None;
    let rights = conversation_rights(&group, None, group.can_add_members(), &[]);
    assert_eq!(add_route(rights, true), Some(AddRoute::MoveToRoom));
    assert_eq!(add_route(rights, false), None, "an app that moves no calls");

    let mut older_server = managed.clone();
    older_server.maybe_permissions = None;
    assert!(
      !older_server.can_add_members(),
      "without the server's word the call moves"
    );
  }

  fn id_of<T: Id>(raw: i64) -> T {
    T::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn policy(inviters: NotetakerInviters) -> NotetakerPolicy {
    NotetakerPolicy {
      is_enabled: true,
      maybe_bot: Some(id(9)),
      inviters,
      ..NotetakerPolicy::default()
    }
  }

  fn id(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
