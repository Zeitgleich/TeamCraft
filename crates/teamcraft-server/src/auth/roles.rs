use teamcraft_proto::Role;

use crate::config::RoleNames;

/// Maps identity provider client roles to TeamCraft roles (ARCHITECTURE.md §5.1.1).
#[derive(Debug, Clone)]
pub struct RoleMapping {
  names: RoleNames,
}

impl RoleMapping {
  pub fn new(names: RoleNames) -> Self {
    Self { names }
  }

  /// The highest TeamCraft role among the token's roles (owner > admin > member > guest), or
  /// `None` if the user has none, in which case the token must be rejected.
  pub fn resolve<'a>(&self, token_roles: impl IntoIterator<Item = &'a str>) -> Option<Role> {
    token_roles
      .into_iter()
      .filter_map(|name| self.role_for(name))
      .max()
  }

  fn role_for(&self, name: &str) -> Option<Role> {
    let names = &self.names;
    // Checked from the highest role down, so a name configured twice yields the higher role.
    [
      (&names.owner, Role::Owner),
      (&names.admin, Role::Admin),
      (&names.member, Role::Member),
      (&names.guest, Role::Guest),
    ]
    .into_iter()
    .find(|(configured, _)| configured.as_str() == name)
    .map(|(_, role)| role)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn highest_role_wins() {
    let mapping = RoleMapping::new(RoleNames::default());
    assert_eq!(mapping.resolve(["guest", "member"]), Some(Role::Member));
    assert_eq!(mapping.resolve(["member", "admin", "guest"]), Some(Role::Admin));
    assert_eq!(mapping.resolve(["owner", "admin"]), Some(Role::Owner));
  }

  #[test]
  fn no_matching_role_means_none() {
    let mapping = RoleMapping::new(RoleNames::default());
    assert_eq!(mapping.resolve([]), None);
    assert_eq!(
      mapping.resolve(["offline_access", "uma_authorization", "Admin"]),
      None
    );
  }

  #[test]
  fn custom_role_names_are_used() {
    let names = RoleNames {
      guest: "external".into(),
      ..RoleNames::default()
    };
    let mapping = RoleMapping::new(names);
    assert_eq!(mapping.resolve(["external"]), Some(Role::Guest));
    // The default name no longer maps once it was renamed.
    assert_eq!(mapping.resolve(["guest"]), None);
  }

  #[test]
  fn role_names_are_case_sensitive_and_exact() {
    let mapping = RoleMapping::new(RoleNames::default());
    assert_eq!(mapping.resolve(["Owner", "admin ", "member-extra"]), None);
  }
}
