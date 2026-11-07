//! "Report a problem" (docs/bug-reports.md): a report is stored on the device first and delivered
//! by the upkeep as an encrypted message in the group DM of the reporter and the recipients the
//! verified policy names. Once in the outbox it is an ordinary message with files: sent with
//! retries, shown as pending or failed in that DM.
//!
//! The report's ID is the message's client message ID, so a delivery cut short (crash, lost
//! answer) that runs again finds the message in the store and doesn't send it twice.

use serde::{Deserialize, Serialize};
use teamcraft_proto::{Base64Bytes, ClientMessageId, UserId};

use super::Client;
use super::files::{
  PendingFile, PendingUpload, UploadPurpose, UploadRetry, UploadSource, save_pending_upload,
};
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::records::{self, Draft};
use crate::store::{LocalStore, MessageContent, Table};
use crate::workspace::{RosterRole, WorkspaceState};

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
    let mut files = vec![ReportFile {
      file_name: report.file_name,
      media_type: teamcraft_client_api::bug_report::REPORT_MEDIA_TYPE.to_owned(),
      bytes: Base64Bytes::new(report.report_text.into_bytes()),
    }];
    if let Some(screenshot) = report.maybe_screenshot {
      files.push(ReportFile {
        file_name: screenshot.file_name,
        media_type: screenshot.media_type,
        bytes: screenshot.bytes,
      });
    }
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
        Err(error) => {
          tracing::warn!(%error, attempts = report.attempts, "a problem report could not be delivered; retried later");
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
    let conversation = self.open_group_dm(&recipients).await?;
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
  fn the_reporter_is_never_a_recipient() {
    assert_eq!(choose_recipients(&[], &[user(1), user(2)], user(1)), [user(2)]);
    assert!(choose_recipients(&[user(1)], &[user(2)], user(1)).is_empty());
  }

  fn user(id: u64) -> UserId {
    id.to_string().parse().unwrap()
  }
}
