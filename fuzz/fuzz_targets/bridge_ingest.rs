//! What tools send to the bridge's endpoints: Slack/Mattermost webhook bodies (JSON and the form
//! variant), plain-text and NDJSON log lines, Slack markup. None of it may panic, and batching
//! whatever came out must produce well-formed code blocks within the size limits.
#![no_main]

use std::time::{Duration, Instant};

use libfuzzer_sys::fuzz_target;
use teamcraft_bridge::batch::{BatchLimits, Batcher, Mentions};
use teamcraft_bridge::render::{LogRules, parse_log_lines, parse_webhook, slack_markup};
use teamcraft_proto::{ConversationId, Id, Snowflake};

const CONTENT_TYPES: [&str; 5] = [
  "application/json",
  "application/x-www-form-urlencoded",
  "text/plain",
  "application/x-ndjson",
  "",
];

fuzz_target!(|data: &[u8]| {
  let Some((&selector, body)) = data.split_first() else {
    return;
  };
  let content_type = CONTENT_TYPES[usize::from(selector) % CONTENT_TYPES.len()];
  let _ = parse_webhook(content_type, body, selector % 2 == 0);
  let _ = slack_markup(&String::from_utf8_lossy(body));

  let rules = LogRules {
    alert_levels: vec!["error".into(), "fatal".into()],
  };
  let lines = parse_log_lines(content_type, body, &rules);
  let limits = BatchLimits {
    interval: Duration::from_secs(1),
    max_bytes: 256,
    max_line_bytes: 64,
  };
  let mut batcher = Batcher::new(limits);
  let conversation = ConversationId::from_snowflake(Snowflake::new(1).unwrap());
  let now = Instant::now();
  let mut batches = Vec::new();
  for line in lines {
    batches.extend(batcher.push(conversation, line, &Mentions::default(), now));
  }
  batches.extend(batcher.take_all());
  for batch in batches {
    // A fence inside a line must not end the block early: exactly the opening and closing fence.
    assert_eq!(batch.text.matches("```").count(), 2, "{:?}", batch.text);
    assert!(batch.line_count >= 1);
    // 256 bytes of lines plus the fences; one line may overshoot by at most max_line_bytes.
    assert!(batch.text.len() <= 256 + 64 + 16, "{} bytes", batch.text.len());
  }
});
