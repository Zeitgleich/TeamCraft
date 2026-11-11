//! Business logic behind the handlers: permission checks, validation, database calls, events.

pub mod access;
pub mod accounts;
pub mod admin;
pub mod admin_join_requests;
pub mod announcements;
pub mod app_links;
pub mod attachments;
pub mod audit;
pub mod backgrounds;
pub mod bots;
pub mod brand_mail;
pub mod brands;
pub mod calendar_settings;
pub mod call_history;
pub mod calls;
pub mod channel_images;
pub mod channel_roles;
pub mod conversations;
pub mod convert;
pub mod crash_reports;
pub mod custom_emoji;
pub mod delivery;
pub mod device_links;
pub mod device_lists;
pub mod devices;
pub mod directory;
pub mod email_digest;
pub mod group_mappings;
pub mod guests;
pub mod holiday_regions;
pub mod identity;
pub mod import;
pub mod inactivity;
pub mod invites;
pub mod key_bag;
pub mod key_packages;
pub mod leavers;
pub mod local_accounts;
pub mod mail_settings;
pub mod meeting_admitters;
pub mod meeting_controls;
pub mod meeting_guests;
pub mod meeting_notices;
pub mod meeting_occurrences;
pub mod meetings;
pub mod members;
pub mod messages;
pub mod mls;
pub mod notetaker;
pub mod notifications;
pub mod oidc;
pub mod oidc_brake;
pub mod oidc_status;
pub mod org_backgrounds;
pub mod org_recovery;
pub mod permission_scheme;
pub mod personal_data;
pub mod presence;
pub mod profiles;
pub mod provider_events;
pub mod push_diagnostics;
pub mod retention;
pub mod role_evidence;
pub mod runtime_settings;
pub mod scheduled_messages;
pub mod scim;
pub mod session;
pub mod shares;
pub mod space_images;
pub mod spaces;
pub mod storage_cleanup;
pub mod sync;
pub mod system_events;
pub mod threads;
pub mod urgent;
pub mod user_groups;
pub mod webhooks;
pub mod workspace;

use teamcraft_proto::channel_name::{checked_channel_name, keeps_channel_name_rules};

use crate::error::ApiError;

/// Trims a required human-readable name and checks its length.
pub fn validate_name(value: &str, what: &str, max_chars: usize) -> Result<String, ApiError> {
  let trimmed = value.trim();
  if trimmed.is_empty() {
    return Err(ApiError::InvalidRequest(format!("{what} must not be empty")));
  }
  validate_text(trimmed, what, max_chars)?;
  if trimmed.chars().any(char::is_control) {
    return Err(ApiError::InvalidRequest(format!(
      "{what} must not contain control characters"
    )));
  }
  Ok(trimmed.to_owned())
}

/// Checks a channel's name and keeps it **byte for byte** (e2ee.md §4.6.5): members hash the name
/// they are listed against the anchor the founder signed, so trimming or normalizing it here would
/// make it stale for deletions. A name that breaks the rules every client keeps (trimmed, NFC, no
/// bidi, control or invisible characters) is refused, never repaired.
pub fn validate_channel_name(value: &str, max_chars: usize) -> Result<String, ApiError> {
  let what = "channel name";
  if value.trim().is_empty() {
    return Err(ApiError::InvalidRequest(format!("{what} must not be empty")));
  }
  validate_text(value, what, max_chars)?;
  if !keeps_channel_name_rules(value) {
    let reason = match checked_channel_name(value) {
      Err(error) => error.to_string(),
      // The checked form differs: surrounding whitespace, or not in NFC.
      Ok(_) => "the name isn't trimmed and in NFC".to_owned(),
    };
    return Err(ApiError::InvalidChannelName(reason));
  }
  Ok(value.to_owned())
}

/// Checks the length of optional free text (an empty string is allowed: it clears the field).
pub fn validate_text(value: &str, what: &str, max_chars: usize) -> Result<(), ApiError> {
  if value.chars().count() > max_chars {
    return Err(ApiError::InvalidRequest(format!(
      "{what} is longer than {max_chars} characters"
    )));
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  /// e2ee.md §4.6.5: a channel's name is stored exactly as sent, unlike other names.
  #[test]
  fn a_channel_name_is_kept_byte_for_byte() {
    assert_eq!(
      validate_channel_name("Caf\u{00E9} 2026", 80).unwrap(),
      "Caf\u{00E9} 2026"
    );
    assert!(validate_channel_name("  ", 80).is_err());
    assert!(validate_channel_name(&"x".repeat(81), 80).is_err());
    // Refused, not repaired: the stored name stays what the founder signed.
    for name in [
      " general",
      "Cafe\u{0301}",
      "a\nb",
      "gen\u{200B}eral",
      "abc\u{202E}gpj",
    ] {
      assert!(
        matches!(
          validate_channel_name(name, 80),
          Err(ApiError::InvalidChannelName(_))
        ),
        "{name:?}"
      );
    }
  }

  #[test]
  fn names_are_trimmed_and_validated() {
    assert_eq!(validate_name("  general ", "name", 10).unwrap(), "general");
    assert!(validate_name("   ", "name", 10).is_err());
    assert!(validate_name("abcdefghijk", "name", 10).is_err());
    assert!(validate_name("a\nb", "name", 10).is_err());
  }

  #[test]
  fn text_may_be_empty_but_not_too_long() {
    assert!(validate_text("", "topic", 5).is_ok());
    assert!(validate_text("123456", "topic", 5).is_err());
    // Counted in characters, not bytes.
    assert!(validate_text("äöüäö", "topic", 5).is_ok());
  }
}
