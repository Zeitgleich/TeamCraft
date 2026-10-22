//! Which messages count as mentioning the user, decided on the device: the server routes push by
//! explicit mentions only, while keywords and the first name need the decrypted text. Desktop
//! (`teamcraft-ui`'s notifier) and the mobile apps (`teamcraft-ffi`) share these rules.

/// Words that notify the whole conversation.
pub const CHANNEL_MENTIONS: [&str; 3] = ["channel", "here", "all"];

/// The user's mention preferences, with who the user is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MentionRules<'a> {
  pub username: &'a str,
  pub display_name: &'a str,
  pub first_name_is_mention: bool,
  pub channel_mentions_notify: bool,
  pub keywords: &'a [String],
}

/// `@username`, `@channel`/`@here`/`@all` (when enabled), the first name (when enabled) or a keyword.
pub fn is_mentioned(body: &str, rules: &MentionRules) -> bool {
  let lower = body.to_lowercase();
  let words = words(&lower);
  let username = format!("@{}", rules.username.to_lowercase());
  if words.contains(&username.as_str()) {
    return true;
  }
  if rules.channel_mentions_notify && has_channel_mention(&words) {
    return true;
  }
  let first_name = rules
    .display_name
    .split_whitespace()
    .next()
    .map(str::to_lowercase);
  if rules.first_name_is_mention && first_name.is_some_and(|name| words.contains(&name.as_str())) {
    return true;
  }
  rules
    .keywords
    .iter()
    .map(|keyword| keyword.trim().to_lowercase())
    .filter(|keyword| !keyword.is_empty())
    .any(|keyword| lower.contains(&keyword))
}

/// Whether sending `body` would notify a whole large conversation and should be confirmed first:
/// the text has `@channel`/`@here`/`@all` and the conversation has more than `confirm_over` members
/// (`0` never asks).
pub fn needs_channel_confirmation(body: &str, member_count: usize, confirm_over: u32) -> bool {
  if confirm_over == 0 || member_count <= confirm_over as usize {
    return false;
  }
  has_channel_mention(&words(&body.to_lowercase()))
}

fn words(lower: &str) -> Vec<&str> {
  lower
    .split(|c: char| !c.is_alphanumeric() && c != '@' && c != '_' && c != '-')
    .collect()
}

fn has_channel_mention(words: &[&str]) -> bool {
  CHANNEL_MENTIONS
    .iter()
    .any(|mention| words.contains(&format!("@{mention}").as_str()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn mentions_by_username_channel_first_name_and_keyword() {
    let keywords = vec!["launch".to_string(), "  ".to_string()];
    let rules = rules(&keywords);
    assert!(is_mentioned("hey @alice!", &rules));
    assert!(!is_mentioned("hey @alicejones", &rules));
    assert!(is_mentioned("@channel standup", &rules));
    assert!(is_mentioned("Alice, can you look?", &rules));
    assert!(is_mentioned("the Launch plan", &rules));
    assert!(!is_mentioned("nothing here", &rules));
  }

  #[test]
  fn disabled_rules_do_not_mention() {
    let rules = MentionRules {
      first_name_is_mention: false,
      channel_mentions_notify: false,
      ..rules(&[])
    };
    assert!(!is_mentioned("@here please", &rules));
    assert!(!is_mentioned("Alice, please", &rules));
    assert!(is_mentioned("@Alice please", &rules));
  }

  #[test]
  fn channel_confirmation_only_for_large_conversations() {
    assert!(needs_channel_confirmation("@channel standup", 25, 10));
    assert!(needs_channel_confirmation("hi @Here!", 11, 10));
    assert!(!needs_channel_confirmation("@channel standup", 10, 10));
    assert!(!needs_channel_confirmation("@channel standup", 25, 0));
    assert!(!needs_channel_confirmation("mail me@channel.example", 25, 10));
    assert!(!needs_channel_confirmation("no mention", 25, 10));
  }

  fn rules(keywords: &[String]) -> MentionRules<'_> {
    MentionRules {
      username: "alice",
      display_name: "Alice Example",
      first_name_is_mention: true,
      channel_mentions_notify: true,
      keywords,
    }
  }
}
