//! Importing history from another chat system (e2ee.md §4.10, admin.md §3.8, issue #402). The import
//! runs on a client, `teamcraft-import`; the owners' signed policy names the account it signs in as
//! ([`ImportPolicy`]), and only that account may use the import endpoints.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{
  Base64Bytes, ClientMessageId, DeviceId, Id, MessageId, SNOWFLAKE_EPOCH_MS, SNOWFLAKE_SEQUENCE_BITS,
  SNOWFLAKE_WORKER_BITS, Snowflake, ThreadId, UserId,
};

/// The Snowflake worker number of imported messages (admin.md §3.8): the highest, which no server
/// instance may use, so imported IDs never collide with generated ones.
pub const IMPORT_WORKER: u16 = (1 << SNOWFLAKE_WORKER_BITS) - 1;
/// Most messages one `POST /admin/import/conversations/{id}/messages` takes.
pub const MAX_IMPORT_BATCH: usize = 100;
/// Most user names one `POST /admin/import/users` takes.
pub const MAX_PROVISION_BATCH: usize = 200;

/// The import group of the signed workspace policy: which account imports, and until when it may
/// join conversations. Off when no importer is named. Clients act on imported messages and on the
/// importer joining a DM only under the policy they verified.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct ImportPolicy {
  /// The import account. It stays named after the import, so imported messages keep their origin.
  #[serde(rename = "importer", skip_serializing_if = "Option::is_none")]
  pub maybe_importer: Option<UserId>,
  /// Until when (Unix milliseconds, server time) the importer may import and join conversations;
  /// absent: not (any more).
  #[serde(rename = "joins_until_ms", skip_serializing_if = "Option::is_none")]
  pub maybe_joins_until_ms: Option<i64>,
}

impl ImportPolicy {
  /// Whether `user` may import at `now_ms` (server time).
  pub fn allows(&self, user: UserId, now_ms: i64) -> bool {
    self.maybe_importer == Some(user) && self.maybe_joins_until_ms.is_some_and(|until| now_ms < until)
  }

  /// Whether `user` is the policy's importer, whether or not the import is still open: its
  /// messages show their imported origin.
  pub fn is_importer(&self, user: UserId) -> bool {
    self.maybe_importer == Some(user)
  }
}

/// The ID of an imported message sent at `sent_at_ms`, after the conversation's last imported one
/// (`maybe_previous`): the first ID of that millisecond on the import worker (times before the
/// epoch take its first millisecond), or the next one after `maybe_previous` when that is not
/// smaller. IDs stay on [`IMPORT_WORKER`]; `None` only beyond the 41-bit time range.
pub fn next_import_id(maybe_previous: Option<MessageId>, sent_at_ms: i64) -> Option<MessageId> {
  let at = import_snowflake(sent_at_ms.saturating_sub(SNOWFLAKE_EPOCH_MS).max(0), 0)?;
  let next = match maybe_previous {
    Some(previous) if previous.snowflake() >= at => {
      let previous = previous.snowflake();
      let millis = previous.unix_millis() - SNOWFLAKE_EPOCH_MS;
      let sequence = previous.sequence();
      if sequence < MAX_SEQUENCE {
        import_snowflake(millis, sequence + 1)?
      } else {
        import_snowflake(millis + 1, 0)?
      }
    },
    _ => at,
  };
  Some(MessageId::from_snowflake(next))
}

const MAX_SEQUENCE: u16 = (1 << SNOWFLAKE_SEQUENCE_BITS) - 1;
const MAX_MILLIS: i64 = (1 << 41) - 1;

fn import_snowflake(millis_since_epoch: i64, sequence: u16) -> Option<Snowflake> {
  if !(0..=MAX_MILLIS).contains(&millis_since_epoch) {
    return None;
  }
  let raw = (millis_since_epoch << (SNOWFLAKE_SEQUENCE_BITS + SNOWFLAKE_WORKER_BITS))
    | (i64::from(IMPORT_WORKER) << SNOWFLAKE_SEQUENCE_BITS)
    | i64::from(sequence);
  Snowflake::new(raw).ok()
}

/// `POST /api/v1/admin/import/users`: pre-provisions accounts from the identity provider by user
/// name, so they can be made members before their first sign-in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProvisionUsersRequest {
  pub usernames: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProvisionUsersResponse {
  /// One entry per requested name, in the request's order.
  pub users: Vec<ProvisionedUser>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProvisionedUser {
  pub username: String,
  #[serde(rename = "user_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_user_id: Option<UserId>,
  pub status: ProvisionStatus,
}

crate::wire_enum::wire_enum! {
  /// What became of one name of [`ProvisionUsersRequest`].
  pub enum ProvisionStatus {
    /// The account existed already (it signed in, or was provisioned before).
    Existing => "existing",
    /// The account was created from the identity provider.
    Created => "created",
    /// The identity provider has no such user, or it is disabled or has no TeamCraft role.
    NotFound => "not_found",
    /// The account was deactivated or deleted here.
    Unavailable => "unavailable",
  }
}

/// `POST /api/v1/admin/import/dms`: the DM of two people, or somebody's notes (one), with the import
/// account as a member that is no participant. Answers with the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportDmRequest {
  /// One or two users, not the importer.
  pub participants: Vec<UserId>,
}

/// `POST /api/v1/admin/import/conversations/{id}/messages`: encrypted messages in their original
/// order, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportMessagesRequest {
  pub device_id: DeviceId,
  pub messages: Vec<ImportedMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportedMessage {
  pub client_message_id: ClientMessageId,
  pub content_key_id: Base64Bytes,
  pub ciphertext: Base64Bytes,
  /// An imported message of the same conversation (sent before) whose thread this one answers.
  #[serde(rename = "thread_id", default, skip_serializing_if = "Option::is_none")]
  pub maybe_thread_id: Option<ThreadId>,
  /// The original time (Unix milliseconds); it decides the ID's place.
  pub sent_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportMessagesResponse {
  /// The stored IDs, in the request's order; a client message ID imported before gives its ID.
  pub messages: Vec<ImportedMessageId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportedMessageId {
  pub client_message_id: ClientMessageId,
  pub id: MessageId,
}

/// `PUT /api/v1/admin/import/conversations/{id}/members/{user}`: a member's state from the old
/// system. Absent fields stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ImportMemberStateRequest {
  /// Read up to here; never moves an existing read position back.
  #[serde(rename = "read_through", default, skip_serializing_if = "Option::is_none")]
  pub maybe_read_through: Option<MessageId>,
  /// Only ever sets the favourite; `false` leaves it.
  #[serde(default)]
  pub is_favorite: bool,
  /// Mutes the conversation (notification level `none`); `false` leaves it.
  #[serde(default)]
  pub is_muted: bool,
}

#[cfg(test)]
mod tests {
  use super::*;

  mod import_ids {
    use super::*;

    #[test]
    fn an_id_carries_its_time_on_the_import_worker() {
      let at = SNOWFLAKE_EPOCH_MS + 5_000;
      let id = next_import_id(None, at).unwrap().snowflake();
      assert_eq!(id.unix_millis(), at);
      assert_eq!(id.worker(), IMPORT_WORKER);
      assert_eq!(id.sequence(), 0);
    }

    #[test]
    fn times_before_the_epoch_take_its_first_millisecond_in_order() {
      let first = next_import_id(None, 1_600_000_000_000).unwrap();
      assert_eq!(first.snowflake().unix_millis(), SNOWFLAKE_EPOCH_MS);
      let second = next_import_id(Some(first), 1_500_000_000_000).unwrap();
      assert!(second > first);
      assert_eq!(second.snowflake().sequence(), 1);
      assert_eq!(second.snowflake().worker(), IMPORT_WORKER);
    }

    #[test]
    fn a_full_millisecond_moves_to_the_next() {
      let full = MessageId::from_snowflake(import_snowflake(10, MAX_SEQUENCE).unwrap());
      let next = next_import_id(Some(full), SNOWFLAKE_EPOCH_MS)
        .unwrap()
        .snowflake();
      assert_eq!(next.unix_millis(), SNOWFLAKE_EPOCH_MS + 11);
      assert_eq!(next.sequence(), 0);
    }

    #[test]
    fn a_later_time_than_the_previous_id_takes_its_own_millisecond() {
      let previous = next_import_id(None, SNOWFLAKE_EPOCH_MS + 10).unwrap();
      let later = next_import_id(Some(previous), SNOWFLAKE_EPOCH_MS + 20).unwrap();
      assert_eq!(later.snowflake().unix_millis(), SNOWFLAKE_EPOCH_MS + 20);
    }

    #[test]
    fn beyond_the_time_range_there_is_none() {
      assert!(next_import_id(None, i64::MAX).is_none());
    }
  }

  #[test]
  fn the_policy_allows_only_its_importer_before_the_date() {
    let importer = "7".parse::<UserId>().unwrap();
    let policy = ImportPolicy {
      maybe_importer: Some(importer),
      maybe_joins_until_ms: Some(1_000),
    };
    assert!(policy.allows(importer, 999));
    assert!(!policy.allows(importer, 1_000));
    assert!(!policy.allows("8".parse::<UserId>().unwrap(), 999));
    assert!(policy.is_importer(importer));
    let ended = ImportPolicy {
      maybe_joins_until_ms: None,
      ..policy
    };
    assert!(!ended.allows(importer, 0));
    assert!(ended.is_importer(importer));
  }

  #[test]
  fn the_policy_is_off_by_default_and_round_trips() {
    assert_eq!(
      serde_json::from_str::<ImportPolicy>("{}").unwrap(),
      ImportPolicy::default()
    );
    let policy = ImportPolicy {
      maybe_importer: Some("7".parse::<UserId>().unwrap()),
      maybe_joins_until_ms: Some(42),
    };
    let json = serde_json::to_string(&policy).unwrap();
    assert_eq!(json, r#"{"importer":"7","joins_until_ms":42}"#);
    assert_eq!(serde_json::from_str::<ImportPolicy>(&json).unwrap(), policy);
  }

  #[test]
  fn provision_status_is_forward_compatible() {
    assert_eq!(
      serde_json::to_string(&ProvisionStatus::NotFound).unwrap(),
      r#""not_found""#
    );
    assert!(matches!(
      serde_json::from_str::<ProvisionStatus>(r#""later""#).unwrap(),
      ProvisionStatus::Unknown(_)
    ));
  }
}
