//! A workspace admin's look at a channel's roles before confirming them (e2ee.md §4.6.1, "Reading
//! the log late"): which rows the list shows, what is marked, whom it takes a role from and why it
//! can't be confirmed. Shared by desktop/web (`teamcraft-ui`) and the mobile apps (`teamcraft-ffi`),
//! so every client judges the same list the same way.
//!
//! The list is the server's word and the names its directory's, so what this device's snapshot
//! doesn't have is marked, somebody who would lose a role is named, and somebody without a name or
//! with a key this device can't check can't be confirmed. What is confirmed is the list as it stood
//! when the admin opened it: if it moved since, they look again.

use teamcraft_proto::UserId;

use crate::model::{ChannelMember, ChannelRole, RoleKeyMark, User};

/// One role in the list a workspace admin reviews.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RoleReviewRow {
  pub user: UserId,
  pub role: ChannelRole,
  /// `None`: the member list names nobody for this role, so nobody can look at who it is.
  pub maybe_name: Option<String>,
  /// This device's snapshot doesn't give this person this role: the server's word alone.
  pub is_new: bool,
  pub key_mark: RoleKeyMark,
}

/// Why a reviewed list can't be confirmed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RoleReviewRefusal {
  /// The member list doesn't name somebody the list gives a role.
  UnknownPerson,
  /// This device can't check the key of somebody the list gives a role.
  UncheckableKey,
  /// A channel keeps an admin.
  NoAdmin,
}

impl RoleReviewRefusal {
  /// The text that says so (`channels.ftl`).
  pub fn i18n_key(self) -> &'static str {
    match self {
      Self::UnknownPerson => "channel-roles-review-unknown-refused",
      Self::UncheckableKey => "channel-roles-review-uncheckable-refused",
      Self::NoAdmin => "channel-roles-review-no-admin",
    }
  }
}

/// The admins and moderators the member list shows, against this device's snapshot.
pub fn role_review_rows(
  roles: &[ChannelMember],
  users: &[User],
  snapshot: &[(UserId, ChannelRole)],
  key_marks: &[(UserId, RoleKeyMark)],
) -> Vec<RoleReviewRow> {
  roles
    .iter()
    .filter(|member| member.role != ChannelRole::Member)
    .map(|member| RoleReviewRow {
      user: member.user,
      role: member.role.clone(),
      maybe_name: users
        .iter()
        .find(|user| user.id == member.user && !user.display_name.trim().is_empty())
        .map(|user| user.display_name.clone()),
      is_new: !snapshot
        .iter()
        .any(|(user, role)| *user == member.user && *role == member.role),
      // Not looked at yet counts as what it is: not checked.
      key_mark: key_marks
        .iter()
        .find(|(user, _)| *user == member.user)
        .map_or(RoleKeyMark::Uncheckable, |(_, mark)| *mark),
    })
    .collect()
}

fn role_rank(role: &ChannelRole) -> u8 {
  match role {
    ChannelRole::Admin => 2,
    ChannelRole::Moderator => 1,
    ChannelRole::Member | ChannelRole::Unknown(_) => 0,
  }
}

/// Who has a role on this device that the list takes away or lowers: their name, `None` where
/// nobody knows who that is.
pub fn dropped_role_holders(
  rows: &[RoleReviewRow],
  users: &[User],
  snapshot: &[(UserId, ChannelRole)],
) -> Vec<Option<String>> {
  snapshot
    .iter()
    .filter(|(user, role)| {
      rows
        .iter()
        .find(|row| row.user == *user)
        .is_none_or(|row| role_rank(&row.role) < role_rank(role))
    })
    .map(|(user, _)| {
      users
        .iter()
        .find(|known| known.id == *user)
        .map(|known| known.display_name.clone())
    })
    .collect()
}

/// [`dropped_role_holders`] with `unknown` for somebody nobody can name.
pub fn dropped_role_names(
  rows: &[RoleReviewRow],
  users: &[User],
  snapshot: &[(UserId, ChannelRole)],
  unknown: &str,
) -> Vec<String> {
  dropped_role_holders(rows, users, snapshot)
    .into_iter()
    .map(|maybe_name| maybe_name.unwrap_or_else(|| unknown.to_owned()))
    .collect()
}

/// Why this list can't be confirmed: nobody confirms a role for somebody they can't see, nor a key
/// this device can't check, and a channel keeps an admin.
pub fn role_review_refusal(rows: &[RoleReviewRow]) -> Option<RoleReviewRefusal> {
  if rows.iter().any(|row| row.maybe_name.is_none()) {
    return Some(RoleReviewRefusal::UnknownPerson);
  }
  if rows.iter().any(|row| row.key_mark == RoleKeyMark::Uncheckable) {
    return Some(RoleReviewRefusal::UncheckableKey);
  }
  if !rows.iter().any(|row| row.role == ChannelRole::Admin) {
    return Some(RoleReviewRefusal::NoAdmin);
  }
  None
}

/// Whether the list or the log moved since the admin opened the list: what they saw is no longer
/// what would be confirmed, so they look again.
pub fn has_review_changed(
  shown_cursor: Option<u64>,
  shown_rows: &[RoleReviewRow],
  live_cursor: Option<u64>,
  live_rows: &[RoleReviewRow],
) -> bool {
  shown_cursor != live_cursor || shown_rows != live_rows
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  fn id(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }

  fn named(raw: i64, name: &str) -> User {
    User {
      id: id(raw),
      username: name.to_lowercase(),
      display_name: name.into(),
      presence: crate::model::PresenceStatus::Online,
      maybe_custom_status: None,
      avatar_hue: 0,
      is_guest: false,
      is_bot: false,
      maybe_organization: None,
      is_deactivated: false,
      is_deleted: false,
      device_setup: crate::model::DeviceSetup::Unknown,
      maybe_email: None,
    }
  }

  fn member(raw: i64, role: ChannelRole) -> ChannelMember {
    ChannelMember {
      user: id(raw),
      role,
      maybe_appointer: None,
    }
  }

  fn row(raw: i64, role: ChannelRole, name: Option<&str>) -> RoleReviewRow {
    RoleReviewRow {
      user: id(raw),
      role,
      maybe_name: name.map(str::to_owned),
      is_new: false,
      key_mark: RoleKeyMark::Checked,
    }
  }

  #[test]
  fn who_this_device_s_snapshot_does_not_have_in_that_role_is_marked_new() {
    let roles = [
      member(1, ChannelRole::Admin),
      member(2, ChannelRole::Admin),
      member(3, ChannelRole::Moderator),
      member(4, ChannelRole::Member),
    ];
    let snapshot = [(id(1), ChannelRole::Admin), (id(3), ChannelRole::Admin)];
    let rows = role_review_rows(&roles, &[], &snapshot, &[]);
    let new: Vec<_> = rows.iter().map(|row| (row.user, row.is_new)).collect();
    assert_eq!(
      new,
      vec![(id(1), false), (id(2), true), (id(3), true)],
      "somebody it doesn't name, a role it doesn't give, and no members"
    );
  }

  /// A key this device can't check can't be confirmed, and neither can one it hasn't looked at.
  #[test]
  fn a_key_this_device_can_t_check_can_not_be_confirmed() {
    let roles = [member(1, ChannelRole::Admin), member(2, ChannelRole::Admin)];
    let users = [named(1, "Alice"), named(2, "Bob")];
    let marks = [(id(1), RoleKeyMark::Checked)];
    let rows = role_review_rows(&roles, &users, &[], &marks);
    assert_eq!(rows[1].key_mark, RoleKeyMark::Uncheckable);
    assert_eq!(
      role_review_refusal(&rows),
      Some(RoleReviewRefusal::UncheckableKey)
    );
    let marks = [(id(1), RoleKeyMark::Checked), (id(2), RoleKeyMark::Changed)];
    let rows = role_review_rows(&roles, &users, &[], &marks);
    assert_eq!(
      role_review_refusal(&rows),
      None,
      "a changed key is marked, not refused"
    );
  }

  /// The server lowering somebody is as much for the reviewer to see as leaving them out.
  #[test]
  fn who_the_list_lowers_is_named() {
    let rows = [
      row(1, ChannelRole::Moderator, Some("Alice")),
      row(2, ChannelRole::Admin, Some("Bob")),
    ];
    let snapshot = [(id(1), ChannelRole::Admin), (id(2), ChannelRole::Moderator)];
    let users = [named(1, "Alice"), named(2, "Bob")];
    assert_eq!(
      dropped_role_names(&rows, &users, &snapshot, "Unknown person"),
      vec!["Alice".to_owned()]
    );
  }

  /// The server leaving somebody out is what a reviewer must see: it takes the role from them.
  #[test]
  fn who_the_list_leaves_out_is_named() {
    let rows = [row(1, ChannelRole::Admin, Some("Alice"))];
    let snapshot = [
      (id(1), ChannelRole::Admin),
      (id(2), ChannelRole::Admin),
      (id(3), ChannelRole::Moderator),
    ];
    assert_eq!(dropped_role_holders(&rows, &[], &snapshot), vec![None, None]);
    assert_eq!(
      dropped_role_names(&rows, &[], &snapshot, "Unknown person"),
      vec!["Unknown person".to_owned(), "Unknown person".to_owned()]
    );
    assert!(dropped_role_names(&rows, &[], &snapshot[..1], "Unknown person").is_empty());
  }

  #[test]
  fn somebody_the_member_list_has_no_name_for_can_not_be_signed() {
    let rows = [
      row(1, ChannelRole::Admin, Some("Alice")),
      row(2, ChannelRole::Moderator, None),
    ];
    assert_eq!(role_review_refusal(&rows), Some(RoleReviewRefusal::UnknownPerson));
  }

  #[test]
  fn a_list_without_an_admin_can_not_be_signed_and_a_full_one_can() {
    let moderators = [row(2, ChannelRole::Moderator, Some("Bob"))];
    assert_eq!(role_review_refusal(&moderators), Some(RoleReviewRefusal::NoAdmin));
    assert_eq!(role_review_refusal(&[]), Some(RoleReviewRefusal::NoAdmin));
    let full = [row(1, ChannelRole::Admin, Some("Alice"))];
    assert_eq!(role_review_refusal(&full), None);
  }

  #[test]
  fn a_moved_log_or_list_is_a_change_and_the_same_one_is_not() {
    let rows = [row(1, ChannelRole::Admin, Some("Alice"))];
    assert!(!has_review_changed(Some(3), &rows, Some(3), &rows));
    assert!(
      has_review_changed(Some(3), &rows, Some(4), &rows),
      "the log moved"
    );
    assert!(
      has_review_changed(Some(3), &rows, None, &rows),
      "no review any more"
    );
    let lowered = [row(1, ChannelRole::Moderator, Some("Alice"))];
    assert!(
      has_review_changed(Some(3), &rows, Some(3), &lowered),
      "the list moved"
    );
  }

  #[test]
  fn every_refusal_has_its_own_text() {
    let keys = [
      RoleReviewRefusal::UnknownPerson.i18n_key(),
      RoleReviewRefusal::UncheckableKey.i18n_key(),
      RoleReviewRefusal::NoAdmin.i18n_key(),
    ];
    assert_eq!(keys.iter().collect::<std::collections::HashSet<_>>().len(), 3);
    assert!(keys.iter().all(|key| key.starts_with("channel-roles-review-")));
  }
}
