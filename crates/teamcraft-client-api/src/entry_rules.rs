//! The rules behind the menu of a sidebar entry (#903, #1105): whether leaving a channel or a
//! space is offered, and the address of a conversation. Shared by the desktop/web UI and the
//! phone apps (through the FFI), so the apps never re-implement them in Kotlin or Swift.

use crate::model::{
  ChannelMember, ChannelRole, Conversation, ConversationId, ConversationKind, SpaceRole, SpaceRoster, UserId,
};

/// Whether "Leave" is offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaveChoice {
  /// Not for this entry (DMs, group DMs, a space the user only has channels in).
  Hidden,
  Allowed,
  /// The user is the last admin: the server refuses until someone else is one.
  LastAdmin,
}

/// The channel rule of the server (`authorize_removal`): only channels can be left, and the last
/// channel admin of a channel with others in it must hand the role on first. Roles not loaded yet
/// leave the decision to the server.
pub fn channel_leave(conversation: &Conversation, me: UserId, roles: &[ChannelMember]) -> LeaveChoice {
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
pub fn space_leave(maybe_role: Option<&SpaceRole>, me: UserId, roster: &SpaceRoster) -> LeaveChoice {
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
    .then(|| format!("{origin}/c/{conversation}"))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::model::{SidebarCategory, SpaceMember, id_from_raw};

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
