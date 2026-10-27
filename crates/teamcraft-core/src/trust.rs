//! Trust decisions the crypto layer asks for: whose identity keys are pinned, and who may change a
//! group's membership.

use std::collections::{BTreeMap, HashSet};

use teamcraft_crypto::device_list::VerifiedDeviceList;
use teamcraft_crypto::identity::{IdentityPublicKey, VerifiedDevice};
use teamcraft_crypto::ids as crypto;
use teamcraft_crypto::membership::{IdentityDirectory, MembershipAction, MembershipAuthority};
use teamcraft_proto::{AdminSelfJoin, UserId};

use crate::convert;
use crate::records::PinnedIdentity;

/// Identity keys this device trusts: pinned on first use (e2ee.md §3), overridden by the keys the
/// verified admin roster vouches for. A key the server changed later is *not* used until the user
/// accepts the change.
pub(crate) struct PinnedDirectory {
  keys: BTreeMap<crypto::UserId, IdentityPublicKey>,
  /// The newest verified device list per user. A user without one never published a list (older
  /// clients); their certified devices are accepted as before.
  device_lists: BTreeMap<crypto::UserId, VerifiedDeviceList>,
  /// Users the server lists as meeting guests (e2ee.md §4.6, "Meeting guests").
  guests: HashSet<crypto::UserId>,
}

/// Who may change a group's membership (e2ee.md §4.6, admin.md §2.3):
///
/// - current members of the MLS group (the signed per-channel roles aren't in the roster yet, so
///   every member counts as allowed to manage members),
/// - workspace owners and admins from the verified roster, for any change except adding
///   themselves, which follows the policy's `admin_self_join` (admin.md §2.4).
pub(crate) struct WorkspaceAuthority {
  members: HashSet<crypto::UserId>,
  admins: HashSet<crypto::UserId>,
  /// Meeting guests are members of the group but manage nobody (e2ee.md §4.6, "Meeting guests").
  guests: HashSet<crypto::UserId>,
  /// In a meeting chat: the users the signed roster vouches for. Anybody else in the group is
  /// treated as a guest, whatever the server says their kind is.
  maybe_vouched_in_meeting: Option<HashSet<crypto::UserId>>,
  self_join: AdminSelfJoin,
  is_public_channel: bool,
}

impl PinnedDirectory {
  pub(crate) fn new(pinned: &BTreeMap<UserId, PinnedIdentity>, roster: &BTreeMap<UserId, [u8; 32]>) -> Self {
    let mut keys: BTreeMap<crypto::UserId, IdentityPublicKey> = pinned
      .iter()
      .map(|(user, identity)| {
        (
          convert::user(*user),
          IdentityPublicKey::from_bytes(identity.public_key),
        )
      })
      .collect();
    for (user, key) in roster {
      keys.insert(convert::user(*user), IdentityPublicKey::from_bytes(*key));
    }
    Self {
      keys,
      device_lists: BTreeMap::new(),
      guests: HashSet::new(),
    }
  }

  pub(crate) fn with_guests(mut self, guests: impl IntoIterator<Item = UserId>) -> Self {
    self.guests = guests.into_iter().map(convert::user).collect();
    self
  }

  pub(crate) fn with_device_lists(mut self, lists: impl IntoIterator<Item = VerifiedDeviceList>) -> Self {
    for list in lists {
      self.device_lists.insert(list.user_id(), list);
    }
    self
  }

  pub(crate) fn device_list(&self, user: crypto::UserId) -> Option<&VerifiedDeviceList> {
    self.device_lists.get(&user)
  }
}

impl IdentityDirectory for PinnedDirectory {
  fn pinned_identity(&self, user_id: crypto::UserId) -> Option<IdentityPublicKey> {
    self.keys.get(&user_id).copied()
  }

  fn is_active_device(&self, device: &VerifiedDevice) -> bool {
    self
      .device_lists
      .get(&device.user_id())
      .is_none_or(|list| list.is_active(device))
  }

  fn is_listed_device(&self, device: &VerifiedDevice) -> bool {
    self
      .device_lists
      .get(&device.user_id())
      .is_some_and(|list| list.is_active(device))
  }

  fn is_guest(&self, user_id: crypto::UserId) -> bool {
    self.guests.contains(&user_id)
  }
}

impl WorkspaceAuthority {
  pub(crate) fn new(
    members: impl IntoIterator<Item = crypto::UserId>,
    admins: impl IntoIterator<Item = UserId>,
    self_join: AdminSelfJoin,
  ) -> Self {
    Self {
      members: members.into_iter().collect(),
      admins: admins.into_iter().map(convert::user).collect(),
      guests: HashSet::new(),
      maybe_vouched_in_meeting: None,
      self_join,
      is_public_channel: false,
    }
  }

  /// The group's meeting guests: they are not allowed to change anybody's membership but their own.
  pub(crate) fn with_guests(mut self, guests: impl IntoIterator<Item = UserId>) -> Self {
    self.guests = guests.into_iter().map(convert::user).collect();
    self
  }

  /// For a meeting chat: only users in `vouched` (the signed roster's members) may manage anybody
  /// else's membership; everybody else in the group is a guest. An empty roster vouches for nobody
  /// and changes nothing (an instance without a roster keeps the old rule).
  pub(crate) fn for_meeting_chat(mut self, vouched: impl IntoIterator<Item = UserId>) -> Self {
    let vouched: HashSet<crypto::UserId> = vouched.into_iter().map(convert::user).collect();
    if !vouched.is_empty() {
      self.maybe_vouched_in_meeting = Some(vouched);
    }
    self
  }

  /// A public, unarchived channel: members joining by themselves is fine.
  pub(crate) fn for_public_channel(mut self, is_public_channel: bool) -> Self {
    self.is_public_channel = is_public_channel;
    self
  }
}

impl MembershipAuthority for WorkspaceAuthority {
  fn is_authorized(
    &self,
    actor: crypto::UserId,
    _conversation_id: crypto::ConversationId,
    action: &MembershipAction,
  ) -> bool {
    let is_unvouched = self
      .maybe_vouched_in_meeting
      .as_ref()
      .is_some_and(|vouched| !vouched.contains(&actor));
    if self.guests.contains(&actor) || is_unvouched {
      return match *action {
        MembershipAction::RemoveUser { user_id } | MembershipAction::RemoveDevice { user_id, .. } => {
          user_id == actor
        },
        MembershipAction::AddUser { .. } => false,
      };
    }
    if self.members.contains(&actor) {
      return true;
    }
    // Anyone the server let into a public channel may add themselves (and only themselves).
    if self.is_public_channel
      && matches!(action, MembershipAction::AddUser { user_id, .. } if *user_id == actor)
    {
      return true;
    }
    if !self.admins.contains(&actor) {
      return false;
    }
    match action {
      MembershipAction::AddUser { user_id, .. } if *user_id == actor => match self.self_join {
        AdminSelfJoin::Visible => true,
        // Under `owner_approval` the admin asks instead and a channel owner adds them with an
        // ordinary member's commit (admin.md §2.4.1); their own self-join is never committed.
        AdminSelfJoin::OwnerApproval | AdminSelfJoin::Disabled | AdminSelfJoin::Unknown(_) => false,
      },
      _ => true,
    }
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_crypto::membership::HistoryShare;
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  #[test]
  fn members_and_admins_may_change_membership_but_self_join_follows_policy() {
    let member = crypto::UserId::new(1);
    let admin = UserId::from_snowflake(Snowflake::new(2).unwrap());
    let stranger = crypto::UserId::new(3);
    let conversation = crypto::ConversationId::new(9);
    let add = |user| MembershipAction::AddUser {
      user_id: user,
      history: HistoryShare::Full,
    };

    for (self_join, may_self_join) in [
      (AdminSelfJoin::Visible, true),
      (AdminSelfJoin::OwnerApproval, false),
      (AdminSelfJoin::Disabled, false),
    ] {
      let authority = WorkspaceAuthority::new([member], [admin], self_join);
      assert!(authority.is_authorized(member, conversation, &add(stranger)));
      assert!(authority.is_authorized(convert::user(admin), conversation, &add(stranger)));
      assert_eq!(
        authority.is_authorized(convert::user(admin), conversation, &add(convert::user(admin))),
        may_self_join
      );
      assert!(!authority.is_authorized(stranger, conversation, &add(stranger)));
    }
  }

  #[test]
  fn a_guest_manages_nobody_but_their_own_leaves() {
    let (member, guest, other) = (
      crypto::UserId::new(1),
      crypto::UserId::new(5),
      crypto::UserId::new(6),
    );
    let conversation = crypto::ConversationId::new(9);
    let authority = WorkspaceAuthority::new(
      [member, guest, other],
      Vec::<UserId>::new(),
      AdminSelfJoin::Visible,
    )
    .with_guests([convert::user_from_crypto(guest).unwrap()]);

    let add = |user| MembershipAction::AddUser {
      user_id: user,
      history: HistoryShare::FromJoin,
    };
    assert!(!authority.is_authorized(guest, conversation, &add(other)));
    assert!(!authority.is_authorized(guest, conversation, &add(guest)));
    assert!(!authority.is_authorized(
      guest,
      conversation,
      &MembershipAction::RemoveUser { user_id: other }
    ));
    assert!(authority.is_authorized(
      guest,
      conversation,
      &MembershipAction::RemoveUser { user_id: guest }
    ));
    assert!(
      authority.is_authorized(member, conversation, &add(other)),
      "members are unaffected"
    );
  }

  #[test]
  fn anyone_may_join_a_public_channel_but_only_themselves() {
    let (member, stranger, other) = (
      crypto::UserId::new(1),
      crypto::UserId::new(3),
      crypto::UserId::new(4),
    );
    let conversation = crypto::ConversationId::new(9);
    let add = |user| MembershipAction::AddUser {
      user_id: user,
      history: HistoryShare::FromJoin,
    };
    let public = WorkspaceAuthority::new([member], Vec::<UserId>::new(), AdminSelfJoin::Visible)
      .for_public_channel(true);
    assert!(public.is_authorized(stranger, conversation, &add(stranger)));
    assert!(!public.is_authorized(stranger, conversation, &add(other)));
    let private = WorkspaceAuthority::new([member], Vec::<UserId>::new(), AdminSelfJoin::Visible);
    assert!(!private.is_authorized(stranger, conversation, &add(stranger)));
  }
}
