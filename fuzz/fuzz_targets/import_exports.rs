//! Exports of other chat systems that `teamcraft-import convert` reads (docs/import-slack.md,
//! docs/import-mattermost.md): a Slack day file, a Mattermost bulk export line, Slack timestamps
//! and both systems' message texts. Never a panic; the texts' conversion keeps code as it was.
#![no_main]

use std::collections::HashMap;

use libfuzzer_sys::fuzz_target;
use teamcraft_import::sources::text::{SlackNames, mattermost_text, slack_text};
use teamcraft_import::sources::{mattermost, slack};

fuzz_target!(|data: &[u8]| {
  if let Ok(messages) = slack::parse_day(data) {
    for message in &messages {
      if let Some(ts) = &message.ts {
        let _ = slack::parse_ts(ts);
      }
    }
  }
  let _ = mattermost::parse_line(data);
  let Ok(text) = std::str::from_utf8(data) else {
    return;
  };
  let _ = slack::parse_ts(text);
  let users = HashMap::from([("U1".to_owned(), 11), ("anna".to_owned(), 7)]);
  let channels = HashMap::from([("C1".to_owned(), "general".to_owned())]);
  let names = SlackNames {
    users: &users,
    channels: &channels,
  };
  let converted = slack_text(text, &names);
  if !text.contains(['`', '<', '&', '*', '~']) {
    assert_eq!(converted, text, "plain text stays as it is");
  }
  let converted = mattermost_text(text, &users);
  if !text.contains('@') {
    assert_eq!(converted, text, "a text without mentions stays as it is");
  }
});
