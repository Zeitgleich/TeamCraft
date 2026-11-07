//! "Report a problem" (docs/bug-reports.md): a report is stored on the device first and delivered
//! by the upkeep as an encrypted message in the group DM of the reporter and the recipients the
//! verified policy names. Once in the outbox it is an ordinary message with files: sent with
//! retries, shown as pending or failed in that DM.
//!
//! The report's ID is the message's client message ID, so a delivery cut short (crash, lost
//! answer) that runs again finds the message in the store and doesn't send it twice.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{Base64Bytes, ClientMessageId, ConversationKind, MAX_BUG_REPORT_RECIPIENTS, UserId};

use super::Client;
use super::files::{
  PendingFile, PendingUpload, UploadPurpose, UploadRetry, UploadSource, save_pending_upload,
};
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records::{self, Draft};
use crate::store::{LocalStore, MessageContent, Table};
use crate::workspace::{RosterRole, WorkspaceState};

/// The largest report text the core takes: the log's bound with room for the header.
const MAX_REPORT_TEXT_BYTES: usize = 2 * 1024 * 1024;
/// Reports that may wait on a device at once.
pub const MAX_QUEUED_REPORTS: usize = 5;
/// A report that couldn't be delivered for this long is dropped.
const MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// The first wait after a failed delivery; it doubles up to [`MAX_RETRY_MS`].
const FIRST_RETRY_MS: i64 = 60 * 1000;
const MAX_RETRY_MS: i64 = 60 * 60 * 1000;
/// Key prefix of queued reports in [`Table::SyncState`].
const KEY_PREFIX: &str = "bug_report/";

/// A report waiting to be delivered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedBugReport {
  pub id: ClientMessageId,
  pub description: String,
  pub files: Vec<ReportFile>,
  pub queued_at_ms: i64,
  #[serde(default)]
  pub attempts: u32,
  #[serde(rename = "retry_at_ms", default, skip_serializing_if = "Option::is_none")]
  pub maybe_retry_at_ms: Option<i64>,
}

/// A file of a report: the report text, and the screenshot if the person added one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportFile {
  pub file_name: String,
  pub media_type: String,
  pub bytes: Base64Bytes,
}

/// Why a report can't be queued.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BugReportError {
  #[error("nobody is set up to receive problem reports")]
  NoRecipients,
  #[error("{MAX_QUEUED_REPORTS} problem reports are waiting to be sent already")]
  QueueFull,
  /// The server answered the request for the recipients' group DM with a conversation whose
  /// verified members aren't the reporter and recipients (a channel, someone else's DM): a
  /// hostile or broken server. Nothing is sent there.
  #[error("the server named a conversation that isn't the report's group DM")]
  WrongConversation,
  /// The group DM's encryption group isn't on this device yet (its welcome is still to come).
  #[error("the report's group DM isn't set up on this device yet")]
  GroupNotReady,
  /// The report the app handed over is malformed or too large (it crossed IPC or FFI).
  #[error("the problem report is invalid: {0}")]
  Invalid(&'static str),
}

#[cfg(feature = "ui")]
impl BugReportError {
  /// How the UI words it; `None` for what it words as a generic failure.
  pub fn blocker(&self) -> Option<teamcraft_client_api::bug_report::BugReportBlocker> {
    use teamcraft_client_api::bug_report::BugReportBlocker;
    match self {
      Self::NoRecipients => Some(BugReportBlocker::NoRecipients),
      Self::QueueFull => Some(BugReportBlocker::QueueFull),
      Self::WrongConversation => Some(BugReportBlocker::WrongConversation),
      Self::GroupNotReady | Self::Invalid(_) => None,
    }
  }
}

/// Who a report goes to: the verified policy's recipients, or the verified roster's owners and
/// admins when it names none; never the reporter, no one twice. Empty before a workspace was
/// verified: the server's word on who the admins are is never taken.
pub fn recipients(store: &impl LocalStore, reporter: UserId) -> Result<Vec<UserId>, CoreError> {
  let state =
    records::get_json::<WorkspaceState>(store, Table::Workspace, super::WORKSPACE_KEY)?.unwrap_or_default();
  let admins: Vec<UserId> = state
    .roster
    .members
    .iter()
    .filter(|member| matches!(member.role, RosterRole::Owner | RosterRole::Admin))
    .map(|member| member.user_id)
    .collect();
  Ok(choose_recipients(
    &state.policy.bug_reports.recipients,
    &admins,
    reporter,
  ))
}

/// The reports waiting on this device, oldest first.
pub fn queued_reports(store: &impl LocalStore) -> Result<Vec<QueuedBugReport>, CoreError> {
  let mut reports: Vec<QueuedBugReport> =
    records::list_json_with_prefix(store, Table::SyncState, KEY_PREFIX)?
      .into_iter()
      .map(|(_, report)| report)
      .collect();
  reports.sort_by_key(|report| report.queued_at_ms);
  Ok(reports)
}

/// Stores a report for the next delivery. Checks the recipients and the queue's bound first, so
/// the dialog can say why a report can't go. Storing the same report again (a retried action)
/// changes nothing.
pub fn queue_report(
  store: &impl LocalStore,
  reporter: UserId,
  report: QueuedBugReport,
) -> Result<(), CoreError> {
  let key = report_key(report.id);
  if records::get_json::<QueuedBugReport>(store, Table::SyncState, &key)?.is_some() {
    return Ok(());
  }
  if recipients(store, reporter)?.is_empty() {
    return Err(BugReportError::NoRecipients.into());
  }
  if queued_reports(store)?.len() >= MAX_QUEUED_REPORTS {
    return Err(BugReportError::QueueFull.into());
  }
  records::put_json(store, Table::SyncState, &key, &report)
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// "Send" in the report dialog: stores the report and delivers it at once if the server can be
  /// reached; offline it waits for the upkeep. Fails only when it can't be queued
  /// ([`BugReportError`]).
  #[cfg(feature = "ui")]
  pub async fn queue_bug_report(
    &mut self,
    report: teamcraft_client_api::bug_report::BugReport,
  ) -> Result<(), CoreError> {
    let files = report_files(&report)?;
    let queued = QueuedBugReport {
      id: report.id,
      description: report.description.trim().to_owned(),
      files,
      queued_at_ms: self.now_ms(),
      attempts: 0,
      maybe_retry_at_ms: None,
    };
    queue_report(&self.store, self.user_id(), queued)?;
    // Offline or refused: it stays queued and the upkeep tries again.
    if let Err(error) = self.deliver_bug_reports().await {
      tracing::debug!(%error, "the problem report waits for the next delivery");
    }
    Ok(())
  }

  /// Puts the queued reports that are due into their group DMs' outbox. Stops at the first report
  /// the server can't be reached for (the rest waits as well); a report the server refuses backs
  /// off and is dropped after [`MAX_AGE_MS`]. Returns how many went into the outbox.
  pub async fn deliver_bug_reports(&mut self) -> Result<usize, CoreError> {
    let now_ms = self.now_ms();
    let mut delivered = 0;
    for report in queued_reports(&self.store)? {
      if now_ms.saturating_sub(report.queued_at_ms) > MAX_AGE_MS {
        tracing::warn!(
          attempts = report.attempts,
          "a problem report could not be delivered for a week; dropped"
        );
        self.store.delete(Table::SyncState, &report_key(report.id))?;
        continue;
      }
      if report
        .maybe_retry_at_ms
        .is_some_and(|retry_at_ms| retry_at_ms > now_ms)
      {
        continue;
      }
      match self.deliver_report(&report).await {
        Ok(()) => delivered += 1,
        Err(error) if error.is_retry_later() => {
          tracing::debug!(%error, "problem reports wait for the server");
          break;
        },
        Err(CoreError::BugReport(BugReportError::GroupNotReady)) => {
          tracing::debug!("a problem report waits for its group DM");
          self.back_off_report(report, now_ms)?;
        },
        Err(error) => {
          tracing::warn!(%error, attempts = report.attempts, "a problem report could not be delivered; retried later");
          // A conversation that isn't the report's says something is wrong with the server: the
          // person hears of it (issue #322), the report stays here and nothing goes there.
          if matches!(error, CoreError::BugReport(BugReportError::WrongConversation)) {
            self.background_failures.push(error);
          }
          self.back_off_report(report, now_ms)?;
        },
      }
    }
    Ok(delivered)
  }

  async fn deliver_report(&mut self, report: &QueuedBugReport) -> Result<(), CoreError> {
    let reporter = self.user_id();
    let recipients = recipients(&self.store, reporter)?;
    if recipients.is_empty() {
      return Err(BugReportError::NoRecipients.into());
    }
    // Recipients read now, not when queued: a policy changed meanwhile names the current ones.
    let conversation = match self.open_group_dm(&recipients).await {
      Err(CoreError::NotADirectConversation(_)) => return Err(BugReportError::WrongConversation.into()),
      other => other?,
    };
    self.check_report_conversation(conversation, reporter, &recipients)?;
    if self.store.message(conversation, report.id)?.is_none() {
      // The files first: a crash before the message is written leaves an upload nobody sends,
      // which the next delivery writes again under the same key.
      save_pending_upload(
        &self.store,
        &PendingUpload {
          conversation_id: conversation,
          client_message_id: report.id,
          files: report.files.iter().map(pending_file).collect(),
          purpose: UploadPurpose::Message,
        },
      )?;
      let draft = Draft {
        conversation_id: conversation,
        maybe_thread_id: None,
        content: MessageContent::text(report.description.clone(), Vec::new()),
      };
      records::enqueue_message_as(&self.store, reporter, self.now_ms(), draft, report.id)?;
    }
    self.store.delete(Table::SyncState, &report_key(report.id))?;
    tracing::info!(conversation_id = %conversation, "problem report queued for its recipients");
    Ok(())
  }

  /// The server names the conversation, so it is checked before anything goes there: a DM or
  /// group DM whose encryption group, as this device verified it, holds the reporter and nobody
  /// but the recipients. A recipient not in it yet (no device to add) only receives less.
  fn check_report_conversation(
    &mut self,
    conversation: teamcraft_proto::ConversationId,
    reporter: UserId,
    recipients: &[UserId],
  ) -> Result<(), CoreError> {
    let is_direct = self
      .cached_conversations()?
      .get(&conversation)
      .is_some_and(|response| matches!(response.kind, ConversationKind::Dm | ConversationKind::Group));
    if !is_direct {
      return Err(BugReportError::WrongConversation.into());
    }
    if !self.has_group(conversation)? {
      return Err(BugReportError::GroupNotReady.into());
    }
    let members = self.group_users(conversation)?;
    let is_only_ours = members
      .iter()
      .all(|member| *member == reporter || recipients.contains(member));
    if !members.contains(&reporter) || !is_only_ours {
      return Err(BugReportError::WrongConversation.into());
    }
    Ok(())
  }

  fn back_off_report(&self, report: QueuedBugReport, now_ms: i64) -> Result<(), CoreError> {
    let attempts = report.attempts.saturating_add(1);
    let wait_ms = FIRST_RETRY_MS
      .saturating_mul(1_i64 << attempts.saturating_sub(1).min(16))
      .min(MAX_RETRY_MS);
    let retried = QueuedBugReport {
      attempts,
      maybe_retry_at_ms: Some(now_ms.saturating_add(wait_ms)),
      ..report
    };
    records::put_json(&self.store, Table::SyncState, &report_key(retried.id), &retried)
  }
}

/// The files of a report, checked again here: the report crossed IPC or FFI from an app. The
/// screenshot is encoded again without its metadata (EXIF, GPS) and named by its type, so neither
/// where it was taken nor its file name travel.
#[cfg(feature = "ui")]
fn report_files(report: &teamcraft_client_api::bug_report::BugReport) -> Result<Vec<ReportFile>, CoreError> {
  use teamcraft_client_api::bug_report::{REPORT_MEDIA_TYPE, check_fields, report_file_name};

  let maybe_screenshot = report.maybe_screenshot.as_ref().map(|screenshot| {
    (
      screenshot.media_type.as_str(),
      screenshot.bytes.as_slice().len() as u64,
    )
  });
  if !check_fields(&report.description, maybe_screenshot).is_empty() {
    return Err(BugReportError::Invalid("description or screenshot").into());
  }
  if report.report_text.len() > MAX_REPORT_TEXT_BYTES {
    return Err(BugReportError::Invalid("report text too large").into());
  }
  // The app's name for the file only if it is the one this version makes.
  let is_report_name = report.file_name.starts_with("teamcraft-report-")
    && report.file_name.ends_with(".txt")
    && report
      .file_name
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'));
  let file_name = if is_report_name {
    report.file_name.clone()
  } else {
    report_file_name(0)
  };
  let mut files = vec![ReportFile {
    file_name,
    media_type: REPORT_MEDIA_TYPE.to_owned(),
    bytes: Base64Bytes::new(report.report_text.clone().into_bytes()),
  }];
  if let Some(screenshot) = &report.maybe_screenshot {
    let (bytes, media_type) = crate::thumbnail::without_metadata(screenshot.bytes.as_slice())
      .ok_or(BugReportError::Invalid("the screenshot is no readable picture"))?;
    let extension = if media_type == "image/jpeg" { "jpg" } else { "png" };
    files.push(ReportFile {
      file_name: format!("screenshot.{extension}"),
      media_type: media_type.to_owned(),
      bytes: Base64Bytes::new(bytes),
    });
  }
  Ok(files)
}

fn choose_recipients(
  policy_recipients: &[UserId],
  roster_admins: &[UserId],
  reporter: UserId,
) -> Vec<UserId> {
  let source = if policy_recipients.is_empty() {
    roster_admins
  } else {
    policy_recipients
  };
  let mut found: Vec<UserId> = Vec::with_capacity(source.len());
  for user in source {
    if *user != reporter && !found.contains(user) {
      found.push(*user);
    }
  }
  // A group DM stays small; a policy naming more (signed by an older or careless owner) is cut.
  found.truncate(MAX_BUG_REPORT_RECIPIENTS);
  found
}

fn report_key(id: ClientMessageId) -> String {
  format!("{KEY_PREFIX}{id}")
}

fn pending_file(file: &ReportFile) -> PendingFile {
  PendingFile {
    file_name: file.file_name.clone(),
    media_type: file.media_type.clone(),
    size_bytes: file.bytes.as_slice().len() as u64,
    source: UploadSource::Bytes {
      bytes: file.bytes.clone(),
    },
    maybe_done: None,
    maybe_failure: None,
    maybe_slot: None,
    maybe_parts: None,
    retry: UploadRetry::default(),
    maybe_thumbnail: None,
    maybe_dimensions: None,
    maybe_duration_ms: None,
    maybe_waveform: None,
    is_held: false,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn reports_go_to_the_policys_recipients_or_else_the_admins() {
    let admins = [user(1), user(2)];
    assert_eq!(choose_recipients(&[], &admins, user(9)), admins);
    assert_eq!(
      choose_recipients(&[user(3), user(3), user(4)], &admins, user(9)),
      [user(3), user(4)]
    );
  }

  #[test]
  fn a_group_dm_stays_small() {
    let many: Vec<UserId> = (1..=40).map(user).collect();
    assert_eq!(
      choose_recipients(&many, &[], user(99)).len(),
      MAX_BUG_REPORT_RECIPIENTS
    );
  }

  #[test]
  fn the_reporter_is_never_a_recipient() {
    assert_eq!(choose_recipients(&[], &[user(1), user(2)], user(1)), [user(2)]);
    assert!(choose_recipients(&[user(1)], &[user(2)], user(1)).is_empty());
  }

  fn user(id: u64) -> UserId {
    id.to_string().parse().unwrap()
  }
}
