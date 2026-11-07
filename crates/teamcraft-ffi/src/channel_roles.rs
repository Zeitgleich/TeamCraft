//! A channel's members section for the mobile apps (issue #1262), as on desktop and web: the
//! members with their roles, the line for a channel where this device and a member's see different
//! roles, and a workspace admin's look at the roles before confirming them (e2ee.md §4.6.1,
//! "Reading the log late").
//!
//! What the look shows and refuses is the shared rule (`teamcraft_client_api::role_review`), so
//! the apps only draw it; confirming is the core's `sign_reviewed_channel_roles`, which signs
//! nothing if the log moved since the admin opened the list.

use std::cell::Cell;
use std::rc::Rc;

use teamcraft_client_api::backend::ChatBackend as _;
use teamcraft_client_api::model::{
  AppointerAuthority, ChannelRole, ConversationId, RoleAppointer, RoleKeyMark, User, UserId,
};
use teamcraft_client_api::role_review::{
  RoleReviewRefusal, RoleReviewRow, dropped_role_holders, has_review_changed, role_review_refusal,
  role_review_rows,
};
use teamcraft_core::client::RoleSigning;

use crate::TeamCraft;
use crate::engine::parse_id;
use crate::error::TeamCraftError;
use crate::reads::Reads;

/// A channel role as the apps show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum ChannelMemberRole {
  Admin,
  Moderator,
  Member,
  /// A role this version doesn't know (a newer server): shown without a badge, and a list with one
  /// isn't confirmed from this app.
  Other,
}

impl From<&ChannelRole> for ChannelMemberRole {
  fn from(role: &ChannelRole) -> Self {
    match role {
      ChannelRole::Admin => Self::Admin,
      ChannelRole::Moderator => Self::Moderator,
      ChannelRole::Member => Self::Member,
      ChannelRole::Unknown(_) => Self::Other,
    }
  }
}

impl ChannelMemberRole {
  /// The role to sign; `None` for one this version can't name.
  fn known(self) -> Option<ChannelRole> {
    match self {
      Self::Admin => Some(ChannelRole::Admin),
      Self::Moderator => Some(ChannelRole::Moderator),
      Self::Member => Some(ChannelRole::Member),
      Self::Other => None,
    }
  }
}

/// One member of the channel with the role the member list holds.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChannelMemberItem {
  pub user_id: String,
  pub name: String,
  pub role: ChannelMemberRole,
  /// Who appointed the role, from the signed role log this device verified (issue #1246); `None`
  /// says nothing (a member, or nothing verified).
  #[uniffi(default = None)]
  pub maybe_appointer: Option<ChannelMemberAppointer>,
}

/// On what authority a role's appointer signed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum AppointerAuthorityItem {
  /// `channel-role-appointer-owner`
  WorkspaceOwner,
  /// `channel-role-appointer-workspace-admin`
  WorkspaceAdmin,
  /// `channel-role-appointer-channel-admin`
  ChannelAdmin,
}

impl From<AppointerAuthority> for AppointerAuthorityItem {
  fn from(authority: AppointerAuthority) -> Self {
    match authority {
      AppointerAuthority::WorkspaceOwner => Self::WorkspaceOwner,
      AppointerAuthority::WorkspaceAdmin => Self::WorkspaceAdmin,
      AppointerAuthority::ChannelAdmin => Self::ChannelAdmin,
    }
  }
}

/// The verified signer of a role, as the line "Appointed by <name>, <authority>" says it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChannelMemberAppointer {
  /// The signer's name from the directory, which is the server's word and so only ever shown as a
  /// name, never as part of the sentence; `None` where they left the directory
  /// (`channel-role-appointed-by-*-unnamed`).
  pub maybe_name: Option<String>,
  pub authority: AppointerAuthorityItem,
}

/// The channel's members section.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChannelMembersInfo {
  /// By name, like desktop and web; empty until the member list is loaded.
  pub members: Vec<ChannelMemberItem>,
  /// This device and another member's see different roles here (`channel-roles-alarm`), for
  /// everybody in the channel.
  #[uniffi(default = false)]
  pub has_role_alarm: bool,
  /// For a workspace admin only: the look at the roles. `None` for anybody else.
  #[uniffi(default = None)]
  pub maybe_review: Option<ChannelRolesReviewItem>,
}

/// What this device can say about the key a confirmed role would name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum RoleKeyMarkItem {
  /// Nothing to mark.
  Checked,
  /// "Key changed": another key than the signed roles name.
  Changed,
  /// "Can't check": confirming is refused.
  Uncheckable,
}

impl From<RoleKeyMark> for RoleKeyMarkItem {
  fn from(mark: RoleKeyMark) -> Self {
    match mark {
      RoleKeyMark::Checked => Self::Checked,
      RoleKeyMark::Changed => Self::Changed,
      RoleKeyMark::Uncheckable => Self::Uncheckable,
    }
  }
}

impl From<RoleKeyMarkItem> for RoleKeyMark {
  fn from(mark: RoleKeyMarkItem) -> Self {
    match mark {
      RoleKeyMarkItem::Checked => Self::Checked,
      RoleKeyMarkItem::Changed => Self::Changed,
      RoleKeyMarkItem::Uncheckable => Self::Uncheckable,
    }
  }
}

/// Why the list can't be confirmed; each has its own text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum RoleReviewRefusalItem {
  /// `channel-roles-review-unknown-refused`
  UnknownPerson,
  /// `channel-roles-review-uncheckable-refused`
  UncheckableKey,
  /// `channel-roles-review-no-admin`
  NoAdmin,
}

impl From<RoleReviewRefusal> for RoleReviewRefusalItem {
  fn from(refusal: RoleReviewRefusal) -> Self {
    match refusal {
      RoleReviewRefusal::UnknownPerson => Self::UnknownPerson,
      RoleReviewRefusal::UncheckableKey => Self::UncheckableKey,
      RoleReviewRefusal::NoAdmin => Self::NoAdmin,
    }
  }
}

/// One admin or moderator in the list a workspace admin looks at.
#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Record)]
pub struct RoleReviewRowItem {
  pub user_id: String,
  /// `None`: the member list names nobody here ("Unknown person"), so the list can't be confirmed.
  pub maybe_name: Option<String>,
  pub role: ChannelMemberRole,
  /// "New": only the server says this person has this role.
  pub is_new: bool,
  pub key_mark: RoleKeyMarkItem,
}

/// A workspace admin's look at the channel's roles, as the members section shows it.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChannelRolesReviewItem {
  /// Where the role log stood; confirming passes it back, and is refused once the log moved.
  pub log_cursor: u64,
  /// This device can't vouch for the roles (`channel-roles-review-hint`); otherwise the quiet hint.
  pub needs_a_look: bool,
  pub rows: Vec<RoleReviewRowItem>,
  /// This device knows no signed roles here (`channel-roles-review-knows-none`).
  #[uniffi(default = false)]
  pub knows_no_roles: bool,
  /// Whom confirming takes a role from or lowers (`channel-roles-review-dropped`); `None` for
  /// somebody nobody can name ("Unknown person").
  #[uniffi(default = [])]
  pub dropped: Vec<Option<String>>,
  /// Why it can't be confirmed; `None`: it can.
  #[uniffi(default = None)]
  pub maybe_refusal: Option<RoleReviewRefusalItem>,
}

impl ChannelRolesReviewItem {
  fn has_new(&self) -> bool {
    self.rows.iter().any(|row| row.is_new)
  }
}

/// What became of confirming the roles a workspace admin looked at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ReviewedRolesOutcome {
  /// Signed: every member's device takes these roles.
  Confirmed,
  /// The roles changed while they looked, and nothing was signed (`channel-roles-review-changed`):
  /// they look again.
  Changed,
  /// These roles can't be signed in a way the channel's members accept; nothing was signed.
  Refused,
}

/// Whether the list or the log moved since the admin opened `shown`: the panel then says the roles
/// changed, doesn't confirm, and offers to look again. `None`: there is no look any more.
#[uniffi::export]
pub fn channel_roles_review_has_changed(
  shown: ChannelRolesReviewItem,
  maybe_live: Option<ChannelRolesReviewItem>,
) -> bool {
  // A row the app can't hand back as it came is never the same row.
  let Some(shown_rows) = rows_from_items(&shown.rows) else {
    return true;
  };
  match maybe_live {
    Some(live) => rows_from_items(&live.rows).is_none_or(|live_rows| {
      has_review_changed(
        Some(shown.log_cursor),
        &shown_rows,
        Some(live.log_cursor),
        &live_rows,
      )
    }),
    None => has_review_changed(Some(shown.log_cursor), &shown_rows, None, &[]),
  }
}

/// Whether the list a workspace admin looked at has a role marked "New", for its hint.
#[uniffi::export]
pub fn channel_roles_review_has_new(review: ChannelRolesReviewItem) -> bool {
  review.has_new()
}

/// Back from the app's records; `None` if a row's ID doesn't parse or its role has no name here.
fn rows_from_items(items: &[RoleReviewRowItem]) -> Option<Vec<RoleReviewRow>> {
  items
    .iter()
    .map(|item| {
      Some(RoleReviewRow {
        user: parse_id::<UserId>(&item.user_id).ok()?,
        role: item.role.known()?,
        maybe_name: item.maybe_name.clone(),
        is_new: item.is_new,
        key_mark: item.key_mark.into(),
      })
    })
    .collect()
}

#[uniffi::export]
impl TeamCraft {
  /// The channel's members section: members with roles, the alarm line, a workspace admin's look.
  pub async fn channel_members(&self, conversation_id: String) -> Result<ChannelMembersInfo, TeamCraftError> {
    self
      .read(move |reads| reads.channel_members(&conversation_id))
      .await
  }

  /// Whether this device and a member's see different roles in this channel.
  pub async fn channel_role_alarm(&self, conversation_id: String) -> Result<bool, TeamCraftError> {
    self
      .read(move |reads| {
        let conversation: ConversationId = parse_id(&conversation_id)?;
        Ok(reads.backend()?.channel_role_alarms().contains(&conversation))
      })
      .await
  }

  /// A workspace admin's look at the channel's roles; `None` for anybody else.
  pub async fn channel_roles_review(
    &self,
    conversation_id: String,
  ) -> Result<Option<ChannelRolesReviewItem>, TeamCraftError> {
    self
      .read(move |reads| Ok(reads.channel_members(&conversation_id)?.maybe_review))
      .await
  }

  /// Reads the channel's members and signed roles afresh: call it when the members section opens,
  /// as desktop and web do (e2ee.md §4.6.1). The change arrives as a store update.
  pub async fn refresh_channel_members(&self, conversation_id: String) -> Result<(), TeamCraftError> {
    self
      .act_locally(move |reads| {
        reads
          .backend()?
          .refresh_channel_roles(parse_id(&conversation_id)?);
        Ok(())
      })
      .await
  }

  /// Confirms exactly `roles`, the rows of the look the admin opened, with the log at `at_cursor`
  /// (its `log_cursor`). Nothing changes on the server; nothing is signed if the log moved since.
  pub async fn sign_reviewed_channel_roles(
    &self,
    conversation_id: String,
    roles: Vec<RoleReviewRowItem>,
    at_cursor: u64,
  ) -> Result<ReviewedRolesOutcome, TeamCraftError> {
    let conversation: ConversationId = parse_id(&conversation_id)?;
    let mut parsed = Vec::with_capacity(roles.len());
    for row in &roles {
      let user = parse_id::<UserId>(&row.user_id)?;
      // A role this version can't name isn't signed as something else.
      let Some(role) = row.role.known() else {
        return Ok(ReviewedRolesOutcome::Refused);
      };
      parsed.push((user, role));
    }
    let roles = parsed;
    self
      .call(move |engine| {
        Box::pin(async move {
          let outcome = Rc::new(Cell::new(ReviewedRolesOutcome::Refused));
          let reported = Rc::clone(&outcome);
          engine
            .with_client(move |client| {
              Box::pin(async move {
                let signing = client
                  .sign_reviewed_channel_roles(conversation, &roles, at_cursor)
                  .await?;
                reported.set(match signing {
                  RoleSigning::Signed => ReviewedRolesOutcome::Confirmed,
                  RoleSigning::ReviewOutdated => ReviewedRolesOutcome::Changed,
                  RoleSigning::Unsigned => ReviewedRolesOutcome::Refused,
                });
                Ok(())
              })
            })
            .await?;
          Ok(outcome.get())
        })
      })
      .await
  }
}

impl Reads {
  pub(crate) fn channel_members(&self, conversation_id: &str) -> Result<ChannelMembersInfo, TeamCraftError> {
    let backend = self.backend()?;
    let conversation: ConversationId = parse_id(conversation_id)?;
    let ids = backend
      .conversation(conversation)
      .map(|info| info.member_ids)
      .unwrap_or_default();
    let mut users: Vec<User> = ids.into_iter().filter_map(|id| backend.user(id)).collect();
    users.sort_by_key(|user| user.display_name.to_lowercase());
    let roles = backend.channel_roles(conversation);
    let members = users
      .iter()
      .map(|user| {
        let held = roles.iter().find(|member| member.user == user.id);
        ChannelMemberItem {
          user_id: user.id.to_string(),
          name: user.display_name.clone(),
          role: held.map_or(ChannelMemberRole::Member, |member| (&member.role).into()),
          maybe_appointer: held.and_then(|member| member.maybe_appointer).map(|appointer| {
            appointer_item(appointer, |id| backend.user(id).map(|found| found.display_name))
          }),
        }
      })
      .collect();
    let maybe_review = backend.channel_roles_review(conversation).map(|review| {
      let rows = role_review_rows(&roles, &users, &review.snapshot, &review.key_marks);
      ChannelRolesReviewItem {
        log_cursor: review.log_cursor,
        needs_a_look: review.needs_a_look,
        knows_no_roles: review.snapshot.is_empty(),
        dropped: dropped_role_holders(&rows, &users, &review.snapshot),
        maybe_refusal: role_review_refusal(&rows).map(Into::into),
        rows: rows.iter().map(row_item).collect(),
      }
    });
    Ok(ChannelMembersInfo {
      members,
      has_role_alarm: backend.channel_role_alarms().contains(&conversation),
      maybe_review,
    })
  }
}

/// The appointer as the apps word it; the name only where the directory has the signer.
fn appointer_item(
  appointer: RoleAppointer,
  name_of: impl FnOnce(UserId) -> Option<String>,
) -> ChannelMemberAppointer {
  ChannelMemberAppointer {
    maybe_name: name_of(appointer.user),
    authority: appointer.authority.into(),
  }
}

fn row_item(row: &RoleReviewRow) -> RoleReviewRowItem {
  RoleReviewRowItem {
    user_id: row.user.to_string(),
    maybe_name: row.maybe_name.clone(),
    role: (&row.role).into(),
    is_new: row.is_new,
    key_mark: row.key_mark.into(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn item(user: &str, role: ChannelMemberRole) -> RoleReviewRowItem {
    RoleReviewRowItem {
      user_id: user.into(),
      maybe_name: Some("Alice".into()),
      role,
      is_new: false,
      key_mark: RoleKeyMarkItem::Checked,
    }
  }

  fn review(cursor: u64, rows: Vec<RoleReviewRowItem>) -> ChannelRolesReviewItem {
    ChannelRolesReviewItem {
      log_cursor: cursor,
      needs_a_look: true,
      rows,
      knows_no_roles: false,
      dropped: Vec::new(),
      maybe_refusal: None,
    }
  }

  #[test]
  fn the_look_changed_when_the_log_or_the_list_moved_or_it_went() {
    let shown = review(3, vec![item("1", ChannelMemberRole::Admin)]);
    assert!(!channel_roles_review_has_changed(
      shown.clone(),
      Some(shown.clone())
    ));
    assert!(channel_roles_review_has_changed(
      shown.clone(),
      Some(review(4, shown.rows.clone()))
    ));
    assert!(channel_roles_review_has_changed(
      shown.clone(),
      Some(review(3, vec![item("1", ChannelMemberRole::Moderator)]))
    ));
    assert!(channel_roles_review_has_changed(shown, None));
  }

  #[test]
  fn a_row_that_is_no_id_is_never_the_same_row() {
    let shown = review(3, vec![item("not an id", ChannelMemberRole::Admin)]);
    let live = review(3, vec![item("1", ChannelMemberRole::Admin)]);
    assert!(channel_roles_review_has_changed(shown, Some(live)));
    let other = review(3, vec![item("1", ChannelMemberRole::Other)]);
    assert!(channel_roles_review_has_changed(other.clone(), Some(other)));
  }

  #[test]
  fn roles_cross_both_ways_and_an_unknown_one_is_never_signed() {
    for role in [
      ChannelMemberRole::Admin,
      ChannelMemberRole::Moderator,
      ChannelMemberRole::Member,
    ] {
      assert_eq!(
        role.known().map(|known| ChannelMemberRole::from(&known)),
        Some(role)
      );
    }
    let other = ChannelMemberRole::from(&ChannelRole::Unknown("owner".into()));
    assert_eq!(other, ChannelMemberRole::Other);
    assert_eq!(other.known(), None, "never signed as another role");
  }

  #[test]
  fn key_marks_and_refusals_cross_as_they_are() {
    for mark in [
      RoleKeyMark::Checked,
      RoleKeyMark::Changed,
      RoleKeyMark::Uncheckable,
    ] {
      assert_eq!(RoleKeyMark::from(RoleKeyMarkItem::from(mark)), mark);
    }
    assert_eq!(
      RoleReviewRefusalItem::from(RoleReviewRefusal::NoAdmin),
      RoleReviewRefusalItem::NoAdmin
    );
  }

  #[test]
  fn the_appointer_is_named_from_the_directory_or_only_by_authority() {
    let appointer = RoleAppointer {
      user: parse_id::<UserId>("7").unwrap(),
      authority: AppointerAuthority::WorkspaceAdmin,
    };
    let named = appointer_item(appointer, |_| Some("Alice".to_owned()));
    assert_eq!(named.maybe_name.as_deref(), Some("Alice"));
    assert_eq!(named.authority, AppointerAuthorityItem::WorkspaceAdmin);
    let unnamed = appointer_item(appointer, |_| None);
    assert_eq!(unnamed.maybe_name, None);
    for (authority, expected) in [
      (
        AppointerAuthority::WorkspaceOwner,
        AppointerAuthorityItem::WorkspaceOwner,
      ),
      (
        AppointerAuthority::ChannelAdmin,
        AppointerAuthorityItem::ChannelAdmin,
      ),
    ] {
      assert_eq!(AppointerAuthorityItem::from(authority), expected);
    }
  }

  #[test]
  fn a_marked_new_row_is_said() {
    let mut row = item("1", ChannelMemberRole::Admin);
    assert!(!channel_roles_review_has_new(review(1, vec![row.clone()])));
    row.is_new = true;
    assert!(channel_roles_review_has_new(review(1, vec![row])));
  }
}
