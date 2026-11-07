//! "Report a problem" (docs/bug-reports.md): what a report holds, the text file it travels as and
//! the checks the dialog runs before it is queued. The person sees [`ReportContext::render`]'s
//! text before sending, and exactly that text is sent.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{Base64Bytes, ClientMessageId, ConversationId, UserId};

use crate::calls::{ActiveCall, CallKind};
use crate::debug_log::timestamp;

/// Longest description, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 4000;
/// Largest screenshot, in bytes.
pub const MAX_SCREENSHOT_BYTES: u64 = 10 * 1024 * 1024;
/// The report file's media type.
pub const REPORT_MEDIA_TYPE: &str = "text/plain";

/// A report the person sent: queued on the device, then delivered as an encrypted message to the
/// recipients of the verified policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BugReport {
  /// Chosen by the app: the message's client message ID, so a delivery that runs twice sends once.
  pub id: ClientMessageId,
  /// What the person wrote; the message text.
  pub description: String,
  /// [`ReportContext::render`]'s text, as the person saw it; the attached report file.
  pub report_text: String,
  /// The report file's name ([`report_file_name`]).
  pub file_name: String,
  #[serde(rename = "screenshot", default, skip_serializing_if = "Option::is_none")]
  pub maybe_screenshot: Option<ReportScreenshot>,
}

/// An image the person added to the report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportScreenshot {
  pub file_name: String,
  pub media_type: String,
  pub bytes: Base64Bytes,
}

/// Everything about the app a report carries besides the person's words. Names never: IDs only.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportContext {
  /// When the report was made, Unix milliseconds on this device's clock.
  pub created_at_ms: i64,
  pub app_version: String,
  /// `desktop`, `web`, `android` or `ios`.
  pub platform: String,
  /// The OS (and browser) as the platform names them.
  pub system: String,
  pub user: Option<UserId>,
  /// Whether the app is connected to the server, in a word (`connected`, `offline`, …).
  pub connection: String,
  #[serde(rename = "call", default, skip_serializing_if = "Option::is_none")]
  pub maybe_call: Option<CallFacts>,
  /// The recent log lines of this device, oldest first, redacted (`teamcraft_core::debug_log`).
  pub log_lines: Vec<String>,
}

/// The call this device is in, without names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallFacts {
  pub conversation: ConversationId,
  pub kind: String,
  pub is_secured: bool,
  pub is_reconnecting: bool,
  pub is_on_limited_network: bool,
  pub is_muted: bool,
  pub is_camera_on: bool,
  pub is_sharing_screen: bool,
  pub participants: Vec<UserId>,
  /// Others whose connection was lost and who are rejoining.
  pub reconnecting: Vec<UserId>,
}

/// Why the dialog can't queue a report yet; each names the field it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BugReportFieldError {
  DescriptionMissing,
  DescriptionTooLong,
  ScreenshotTooLarge,
  ScreenshotNotAnImage,
}

/// Why no report can be sent at all (an alert next to the send button, not a field's error).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BugReportBlocker {
  /// Nobody but the reporter would receive it.
  NoRecipients,
  /// As many reports as a device keeps wait to be delivered already.
  QueueFull,
}

impl BugReportBlocker {
  /// Stable name on the wire (`crate::failure`).
  pub fn as_str(self) -> &'static str {
    match self {
      Self::NoRecipients => "no_recipients",
      Self::QueueFull => "queue_full",
    }
  }

  pub(crate) fn from_wire(name: &str) -> Option<Self> {
    [Self::NoRecipients, Self::QueueFull]
      .into_iter()
      .find(|blocker| blocker.as_str() == name)
  }
}

impl CallFacts {
  pub fn of(call: &ActiveCall) -> Self {
    let kind = match call.kind {
      CallKind::Direct => "direct",
      CallKind::Group => "group",
      CallKind::Huddle => "huddle",
    };
    Self {
      conversation: call.conversation,
      kind: kind.to_owned(),
      is_secured: call.is_secured,
      is_reconnecting: call.is_reconnecting,
      is_on_limited_network: call.is_on_limited_network,
      is_muted: call.is_muted,
      is_camera_on: call.is_camera_on,
      is_sharing_screen: call.is_sharing_screen,
      participants: call
        .participants
        .iter()
        .map(|participant| participant.user)
        .collect(),
      reconnecting: call.reconnecting.clone(),
    }
  }
}

impl ReportContext {
  /// The report file's text: a header of facts, then the log. Plain English for the team that
  /// reads it, like the mobile diagnostics export.
  pub fn render(&self) -> String {
    let mut text = String::from("TeamCraft problem report\n\n");
    line(
      &mut text,
      "Created",
      &format!("{} (device clock)", timestamp(self.created_at_ms)),
    );
    line(&mut text, "App version", &self.app_version);
    line(&mut text, "Platform", &self.platform);
    line(&mut text, "System", &self.system);
    let user = self.user.map_or_else(|| "-".to_owned(), |user| user.to_string());
    line(&mut text, "User", &user);
    line(&mut text, "Connection", &self.connection);
    match &self.maybe_call {
      Some(call) => {
        text.push_str("\nCall\n");
        line(&mut text, "Conversation", &call.conversation.to_string());
        line(&mut text, "Kind", &call.kind);
        line(&mut text, "Secured", &yes_no(call.is_secured));
        line(&mut text, "Reconnecting", &yes_no(call.is_reconnecting));
        line(&mut text, "Limited network", &yes_no(call.is_on_limited_network));
        line(&mut text, "Muted", &yes_no(call.is_muted));
        line(&mut text, "Camera", &yes_no(call.is_camera_on));
        line(&mut text, "Sharing screen", &yes_no(call.is_sharing_screen));
        line(&mut text, "Participants", &ids(&call.participants));
        line(&mut text, "Rejoining", &ids(&call.reconnecting));
      },
      None => line(&mut text, "Call", "none"),
    }
    text.push_str(&format!("\nLog ({} lines, oldest first)\n", self.log_lines.len()));
    for log_line in &self.log_lines {
      text.push_str(log_line);
      text.push('\n');
    }
    text
  }
}

/// The checks of the dialog's fields, in the order of the fields.
pub fn check_fields(description: &str, maybe_screenshot: Option<(&str, u64)>) -> Vec<BugReportFieldError> {
  let mut errors = Vec::new();
  let description = description.trim();
  if description.is_empty() {
    errors.push(BugReportFieldError::DescriptionMissing);
  } else if description.chars().count() > MAX_DESCRIPTION_CHARS {
    errors.push(BugReportFieldError::DescriptionTooLong);
  }
  if let Some((media_type, size_bytes)) = maybe_screenshot {
    if !matches!(media_type, "image/png" | "image/jpeg" | "image/webp") {
      errors.push(BugReportFieldError::ScreenshotNotAnImage);
    } else if size_bytes > MAX_SCREENSHOT_BYTES {
      errors.push(BugReportFieldError::ScreenshotTooLarge);
    }
  }
  errors
}

/// `teamcraft-report-2026-11-07T12-00-00Z.txt`.
pub fn report_file_name(created_at_ms: i64) -> String {
  format!(
    "teamcraft-report-{}.txt",
    timestamp(created_at_ms).replace(':', "-")
  )
}

fn line(text: &mut String, label: &str, value: &str) {
  text.push_str(label);
  text.push_str(": ");
  text.push_str(value);
  text.push('\n');
}

fn yes_no(value: bool) -> String {
  if value { "yes" } else { "no" }.to_owned()
}

fn ids(users: &[UserId]) -> String {
  if users.is_empty() {
    return "-".to_owned();
  }
  users
    .iter()
    .map(ToString::to_string)
    .collect::<Vec<_>>()
    .join(", ")
}

#[cfg(test)]
mod tests {
  use super::*;

  const NOV_1_NOON_MS: i64 = 1_793_534_400_000;

  #[test]
  fn the_report_text_lists_facts_then_the_log() {
    let context = ReportContext {
      created_at_ms: NOV_1_NOON_MS,
      app_version: "1.2.3".to_owned(),
      platform: "web".to_owned(),
      system: "Linux, Firefox 140".to_owned(),
      user: Some(user(5)),
      connection: "connected".to_owned(),
      maybe_call: None,
      log_lines: vec!["a".to_owned(), "b".to_owned()],
    };
    assert_eq!(
      context.render(),
      "TeamCraft problem report\n\nCreated: 2026-11-01T12:00:00Z (device clock)\nApp version: 1.2.3\n\
       Platform: web\nSystem: Linux, Firefox 140\nUser: 5\nConnection: connected\nCall: none\n\n\
       Log (2 lines, oldest first)\na\nb\n"
    );
  }

  #[test]
  fn a_call_is_described_by_ids_only() {
    let context = ReportContext {
      maybe_call: Some(CallFacts {
        conversation: "77".parse().unwrap(),
        kind: "group".to_owned(),
        is_secured: true,
        is_reconnecting: false,
        is_on_limited_network: true,
        is_muted: false,
        is_camera_on: true,
        is_sharing_screen: false,
        participants: vec![user(5), user(6)],
        reconnecting: vec![user(6)],
      }),
      ..ReportContext::default()
    };
    let text = context.render();
    assert!(
      text.contains("\nCall\nConversation: 77\nKind: group\nSecured: yes\n"),
      "{text}"
    );
    assert!(text.contains("Limited network: yes\n"), "{text}");
    assert!(text.contains("Participants: 5, 6\nRejoining: 6\n"), "{text}");
  }

  #[test]
  fn fields_are_checked_in_their_order() {
    assert_eq!(
      check_fields("  ", Some(("application/pdf", 10))),
      [
        BugReportFieldError::DescriptionMissing,
        BugReportFieldError::ScreenshotNotAnImage
      ]
    );
    let long = "x".repeat(MAX_DESCRIPTION_CHARS + 1);
    assert_eq!(
      check_fields(&long, Some(("image/png", MAX_SCREENSHOT_BYTES + 1))),
      [
        BugReportFieldError::DescriptionTooLong,
        BugReportFieldError::ScreenshotTooLarge
      ]
    );
    assert!(check_fields("The call drops after a minute", Some(("image/jpeg", 1_000))).is_empty());
    assert!(check_fields("The call drops after a minute", None).is_empty());
  }

  #[test]
  fn the_file_name_carries_the_time_without_colons() {
    assert_eq!(
      report_file_name(NOV_1_NOON_MS),
      "teamcraft-report-2026-11-01T12-00-00Z.txt"
    );
  }

  fn user(id: u64) -> UserId {
    id.to_string().parse().unwrap()
  }
}
