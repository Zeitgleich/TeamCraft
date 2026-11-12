//! Turning what tools send into message text: Slack/Mattermost incoming-webhook payloads and
//! log streams (plain text or NDJSON).

use serde::Deserialize;
use serde_json::Value;
use teamcraft_proto::{find_level, parse_level_field};

use crate::batch::LogLine;

/// The Slack/Mattermost incoming-webhook format. Unknown fields are ignored; `channel`,
/// `icon_url`, `icon_emoji` and blocks have no meaning here (the source decides the conversation).
#[derive(Debug, Default, Deserialize)]
pub struct WebhookPayload {
  #[serde(default)]
  pub text: Option<String>,
  #[serde(default)]
  pub username: Option<String>,
  #[serde(default)]
  pub attachments: Vec<Attachment>,
  /// Extension: `true` makes the message an alert that mentions the source's people.
  #[serde(default)]
  pub mention: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct Attachment {
  #[serde(default)]
  pub fallback: Option<String>,
  #[serde(default)]
  pub color: Option<String>,
  #[serde(default)]
  pub pretext: Option<String>,
  #[serde(default)]
  pub author_name: Option<String>,
  #[serde(default)]
  pub title: Option<String>,
  #[serde(default)]
  pub title_link: Option<String>,
  #[serde(default)]
  pub text: Option<String>,
  #[serde(default)]
  pub fields: Vec<Field>,
  #[serde(default)]
  pub footer: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Field {
  #[serde(default)]
  pub title: Option<String>,
  #[serde(default)]
  pub value: Option<String>,
}

/// Why a webhook body is not usable.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PayloadError {
  #[error("the body is not valid JSON: {0}")]
  Json(String),
  #[error("a form body needs a `payload` field with the JSON")]
  MissingFormPayload,
  #[error("the message has no text")]
  Empty,
}

/// A rendered webhook message.
#[derive(Debug, PartialEq, Eq)]
pub struct WebhookMessage {
  pub text: String,
  /// The payload asked for a mention, or an attachment is `danger` and the source treats those as alerts.
  pub is_alert: bool,
}

/// Parses a request body: JSON, or `application/x-www-form-urlencoded` with the JSON in `payload`
/// (the original Slack form). A form-typed body that is itself a JSON object is read as JSON, as
/// Slack does: `curl -d '{...}'` sends the form type unless a header says otherwise.
pub fn parse_webhook(
  content_type: &str,
  body: &[u8],
  danger_is_alert: bool,
) -> Result<WebhookMessage, PayloadError> {
  let is_form = content_type.starts_with("application/x-www-form-urlencoded");
  let json = if is_form && !body.trim_ascii_start().starts_with(b"{") {
    url::form_urlencoded::parse(body)
      .find(|(key, _)| key == "payload")
      .map(|(_, value)| value.into_owned())
      .ok_or(PayloadError::MissingFormPayload)?
      .into_bytes()
  } else {
    body.to_vec()
  };
  let payload: WebhookPayload =
    serde_json::from_slice(&json).map_err(|error| PayloadError::Json(error.to_string()))?;
  render_webhook(&payload, danger_is_alert)
}

pub fn render_webhook(
  payload: &WebhookPayload,
  danger_is_alert: bool,
) -> Result<WebhookMessage, PayloadError> {
  let mut blocks: Vec<String> = Vec::new();
  if let Some(text) = non_empty(payload.text.as_deref()) {
    blocks.push(slack_markup(text));
  }
  let mut has_danger = false;
  for attachment in &payload.attachments {
    if attachment
      .color
      .as_deref()
      .is_some_and(|color| color.eq_ignore_ascii_case("danger"))
    {
      has_danger = true;
    }
    if let Some(text) = render_attachment(attachment) {
      blocks.push(text);
    }
  }
  if blocks.is_empty() {
    return Err(PayloadError::Empty);
  }
  let mut text = blocks.join("\n\n");
  if let Some(name) = non_empty(payload.username.as_deref()) {
    text = format!("**{}**\n{text}", name.replace('*', ""));
  }
  Ok(WebhookMessage {
    text,
    is_alert: payload.mention || (danger_is_alert && has_danger),
  })
}

fn render_attachment(attachment: &Attachment) -> Option<String> {
  let mut lines: Vec<String> = Vec::new();
  if let Some(pretext) = non_empty(attachment.pretext.as_deref()) {
    lines.push(slack_markup(pretext));
  }
  if let Some(title) = non_empty(attachment.title.as_deref()) {
    let title = slack_markup(title);
    lines.push(match non_empty(attachment.title_link.as_deref()) {
      Some(link) => format!("> **[{title}]({link})**"),
      None => format!("> **{title}**"),
    });
  }
  if let Some(author) = non_empty(attachment.author_name.as_deref()) {
    lines.push(format!("> {}", slack_markup(author)));
  }
  if let Some(text) = non_empty(attachment.text.as_deref()) {
    lines.extend(slack_markup(text).lines().map(|line| format!("> {line}")));
  }
  for field in &attachment.fields {
    match (
      non_empty(field.title.as_deref()),
      non_empty(field.value.as_deref()),
    ) {
      (Some(title), Some(value)) => {
        lines.push(format!("> **{}:** {}", slack_markup(title), slack_markup(value)))
      },
      (None, Some(value)) => lines.push(format!("> {}", slack_markup(value))),
      _ => {},
    }
  }
  if let Some(footer) = non_empty(attachment.footer.as_deref()) {
    lines.push(format!("> _{}_", slack_markup(footer)));
  }
  if lines.is_empty() {
    // Clients that can't show attachments get `fallback`; so do we if nothing else is there.
    return non_empty(attachment.fallback.as_deref()).map(slack_markup);
  }
  Some(lines.join("\n"))
}

/// Slack's mrkdwn to Markdown: `<url|label>` links, mentions, entities.
pub fn slack_markup(text: &str) -> String {
  let mut out = String::with_capacity(text.len());
  let mut rest = text;
  while let Some(open) = rest.find('<') {
    out.push_str(&rest[..open]);
    let after = &rest[open + 1..];
    let Some(close) = after.find('>') else {
      out.push('<');
      rest = after;
      continue;
    };
    let inside = &after[..close];
    out.push_str(&markup_token(inside));
    rest = &after[close + 1..];
  }
  out.push_str(rest);
  out
    .replace("&lt;", "<")
    .replace("&gt;", ">")
    .replace("&amp;", "&")
}

fn markup_token(inside: &str) -> String {
  if let Some(special) = inside.strip_prefix('!') {
    let name = special.split('|').next().unwrap_or(special);
    return format!("@{name}");
  }
  if let Some(user) = inside.strip_prefix('@') {
    return format!("@{}", user.split('|').next().unwrap_or(user));
  }
  if let Some(channel) = inside.strip_prefix('#') {
    return format!(
      "#{}",
      channel
        .split('|')
        .nth(1)
        .unwrap_or_else(|| channel.split('|').next().unwrap_or(channel))
    );
  }
  match inside.split_once('|') {
    Some((url, label)) if !label.is_empty() => format!("[{label}]({url})"),
    Some((url, _)) => url.to_owned(),
    None => inside.to_owned(),
  }
}

fn non_empty(text: Option<&str>) -> Option<&str> {
  text.map(str::trim).filter(|text| !text.is_empty())
}

/// What a source considers an alert line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRules {
  /// Lower-case level names that make a line an alert (`error`, `fatal`, ...).
  pub alert_levels: Vec<String>,
}

/// Splits a log body into lines. `ndjson` bodies are lines of JSON objects whose message and level
/// are lifted out; lines that are not JSON stay as they are. Plain bodies are one line per line.
pub fn parse_log_lines(content_type: &str, body: &[u8], rules: &LogRules) -> Vec<LogLine> {
  let text = String::from_utf8_lossy(body);
  let is_ndjson = content_type.starts_with("application/x-ndjson")
    || content_type.starts_with("application/ndjson")
    || content_type.starts_with("application/jsonl");
  text
    .lines()
    .map(str::trim_end)
    .filter(|line| !line.trim().is_empty())
    .map(|line| {
      let parsed = if is_ndjson { json_line(line) } else { None };
      let (text, level) = parsed.unwrap_or_else(|| (line.to_owned(), plain_level(line)));
      let is_alert = level.is_some_and(|level| rules.alert_levels.contains(&level));
      LogLine { text, is_alert }
    })
    .collect()
}

/// Fields of an NDJSON object that carry the message, the level and the time, in order of
/// preference (`MESSAGE` and `PRIORITY` are journald's `-o json`).
const MESSAGE_FIELDS: [&str; 5] = ["message", "msg", "text", "log", "MESSAGE"];
const LEVEL_FIELDS: [&str; 5] = ["level", "severity", "lvl", "levelname", "PRIORITY"];
const TIME_FIELDS: [&str; 4] = ["time", "ts", "timestamp", "@timestamp"];

/// `[LEVEL] time message` from an object with a message field; `None` if it isn't one. The level
/// comes first so the apps, which read it with the same detection (`teamcraft_proto::find_level`),
/// show it as the line's level and cut the prefix off. The level is a word or a number (syslog
/// priority `0..=7`, journald's `PRIORITY`; `10..=60` of bunyan and pino).
fn json_line(line: &str) -> Option<(String, Option<String>)> {
  let value: Value = serde_json::from_str(line).ok()?;
  let object = value.as_object()?;
  let field = |names: &[&str]| {
    names.iter().find_map(|name| object.get(*name)).and_then(|value| {
      value
        .as_str()
        .map(str::to_owned)
        .or_else(|| Some(value.to_string()))
    })
  };
  let message = field(&MESSAGE_FIELDS)?;
  let raw_level = field(&LEVEL_FIELDS);
  let time = field(&TIME_FIELDS);
  let parsed = raw_level.as_deref().and_then(parse_level_field);
  let mut text = String::new();
  match (parsed, &raw_level) {
    (Some((_, name)), _) => text.push_str(&format!("[{}] ", name.to_uppercase())),
    (None, Some(raw)) => text.push_str(&format!("[{}] ", raw.to_uppercase())),
    (None, None) => {},
  }
  if let Some(time) = time {
    text.push_str(&time);
    text.push(' ');
  }
  text.push_str(&message);
  Some((text, parsed.map(|(_, name)| name.to_owned())))
}

/// The canonical name of the level an unstructured line names near its start (see
/// [`teamcraft_proto::find_level`]): `ERROR something`, `2026-11-12T10:00:00Z WARN x`, `level=error`.
fn plain_level(line: &str) -> Option<String> {
  find_level(line).map(|found| found.name.to_owned())
}

#[cfg(test)]
mod tests {
  use super::*;

  mod webhooks {
    use super::*;

    fn parse(body: &str) -> Result<WebhookMessage, PayloadError> {
      parse_webhook("application/json", body.as_bytes(), true)
    }

    #[test]
    fn plain_text_and_username() {
      let message =
        parse(r##"{"text":"Deploy finished","username":"ci","icon_url":"http://x/y.png","channel":"#ops"}"##)
          .unwrap();
      assert_eq!(message.text, "**ci**\nDeploy finished");
      assert!(!message.is_alert);
    }

    #[test]
    fn slack_markup_becomes_markdown() {
      let message = parse(r#"{"text":"<https://ci.example.com/42|Build 42> failed for <@U1|bob> &amp; <!here> in <#C1|ops> <https://x.y>"}"#).unwrap();
      assert_eq!(
        message.text,
        "[Build 42](https://ci.example.com/42) failed for @U1 & @here in #ops https://x.y"
      );
    }

    #[test]
    fn attachments_render_as_a_quote_with_title_text_and_fields() {
      let message = parse(
        r##"{"attachments":[{"color":"warning","pretext":"Heads up","title":"Disk 91%","title_link":"https://mon.example.com/1",
            "text":"line one\nline two","fields":[{"title":"Host","value":"nas","short":true},{"value":"only value"}],"footer":"grafana"}]}"##,
      )
      .unwrap();
      assert_eq!(
        message.text,
        "Heads up\n> **[Disk 91%](https://mon.example.com/1)**\n> line one\n> line two\n> **Host:** nas\n> only value\n> _grafana_"
      );
    }

    #[test]
    fn danger_attachments_alert_only_when_the_source_wants_it_and_mention_always_does() {
      let danger = r#"{"attachments":[{"color":"danger","title":"Down"}]}"#;
      assert!(
        parse_webhook("application/json", danger.as_bytes(), true)
          .unwrap()
          .is_alert
      );
      assert!(
        !parse_webhook("application/json", danger.as_bytes(), false)
          .unwrap()
          .is_alert
      );
      assert!(
        parse_webhook("application/json", br#"{"text":"x","mention":true}"#, false)
          .unwrap()
          .is_alert
      );
    }

    #[test]
    fn the_fallback_is_used_when_an_attachment_has_nothing_else() {
      assert_eq!(
        parse(r#"{"attachments":[{"fallback":"Plain fallback"}]}"#)
          .unwrap()
          .text,
        "Plain fallback"
      );
    }

    #[test]
    fn the_original_slack_form_is_understood() {
      let body = "payload=%7B%22text%22%3A%22hi%20there%22%7D";
      let message = parse_webhook("application/x-www-form-urlencoded", body.as_bytes(), false).unwrap();
      assert_eq!(message.text, "hi there");
      assert_eq!(
        parse_webhook("application/x-www-form-urlencoded", b"other=1", false),
        Err(PayloadError::MissingFormPayload)
      );
    }

    #[test]
    fn json_sent_with_the_form_type_is_read_as_json() {
      let body = br#" {"text":"Deploy finished"}"#;
      let message = parse_webhook("application/x-www-form-urlencoded", body, false).unwrap();
      assert_eq!(message.text, "Deploy finished");
    }

    #[test]
    fn empty_and_malformed_bodies_are_rejected() {
      assert_eq!(parse("{}"), Err(PayloadError::Empty));
      assert_eq!(parse(r#"{"text":"   "}"#), Err(PayloadError::Empty));
      assert!(matches!(parse("not json"), Err(PayloadError::Json(_))));
    }
  }

  mod logs {
    use super::*;

    fn rules() -> LogRules {
      LogRules {
        alert_levels: vec!["error".into(), "fatal".into()],
      }
    }

    #[test]
    fn plain_text_is_split_into_lines_and_alert_levels_are_recognized() {
      let lines = parse_log_lines(
        "text/plain",
        b"INFO started\n\n[error] it broke\r\nFATAL: out of memory\nwarning only\n",
        &rules(),
      );
      let summary: Vec<_> = lines.iter().map(|l| (l.text.as_str(), l.is_alert)).collect();
      assert_eq!(
        summary,
        [
          ("INFO started", false),
          ("[error] it broke", true),
          ("FATAL: out of memory", true),
          ("warning only", false)
        ]
      );
    }

    #[test]
    fn ndjson_lines_are_flattened_to_level_and_message() {
      let body = br#"{"time":"2026-10-22T10:00:00Z","level":"ERROR","msg":"db down","host":"a"}
{"message":"ok","severity":"info"}
not json at all
{"unrelated":1}
"#;
      let lines = parse_log_lines("application/x-ndjson", body, &rules());
      let summary: Vec<_> = lines.iter().map(|l| (l.text.as_str(), l.is_alert)).collect();
      assert_eq!(
        summary,
        [
          ("[ERROR] 2026-10-22T10:00:00Z db down", true),
          ("[INFO] ok", false),
          ("not json at all", false),
          (r#"{"unrelated":1}"#, false)
        ]
      );
    }

    #[test]
    fn timestamped_lines_alert_on_their_level() {
      let body = "\
2026-11-12T10:00:00Z ERROR disk full\n\
2026-11-12T10:00:00+0100 host unit[42]: FATAL: out of memory\n\
Nov 12 10:00:00 host app[1]: ERROR boom\n\
time=2026-11-12T10:00:00Z level=error msg=boom\n\
2026/11/12 10:00:00 [error] 12#0: upstream\n\
2026-11-12T10:00:00Z host app[1]: no error found\n\
2026-11-12T10:00:00Z INFO all fine\n";
      let lines = parse_log_lines("text/plain", body.as_bytes(), &rules());
      let alerts: Vec<bool> = lines.iter().map(|line| line.is_alert).collect();
      assert_eq!(alerts, [true, true, true, true, true, false, false]);
    }

    #[test]
    fn journald_json_uses_message_and_priority() {
      let body = br#"{"MESSAGE":"disk full","PRIORITY":"3","__REALTIME_TIMESTAMP":"1"}
{"MESSAGE":"started","PRIORITY":"6"}
{"MESSAGE":"crashed","PRIORITY":2}
{"msg":"slow","level":40}
{"msg":"dead","level":60}
{"msg":"odd","level":"loud"}
"#;
      let lines = parse_log_lines("application/x-ndjson", body, &rules());
      let summary: Vec<_> = lines.iter().map(|l| (l.text.as_str(), l.is_alert)).collect();
      assert_eq!(
        summary,
        [
          ("[ERROR] disk full", true),
          ("[INFO] started", false),
          ("[CRITICAL] crashed", false),
          ("[WARN] slow", false),
          ("[FATAL] dead", true),
          ("[LOUD] odd", false),
        ]
      );
    }

    #[test]
    fn garbage_and_oversized_lines_are_kept_without_a_level() {
      let huge = format!("{{\"msg\":\"{}\",\"level\":\"error\"}}", "x".repeat(1_000_000));
      let lines = parse_log_lines("application/x-ndjson", huge.as_bytes(), &rules());
      assert_eq!(lines.len(), 1);
      assert!(lines[0].is_alert);
      let lines = parse_log_lines("text/plain", &[0xff, 0, b'[', b'['], &rules());
      assert_eq!(lines.len(), 1);
      assert!(!lines[0].is_alert);
    }

    /// What the bridge writes is read back by the apps' log view to the same level.
    #[test]
    fn a_line_written_by_the_bridge_has_the_same_level_in_the_apps() {
      use teamcraft_client_api::model::{LogLevel, Message, MessageKind, as_log_line, id_from_raw};

      let ndjson = br#"{"time":"2026-11-12T10:00:00Z","level":"ERROR","msg":"db down"}
{"time":"2026-11-12T10:00:00Z","level":"warn","msg":"slow"}
{"MESSAGE":"fine","PRIORITY":"6"}
{"MESSAGE":"gone","PRIORITY":"1"}
{"msg":"noisy","level":20}
"#;
      let plain = b"2026-11-12T10:00:00Z ERROR disk full\nlevel=warn msg=slow\nstarted\n";
      let levels = |content_type: &str, body: &[u8]| {
        parse_log_lines(content_type, body, &rules())
          .into_iter()
          .map(|line| (line.is_alert, app_level(&line.text)))
          .collect::<Vec<_>>()
      };
      fn chat_message(body: &str) -> Message {
        Message {
          id: id_from_raw(1),
          conversation: id_from_raw(2),
          author: id_from_raw(3),
          kind: MessageKind::User,
          sent_at: 0,
          body: body.to_owned(),
          is_edited: false,
          reactions: Vec::new(),
          reply_count: 0,
          maybe_thread: None,
          maybe_imported: None,
          maybe_source: None,
          maybe_placeholder: None,
          group_mentions: Vec::new(),
          custom_emoji: Vec::new(),
          mentions_me: false,
          maybe_poll: None,
        }
      }
      fn app_level(text: &str) -> LogLevel {
        match as_log_line(chat_message(text)).kind {
          MessageKind::Log(level) => level,
          other => panic!("not a log line: {other:?}"),
        }
      }
      assert_eq!(
        levels("application/x-ndjson", ndjson),
        [
          (true, LogLevel::Error),
          (false, LogLevel::Warn),
          (false, LogLevel::Info),
          (false, LogLevel::Error),
          (false, LogLevel::Debug),
        ]
      );
      assert_eq!(
        levels("text/plain", plain),
        [
          (true, LogLevel::Error),
          (false, LogLevel::Warn),
          (false, LogLevel::Info)
        ]
      );
    }

    #[test]
    fn invalid_utf8_does_not_break_ingestion() {
      let lines = parse_log_lines("text/plain", b"ok\xff\xfe line\n", &rules());
      assert_eq!(lines.len(), 1);
      assert!(lines[0].text.starts_with("ok"));
    }
  }
}
