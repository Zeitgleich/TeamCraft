//! What the menu of a sidebar entry offers (#903): a channel or chat, or a space on the rail or
//! in the sidebar's head. Only actions the user may take are offered (#691); where leaving is not
//! allowed for a reason the user can fix, the entry says why instead of failing after the click.
//! The menu itself is `components::entry_menu`; the rules are here, without the DOM, so they are
//! tested natively.

use leptos::prelude::*;

use crate::model::{
  ChannelMember, ChannelRole, Conversation, ConversationId, ConversationKind, SpaceId, SpaceRole, SpaceRoster,
};
use crate::route::Route;
use crate::state::AppState;

/// The entry a menu is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MenuEntry {
  Conversation(ConversationId),
  Space(SpaceId),
}

/// Whether "Leave" is offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaveChoice {
  /// Not for this entry (DMs, group DMs, a space the user only has channels in).
  Hidden,
  Allowed,
  /// The user is the last admin: the server refuses until someone else is one.
  LastAdmin,
}

/// The menu of a channel or chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversationMenu {
  pub has_unread: bool,
  pub is_muted: bool,
  /// The address to share; `None` where the app has no web address (a desktop without a server).
  pub maybe_link: Option<String>,
  pub leave: LeaveChoice,
}

/// The menu of a space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceMenu {
  pub name: String,
  pub has_unread: bool,
  pub is_muted: bool,
  pub leave: LeaveChoice,
}

impl ConversationMenu {
  /// The menu of a conversation the user is in; `None` for one that is gone. `maybe_origin` is
  /// where the web app lives ([`crate::components::space_invites::web_origin`]).
  pub fn of(state: AppState, conversation: ConversationId, maybe_origin: Option<&str>) -> Option<Self> {
    let found = state.read(|backend| backend.conversation(conversation))?;
    let roles = state.read(|backend| backend.channel_roles(conversation));
    let is_muted = state
      .with_mutes(vec![found.clone()])
      .first()
      .is_some_and(|conversation| conversation.is_muted);
    Some(Self {
      has_unread: found.unread_count > 0 || found.mention_count > 0,
      is_muted,
      maybe_link: maybe_origin.and_then(|origin| conversation_link(origin, conversation)),
      leave: channel_leave(&found, state.me().id, &roles),
    })
  }
}

impl SpaceMenu {
  /// The menu of a space the user sees; `None` for one that is gone.
  pub fn of(state: AppState, space: SpaceId) -> Option<Self> {
    let found = state
      .read(|backend| backend.spaces())
      .into_iter()
      .find(|candidate| candidate.id == space)?;
    let has_unread = state
      .read(|backend| backend.space_conversations(space))
      .iter()
      .any(|conversation| conversation.unread_count > 0 || conversation.mention_count > 0);
    let is_muted = state
      .settings
      .prefs
      .with(|prefs| prefs.notifications.is_space_muted(&space.to_string()));
    let roster = if found.maybe_my_role == Some(SpaceRole::SpaceAdmin) {
      state.read(|backend| backend.space_members(space))
    } else {
      SpaceRoster::Loading
    };
    Some(Self {
      name: found.name,
      has_unread,
      is_muted,
      leave: space_leave(found.maybe_my_role.as_ref(), state.me().id, &roster),
    })
  }
}

/// The channel rule of the server (`authorize_removal`): only channels can be left, and the last
/// channel admin of a channel with others in it must hand the role on first. Roles not loaded yet
/// leave the decision to the server.
pub fn channel_leave(
  conversation: &Conversation,
  me: crate::model::UserId,
  roles: &[ChannelMember],
) -> LeaveChoice {
  if !matches!(
    conversation.kind,
    ConversationKind::Public | ConversationKind::Private
  ) {
    return LeaveChoice::Hidden;
  }
  let is_admin = roles
    .iter()
    .any(|member| member.user == me && member.role == ChannelRole::Admin);
  let admins = roles
    .iter()
    .filter(|member| member.role == ChannelRole::Admin)
    .count();
  if is_admin && admins <= 1 && conversation.member_ids.len() > 1 {
    LeaveChoice::LastAdmin
  } else {
    LeaveChoice::Allowed
  }
}

/// The space rule of the server (`remove_space_member`): any member may leave, the last space
/// admin may not. Whether another admin exists is known once the member list is loaded; until
/// then an admin is not offered to leave.
pub fn space_leave(
  maybe_role: Option<&SpaceRole>,
  me: crate::model::UserId,
  roster: &SpaceRoster,
) -> LeaveChoice {
  match maybe_role {
    None => LeaveChoice::Hidden,
    Some(SpaceRole::SpaceAdmin) => match roster {
      SpaceRoster::Loaded(members)
        if members
          .iter()
          .any(|member| member.user != me && member.role == SpaceRole::SpaceAdmin) =>
      {
        LeaveChoice::Allowed
      },
      _ => LeaveChoice::LastAdmin,
    },
    Some(_) => LeaveChoice::Allowed,
  }
}

/// The address of a conversation for colleagues: the web app's path of it under the server's
/// origin. Only http(s) origins make a link someone else can open.
pub fn conversation_link(origin: &str, conversation: ConversationId) -> Option<String> {
  let origin = origin.trim_end_matches('/');
  (origin.starts_with("https://") || origin.starts_with("http://"))
    .then(|| format!("{origin}{}", Route::Conversation(conversation).path()))
}

impl AppState {
  /// Marks every channel of the space as read.
  pub fn mark_space_read(&self, space: SpaceId) {
    let unread: Vec<ConversationId> = self
      .read(|backend| backend.space_conversations(space))
      .into_iter()
      .filter(|conversation| conversation.unread_count > 0 || conversation.mention_count > 0)
      .map(|conversation| conversation.id)
      .collect();
    if unread.is_empty() {
      return;
    }
    self.write(|backend| {
      for conversation in unread {
        backend.mark_read(conversation);
      }
    });
  }

  /// Leaves the space and its channels; the chats open if it was the one shown. A refusal reaches
  /// the user through the backend's failure toast.
  pub fn leave_space(&self, space: SpaceId) {
    if self.space.get_untracked() == space {
      self.open_chats();
    }
    let me = self.me().id;
    self.write(|backend| backend.remove_space_member(space, me));
  }
}

#[cfg(test)]
mod tests {
  use std::rc::Rc;

  use super::*;
  use crate::backend::Backend;
  use crate::mock::MockBackend;
  use crate::model::{SidebarCategory, SpaceMember, TimestampMs, UserId, id_from_raw};
  use crate::settings::Settings;

  const NOW: TimestampMs = 1_792_584_000_000;

  mod leaving_a_channel {
    use super::*;

    #[test]
    fn dms_and_group_dms_have_no_leave() {
      for kind in [ConversationKind::Direct, ConversationKind::Group] {
        let conversation = conversation(kind, 2);
        assert_eq!(channel_leave(&conversation, me(), &[]), LeaveChoice::Hidden);
      }
    }

    #[test]
    fn members_and_moderators_may_leave() {
      for role in [ChannelRole::Member, ChannelRole::Moderator] {
        let roles = [member(me(), role), member(other(), ChannelRole::Admin)];
        for kind in [ConversationKind::Public, ConversationKind::Private] {
          assert_eq!(
            channel_leave(&conversation(kind, 2), me(), &roles),
            LeaveChoice::Allowed
          );
        }
      }
    }

    #[test]
    fn the_last_admin_is_told_to_hand_the_role_on() {
      let roles = [
        member(me(), ChannelRole::Admin),
        member(other(), ChannelRole::Member),
      ];
      assert_eq!(
        channel_leave(&conversation(ConversationKind::Public, 2), me(), &roles),
        LeaveChoice::LastAdmin
      );
      // Alone in the channel, the last admin may go (the server allows it).
      assert_eq!(
        channel_leave(&conversation(ConversationKind::Public, 1), me(), &roles[..1]),
        LeaveChoice::Allowed
      );
      // A second admin frees the first.
      let two_admins = [
        member(me(), ChannelRole::Admin),
        member(other(), ChannelRole::Admin),
      ];
      assert_eq!(
        channel_leave(&conversation(ConversationKind::Public, 2), me(), &two_admins),
        LeaveChoice::Allowed
      );
    }

    #[test]
    fn unknown_roles_leave_it_to_the_server() {
      assert_eq!(
        channel_leave(&conversation(ConversationKind::Private, 5), me(), &[]),
        LeaveChoice::Allowed
      );
    }
  }

  mod leaving_a_space {
    use super::*;

    #[test]
    fn members_and_guests_may_leave() {
      for role in [SpaceRole::Member, SpaceRole::Guest] {
        assert_eq!(
          space_leave(Some(&role), me(), &SpaceRoster::Loading),
          LeaveChoice::Allowed
        );
      }
    }

    #[test]
    fn without_a_role_there_is_nothing_to_leave() {
      assert_eq!(
        space_leave(None, me(), &SpaceRoster::Loading),
        LeaveChoice::Hidden
      );
    }

    #[test]
    fn an_admin_may_leave_only_beside_another_admin() {
      let admin = Some(&SpaceRole::SpaceAdmin);
      assert_eq!(
        space_leave(admin, me(), &SpaceRoster::Loading),
        LeaveChoice::LastAdmin
      );
      let alone = SpaceRoster::Loaded(vec![
        space_member(me(), SpaceRole::SpaceAdmin),
        space_member(other(), SpaceRole::Member),
      ]);
      assert_eq!(space_leave(admin, me(), &alone), LeaveChoice::LastAdmin);
      let together = SpaceRoster::Loaded(vec![
        space_member(me(), SpaceRole::SpaceAdmin),
        space_member(other(), SpaceRole::SpaceAdmin),
      ]);
      assert_eq!(space_leave(admin, me(), &together), LeaveChoice::Allowed);
    }
  }

  mod links {
    use super::*;

    #[test]
    fn a_conversation_links_to_its_path_on_the_server() {
      let id = id_from_raw::<ConversationId>(42);
      assert_eq!(
        conversation_link("https://chat.example.com/", id).as_deref(),
        Some("https://chat.example.com/c/42")
      );
      assert_eq!(conversation_link("tauri://localhost", id), None);
    }
  }

  mod wiring {
    use super::*;

    #[test]
    fn mark_space_read_reads_every_channel_of_the_space() {
      let state = demo_state();
      let space = state.read(|backend| backend.spaces())[1].id;
      assert!(SpaceMenu::of(state, space).is_some_and(|menu| menu.has_unread));
      state.mark_space_read(space);
      let unread: u32 = state
        .read(|backend| backend.space_conversations(space))
        .iter()
        .map(|conversation| conversation.unread_count)
        .sum();
      assert_eq!(unread, 0);
      assert!(SpaceMenu::of(state, space).is_some_and(|menu| !menu.has_unread));
    }

    #[test]
    fn a_muted_space_mutes_its_channels_in_their_menu() {
      let state = demo_state();
      let space = state.read(|backend| backend.spaces())[1].id;
      let channel = state.read(|backend| backend.space_conversations(space))[0].id;
      assert!(ConversationMenu::of(state, channel, None).is_some_and(|menu| !menu.is_muted));
      state.settings.prefs.update(|prefs| {
        prefs.notifications.set_space(
          &space.to_string(),
          crate::components::sidebar::space_mute_entry(true),
        );
      });
      assert!(SpaceMenu::of(state, space).is_some_and(|menu| menu.is_muted));
      assert!(ConversationMenu::of(state, channel, None).is_some_and(|menu| menu.is_muted));
    }

    #[test]
    fn mute_and_unmute_from_the_menu_reach_the_entry() {
      use crate::components::conversation_notify::{NotifyTarget, update_entry};
      use crate::prefs::MuteFor;

      let state = demo_state();
      let space = state.read(|backend| backend.spaces())[1].id;
      let channel = state.read(|backend| backend.space_conversations(space))[0].id;
      update_entry(state, NotifyTarget::Conversation(channel), |entry| {
        entry.mute = MuteFor::OneHour
      });
      assert!(ConversationMenu::of(state, channel, None).is_some_and(|menu| menu.is_muted));
      update_entry(state, NotifyTarget::Conversation(channel), |entry| {
        entry.mute = MuteFor::Off
      });
      assert!(ConversationMenu::of(state, channel, None).is_some_and(|menu| !menu.is_muted));
      update_entry(state, NotifyTarget::Space(space), |entry| {
        entry.mute = MuteFor::UntilTomorrow
      });
      assert!(SpaceMenu::of(state, space).is_some_and(|menu| menu.is_muted));
    }

    #[test]
    fn mark_read_clears_the_entry() {
      let state = demo_state();
      let space = state.read(|backend| backend.spaces())[1].id;
      let unread = state
        .read(|backend| backend.space_conversations(space))
        .into_iter()
        .find(|conversation| conversation.has_unread())
        .map(|conversation| conversation.id)
        .expect("an unread demo channel");
      assert!(ConversationMenu::of(state, unread, None).is_some_and(|menu| menu.has_unread));
      state.write(|backend| backend.mark_read(unread));
      assert!(ConversationMenu::of(state, unread, None).is_some_and(|menu| !menu.has_unread));
    }

    #[test]
    fn the_demo_admin_space_waits_for_the_member_list() {
      let state = demo_state();
      let spaces = state.read(|backend| backend.spaces());
      let (admin_space, member_space) = (spaces[0].id, spaces[1].id);
      assert_eq!(
        SpaceMenu::of(state, member_space).map(|menu| menu.leave),
        Some(LeaveChoice::Allowed)
      );
      assert_ne!(
        SpaceMenu::of(state, admin_space).map(|menu| menu.leave),
        Some(LeaveChoice::Hidden)
      );
    }

    #[test]
    fn a_dm_menu_offers_no_leave() {
      let state = demo_state();
      let dm = state
        .read(|backend| backend.direct_conversations())
        .into_iter()
        .find(|conversation| conversation.kind == ConversationKind::Direct)
        .map(|conversation| conversation.id)
        .expect("a demo DM");
      assert_eq!(
        ConversationMenu::of(state, dm, Some("https://chat.example.com"))
          .map(|menu| (menu.leave, menu.maybe_link)),
        Some((
          LeaveChoice::Hidden,
          Some(format!("https://chat.example.com/c/{dm}"))
        ))
      );
    }
  }

  fn demo_state() -> AppState {
    let backend: Rc<dyn Backend> = Rc::new(MockBackend::new(NOW));
    AppState::new(backend, Settings::for_tests(), None)
  }

  fn me() -> UserId {
    id_from_raw(1)
  }

  fn other() -> UserId {
    id_from_raw(2)
  }

  fn member(user: UserId, role: ChannelRole) -> ChannelMember {
    ChannelMember {
      user,
      role,
      is_role_signed: true,
    }
  }

  fn space_member(user: UserId, role: SpaceRole) -> SpaceMember {
    SpaceMember {
      user,
      display_name: String::new(),
      is_guest: false,
      is_bot: false,
      role,
    }
  }

  fn conversation(kind: ConversationKind, members: u64) -> Conversation {
    Conversation {
      id: id_from_raw(7),
      maybe_space: None,
      kind,
      category: SidebarCategory::Channels,
      name: "c7".into(),
      topic: String::new(),
      member_ids: (1..=members).map(id_from_raw).collect(),
      unread_count: 0,
      mention_count: 0,
      is_favorite: false,
      is_muted: false,
      can_post: true,
      bookmarks: Vec::new(),
      is_catching_up: false,
      is_notes_to_self: false,
      maybe_permissions: None,
      has_only_deactivated_others: false,
      maybe_image_version: None,
    }
  }
}
