//! Turning what tools send into message text: Slack/Mattermost incoming-webhook payloads and
//! log streams (plain text or NDJSON).

use serde::Deserialize;
use serde_json::Value;

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
/// (the original Slack form).
pub fn parse_webhook(
  content_type: &str,
  body: &[u8],
  danger_is_alert: bool,
) -> Result<WebhookMessage, PayloadError> {
  let json = if content_type.starts_with("application/x-www-form-urlencoded") {
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
#[derive(Debug, Clone)]
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

/// `[LEVEL] message` from an object with a message field; `None` if it isn't one.
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
  let message = field(&["message", "msg", "text", "log"])?;
  let level = field(&["level", "severity", "lvl"]).map(|level| level.to_lowercase());
  let time = field(&["time", "ts", "timestamp", "@timestamp"]);
  let mut text = String::new();
  if let Some(time) = time {
    text.push_str(&time);
    text.push(' ');
  }
  if let Some(level) = &level {
    text.push_str(&format!("[{}] ", level.to_uppercase()));
  }
  text.push_str(&message);
  Some((text, level))
}

/// A level word at the start or in brackets of an unstructured line: `ERROR something`, `[warn] x`.
fn plain_level(line: &str) -> Option<String> {
  let first = line
    .split(|c: char| c.is_whitespace() || c == ':')
    .find(|part| !part.is_empty())?
    .trim_matches(['[', ']']);
  let lower = first.to_lowercase();
  matches!(
    lower.as_str(),
    "trace"
      | "debug"
      | "info"
      | "notice"
      | "warn"
      | "warning"
      | "error"
      | "err"
      | "fatal"
      | "critical"
      | "alert"
      | "emergency"
  )
  .then_some(lower)
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
          ("2026-10-22T10:00:00Z [ERROR] db down", true),
          ("[INFO] ok", false),
          ("not json at all", false),
          (r#"{"unrelated":1}"#, false)
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
