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
/// - current members of the MLS group: channel roles are signed (e2ee.md §4.6.1,
///   [`crate::channel_roles`]) but don't decide commits yet, so every member counts as allowed to
///   manage members, also to add an admin, until channels from before signed roles are signed
///   (security review A11),
/// - workspace owners and admins from the verified roster who aren't members, for any change
///   except bringing an owner or admin into a conversation that isn't an open public channel:
///   themselves or another one alike, as the policy's `admin_self_join` says (admin.md §2.4,
///   issue #816).
pub(crate) struct WorkspaceAuthority {
  members: HashSet<crypto::UserId>,
  /// Owners and admins of the fresh verified roster: who acts with an admin's authority, and who
  /// counts as an admin being let in.
  admins: HashSet<crypto::UserId>,
  /// Meeting guests are members of the group but manage nobody (e2ee.md §4.6, "Meeting guests").
  guests: HashSet<crypto::UserId>,
  /// In a meeting chat: the users the signed roster vouches for. Anybody else in the group is
  /// treated as a guest, whatever the server says their kind is.
  maybe_vouched_in_meeting: Option<HashSet<crypto::UserId>>,
  self_join: AdminSelfJoin,
  is_public_channel: bool,
  /// The admins of the channel's signed role log (e2ee.md §4.6.1), once anything is signed for it;
  /// `None` for a channel from before signed roles, which keeps the rule "every member".
  maybe_signed_channel_admins: Option<HashSet<crypto::UserId>>,
  /// Decides like clients before v1.0.9, which held only an admin's own self-join to the policy.
  /// Only for tests of how this version takes such members' commits.
  is_before_v1_0_9: bool,
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
      maybe_signed_channel_admins: None,
      is_before_v1_0_9: false,
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

  /// The channel's signed admins. From then on, under `owner_approval`, a member lets an admin in
  /// only if it is one of them (e2ee.md §4.6.1, issue #380). Pass `None` while nothing is signed
  /// for the channel: it keeps the rule of before, so a channel nobody signed yet doesn't refuse
  /// every approval.
  pub(crate) fn with_signed_channel_admins(
    mut self,
    maybe_admins: Option<impl IntoIterator<Item = UserId>>,
  ) -> Self {
    self.maybe_signed_channel_admins =
      maybe_admins.map(|admins| admins.into_iter().map(convert::user).collect());
    self
  }

  /// Tests of mixed versions only: decide like clients before v1.0.9 (issue #816).
  pub(crate) fn like_before_v1_0_9(mut self, is_before_v1_0_9: bool) -> Self {
    self.is_before_v1_0_9 = is_before_v1_0_9;
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
      return self.member_may(actor, action);
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
      MembershipAction::AddUser { user_id, .. } if self.lets_an_admin_in(actor, *user_id) => {
        match self.self_join {
          AdminSelfJoin::Visible => true,
          // Under `owner_approval` the admin asks instead and a channel owner adds them with an
          // ordinary member's commit (admin.md §2.4.1): neither their own self-join nor another
          // admin's signed addition of them is ever committed.
          AdminSelfJoin::OwnerApproval | AdminSelfJoin::Disabled | AdminSelfJoin::Unknown(_) => false,
        }
      },
      // Anybody else, or a removal: an admin's change for the members to commit (admin.md §2.2).
      _ => true,
    }
  }
}

impl WorkspaceAuthority {
  /// A member's change: anything, except bringing a workspace admin into a private channel whose
  /// roles are signed while the policy asks for an owner's approval: that takes a signed channel
  /// admin (admin.md §2.4.1). The server's member list never decides it.
  fn member_may(&self, actor: crypto::UserId, action: &MembershipAction) -> bool {
    let MembershipAction::AddUser { user_id, .. } = action else {
      return true;
    };
    let Some(signed_admins) = &self.maybe_signed_channel_admins else {
      return true;
    };
    let is_admin_added = self.admins.contains(user_id);
    let needs_owner = self.self_join == AdminSelfJoin::OwnerApproval && !self.is_public_channel;
    !(is_admin_added && needs_owner) || signed_admins.contains(&actor)
  }

  /// Whether `actor`, an admin who isn't a member, adding `target` lets an admin in without the
  /// members: their own self-join, or another admin's addition of them, so two admins can't add
  /// each other around the policy (issue #816). `admin_self_join` decides those (admin.md §2.4).
  /// An open public channel is anybody's to join.
  fn lets_an_admin_in(&self, actor: crypto::UserId, target: crypto::UserId) -> bool {
    if self.is_public_channel {
      return false;
    }
    target == actor || (!self.is_before_v1_0_9 && self.admins.contains(&target))
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_crypto::membership::HistoryShare;
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  /// Issue #380: under `owner_approval` in a channel with signed roles, only a signed admin lets
  /// an admin in; everything else a member does is unchanged. Nothing signed: every member, as before.
  #[test]
  fn signed_channel_roles_decide_who_lets_an_admin_in() {
    let (owner, member, guest_admin_target) = (
      crypto::UserId::new(1),
      crypto::UserId::new(2),
      UserId::from_snowflake(Snowflake::new(3).unwrap()),
    );
    let conversation = crypto::ConversationId::new(9);
    let add = |user| MembershipAction::AddUser {
      user_id: user,
      history: HistoryShare::FromJoin,
    };
    let admin_target = convert::user(guest_admin_target);
    let owner_user = UserId::from_snowflake(Snowflake::new(1).unwrap());
    let authority = |policy, signed: Option<Vec<UserId>>| {
      WorkspaceAuthority::new([owner, member], [guest_admin_target], policy)
        .with_signed_channel_admins(signed)
    };

    // Signed: the owner may, a member (the server's "owner" included) may not.
    let signed = authority(AdminSelfJoin::OwnerApproval, Some(vec![owner_user]));
    assert!(signed.is_authorized(owner, conversation, &add(admin_target)));
    assert!(!signed.is_authorized(member, conversation, &add(admin_target)));
    // Everything else a member does is untouched: ordinary adds, removals.
    assert!(signed.is_authorized(member, conversation, &add(crypto::UserId::new(7))));
    assert!(signed.is_authorized(
      member,
      conversation,
      &MembershipAction::RemoveUser { user_id: owner }
    ));
    // Nothing signed (a channel from before): every member, so no approval is refused.
    let legacy = authority(AdminSelfJoin::OwnerApproval, None);
    assert!(legacy.is_authorized(member, conversation, &add(admin_target)));
    // An empty signed snapshot still counts as signed: nobody is admin.
    let none = authority(AdminSelfJoin::OwnerApproval, Some(Vec::new()));
    assert!(!none.is_authorized(owner, conversation, &add(admin_target)));
    // Other policies and public channels don't ask for an owner.
    let visible = authority(AdminSelfJoin::Visible, Some(vec![owner_user]));
    assert!(visible.is_authorized(member, conversation, &add(admin_target)));
    let public = authority(AdminSelfJoin::OwnerApproval, Some(vec![owner_user])).for_public_channel(true);
    assert!(public.is_authorized(member, conversation, &add(admin_target)));
  }

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

  /// Issue #816: an admin outside the group bringing another admin in follows the policy like a
  /// self-join; a member adding an admin, admin or not, is an ordinary addition.
  #[test]
  fn an_admin_outside_lets_another_admin_in_only_as_the_policy_says() {
    let (member, user, conversation) = (
      crypto::UserId::new(1),
      crypto::UserId::new(3),
      crypto::ConversationId::new(9),
    );
    let (outsider, other, member_admin) = (admin(2), admin(4), admin(5));
    let (outsider_id, other_id) = (convert::user(outsider), convert::user(other));

    for (self_join, may_let_in) in [
      (AdminSelfJoin::Visible, true),
      (AdminSelfJoin::OwnerApproval, false),
      (AdminSelfJoin::Disabled, false),
      (AdminSelfJoin::Unknown("later".into()), false),
    ] {
      let authority = WorkspaceAuthority::new(
        [member, convert::user(member_admin)],
        [outsider, other, member_admin],
        self_join.clone(),
      );
      assert_eq!(
        authority.is_authorized(outsider_id, conversation, &add(other_id)),
        may_let_in,
        "{self_join:?}: another admin"
      );
      assert_eq!(
        authority.is_authorized(outsider_id, conversation, &add(outsider_id)),
        may_let_in,
        "{self_join:?}: the self-join"
      );
      for adder in [member, convert::user(member_admin)] {
        assert!(
          authority.is_authorized(adder, conversation, &add(other_id)),
          "{self_join:?}: a member adds an admin"
        );
      }
      // Admin changes about anybody else stay as they were (admin.md §2.2).
      assert!(authority.is_authorized(outsider_id, conversation, &add(user)));
      assert!(authority.is_authorized(
        outsider_id,
        conversation,
        &MembershipAction::RemoveUser { user_id: other_id }
      ));
    }
  }

  #[test]
  fn admins_join_an_open_channel_whatever_the_policy() {
    let (member, conversation) = (crypto::UserId::new(1), crypto::ConversationId::new(9));
    let (outsider, other) = (admin(2), admin(4));
    let public =
      WorkspaceAuthority::new([member], [outsider, other], AdminSelfJoin::Disabled).for_public_channel(true);
    let (outsider_id, other_id) = (convert::user(outsider), convert::user(other));
    assert!(public.is_authorized(outsider_id, conversation, &add(other_id)));
    assert!(public.is_authorized(outsider_id, conversation, &add(outsider_id)));
  }

  /// The seam for the tests of mixed versions: what v1.0.8 let through, and only that.
  #[test]
  fn before_v1_0_9_only_the_self_join_followed_the_policy() {
    let (member, conversation) = (crypto::UserId::new(1), crypto::ConversationId::new(9));
    let (outsider, other) = (admin(2), admin(4));
    let before =
      WorkspaceAuthority::new([member], [outsider, other], AdminSelfJoin::Disabled).like_before_v1_0_9(true);
    let (outsider_id, other_id) = (convert::user(outsider), convert::user(other));
    assert!(before.is_authorized(outsider_id, conversation, &add(other_id)));
    assert!(!before.is_authorized(outsider_id, conversation, &add(outsider_id)));
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

  /// A roster admin with the ID `id`.
  fn admin(id: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(id).unwrap())
  }

  fn add(user: crypto::UserId) -> MembershipAction {
    MembershipAction::AddUser {
      user_id: user,
      history: HistoryShare::FromJoin,
    }
  }
}
