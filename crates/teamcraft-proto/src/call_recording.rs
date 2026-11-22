//! Call recording (issue #216, e2ee.md §8.3, admin.md §3.13): who may record a call, as a group of
//! the owner-signed workspace policy. A participant's own app records (the server never has the
//! media keys); every other app shows the signed notice of it and checks the recorder against this
//! policy. Off by default.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::Role;
use crate::wire_enum::wire_enum;

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
  }
}

/// The part of a person that decides whether they may record: their place in the verified roster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecorderRole {
  Guest,
  Member,
  /// An admin or owner of the verified roster.
  Admin,
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
    }
  }
}

impl CallRecordingPolicy {
  /// Whether someone of `role` may record. A policy from a newer app (`Unknown`) allows nobody.
  pub fn allows(&self, role: RecorderRole) -> bool {
    match (&self.recorders, role) {
      (_, RecorderRole::Guest) => false,
      (CallRecorders::Members, _) => true,
      (CallRecorders::Admins, RecorderRole::Admin) => true,
      (CallRecorders::Admins, RecorderRole::Member) | (CallRecorders::Off | CallRecorders::Unknown(_), _) => {
        false
      },
    }
  }

  /// Whether anybody may record: the call screen offers the button only then.
  pub fn is_enabled(&self) -> bool {
    matches!(self.recorders, CallRecorders::Admins | CallRecorders::Members)
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

  /// Whether owners' apps may sign it: the length in range and no unknown values.
  pub fn is_valid(&self) -> bool {
    (1..=MAX_RECORDING_MINUTES).contains(&self.max_minutes)
      && !matches!(self.recorders, CallRecorders::Unknown(_))
      && !matches!(self.local_files, RecordingFiles::Unknown(_))
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
      let cases = [
        (CallRecorders::Off, [false, false, false]),
        (CallRecorders::Admins, [false, false, true]),
        (CallRecorders::Members, [false, true, true]),
      ];
      for (recorders, expected) in cases {
        let policy = CallRecordingPolicy {
          recorders: recorders.clone(),
          ..CallRecordingPolicy::default()
        };
        let allowed =
          [RecorderRole::Guest, RecorderRole::Member, RecorderRole::Admin].map(|role| policy.allows(role));
        assert_eq!(allowed, expected, "{recorders:?}");
        assert_eq!(policy.is_enabled(), recorders != CallRecorders::Off);
      }
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
}
