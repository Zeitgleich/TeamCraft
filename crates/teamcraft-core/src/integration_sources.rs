//! Integration-bridge sources managed in the app (docs/bridge.md, "Sources"; e2ee.md §13.1).
//!
//! A channel admin's client announces a source with a hidden, encrypted control message in the
//! channel ([`MessageContent::IntegrationSource`](crate::store::MessageContent)). Every member
//! device that reads it, the bridge's included, keeps what it learned in a [`SourceLedger`] per
//! conversation. Which sources are in force is decided when they are used, from the ledger and the
//! [`SourceAuthority`] of that moment:
//!
//! - **Created** counts only from somebody who may manage the channel's sources now: a current
//!   member of the channel's encryption group (decrypting the message proved it was one when it
//!   wrote), not a guest or bot, and a workspace admin of the verified roster or a channel admin.
//!   The channel role is the server's word (signed channel roles: issue #380), so it only picks
//!   among the group's members: the server can't make anybody outside the group a source admin, and
//!   a source can only post into the channel whose group announced it.
//! - **Revoked** counts from any member of the group, for good: revoking fails safe, and a revoked
//!   source never comes back, whatever arrives later.
//! - **Renamed** counts from somebody who may manage the sources; the newest wins.
//!
//! The ledger is bounded ([`MAX_LEDGER_ENTRIES`]) and order-independent: history loads newest
//! first, so a revocation may arrive before the creation it revokes.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  ChannelRole, ConversationId, ListConversationMembersResponse, MAX_SOURCE_NAME_CHARS, MessageId,
  SourceChange, SourceId, SourceKind, SourceOptions, SourceTokenHash, UserId, UserKind, UserSummary,
};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::workspace::WorkspaceState;

/// Sources (revoked ones included) a device remembers per conversation; changes about further
/// sources are ignored, so a member can't grow every device's store without bound.
pub const MAX_LEDGER_ENTRIES: usize = 1024;

/// Everything this device learned about a conversation's sources.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceLedger {
  /// Counts the changes that altered the ledger, so a reader notices news cheaply.
  #[serde(default)]
  pub revision: u64,
  #[serde(default)]
  entries: BTreeMap<SourceId, LedgerEntry>,
}

/// A source that is in force: created by somebody allowed to, not revoked, of a known kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveSource {
  pub id: SourceId,
  pub conversation: ConversationId,
  pub name: String,
  pub kind: SourceKind,
  pub token_sha256: SourceTokenHash,
  /// Already [bounded](SourceOptions::bounded).
  pub options: SourceOptions,
  pub created_by: UserId,
  pub created_at_ms: i64,
}

/// Who may manage a conversation's sources at the moment.
#[derive(Clone, Debug, Default)]
pub struct SourceAuthority {
  /// The users in the conversation's encryption group; `None` where this device can't look into
  /// the group (the UI process), which then relies on decryption alone.
  maybe_group_users: Option<HashSet<UserId>>,
  /// Verified workspace admins and the channel's admins.
  managers: HashSet<UserId>,
  /// Guests and bots: they manage no sources, whatever their role.
  excluded: HashSet<UserId>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct LedgerEntry {
  #[serde(rename = "created", default, skip_serializing_if = "Option::is_none")]
  maybe_created: Option<CreatedRecord>,
  #[serde(rename = "renamed", default, skip_serializing_if = "Option::is_none")]
  maybe_renamed: Option<RenamedRecord>,
  /// Members who revoked the source; any one of them is enough.
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  revoked_by: Vec<UserId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CreatedRecord {
  by: UserId,
  at_ms: i64,
  /// `None` while the own announcement isn't sent yet.
  #[serde(rename = "server_id", default, skip_serializing_if = "Option::is_none")]
  maybe_server_id: Option<MessageId>,
  name: String,
  kind: SourceKind,
  token_sha256: SourceTokenHash,
  options: SourceOptions,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct RenamedRecord {
  by: UserId,
  #[serde(rename = "server_id", default, skip_serializing_if = "Option::is_none")]
  maybe_server_id: Option<MessageId>,
  name: String,
}

impl SourceLedger {
  /// The ledger of `conversation` as stored (empty if nothing was learned yet).
  pub fn load(store: &impl LocalStore, conversation: ConversationId) -> Result<Self, CoreError> {
    Ok(records::get_json(store, Table::Account, &ledger_key(conversation))?.unwrap_or_default())
  }

  /// Takes in one change that `sender` wrote in the conversation (`maybe_server_id`: its server ID,
  /// `None` for an own change not sent yet). Returns whether the ledger changed.
  pub fn apply(
    &mut self,
    sender: UserId,
    change: &SourceChange,
    at_ms: i64,
    maybe_server_id: Option<MessageId>,
  ) -> bool {
    let Some(source) = change.source() else {
      return false;
    };
    if !self.entries.contains_key(&source) && self.entries.len() >= MAX_LEDGER_ENTRIES {
      tracing::warn!("ignored a source change: the conversation has too many sources");
      return false;
    }
    let entry = self.entries.entry(source).or_default();
    let is_changed = match change {
      SourceChange::Created {
        name,
        kind,
        token_sha256,
        options,
        ..
      } => {
        let Some(name) = clean_name(name) else {
          return false;
        };
        let record = CreatedRecord {
          by: sender,
          at_ms,
          maybe_server_id,
          name,
          kind: kind.clone(),
          token_sha256: *token_sha256,
          options: options.bounded(),
        };
        // The first announcement the server numbered wins: a later one with the same ID (a
        // member reusing it) changes nothing, whatever order they arrive in.
        let is_earlier = entry
          .maybe_created
          .as_ref()
          .is_none_or(|existing| is_before(maybe_server_id, existing.maybe_server_id));
        if is_earlier && entry.maybe_created.as_ref() != Some(&record) {
          entry.maybe_created = Some(record);
          true
        } else {
          false
        }
      },
      SourceChange::Renamed { name, .. } => {
        let Some(name) = clean_name(name) else {
          return false;
        };
        let is_newer = entry
          .maybe_renamed
          .as_ref()
          .is_none_or(|existing| !is_before(maybe_server_id, existing.maybe_server_id));
        let record = RenamedRecord {
          by: sender,
          maybe_server_id,
          name,
        };
        if is_newer && entry.maybe_renamed.as_ref() != Some(&record) {
          entry.maybe_renamed = Some(record);
          true
        } else {
          false
        }
      },
      SourceChange::Revoked { .. } => {
        if entry.revoked_by.contains(&sender) {
          false
        } else {
          entry.revoked_by.push(sender);
          true
        }
      },
      SourceChange::Unknown => false,
    };
    if is_changed {
      self.revision += 1;
    }
    is_changed
  }

  /// The sources in force under `authority`, oldest first.
  pub fn active(&self, conversation: ConversationId, authority: &SourceAuthority) -> Vec<ActiveSource> {
    let mut active: Vec<ActiveSource> = self
      .entries
      .iter()
      .filter(|(_, entry)| entry.revoked_by.is_empty())
      .filter_map(|(id, entry)| {
        let created = entry.maybe_created.as_ref()?;
        if matches!(created.kind, SourceKind::Unknown(_)) || !authority.may_manage(created.by) {
          return None;
        }
        let name = entry
          .maybe_renamed
          .as_ref()
          .filter(|renamed| authority.may_manage(renamed.by))
          .map_or_else(|| created.name.clone(), |renamed| renamed.name.clone());
        Some(ActiveSource {
          id: *id,
          conversation,
          name,
          kind: created.kind.clone(),
          token_sha256: created.token_sha256,
          options: created.options.clone(),
          created_by: created.by,
          created_at_ms: created.at_ms,
        })
      })
      .collect();
    active.sort_by_key(|source| (source.created_at_ms, source.id));
    active
  }
}

impl SourceAuthority {
  /// The authority as this device's store knows it: `workspace` (its roster counts only while
  /// `is_roster_fresh`), the conversation's member list with the server's roles, and the server's
  /// account kinds. `extra_guests` adds guests the caller knows of otherwise (meeting guests).
  pub fn from_store(
    store: &impl LocalStore,
    conversation: ConversationId,
    workspace: &WorkspaceState,
    is_roster_fresh: bool,
    maybe_group_users: Option<HashSet<UserId>>,
    extra_guests: impl IntoIterator<Item = UserId>,
  ) -> Result<Self, CoreError> {
    let members: ListConversationMembersResponse =
      records::get_json(store, Table::Members, &conversation.to_string())?
        .unwrap_or(ListConversationMembersResponse { members: Vec::new() });
    let mut managers: HashSet<UserId> = members
      .members
      .iter()
      .filter(|member| member.role == ChannelRole::Admin)
      .map(|member| member.user.id)
      .collect();
    if is_roster_fresh {
      managers.extend(workspace.admins());
    }
    let mut excluded: HashSet<UserId> = extra_guests.into_iter().collect();
    for user in managers.iter().chain(maybe_group_users.iter().flatten()) {
      let maybe_summary: Option<UserSummary> = records::get_json(store, Table::Users, &user.to_string())?;
      if maybe_summary.is_some_and(|summary| matches!(summary.kind, UserKind::Guest | UserKind::Bot)) {
        excluded.insert(*user);
      }
    }
    Ok(Self {
      maybe_group_users,
      managers,
      excluded,
    })
  }

  /// Whether `user` may create and rename sources now.
  pub fn may_manage(&self, user: UserId) -> bool {
    let is_in_group = self
      .maybe_group_users
      .as_ref()
      .is_none_or(|users| users.contains(&user));
    is_in_group && !self.excluded.contains(&user) && self.managers.contains(&user)
  }
}

/// Applies a received or own source change to the stored ledger of `conversation`.
pub(crate) fn apply_source_change(
  store: &impl LocalStore,
  conversation: ConversationId,
  sender: UserId,
  change: &SourceChange,
  at_ms: i64,
  maybe_server_id: Option<MessageId>,
) -> Result<(), CoreError> {
  let mut ledger = SourceLedger::load(store, conversation)?;
  if ledger.apply(sender, change, at_ms, maybe_server_id) {
    records::put_json(store, Table::Account, &ledger_key(conversation), &ledger)?;
  }
  Ok(())
}

fn ledger_key(conversation: ConversationId) -> String {
  format!("integration_sources/{conversation}")
}

/// A name without control characters, trimmed, of 1 to [`MAX_SOURCE_NAME_CHARS`] characters.
fn clean_name(name: &str) -> Option<String> {
  let name: String = name.chars().filter(|c| !c.is_control()).collect();
  let name = name.trim();
  (!name.is_empty() && name.chars().count() <= MAX_SOURCE_NAME_CHARS).then(|| name.to_owned())
}

/// Whether a change numbered `candidate` came before one numbered `existing`. A change not sent yet
/// (`None`) is the newest there is.
fn is_before(candidate: Option<MessageId>, existing: Option<MessageId>) -> bool {
  match (candidate, existing) {
    (Some(candidate), Some(existing)) => candidate < existing,
    (Some(_), None) => true,
    (None, _) => false,
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::{Id, Snowflake};

  use super::*;

  const CONVERSATION: i64 = 9;
  const ADMIN: i64 = 1;
  const MEMBER: i64 = 2;
  const OTHER_ADMIN: i64 = 3;

  mod creation {
    use super::*;

    #[test]
    fn a_source_from_an_admin_is_active_and_one_from_a_member_is_not() {
      let mut ledger = SourceLedger::default();
      assert!(ledger.apply(user(ADMIN), &created(1, "ci"), 10, Some(message(100))));
      assert!(ledger.apply(user(MEMBER), &created(2, "sneaky"), 11, Some(message(101))));
      let active = ledger.active(conversation(), &authority());
      assert_eq!(active.len(), 1);
      assert_eq!(
        (active[0].name.as_str(), active[0].created_by),
        ("ci", user(ADMIN))
      );
      assert_eq!(active[0].conversation, conversation());
    }

    #[test]
    fn the_first_numbered_announcement_of_an_id_wins_in_any_order() {
      let first = created_with(1, "real", [1; 32]);
      let copy = created_with(1, "copy", [2; 32]);
      for order in [[(&first, 100), (&copy, 200)], [(&copy, 200), (&first, 100)]] {
        let mut ledger = SourceLedger::default();
        for (change, id) in order {
          ledger.apply(user(ADMIN), change, 0, Some(message(id)));
        }
        let active = ledger.active(conversation(), &authority());
        assert_eq!(active[0].name, "real");
        assert_eq!(active[0].token_sha256, SourceTokenHash::from_bytes([1; 32]));
      }
    }

    #[test]
    fn an_own_pending_announcement_is_settled_by_its_echo() {
      let mut ledger = SourceLedger::default();
      ledger.apply(user(ADMIN), &created(1, "ci"), 10, None);
      assert_eq!(ledger.active(conversation(), &authority()).len(), 1);
      assert!(ledger.apply(user(ADMIN), &created(1, "ci"), 10, Some(message(100))));
      assert!(
        !ledger.apply(user(ADMIN), &created(1, "ci"), 10, Some(message(100))),
        "a duplicate changes nothing"
      );
    }

    #[test]
    fn bad_names_unknown_kinds_and_unknown_changes_are_ignored() {
      let mut ledger = SourceLedger::default();
      assert!(!ledger.apply(user(ADMIN), &created(1, "  "), 0, Some(message(1))));
      assert!(!ledger.apply(user(ADMIN), &created(2, &"x".repeat(81)), 0, Some(message(2))));
      assert!(!ledger.apply(user(ADMIN), &SourceChange::Unknown, 0, Some(message(3))));
      let SourceChange::Created {
        source,
        name,
        token_sha256,
        options,
        ..
      } = created(4, "metrics")
      else {
        unreachable!()
      };
      let future = SourceChange::Created {
        source,
        name,
        kind: SourceKind::Unknown("metrics".into()),
        token_sha256,
        options,
      };
      assert!(ledger.apply(user(ADMIN), &future, 0, Some(message(4))));
      assert!(ledger.active(conversation(), &authority()).is_empty());
      assert!(ledger.apply(user(ADMIN), &created(5, "a\u{7}b "), 0, Some(message(5))));
      assert_eq!(ledger.active(conversation(), &authority())[0].name, "ab");
    }

    #[test]
    fn options_are_bounded_when_taken_in() {
      let SourceChange::Created {
        source,
        name,
        kind,
        token_sha256,
        ..
      } = created(1, "ci")
      else {
        unreachable!()
      };
      let change = SourceChange::Created {
        source,
        name,
        kind,
        token_sha256,
        options: SourceOptions {
          rate_limit_per_minute: u32::MAX,
          ..SourceOptions::default()
        },
      };
      let mut ledger = SourceLedger::default();
      ledger.apply(user(ADMIN), &change, 0, Some(message(1)));
      assert_eq!(
        ledger.active(conversation(), &authority())[0]
          .options
          .rate_limit_per_minute,
        teamcraft_proto::MAX_SOURCE_RATE_LIMIT_PER_MINUTE
      );
    }

    #[test]
    fn the_ledger_is_bounded() {
      let mut ledger = SourceLedger::default();
      for index in 0..MAX_LEDGER_ENTRIES as i64 {
        ledger.apply(user(ADMIN), &created(index + 1, "s"), 0, Some(message(index + 1)));
      }
      assert!(!ledger.apply(user(ADMIN), &created(5000, "one more"), 0, Some(message(5000))));
      assert!(
        ledger.apply(user(ADMIN), &revoked(1), 0, Some(message(5001))),
        "known sources still change"
      );
    }
  }

  mod authority_rules {
    use super::*;

    #[test]
    fn a_creator_who_lost_the_role_or_left_the_group_has_no_active_sources() {
      let mut ledger = SourceLedger::default();
      ledger.apply(user(ADMIN), &created(1, "ci"), 0, Some(message(1)));
      let demoted = SourceAuthority {
        managers: HashSet::from([user(OTHER_ADMIN)]),
        ..authority()
      };
      assert!(ledger.active(conversation(), &demoted).is_empty());
      let left = SourceAuthority {
        maybe_group_users: Some(HashSet::from([user(MEMBER)])),
        ..authority()
      };
      assert!(ledger.active(conversation(), &left).is_empty());
    }

    #[test]
    fn guests_and_bots_manage_nothing_even_as_admins() {
      let excluded = SourceAuthority {
        excluded: HashSet::from([user(ADMIN)]),
        ..authority()
      };
      assert!(!excluded.may_manage(user(ADMIN)));
      assert!(excluded.may_manage(user(OTHER_ADMIN)));
    }

    #[test]
    fn without_a_view_of_the_group_decryption_alone_counts() {
      let ui = SourceAuthority {
        maybe_group_users: None,
        ..authority()
      };
      assert!(ui.may_manage(user(ADMIN)));
    }

    #[test]
    fn the_store_gives_channel_admins_fresh_roster_admins_and_excludes_guests() {
      let store = crate::store::MemoryStore::new();
      let members = ListConversationMembersResponse {
        members: vec![
          member(ADMIN, ChannelRole::Admin, UserKind::Member),
          member(MEMBER, ChannelRole::Member, UserKind::Member),
          member(5, ChannelRole::Admin, UserKind::Guest),
        ],
      };
      records::put_json(&store, Table::Members, &conversation().to_string(), &members).unwrap();
      for entry in &members.members {
        records::put_json(&store, Table::Users, &entry.user.id.to_string(), &entry.user).unwrap();
      }
      let authority =
        SourceAuthority::from_store(&store, conversation(), &WorkspaceState::default(), true, None, [])
          .unwrap();
      assert!(authority.may_manage(user(ADMIN)));
      assert!(!authority.may_manage(user(MEMBER)));
      assert!(
        !authority.may_manage(user(5)),
        "a guest stays a guest whatever the role"
      );
    }
  }

  mod revocation {
    use super::*;

    #[test]
    fn any_member_revokes_for_good_even_before_the_creation_arrives() {
      let mut ledger = SourceLedger::default();
      ledger.apply(user(MEMBER), &revoked(1), 0, Some(message(200)));
      ledger.apply(user(ADMIN), &created(1, "ci"), 0, Some(message(100)));
      assert!(ledger.active(conversation(), &authority()).is_empty());
      ledger.apply(user(ADMIN), &created(1, "ci"), 0, Some(message(50)));
      assert!(
        ledger.active(conversation(), &authority()).is_empty(),
        "never comes back"
      );
    }
  }

  mod renaming {
    use super::*;

    #[test]
    fn the_newest_rename_by_a_manager_names_the_source() {
      let mut ledger = SourceLedger::default();
      ledger.apply(user(ADMIN), &created(1, "ci"), 0, Some(message(1)));
      ledger.apply(user(OTHER_ADMIN), &renamed(1, "builds"), 0, Some(message(3)));
      ledger.apply(user(ADMIN), &renamed(1, "older"), 0, Some(message(2)));
      assert_eq!(ledger.active(conversation(), &authority())[0].name, "builds");
      ledger.apply(user(MEMBER), &renamed(1, "mine"), 0, Some(message(4)));
      assert_eq!(
        ledger.active(conversation(), &authority())[0].name,
        "ci",
        "a rename by a member falls back to the original name"
      );
    }
  }

  #[test]
  fn the_stored_ledger_survives_a_reload() {
    let store = crate::store::MemoryStore::new();
    apply_source_change(
      &store,
      conversation(),
      user(ADMIN),
      &created(1, "ci"),
      0,
      Some(message(1)),
    )
    .unwrap();
    let ledger = SourceLedger::load(&store, conversation()).unwrap();
    assert_eq!(ledger.revision, 1);
    assert_eq!(ledger.active(conversation(), &authority()).len(), 1);
  }

  fn authority() -> SourceAuthority {
    SourceAuthority {
      maybe_group_users: Some(HashSet::from([user(ADMIN), user(MEMBER), user(OTHER_ADMIN)])),
      managers: HashSet::from([user(ADMIN), user(OTHER_ADMIN)]),
      excluded: HashSet::new(),
    }
  }

  fn created(id: i64, name: &str) -> SourceChange {
    created_with(id, name, [7; 32])
  }

  fn created_with(id: i64, name: &str, hash: [u8; 32]) -> SourceChange {
    SourceChange::Created {
      source: source(id),
      name: name.into(),
      kind: SourceKind::Webhook,
      token_sha256: SourceTokenHash::from_bytes(hash),
      options: SourceOptions::default(),
    }
  }

  fn renamed(id: i64, name: &str) -> SourceChange {
    SourceChange::Renamed {
      source: source(id),
      name: name.into(),
    }
  }

  fn revoked(id: i64) -> SourceChange {
    SourceChange::Revoked { source: source(id) }
  }

  fn source(id: i64) -> SourceId {
    let mut bytes = [0; 16];
    bytes[8..].copy_from_slice(&id.to_be_bytes());
    SourceId::from_bytes(bytes)
  }

  fn member(id: i64, role: ChannelRole, kind: UserKind) -> teamcraft_proto::ConversationMemberResponse {
    let summary = UserSummary {
      id: user(id),
      maybe_username: None,
      display_name: format!("user {id}"),
      kind,
      maybe_email: None,
      maybe_organization: None,
      maybe_has_devices: None,
    };
    teamcraft_proto::ConversationMemberResponse { user: summary, role }
  }

  fn user(id: i64) -> UserId {
    UserId::from_snowflake(Snowflake::new(id).unwrap())
  }

  fn message(id: i64) -> MessageId {
    MessageId::from_snowflake(Snowflake::new(id).unwrap())
  }

  fn conversation() -> ConversationId {
    ConversationId::from_snowflake(Snowflake::new(CONVERSATION).unwrap())
  }
}
