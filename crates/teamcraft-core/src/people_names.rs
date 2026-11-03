//! The name a person shows in the apps. A deleted account (admin.md §3.2.1, issue #630) keeps the
//! server's English placeholder as its display name; every name the core puts together (member
//! lists, DM titles, notifications, search) shows the app's own "Deleted user" in the user's
//! language instead (issue #1066). The shells set that text once at start and when the language
//! changes; until then the server's placeholder shows.

use std::sync::{PoisonError, RwLock};

use teamcraft_proto::UserSummary;

/// The app's "Deleted user" in the user's language, process-wide like the language itself (the
/// push extensions name senders without a running client).
static DELETED_USER_NAME: RwLock<Option<String>> = RwLock::new(None);

/// Sets how a deleted account is named: the shell's translation of `user-deleted-name`. An empty
/// text goes back to the server's placeholder.
pub fn set_deleted_user_name(name: String) {
  let maybe_name = (!name.trim().is_empty()).then_some(name);
  // A panic while holding the lock leaves a whole string behind; keep using it.
  *DELETED_USER_NAME.write().unwrap_or_else(PoisonError::into_inner) = maybe_name;
}

/// The name `user` shows: their display name, or the app's own "Deleted user" for a deleted
/// account.
pub fn shown_name(user: &UserSummary) -> String {
  if user.is_deleted
    && let Some(name) = DELETED_USER_NAME
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .as_ref()
  {
    return name.clone();
  }
  user.display_name.clone()
}

/// What tests set as the name: all of them set this one, so they don't race over the process-wide
/// name.
#[cfg(test)]
pub(crate) const TEST_DELETED_NAME: &str = "Account removed";

#[cfg(test)]
mod tests {
  use teamcraft_proto::UserKind;

  use super::*;

  #[test]
  fn a_deleted_account_shows_the_name_the_shell_set() {
    let mut user = UserSummary {
      id: "7".parse().unwrap(),
      maybe_username: None,
      display_name: "Deleted user".into(),
      kind: UserKind::Member,
      maybe_email: None,
      maybe_organization: None,
      maybe_has_devices: None,
      is_deactivated: true,
      is_deleted: true,
    };
    set_deleted_user_name(TEST_DELETED_NAME.into());
    assert_eq!(shown_name(&user), TEST_DELETED_NAME);
    user.is_deleted = false;
    user.display_name = "Alex Example".into();
    assert_eq!(shown_name(&user), "Alex Example", "only deleted accounts");
  }
}
