//! Call recording (issue #216, e2ee.md §8.3, admin.md §3.13): who may record a call, as a group of
//! the owner-signed workspace policy. A participant's own app records (the server never has the
//! media keys); every other app shows the signed notice of it and checks the recorder against this
//! policy. Off by default.
//!
//! The policy may also name a **recorder bot** (#2273): a bot account with a device of its own that
//! joins the calls of the conversations it is a member of and records them unattended, with the same
//! signed notice as any recorder. In `recorder_bot_only` mode it is the only one that records.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::wire_enum::wire_enum;
use crate::{Role, UserId};

/// Default of [`CallRecordingPolicy::max_minutes`].
pub const DEFAULT_RECORDING_MINUTES: u16 = 120;
/// Highest [`CallRecordingPolicy::max_minutes`] (eight hours).
pub const MAX_RECORDING_MINUTES: u16 = 480;
/// Largest signed recording notice the SFU keeps and forwards (`set_recording`); a notice is about
/// 100 bytes.
pub const MAX_RECORDING_NOTICE_BYTES: usize = 256;

/// The `call_recording` group of the signed workspace policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(default)]
pub struct CallRecordingPolicy {
  /// Who may record.
  pub recorders: CallRecorders,
  /// The longest recording: 1 to [`MAX_RECORDING_MINUTES`]; apps clamp a value outside.
  pub max_minutes: u16,
  /// Whether the recorder may save a recording as a file on their device, unencrypted (#2116).
  /// Posting it into the call's conversation is always offered.
  pub local_files: RecordingFiles,
  /// The recorder bot (#2273): the bot account that records calls unattended. Only this account,
  /// among all bot accounts, may record, and only while recording is on; `recorder_bot_only` needs
  /// it. Signed like the rest, so a server can't swap in another account.
  #[serde(rename = "bot", default, skip_serializing_if = "Option::is_none")]
  pub maybe_bot: Option<UserId>,
}

wire_enum! {
  /// Whether a recording may be saved as a file on the recorder's device (#2116).
  pub enum RecordingFiles {
    /// "Save as file…" is offered, with a warning that the file is then unencrypted: the default.
    Allowed => "allowed",
    /// Only posting into the call's conversation (an encrypted attachment) and deleting.
    Forbidden => "forbidden",
  }
}

wire_enum! {
  /// Who may record calls. Guests never may.
  pub enum CallRecorders {
    /// Nobody: the default.
    Off => "off",
    /// Owners and admins of the verified roster.
    Admins => "admins",
    /// Everybody who isn't a guest.
    Members => "members",
    /// Only the recorder bot ([`CallRecordingPolicy::maybe_bot`]): the compliance setup, where
    /// calls are recorded centrally and people's own record buttons say so (#2273).
    RecorderBotOnly => "recorder_bot_only",
  }
}

/// The part of a person that decides whether they may record: their place in the verified roster,
/// or being the policy's recorder bot. Built with [`CallRecordingPolicy::role_of`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderRole {
  /// A guest, or a bot account other than the recorder bot.
  Guest,
  Member,
  /// An admin or owner of the verified roster.
  Admin,
  /// The bot account the signed policy names as its recorder.
  RecorderBot,
}

impl RecorderRole {
  /// From a workspace role: owners count as admins, and a role from a newer app as a guest.
  pub fn of(role: &Role) -> Self {
    match role {
      Role::Owner | Role::Admin => Self::Admin,
      Role::Member => Self::Member,
      Role::Guest | Role::Unknown(_) => Self::Guest,
    }
  }
}

impl Default for CallRecordingPolicy {
  fn default() -> Self {
    Self {
      recorders: CallRecorders::Off,
      max_minutes: DEFAULT_RECORDING_MINUTES,
      local_files: RecordingFiles::Allowed,
      maybe_bot: None,
    }
  }
}

impl CallRecordingPolicy {
  /// How `user` stands under this policy. The signed bot comes first: the server's word that the
  /// account is a bot (or a guest) doesn't take its role away, and makes nobody else the recorder.
  /// Every other guest or bot account is a guest here; `is_admin` comes from the verified roster.
  pub fn role_of(&self, user: UserId, is_guest_or_bot: bool, is_admin: bool) -> RecorderRole {
    match (self.maybe_bot == Some(user), is_guest_or_bot, is_admin) {
      (true, _, _) => RecorderRole::RecorderBot,
      (false, true, _) => RecorderRole::Guest,
      (false, false, true) => RecorderRole::Admin,
      (false, false, false) => RecorderRole::Member,
    }
  }

  /// Whether someone of `role` may record. A policy from a newer app (`Unknown`) allows nobody.
  /// The recorder bot records whenever recording is on: owners who name it want it to; in
  /// `recorder_bot_only` nobody else does.
  pub fn allows(&self, role: RecorderRole) -> bool {
    match (&self.recorders, role) {
      (_, RecorderRole::Guest) | (CallRecorders::Off | CallRecorders::Unknown(_), _) => false,
      (
        CallRecorders::Admins | CallRecorders::Members | CallRecorders::RecorderBotOnly,
        RecorderRole::RecorderBot,
      ) => self.maybe_bot.is_some(),
      (CallRecorders::Members, RecorderRole::Member | RecorderRole::Admin)
      | (CallRecorders::Admins, RecorderRole::Admin) => true,
      (CallRecorders::Admins, RecorderRole::Member)
      | (CallRecorders::RecorderBotOnly, RecorderRole::Member | RecorderRole::Admin) => false,
    }
  }

  /// Whether anybody may record. Under `recorder_bot_only` the call screen shows the record button
  /// disabled, with why ([`Self::is_recorder_bot_only`]).
  pub fn is_enabled(&self) -> bool {
    match self.recorders {
      CallRecorders::Admins | CallRecorders::Members => true,
      CallRecorders::RecorderBotOnly => self.maybe_bot.is_some(),
      CallRecorders::Off | CallRecorders::Unknown(_) => false,
    }
  }

  /// Only the recorder bot records: people's record buttons say so instead of recording.
  pub fn is_recorder_bot_only(&self) -> bool {
    matches!(self.recorders, CallRecorders::RecorderBotOnly)
  }

  /// The recorder bot, while recording is on: the account clients show as the recorder.
  pub fn active_bot(&self) -> Option<UserId> {
    self.maybe_bot.filter(|_| self.is_enabled())
  }

  /// The longest recording, in seconds, with the stored value clamped to the allowed range.
  pub fn max_seconds(&self) -> u32 {
    u32::from(self.max_minutes.clamp(1, MAX_RECORDING_MINUTES)) * 60
  }

  /// Whether a finished recording may be saved as a file on the device. A value from a newer app
  /// (`Unknown`) allows it nowhere.
  pub fn allows_local_files(&self) -> bool {
    matches!(self.local_files, RecordingFiles::Allowed)
  }

  /// Whether owners' apps may sign it: the length in range, no unknown values, and the bot named
  /// where only the bot records.
  pub fn is_valid(&self) -> bool {
    (1..=MAX_RECORDING_MINUTES).contains(&self.max_minutes)
      && !matches!(self.recorders, CallRecorders::Unknown(_))
      && !matches!(self.local_files, RecordingFiles::Unknown(_))
      && !(self.is_recorder_bot_only() && self.maybe_bot.is_none())
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod wire {
    use super::*;

    #[test]
    fn the_default_is_off_for_two_hours() {
      let policy = CallRecordingPolicy::default();
      assert_eq!(
        serde_json::to_string(&policy).unwrap(),
        r#"{"recorders":"off","max_minutes":120,"local_files":"allowed"}"#
      );
      assert_eq!(serde_json::from_str::<CallRecordingPolicy>("{}").unwrap(), policy);
    }

    #[test]
    fn a_policy_from_before_local_files_allows_them() {
      let policy: CallRecordingPolicy =
        serde_json::from_str(r#"{"recorders":"members","max_minutes":60}"#).unwrap();
      assert!(policy.allows_local_files());
      assert!(policy.is_valid());
    }

    #[test]
    fn local_files_can_be_forbidden_and_a_newer_value_forbids_them() {
      let policy: CallRecordingPolicy = serde_json::from_str(r#"{"local_files":"forbidden"}"#).unwrap();
      assert!(!policy.allows_local_files());
      assert!(policy.is_valid());
      let policy: CallRecordingPolicy = serde_json::from_str(r#"{"local_files":"encrypted_only"}"#).unwrap();
      assert!(!policy.allows_local_files());
      assert!(!policy.is_valid());
    }

    #[test]
    fn the_recorder_bot_is_signed_with_its_mode() {
      let policy = CallRecordingPolicy {
        recorders: CallRecorders::RecorderBotOnly,
        maybe_bot: Some(user(9)),
        ..CallRecordingPolicy::default()
      };
      let json = serde_json::to_string(&policy).unwrap();
      assert_eq!(
        json,
        r#"{"recorders":"recorder_bot_only","max_minutes":120,"local_files":"allowed","bot":"9"}"#
      );
      assert_eq!(
        serde_json::from_str::<CallRecordingPolicy>(&json).unwrap(),
        policy
      );
    }

    #[test]
    fn an_owner_app_from_before_the_bot_re_signs_without_it_and_nobody_records() {
      // An older app keeps the unknown mode as `Unknown` and drops the bot: allows nobody.
      let policy: CallRecordingPolicy = serde_json::from_str(r#"{"recorders":"recorder_bot_only"}"#).unwrap();
      assert!(policy.is_recorder_bot_only());
      assert!(!policy.is_enabled());
      assert!(!policy.allows(RecorderRole::RecorderBot));
      assert!(!policy.allows(RecorderRole::Admin));
    }

    #[test]
    fn a_newer_value_reads_as_unknown_and_allows_nobody() {
      let policy: CallRecordingPolicy = serde_json::from_str(r#"{"recorders":"hosts"}"#).unwrap();
      assert_eq!(policy.recorders, CallRecorders::Unknown("hosts".into()));
      assert!(!policy.is_enabled());
      assert!(!policy.allows(RecorderRole::Admin));
      assert!(!policy.is_valid());
    }
  }

  mod allows {
    use super::*;

    #[test]
    fn every_mode_against_every_role() {
      // Guest, member, admin, recorder bot.
      let cases = [
        (CallRecorders::Off, [false, false, false, false]),
        (CallRecorders::Admins, [false, false, true, true]),
        (CallRecorders::Members, [false, true, true, true]),
        (CallRecorders::RecorderBotOnly, [false, false, false, true]),
        (
          CallRecorders::Unknown("hosts".into()),
          [false, false, false, false],
        ),
      ];
      for (recorders, expected) in cases {
        let policy = CallRecordingPolicy {
          recorders: recorders.clone(),
          maybe_bot: Some(user(9)),
          ..CallRecordingPolicy::default()
        };
        let allowed = [
          RecorderRole::Guest,
          RecorderRole::Member,
          RecorderRole::Admin,
          RecorderRole::RecorderBot,
        ]
        .map(|role| policy.allows(role));
        assert_eq!(allowed, expected, "{recorders:?}");
        assert_eq!(
          policy.is_enabled(),
          !matches!(recorders, CallRecorders::Off | CallRecorders::Unknown(_))
        );
      }
    }

    #[test]
    fn without_a_bot_nobody_is_the_recorder_bot() {
      for recorders in [
        CallRecorders::Admins,
        CallRecorders::Members,
        CallRecorders::RecorderBotOnly,
      ] {
        let policy = CallRecordingPolicy {
          recorders: recorders.clone(),
          ..CallRecordingPolicy::default()
        };
        assert!(!policy.allows(RecorderRole::RecorderBot), "{recorders:?}");
        assert_eq!(policy.active_bot(), None);
      }
      let bot_only = CallRecordingPolicy {
        recorders: CallRecorders::RecorderBotOnly,
        ..CallRecordingPolicy::default()
      };
      assert!(!bot_only.is_enabled());
      assert!(!bot_only.is_valid());
    }

    #[test]
    fn only_the_signed_bot_is_the_recorder_bot() {
      let policy = CallRecordingPolicy {
        recorders: CallRecorders::RecorderBotOnly,
        maybe_bot: Some(user(9)),
        ..CallRecordingPolicy::default()
      };
      assert!(policy.is_valid());
      assert_eq!(policy.active_bot(), Some(user(9)));
      // The signature names it: the server's word that it is a bot (or a guest) changes nothing.
      for is_guest_or_bot in [true, false] {
        assert_eq!(
          policy.role_of(user(9), is_guest_or_bot, false),
          RecorderRole::RecorderBot
        );
      }
      // Any other bot account is a guest, whatever the roster says.
      assert_eq!(policy.role_of(user(8), true, true), RecorderRole::Guest);
      assert!(!policy.allows(policy.role_of(user(8), true, true)));
      // People can't record in this mode, admins neither.
      assert_eq!(policy.role_of(user(1), false, true), RecorderRole::Admin);
      assert!(!policy.allows(policy.role_of(user(1), false, true)));
      assert!(policy.allows(policy.role_of(user(9), true, false)));
      let off = CallRecordingPolicy {
        maybe_bot: Some(user(9)),
        ..CallRecordingPolicy::default()
      };
      assert!(!off.allows(off.role_of(user(9), true, false)));
      assert_eq!(off.active_bot(), None);
    }

    #[test]
    fn roles_map_to_recorder_roles() {
      assert_eq!(RecorderRole::of(&Role::Owner), RecorderRole::Admin);
      assert_eq!(RecorderRole::of(&Role::Admin), RecorderRole::Admin);
      assert_eq!(RecorderRole::of(&Role::Member), RecorderRole::Member);
      assert_eq!(RecorderRole::of(&Role::Guest), RecorderRole::Guest);
      assert_eq!(
        RecorderRole::of(&Role::Unknown("intern".into())),
        RecorderRole::Guest
      );
    }

    #[test]
    fn the_length_is_clamped_and_checked() {
      let mut policy = CallRecordingPolicy::default();
      assert_eq!(policy.max_seconds(), 120 * 60);
      assert!(policy.is_valid());
      policy.max_minutes = 0;
      assert_eq!(policy.max_seconds(), 60);
      assert!(!policy.is_valid());
      policy.max_minutes = 10_000;
      assert_eq!(policy.max_seconds(), u32::from(MAX_RECORDING_MINUTES) * 60);
      assert!(!policy.is_valid());
    }
  }

  fn user(id: i64) -> UserId {
    use crate::Id;
    UserId::from_snowflake(crate::Snowflake::new(id).unwrap())
  }
}
