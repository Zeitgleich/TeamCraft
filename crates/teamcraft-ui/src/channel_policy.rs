//! Server-owned settings of one channel that its owners change: retention (PLAN.md §13). The server
//! checks the permission and the workspace limit; this module reads and writes the values and turns
//! the text fields of the dialog into a setting.

use leptos::prelude::*;
use teamcraft_proto::{ConversationId, RetentionSetting, RetentionStatusResponse, SetRetentionRequest};

use crate::rest::{RestClient, RestError};

/// Why the retention fields could not be turned into a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetentionInputError {
  /// Not a whole number.
  NotANumber,
  /// Zero would delete everything at once.
  NotPositive,
}

impl RetentionInputError {
  /// The i18n key of the message shown next to the fields.
  pub fn message_key(self) -> &'static str {
    match self {
      Self::NotANumber => "retention-error-number",
      Self::NotPositive => "retention-error-positive",
    }
  }
}

/// Reads and writes the retention of channels through the REST API. Provided as context once the
/// user is signed in.
#[derive(Clone, Copy)]
pub struct ChannelPolicyHub {
  client: StoredValue<RestClient, LocalStorage>,
}

impl ChannelPolicyHub {
  pub fn new(client: RestClient) -> Self {
    Self {
      client: StoredValue::new_local(client),
    }
  }

  /// The channel's own retention, the effective one and the workspace limit.
  pub async fn retention(&self, conversation: ConversationId) -> Result<RetentionStatusResponse, RestError> {
    let client = self.client.get_value();
    client
      .get(&format!("/api/v1/conversations/{conversation}/retention"))
      .await
  }

  /// Sets the channel's own retention; an empty setting makes it inherit again.
  pub async fn set_retention(
    &self,
    conversation: ConversationId,
    retention: RetentionSetting,
  ) -> Result<RetentionStatusResponse, RestError> {
    let client = self.client.get_value();
    client
      .put(
        &format!("/api/v1/conversations/{conversation}/retention"),
        &SetRetentionRequest { retention },
      )
      .await
  }
}

/// The two text fields as a setting; a blank field keeps that dimension unset.
pub fn parse_retention(days: &str, messages: &str) -> Result<RetentionSetting, RetentionInputError> {
  Ok(RetentionSetting {
    maybe_max_age_days: parse_positive(days)?,
    maybe_max_messages: parse_positive(messages)?.map(u64::from),
  })
}

/// What is wrong with one retention field, if anything, so each field shows its own message.
pub fn check_retention_field(text: &str) -> Option<RetentionInputError> {
  parse_positive(text).err()
}

fn parse_positive(text: &str) -> Result<Option<u32>, RetentionInputError> {
  let text = text.trim();
  if text.is_empty() {
    return Ok(None);
  }
  match text.parse::<u32>() {
    Ok(0) => Err(RetentionInputError::NotPositive),
    Ok(value) => Ok(Some(value)),
    Err(_) => Err(RetentionInputError::NotANumber),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn blank_fields_stay_unset_and_numbers_are_read() {
    assert_eq!(parse_retention("", "  ").unwrap(), RetentionSetting::default());
    let both = parse_retention(" 30 ", "500").unwrap();
    assert_eq!(
      (both.maybe_max_age_days, both.maybe_max_messages),
      (Some(30), Some(500))
    );
  }

  #[test]
  fn each_field_is_checked_on_its_own() {
    assert_eq!(check_retention_field(""), None);
    assert_eq!(check_retention_field("30"), None);
    assert_eq!(check_retention_field("0"), Some(RetentionInputError::NotPositive));
    assert_eq!(check_retention_field("x"), Some(RetentionInputError::NotANumber));
  }

  #[test]
  fn zero_and_text_are_rejected() {
    assert_eq!(parse_retention("0", ""), Err(RetentionInputError::NotPositive));
    assert_eq!(parse_retention("", "abc"), Err(RetentionInputError::NotANumber));
    assert_eq!(parse_retention("-3", ""), Err(RetentionInputError::NotANumber));
  }
}
