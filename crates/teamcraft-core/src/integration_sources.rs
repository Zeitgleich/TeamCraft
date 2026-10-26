//! Integration-bridge sources managed in the app (docs/bridge.md, "Sources"; e2ee.md §13.1).
//!
//! A workspace admin's client announces a source with a hidden, encrypted control message in the
//! channel ([`MessageContent::IntegrationSource`](crate::store::MessageContent)). Every member
//! device that reads it, the bridge's included, keeps what it learned in a [`SourceLedger`] per
//! conversation. Only signed data decides who may manage sources: the verified, fresh workspace
//! roster (admin.md §2.3). Channel roles are the server's word until they are signed (issue #380),
//! so they count for nothing here; neither do the server's account kinds, except to take rights
//! away (guests and bots manage nothing).
//!
//! - **Taking in** ([`SourceLedger::apply`]): a creation or rename counts only from a roster admin
//!   at the message's server time; a revocation from a roster admin or the source's creator.
//!   Anything else never reaches the ledger, so a member can't fill it.
//! - **In force** ([`SourceLedger::active`]): created by somebody who is a roster admin now and in
//!   the channel's encryption group, not revoked, of a known kind; at most
//!   [`MAX_ACTIVE_SOURCES`] per channel. A revoked source never comes back.
//!
//! The ledger is bounded ([`MAX_LEDGER_ENTRIES`]) and order-independent: history loads newest
//! first, so a revocation may arrive before the creation it revokes.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};
use teamcraft_proto::{
  ConversationId, MAX_SOURCE_NAME_CHARS, MessageId, SourceChange, SourceId, SourceKind, SourceOptions,
  SourceTokenHash, UserId, UserKind, UserSummary,
};

use crate::error::CoreError;
use crate::records;
use crate::store::{LocalStore, Table};
use crate::workspace::WorkspaceState;

/// Sources (revoked ones included) a device remembers per conversation; changes about further
/// sources are ignored, so not even an admin can grow every device's store without bound.
pub const MAX_LEDGER_ENTRIES: usize = 1024;
/// Sources in force per conversation; the oldest ones count.
pub const MAX_ACTIVE_SOURCES: usize = 50;

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
  /// Admins of the verified, fresh workspace roster.
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
  /// Admins (or the creator) who revoked the source; any one of them is enough.
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
  /// `None` for an own change not sent yet). `is_manager`: the sender was a roster admin when the
  /// change was written. Without it only the creator's revocation counts. Returns whether the
  /// ledger changed.
  pub fn apply(
    &mut self,
    sender: UserId,
    is_manager: bool,
    change: &SourceChange,
    at_ms: i64,
    maybe_server_id: Option<MessageId>,
  ) -> bool {
    let Some(source) = change.source() else {
      return false;
    };
    let is_creator = self
      .entries
      .get(&source)
      .and_then(|entry| entry.maybe_created.as_ref())
      .is_some_and(|created| created.by == sender);
    let is_allowed = is_manager || (is_creator && matches!(change, SourceChange::Revoked { .. }));
    if !is_allowed {
      tracing::debug!("ignored a source change from somebody who may not manage sources");
      return false;
    }
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
    if active.len() > MAX_ACTIVE_SOURCES {
      tracing::warn!(conversation_id = %conversation, count = active.len(), "too many sources; the newest are not served");
      active.truncate(MAX_ACTIVE_SOURCES);
    }
    active
  }
}

impl SourceAuthority {
  /// The authority as this device's store knows it: the admins of `workspace` while
  /// `is_roster_fresh`, minus anybody the server calls a guest or bot (which only takes rights
  /// away). `extra_guests` adds guests the caller knows of otherwise (meeting guests).
  pub fn from_store(
    store: &impl LocalStore,
    workspace: &WorkspaceState,
    is_roster_fresh: bool,
    maybe_group_users: Option<HashSet<UserId>>,
    extra_guests: impl IntoIterator<Item = UserId>,
  ) -> Result<Self, CoreError> {
    let managers: HashSet<UserId> = if is_roster_fresh {
      workspace.admins().collect()
    } else {
      HashSet::new()
    };
    let mut excluded: HashSet<UserId> = extra_guests.into_iter().collect();
    for user in &managers {
      if is_guest_or_bot(store, *user)? {
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
  let is_manager = was_manager(store, sender, at_ms)?;
  if ledger.apply(sender, is_manager, change, at_ms, maybe_server_id) {
    records::put_json(store, Table::Account, &ledger_key(conversation), &ledger)?;
  }
  Ok(())
}

/// Whether `user` was a source manager at `at_ms` (the message's server time): an admin of the
/// verified roster this device holds, fresh at that time, and no guest or bot.
fn was_manager(store: &impl LocalStore, user: UserId, at_ms: i64) -> Result<bool, CoreError> {
  let workspace: WorkspaceState =
    records::get_json(store, Table::Workspace, crate::client::WORKSPACE_KEY)?.unwrap_or_default();
  let max_age_ms =
    i64::try_from(crate::ClientSettings::default().workspace_max_age.as_millis()).unwrap_or(i64::MAX);
  Ok(
    workspace.is_roster_fresh(at_ms, max_age_ms)
      && workspace.is_admin(user)
      && !is_guest_or_bot(store, user)?,
  )
}

fn is_guest_or_bot(store: &impl LocalStore, user: UserId) -> Result<bool, CoreError> {
  let maybe_summary: Option<UserSummary> = records::get_json(store, Table::Users, &user.to_string())?;
  Ok(maybe_summary.is_some_and(|summary| matches!(summary.kind, UserKind::Guest | UserKind::Bot)))
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

  mod taking_in {
    use super::*;

    #[test]
    fn a_source_from_an_admin_is_active_and_one_from_a_member_never_reaches_the_ledger() {
      let mut ledger = SourceLedger::default();
      assert!(take(&mut ledger, ADMIN, &created(1, "ci"), 100));
      assert!(!take(&mut ledger, MEMBER, &created(2, "sneaky"), 101));
      let active = ledger.active(conversation(), &authority());
      assert_eq!(active.len(), 1);
      assert_eq!(
        (active[0].name.as_str(), active[0].created_by),
        ("ci", user(ADMIN))
      );
      assert_eq!(active[0].conversation, conversation());
      assert_eq!(ledger.revision, 1);
    }

    #[test]
    fn a_member_cannot_fill_the_ledger_with_revocations_or_renames() {
      let mut ledger = SourceLedger::default();
      for index in 1..2001 {
        assert!(!take(&mut ledger, MEMBER, &revoked(index), index));
        assert!(!take(&mut ledger, MEMBER, &renamed(index, "x"), index));
      }
      assert_eq!(ledger, SourceLedger::default());
      assert!(take(&mut ledger, ADMIN, &created(1, "ci"), 5000));
    }

    #[test]
    fn the_creator_may_revoke_but_a_member_may_not() {
      let mut ledger = SourceLedger::default();
      take(&mut ledger, ADMIN, &created(1, "ci"), 1);
      assert!(!take(&mut ledger, MEMBER, &revoked(1), 2));
      assert_eq!(ledger.active(conversation(), &authority()).len(), 1);
      // The creator, even after losing the admin role, can still take back their own source.
      assert!(ledger.apply(user(ADMIN), false, &revoked(1), 0, Some(message(3))));
      assert!(ledger.active(conversation(), &authority()).is_empty());
    }

    #[test]
    fn the_first_numbered_announcement_of_an_id_wins_in_any_order() {
      let first = created_with(1, "real", [1; 32]);
      let copy = created_with(1, "copy", [2; 32]);
      for order in [[(&first, 100), (&copy, 200)], [(&copy, 200), (&first, 100)]] {
        let mut ledger = SourceLedger::default();
        for (change, id) in order {
          take(&mut ledger, OTHER_ADMIN, change, id);
        }
        let active = ledger.active(conversation(), &authority());
        assert_eq!(active[0].name, "real");
        assert_eq!(active[0].token_sha256, SourceTokenHash::from_bytes([1; 32]));
      }
    }

    #[test]
    fn an_own_pending_announcement_is_settled_by_its_echo() {
      let mut ledger = SourceLedger::default();
      ledger.apply(user(ADMIN), true, &created(1, "ci"), 10, None);
      assert_eq!(ledger.active(conversation(), &authority()).len(), 1);
      assert!(take(&mut ledger, ADMIN, &created(1, "ci"), 100));
      assert!(
        !take(&mut ledger, ADMIN, &created(1, "ci"), 100),
        "a duplicate changes nothing"
      );
    }

    #[test]
    fn bad_names_unknown_kinds_and_unknown_changes_are_ignored() {
      let mut ledger = SourceLedger::default();
      assert!(!take(&mut ledger, ADMIN, &created(1, "  "), 1));
      assert!(!take(&mut ledger, ADMIN, &created(2, &"x".repeat(81)), 2));
      assert!(!take(&mut ledger, ADMIN, &SourceChange::Unknown, 3));
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
      assert!(take(&mut ledger, ADMIN, &future, 4));
      assert!(ledger.active(conversation(), &authority()).is_empty());
      assert!(take(&mut ledger, ADMIN, &created(5, "a\u{7}b "), 5));
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
      take(&mut ledger, ADMIN, &change, 1);
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
        take(&mut ledger, ADMIN, &created(index + 1, "s"), index + 1);
      }
      assert!(!take(&mut ledger, ADMIN, &created(5000, "one more"), 5000));
      assert!(
        take(&mut ledger, ADMIN, &revoked(1), 5001),
        "known sources still change"
      );
    }

    #[test]
    fn at_most_the_oldest_fifty_sources_are_in_force() {
      let mut ledger = SourceLedger::default();
      for index in 0..60 {
        ledger.apply(
          user(ADMIN),
          true,
          &created(index + 1, "s"),
          index,
          Some(message(index + 1)),
        );
      }
      let active = ledger.active(conversation(), &authority());
      assert_eq!(active.len(), MAX_ACTIVE_SOURCES);
      assert_eq!(active[0].created_at_ms, 0);
    }
  }

  mod authority_rules {
    use super::*;

    #[test]
    fn a_creator_who_lost_the_role_or_left_the_group_has_no_active_sources() {
      let mut ledger = SourceLedger::default();
      take(&mut ledger, ADMIN, &created(1, "ci"), 1);
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
    fn only_a_fresh_signed_roster_makes_managers_and_the_server_can_only_take_rights_away() {
      let store = crate::store::MemoryStore::new();
      let workspace = workspace_with_admins(&[ADMIN, 5]);
      records::put_json(
        &store,
        Table::Users,
        &user(5).to_string(),
        &summary(5, UserKind::Guest),
      )
      .unwrap();
      let authority = SourceAuthority::from_store(&store, &workspace, true, None, []).unwrap();
      assert!(authority.may_manage(user(ADMIN)));
      assert!(
        !authority.may_manage(user(MEMBER)),
        "no channel role makes a manager"
      );
      assert!(
        !authority.may_manage(user(5)),
        "a guest stays a guest whatever the roster"
      );
      let stale = SourceAuthority::from_store(&store, &workspace, false, None, []).unwrap();
      assert!(!stale.may_manage(user(ADMIN)), "a stale roster grants nothing");
    }

    #[test]
    fn a_change_counts_from_whoever_the_stored_roster_names_at_the_message_time() {
      let store = crate::store::MemoryStore::new();
      records::put_json(
        &store,
        Table::Workspace,
        crate::client::WORKSPACE_KEY,
        &workspace_with_admins(&[ADMIN]),
      )
      .unwrap();
      apply_source_change(
        &store,
        conversation(),
        user(MEMBER),
        &created(1, "m"),
        0,
        Some(message(1)),
      )
      .unwrap();
      apply_source_change(
        &store,
        conversation(),
        user(ADMIN),
        &created(2, "a"),
        0,
        Some(message(2)),
      )
      .unwrap();
      let ledger = SourceLedger::load(&store, conversation()).unwrap();
      assert_eq!(ledger.revision, 1);
      assert_eq!(ledger.active(conversation(), &authority())[0].name, "a");
    }
  }

  mod revocation {
    use super::*;

    #[test]
    fn an_admin_revokes_for_good_even_before_the_creation_arrives() {
      let mut ledger = SourceLedger::default();
      take(&mut ledger, OTHER_ADMIN, &revoked(1), 200);
      take(&mut ledger, ADMIN, &created(1, "ci"), 100);
      assert!(ledger.active(conversation(), &authority()).is_empty());
      take(&mut ledger, ADMIN, &created(1, "ci"), 50);
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
      take(&mut ledger, ADMIN, &created(1, "ci"), 1);
      take(&mut ledger, OTHER_ADMIN, &renamed(1, "builds"), 3);
      take(&mut ledger, ADMIN, &renamed(1, "older"), 2);
      assert_eq!(ledger.active(conversation(), &authority())[0].name, "builds");
      let demoted = SourceAuthority {
        managers: HashSet::from([user(ADMIN)]),
        ..authority()
      };
      assert_eq!(
        ledger.active(conversation(), &demoted)[0].name,
        "ci",
        "a rename by somebody no longer allowed falls back to the original name"
      );
    }
  }

  /// Applies `change` from `who` (admins are managers) with server ID `id`.
  fn take(ledger: &mut SourceLedger, who: i64, change: &SourceChange, id: i64) -> bool {
    let is_manager = who != MEMBER;
    ledger.apply(user(who), is_manager, change, 0, Some(message(id)))
  }

  fn authority() -> SourceAuthority {
    SourceAuthority {
      maybe_group_users: Some(HashSet::from([user(ADMIN), user(MEMBER), user(OTHER_ADMIN)])),
      managers: HashSet::from([user(ADMIN), user(OTHER_ADMIN)]),
      excluded: HashSet::new(),
    }
  }

  fn workspace_with_admins(admins: &[i64]) -> WorkspaceState {
    use crate::workspace::{RosterMember, RosterRole};
    let mut workspace = WorkspaceState::default();
    workspace.roster.members = admins
      .iter()
      .map(|id| RosterMember {
        user_id: user(*id),
        role: RosterRole::Admin,
        identity_key: teamcraft_proto::Base64Bytes::new(vec![0; 32]),
      })
      .collect();
    workspace
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

  fn summary(id: i64, kind: UserKind) -> UserSummary {
    UserSummary {
      id: user(id),
      maybe_username: None,
      display_name: format!("user {id}"),
      kind,
      maybe_email: None,
      maybe_organization: None,
      maybe_has_devices: None,
      is_deactivated: false,
    }
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
