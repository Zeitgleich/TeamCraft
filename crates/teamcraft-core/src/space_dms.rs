//! Which DMs and group DMs a space shows besides Chats (#701).
//!
//! A DM or group DM belongs to a space when every other participant is a member of it; guests and
//! other external people follow the same rule. The conversation stays the one Chats lists: the space
//! only shows it too, so its unread messages are counted once wherever a total is made.
//!
//! The rule runs on the device over what it already holds: the space's member list as the last sync
//! stored it (`Table::SpaceRosters`), so it works offline from the cached state. Where the user may
//! not see the members (a guest) or the list was never read, the members of the space's channels
//! the user is in stand in: the people the user can see in the space. Membership is a set, kept per
//! space while the stored list doesn't change, so a big space costs one lookup per DM participant,
//! not a pass over its members on every read.

#[cfg(feature = "ui")]
use std::collections::HashMap;
use std::collections::HashSet;
#[cfg(feature = "ui")]
use std::collections::hash_map::DefaultHasher;
#[cfg(feature = "ui")]
use std::hash::{Hash, Hasher};
#[cfg(feature = "ui")]
use std::rc::Rc;

use teamcraft_proto::UserId;
#[cfg(feature = "ui")]
use teamcraft_proto::{ConversationId, ConversationKind, ConversationResponse, SpaceId};

#[cfg(feature = "ui")]
use crate::client::workspace_admin::StoredRoster;
#[cfg(feature = "ui")]
use crate::error::CoreError;
#[cfg(feature = "ui")]
use crate::store::{LocalStore, Table};
#[cfg(feature = "ui")]
use crate::ui_backend::CoreChatBackend;

/// Whether a DM or group DM with `participants` belongs to a space with `members`: it has people
/// besides `me` (notes to self belong nowhere), and all of them are members.
pub fn is_with_members(
  me: UserId,
  participants: impl IntoIterator<Item = UserId>,
  members: &HashSet<UserId>,
) -> bool {
  let mut others = participants.into_iter().filter(|user| *user != me).peekable();
  others.peek().is_some() && others.all(|user| members.contains(&user))
}

/// A space's members as a set, with the fingerprint of the stored list it was read from.
#[cfg(feature = "ui")]
#[derive(Debug)]
pub(crate) struct KeptMembers {
  fingerprint: u64,
  members: Rc<HashSet<UserId>>,
}

/// A DM or group DM with the people in it besides the user.
#[cfg(feature = "ui")]
struct DirectOthers {
  conversation: ConversationId,
  others: Vec<UserId>,
}

#[cfg(feature = "ui")]
impl<S: LocalStore> CoreChatBackend<S> {
  /// For each of `spaces`, the DMs and group DMs among `conversations` that belong to it, in the
  /// order of `conversations`. Without any DM nothing is read.
  pub(crate) fn space_direct_conversations_of(
    &self,
    spaces: &[SpaceId],
    conversations: &[ConversationResponse],
  ) -> HashMap<SpaceId, Vec<ConversationId>> {
    let me = self.me_id();
    let direct = direct_others(me, conversations);
    self
      .space_members_kept
      .borrow_mut()
      .retain(|space, _| spaces.contains(space));
    if direct.is_empty() {
      return HashMap::new();
    }
    spaces
      .iter()
      .map(|space| {
        let members = self.space_member_set(*space, conversations);
        let listed = direct
          .iter()
          .filter(|dm| is_with_members(me, dm.others.iter().copied(), &members))
          .map(|dm| dm.conversation)
          .collect();
        (*space, listed)
      })
      .collect()
  }

  /// The members of `space`: the stored member list, else the members of its channels the user is
  /// in (a guest, or before the list was first read).
  fn space_member_set(&self, space: SpaceId, conversations: &[ConversationResponse]) -> Rc<HashSet<UserId>> {
    let key = space.to_string();
    let maybe_bytes: Option<Vec<u8>> = self.read(
      self
        .store_ref()
        .get(Table::SpaceRosters, &key)
        .map_err(CoreError::from),
    );
    let Some(bytes) = maybe_bytes else {
      return Rc::new(self.channel_members(space, conversations));
    };
    let fingerprint = fingerprint(&bytes);
    if let Some(kept) = self.space_members_kept.borrow().get(&space)
      && kept.fingerprint == fingerprint
    {
      return Rc::clone(&kept.members);
    }
    let members = match serde_json::from_slice::<StoredRoster>(&bytes) {
      Ok(StoredRoster::Visible(roster)) => roster.members.into_iter().map(|member| member.user.id).collect(),
      Ok(StoredRoster::Hidden) => self.channel_members(space, conversations),
      Err(error) => {
        tracing::debug!(%error, space_id = %space, "stored space members unreadable");
        self.channel_members(space, conversations)
      },
    };
    let members = Rc::new(members);
    self.space_members_kept.borrow_mut().insert(
      space,
      KeptMembers {
        fingerprint,
        members: Rc::clone(&members),
      },
    );
    members
  }

  /// Everybody in the channels of `space` the user is in.
  fn channel_members(&self, space: SpaceId, conversations: &[ConversationResponse]) -> HashSet<UserId> {
    conversations
      .iter()
      .filter(|conversation| conversation.maybe_space_id == Some(space))
      .flat_map(|conversation| self.members(conversation.id))
      .collect()
  }
}

/// The DMs and group DMs with the people in them besides `me`; notes to self have nobody else and
/// are left out.
#[cfg(feature = "ui")]
fn direct_others(me: UserId, conversations: &[ConversationResponse]) -> Vec<DirectOthers> {
  conversations
    .iter()
    .filter(|conversation| matches!(conversation.kind, ConversationKind::Dm | ConversationKind::Group))
    .filter_map(|conversation| {
      let others: Vec<UserId> = conversation
        .participants
        .iter()
        .map(|participant| participant.id)
        .filter(|user| *user != me)
        .collect();
      (!others.is_empty()).then_some(DirectOthers {
        conversation: conversation.id,
        others,
      })
    })
    .collect()
}

/// Tells a changed stored list from the one a set was read from, without reading it again.
#[cfg(feature = "ui")]
fn fingerprint(bytes: &[u8]) -> u64 {
  let mut hasher = DefaultHasher::new();
  bytes.hash(&mut hasher);
  hasher.finish()
}

#[cfg(test)]
mod tests {
  use super::*;

  use teamcraft_proto::{Id, Snowflake};

  const ME: i64 = 1;
  const ANNA: i64 = 2;
  const BEN: i64 = 3;
  const GUEST: i64 = 4;

  #[test]
  fn a_dm_belongs_when_the_other_person_is_a_member() {
    let members = set(&[ME, ANNA, GUEST]);
    assert!(with(&[ME, ANNA], &members));
    assert!(!with(&[ME, BEN], &members));
  }

  #[test]
  fn a_group_dm_belongs_only_when_everybody_else_is_a_member() {
    let members = set(&[ME, ANNA]);
    assert!(with(&[ME, ANNA], &members));
    assert!(!with(&[ME, ANNA, BEN], &members));
    // The user's own membership doesn't matter, only the others'.
    assert!(with(&[ME, ANNA, BEN], &set(&[ANNA, BEN])));
  }

  #[test]
  fn guests_follow_the_same_rule() {
    assert!(with(&[ME, GUEST], &set(&[ME, GUEST])));
    assert!(!with(&[ME, GUEST], &set(&[ME, ANNA])));
  }

  #[test]
  fn notes_to_self_belong_to_no_space() {
    assert!(!with(&[ME], &set(&[ME])));
    assert!(!with(&[], &set(&[ME])));
  }

  #[test]
  fn a_member_who_left_takes_the_dm_out_and_back_in_on_return() {
    let mut members = set(&[ME, ANNA]);
    assert!(with(&[ME, ANNA], &members));
    members.remove(&id(ANNA));
    assert!(!with(&[ME, ANNA], &members));
    members.insert(id(ANNA));
    assert!(with(&[ME, ANNA], &members));
  }

  #[test]
  fn a_large_space_is_a_lookup_per_participant() {
    let members: HashSet<UserId> = (1..=100_000).map(id).collect();
    assert!(with(&[ME, 99_999], &members));
    assert!(!with(&[ME, 99_999, 100_001], &members));
  }

  #[cfg(feature = "ui")]
  #[test]
  fn the_fingerprint_follows_the_stored_list() {
    assert_eq!(fingerprint(b"[1,2]"), fingerprint(b"[1,2]"));
    assert_ne!(fingerprint(b"[1,2]"), fingerprint(b"[1,3]"));
  }

  /// The rule over the stored state, as the snapshot reads it.
  #[cfg(feature = "ui")]
  mod stored {
    use std::sync::Arc;

    use teamcraft_client_api::backend::ChatBackend;
    use teamcraft_proto::{
      ChannelRole, ConversationMemberResponse, ConversationVisibility, HistoryPolicy, JoinPolicy,
      ListConversationMembersResponse, ListSpaceMembersResponse, SpaceMemberResponse, SpaceResponse,
      SpaceRole, UserKind, UserSummary,
    };

    use super::*;
    use crate::clock::ManualClock;
    use crate::records;
    use crate::store::MemoryStore;

    const SPACE: i64 = 50;
    const CHANNEL: i64 = 60;
    const DM_ANNA: i64 = 70;
    const DM_BEN: i64 = 71;
    const GROUP: i64 = 72;
    const NOTES: i64 = 73;

    #[test]
    fn the_space_lists_the_dms_with_its_members_from_the_stored_list() {
      let store = workspace();
      put_roster(&store, &[ME, ANNA, GUEST]);
      let backend = backend(&store);
      assert_eq!(listed(&backend), [DM_ANNA].map(conversation_id).to_vec());
      // One conversation, in Chats as before: nothing is copied.
      assert_eq!(backend.direct_conversations().len(), 4);
      let in_space: Vec<ConversationId> = backend
        .space_direct_conversations(space_id())
        .into_iter()
        .map(|conversation| conversation.id)
        .collect();
      assert_eq!(in_space, [DM_ANNA].map(conversation_id).to_vec());
    }

    #[test]
    fn a_member_who_leaves_takes_the_dm_out_and_a_later_join_brings_it_back() {
      let store = workspace();
      put_roster(&store, &[ME, ANNA, BEN]);
      let backend = backend(&store);
      assert_eq!(
        listed(&backend),
        [DM_ANNA, DM_BEN, GROUP].map(conversation_id).to_vec()
      );
      put_roster(&store, &[ME, ANNA]);
      assert_eq!(listed(&backend), [DM_ANNA].map(conversation_id).to_vec());
      put_roster(&store, &[ME, ANNA, BEN]);
      assert_eq!(
        listed(&backend),
        [DM_ANNA, DM_BEN, GROUP].map(conversation_id).to_vec()
      );
    }

    /// A guest may not list the space's members, and an offline start may have no list yet: the
    /// people of the space's channels stand in.
    #[test]
    fn without_a_member_list_the_channel_members_stand_in() {
      let store = workspace();
      put_channel_members(&store, &[ME, BEN]);
      let backend = backend(&store);
      assert_eq!(listed(&backend), [DM_BEN].map(conversation_id).to_vec());
      records::put_json(
        &store,
        Table::SpaceRosters,
        &space_id().to_string(),
        &StoredRoster::Hidden,
      )
      .unwrap();
      assert_eq!(listed(&backend), [DM_BEN].map(conversation_id).to_vec());
    }

    #[test]
    fn a_large_space_is_read_once_while_its_list_stays() {
      let store = workspace();
      let mut everybody: Vec<i64> = (100..20_100).collect();
      everybody.extend([ME, ANNA]);
      put_roster(&store, &everybody);
      let backend = backend(&store);
      assert_eq!(listed(&backend), [DM_ANNA].map(conversation_id).to_vec());
      let kept = Rc::clone(&backend.space_members_kept.borrow()[&space_id()].members);
      assert_eq!(listed(&backend), [DM_ANNA].map(conversation_id).to_vec());
      // The same set, not read again.
      assert!(Rc::ptr_eq(
        &kept,
        &backend.space_members_kept.borrow()[&space_id()].members
      ));
    }

    fn listed(backend: &CoreChatBackend<Arc<MemoryStore>>) -> Vec<ConversationId> {
      let spaces = backend.spaces();
      assert_eq!(spaces.len(), 1);
      spaces[0].direct_conversations.clone()
    }

    fn backend(store: &Arc<MemoryStore>) -> CoreChatBackend<Arc<MemoryStore>> {
      CoreChatBackend::new(Arc::clone(store), id(ME), Box::new(ManualClock::new(0)))
    }

    /// A space with one channel; DMs with Anna and Ben, a group DM with both, and the notes.
    fn workspace() -> Arc<MemoryStore> {
      let store = Arc::new(MemoryStore::new());
      records::put_json(&store, Table::Spaces, &space_id().to_string(), &space()).unwrap();
      let mut channel = conversation(CHANNEL, ConversationKind::Channel, &[]);
      channel.maybe_space_id = Some(space_id());
      for conversation in [
        channel,
        conversation(DM_ANNA, ConversationKind::Dm, &[ME, ANNA]),
        conversation(DM_BEN, ConversationKind::Dm, &[ME, BEN]),
        conversation(GROUP, ConversationKind::Group, &[ME, ANNA, BEN]),
        conversation(NOTES, ConversationKind::Dm, &[ME]),
      ] {
        records::put_json(
          &store,
          Table::Conversations,
          &conversation.id.to_string(),
          &conversation,
        )
        .unwrap();
      }
      store
    }

    fn put_roster(store: &Arc<MemoryStore>, users: &[i64]) {
      let roster = ListSpaceMembersResponse {
        members: users
          .iter()
          .map(|user| SpaceMemberResponse {
            user: summary(*user),
            role: SpaceRole::Member,
            maybe_expires_at: None,
            maybe_sponsor_user_id: None,
          })
          .collect(),
      };
      records::put_json(
        store,
        Table::SpaceRosters,
        &space_id().to_string(),
        &StoredRoster::Visible(roster),
      )
      .unwrap();
    }

    fn put_channel_members(store: &Arc<MemoryStore>, users: &[i64]) {
      let members = ListConversationMembersResponse {
        members: users
          .iter()
          .map(|user| ConversationMemberResponse {
            user: summary(*user),
            role: ChannelRole::Member,
          })
          .collect(),
      };
      records::put_json(
        store,
        Table::Members,
        &conversation_id(CHANNEL).to_string(),
        &members,
      )
      .unwrap();
    }

    fn space() -> SpaceResponse {
      SpaceResponse {
        id: space_id(),
        name: "Project A".into(),
        maybe_icon: None,
        maybe_color: None,
        maybe_description: None,
        maybe_image: None,
        join_policy: JoinPolicy::Open,
        is_archived: false,
        maybe_brand_id: None,
        maybe_my_role: Some(SpaceRole::Member),
        maybe_my_notifications: None,
        member_count: 3,
        can_add_members: false,
        can_invite_guests: false,
        can_manage_space: false,
        can_delete_space: false,
        deleted_channel_count: 0,
        can_create_channels: false,
      }
    }

    fn conversation(raw: i64, kind: ConversationKind, participants: &[i64]) -> ConversationResponse {
      ConversationResponse {
        id: conversation_id(raw),
        maybe_space_id: None,
        kind,
        maybe_name: None,
        maybe_topic: None,
        maybe_description: None,
        visibility: ConversationVisibility::Private,
        history_policy: HistoryPolicy::Full,
        maybe_category_id: None,
        is_archived: false,
        moderation: Default::default(),
        is_integration: false,
        contains_guests: false,
        maybe_organizer_user_id: None,
        maybe_my_permissions: None,
        maybe_importer_user_id: None,
        maybe_imported_through: None,
        maybe_my_membership: None,
        participants: participants.iter().copied().map(summary).collect(),
        maybe_image: None,
      }
    }

    fn summary(raw: i64) -> UserSummary {
      UserSummary {
        id: id(raw),
        maybe_username: None,
        display_name: format!("User {raw}"),
        kind: UserKind::Member,
        maybe_email: None,
        maybe_organization: None,
        maybe_has_devices: None,
        is_awaiting_first_sign_in: false,
        is_deactivated: false,
        is_deleted: false,
      }
    }

    fn space_id() -> SpaceId {
      SpaceId::from_snowflake(Snowflake::new(SPACE).unwrap())
    }

    fn conversation_id(raw: i64) -> ConversationId {
      ConversationId::from_snowflake(Snowflake::new(raw).unwrap())
    }
  }

  fn with(participants: &[i64], members: &HashSet<UserId>) -> bool {
    is_with_members(id(ME), participants.iter().copied().map(id), members)
  }

  fn set(users: &[i64]) -> HashSet<UserId> {
    users.iter().copied().map(id).collect()
  }

  fn id(raw: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(raw).unwrap())
  }
}
