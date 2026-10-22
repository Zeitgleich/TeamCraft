//! Business logic behind the handlers: permission checks, validation, database calls, events.

pub mod access;
pub mod accounts;
pub mod attachments;
pub mod conversations;
pub mod convert;
pub mod devices;
pub mod directory;
pub mod identity;
pub mod invites;
pub mod key_bag;
pub mod key_packages;
pub mod members;
pub mod messages;
pub mod mls;
pub mod presence;
pub mod spaces;

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
