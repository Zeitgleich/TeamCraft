//! Which messages count as mentioning the user, decided on the device: the server routes push by
//! explicit mentions only, while keywords and the first name need the decrypted text. Desktop
//! (`teamcraft-ui`'s notifier) and the mobile apps (`teamcraft-ffi`) share these rules, and so do
//! the composer and the core's send path when they work out who a group mention reaches.

use std::collections::BTreeSet;

use crate::markdown::{self, Block, Inline};
use crate::model::{MentionableGroup, UserId};

pub use teamcraft_proto::{CHANNEL_MENTIONS, addresses_everyone};

/// The user's mention preferences, with who the user is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MentionRules<'a> {
  pub username: &'a str,
  pub display_name: &'a str,
  pub first_name_is_mention: bool,
  /// Whether `@channel` mentions the user here: the general choice, or the conversation's own
  /// (`ChannelMentions::counts`).
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
  if rules.channel_mentions_notify && addresses_everyone(body) {
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
  addresses_everyone(body)
}

fn words(lower: &str) -> Vec<&str> {
  lower
    .split(|c: char| !c.is_alphanumeric() && c != '@' && c != '_' && c != '-')
    .collect()
}

/// Handles recorded per message at most: a text naming more groups than this is not a mention,
/// and what a sender claims stays small to show (the handles come from the ciphertext).
pub const MAX_GROUP_MENTIONS_PER_MESSAGE: usize = 16;

/// What a text's group mentions reach, with the groups the sender may mention.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupMentionReach {
  /// The handles mentioned, once each, in the order the text names them.
  pub handles: Vec<String>,
  /// The people those mentions notify: members of the mentioned groups in the conversation.
  pub notified: Vec<UserId>,
  /// Members of the mentioned groups outside the conversation: never notified, named in a hint.
  pub outside_count: u32,
}

/// A valid group handle: as the server allows them (`user_groups::check_handle`).
pub fn is_group_handle(handle: &str) -> bool {
  (2..=32).contains(&handle.len())
    && handle.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
    && handle
      .chars()
      .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
}

/// Every `@word` of `body` that can be a group handle, lower-cased, once each. The message's own
/// markdown decides what is a mention: `@team` in code is an example, not a page.
pub fn mentioned_handles(body: &str) -> Vec<String> {
  let mut found = Vec::new();
  for block in markdown::parse(body) {
    match block {
      Block::Paragraph(lines) | Block::Quote(lines) => {
        lines.iter().for_each(|line| collect_handles(line, &mut found));
      },
      Block::Heading { content, .. } => collect_handles(&content, &mut found),
      Block::List { items, .. } => items.iter().for_each(|item| collect_handles(item, &mut found)),
      Block::Code { .. } => {},
    }
  }
  found
}

fn collect_handles(inlines: &[Inline], found: &mut Vec<String>) {
  for inline in inlines {
    match inline {
      Inline::Mention(name) => {
        let handle = name.to_lowercase();
        if is_group_handle(&handle) && !found.contains(&handle) {
          found.push(handle);
        }
      },
      Inline::Bold(inner) | Inline::Italic(inner) | Inline::Underline(inner) | Inline::Strike(inner) => {
        collect_handles(inner, found);
      },
      Inline::Text(_) | Inline::Code(_) | Inline::Link(_) | Inline::LabeledLink { .. } => {},
    }
  }
}

/// Who `body`'s group mentions reach among `groups` (what the sender may mention here). A handle of
/// no such group is plain text.
pub fn group_mention_reach(body: &str, groups: &[MentionableGroup]) -> GroupMentionReach {
  if !body.contains('@') {
    return GroupMentionReach::default();
  }
  let mut notified = BTreeSet::new();
  let mut reach = GroupMentionReach::default();
  for handle in mentioned_handles(body) {
    if reach.handles.len() == MAX_GROUP_MENTIONS_PER_MESSAGE {
      break;
    }
    let Some(group) = groups.iter().find(|group| group.handle == handle) else {
      continue;
    };
    reach.handles.push(handle);
    notified.extend(group.notified.iter().copied());
    reach.outside_count = reach.outside_count.saturating_add(group.outside_count);
  }
  reach.notified = notified.into_iter().collect();
  reach
}

/// Whether sending would notify more people through group mentions than `confirm_over` (the same
/// setting as for `@channel`; `0` never asks).
pub fn needs_group_confirmation(reach: &GroupMentionReach, confirm_over: u32) -> bool {
  confirm_over != 0 && reach.notified.len() > confirm_over as usize
}

/// The people a message names to the server: `direct` first (each is someone the sender picked),
/// then the members `reach` adds, at most `cap`. The cap keeps a group too large for the server
/// from having the whole message refused; the composer says so before sending.
pub fn expanded_mentions(direct: Vec<UserId>, reach: &GroupMentionReach, cap: usize) -> Vec<UserId> {
  let mut mentioned = direct;
  let already: BTreeSet<UserId> = mentioned.iter().copied().collect();
  mentioned.extend(
    reach
      .notified
      .iter()
      .copied()
      .filter(|user| !already.contains(user)),
  );
  mentioned.truncate(cap);
  mentioned.sort();
  mentioned.dedup();
  mentioned
}

#[cfg(test)]
mod tests {
  use super::*;

  use crate::model::Id;

  fn id(raw: i64) -> UserId {
    UserId::from_snowflake(crate::model::Snowflake::new(raw).unwrap())
  }

  fn group(handle: &str, notified: &[UserId], outside_count: u32) -> MentionableGroup {
    MentionableGroup {
      handle: handle.to_owned(),
      name: handle.to_owned(),
      notified: notified.to_vec(),
      outside_count,
    }
  }

  #[test]
  fn handles_are_read_from_the_markdown_not_from_code() {
    assert_eq!(
      mentioned_handles("hi @Accounting, and **@oncall**!"),
      ["accounting", "oncall"]
    );
    assert_eq!(
      mentioned_handles("`@accounting`\n```\n@oncall\n```"),
      Vec::<String>::new()
    );
    assert_eq!(mentioned_handles("- @team\n- @team"), ["team"]);
    assert_eq!(mentioned_handles("mail me@team.example"), Vec::<String>::new());
    assert!(mentioned_handles("@x").is_empty(), "shorter than any handle");
  }

  #[test]
  fn reach_counts_only_known_groups_and_each_person_once() {
    let (ann, bob, cid) = (id(1), id(2), id(3));
    let groups = [
      group("accounting", &[ann, bob], 2),
      group("oncall", &[bob, cid], 1),
    ];
    let reach = group_mention_reach("@accounting and @oncall and @unknown", &groups);
    assert_eq!(reach.handles, ["accounting", "oncall"]);
    assert_eq!(reach.notified, [ann, bob, cid]);
    assert_eq!(reach.outside_count, 3);
    assert_eq!(
      group_mention_reach("no mention", &groups),
      GroupMentionReach::default()
    );
    assert!(
      group_mention_reach("@accounting", &[]).handles.is_empty(),
      "no permission: no groups"
    );
  }

  #[test]
  fn group_confirmation_uses_the_channel_setting() {
    let people: Vec<UserId> = (1..=3).map(id).collect();
    let reach = group_mention_reach("@team", &[group("team", &people, 0)]);
    assert!(needs_group_confirmation(&reach, 2));
    assert!(!needs_group_confirmation(&reach, 3));
    assert!(!needs_group_confirmation(&reach, 0), "0 never asks");
  }

  #[test]
  fn expansion_is_capped_and_keeps_people_named_directly() {
    let (ann, bob, cid, dee) = (id(1), id(2), id(3), id(4));
    let reach = GroupMentionReach {
      notified: vec![ann, bob, dee],
      ..GroupMentionReach::default()
    };
    assert_eq!(
      expanded_mentions(vec![cid, ann], &reach, 10),
      [ann, bob, cid, dee]
    );
    let capped = expanded_mentions(vec![dee], &reach, 2);
    assert_eq!(capped, [ann, dee], "named directly stays in");
    assert_eq!(expanded_mentions(vec![], &GroupMentionReach::default(), 2), []);
  }

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
