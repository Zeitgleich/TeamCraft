use std::cmp::Ordering;

use crate::wire_enum::wire_enum;

wire_enum! {
  /// Instance role. Ordering reflects privilege: `Guest < Member < Admin < Owner`.
  /// Unknown roles rank below every known role, so they never gain privileges.
  pub enum Role {
    Guest => "guest",
    Member => "member",
    Admin => "admin",
    Owner => "owner",
  }
}

impl Role {
  /// Admins and owners include everything a member can do.
  pub fn includes(&self, required: &Role) -> bool {
    self >= required
  }

  fn rank(&self) -> u8 {
    match self {
      Self::Unknown(_) => 0,
      Self::Guest => 1,
      Self::Member => 2,
      Self::Admin => 3,
      Self::Owner => 4,
    }
  }
}

impl PartialOrd for Role {
  fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
    Some(self.cmp(other))
  }
}

impl Ord for Role {
  fn cmp(&self, other: &Self) -> Ordering {
    // The name tiebreak keeps `Ord` consistent with `Eq` for two different unknown roles.
    self
      .rank()
      .cmp(&other.rank())
      .then_with(|| self.as_str().cmp(other.as_str()))
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn orders_by_privilege() {
    assert!(Role::Owner > Role::Admin);
    assert!(Role::Admin > Role::Member);
    assert!(Role::Member > Role::Guest);
    assert!(Role::Guest > Role::Unknown("auditor".into()));
  }

  #[test]
  fn highest_role_wins_with_max() {
    let roles = [Role::Guest, Role::Admin, Role::Member];
    assert_eq!(roles.into_iter().max(), Some(Role::Admin));
  }

  #[test]
  fn includes_follows_ordering() {
    assert!(Role::Admin.includes(&Role::Member));
    assert!(!Role::Guest.includes(&Role::Member));
    assert!(!Role::Unknown("x".into()).includes(&Role::Guest));
  }

  #[test]
  fn wire_format_is_stable() {
    assert_eq!(serde_json::to_string(&Role::Owner).unwrap(), "\"owner\"");
    assert_eq!(serde_json::to_string(&Role::Guest).unwrap(), "\"guest\"");
    assert_eq!(serde_json::from_str::<Role>("\"admin\"").unwrap(), Role::Admin);
  }

  #[test]
  fn unknown_variant_round_trips() {
    let role: Role = serde_json::from_str("\"auditor\"").unwrap();
    assert_eq!(role, Role::Unknown("auditor".into()));
    assert_eq!(serde_json::to_string(&role).unwrap(), "\"auditor\"");
  }
}
