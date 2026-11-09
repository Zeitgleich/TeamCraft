//! Inactive channels (PLAN.md §12, issue #212, admin.md §3.1.3): the owners are told that a channel
//! went quiet, it is archived some days later if nobody wrote in it or kept it open, and, only with an
//! explicit period in the signed policy, an archived channel is deleted later. Also: idle local guest
//! accounts are deactivated after a grace period.
//!
//! The server is content-blind, so "quiet" is read from what it stores anyway: the time in the newest
//! message's ID. The policy is off unless the signed workspace policy turns it on; every run is
//! bounded (`max_per_run`) and every step has an audit entry without an actor.

use serde_json::json;
use teamcraft_db::{InactivityCandidate, NewAuditEntry, PolicyArchivedChannel};
use teamcraft_proto::{
  ChannelInactivityStatus, ConversationId, ConversationKind, DeactivationReason, GatewayEvent,
  INACTIVITY_DELETE_NOTICE_DAYS, InactivityPolicy, MAX_INACTIVITY_SNOOZE_DAYS, Permission,
  SnoozeInactivityRequest, SystemEventKind, WebhookEventKind,
};
use time::{Duration, OffsetDateTime};
use tracing::{info, warn};

use super::access::{load_conversation, require};
use super::admin::{AfterDeactivation, deactivate_user};
use super::audit::{self, Target as AuditTarget};
use super::convert::unix_millis;
use super::{conversations, system_events};
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::meetings::ical::Method;
use crate::meetings::mailer::OutgoingMail;
use crate::state::AppState;

/// Channels one run looks at, quietest first. A bigger instance continues at the next run.
const CANDIDATE_LIMIT: i64 = 2000;
/// Archived channels one run looks at for the deletion step.
const ARCHIVED_LIMIT: i64 = 500;
/// A notice this many days past the date it led to is stale (the policy was off, the server down):
/// it is repeated instead of acted on, so switching a policy on again never archives or deletes on an
/// old notice.
const MIN_STALE_GRACE_DAYS: i64 = 7;

/// What one run did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InactivityRun {
  /// Channels whose owners were told that the channel is quiet.
  pub noticed: usize,
  /// Channels archived.
  pub archived: usize,
  /// Archived channels whose owners were told that they will be deleted.
  pub delete_warned: usize,
  /// Channels deleted.
  pub deleted: usize,
  /// A limit held something back until the next run.
  pub is_limited: bool,
}

/// One run of the policy at `now`. Does nothing while the policy is off. At most `max_per_run` channels
/// are noticed, archived, warned and deleted each; what is left waits for the next run, and the audit
/// log says so.
pub async fn run_once(state: &AppState, now: OffsetDateTime) -> Result<InactivityRun, ApiError> {
  let policy = state.policy.get().inactivity.clone();
  let mut run = InactivityRun::default();
  if !policy.is_enabled {
    return Ok(run);
  }
  let limit = policy.max_per_run as usize;
  let quiet_before = now - Duration::days(i64::from(policy.notice_after_days));
  let candidates = state
    .database
    .list_inactivity_candidates(now, quiet_before, CANDIDATE_LIMIT)
    .await?;
  for candidate in candidates {
    if let Err(error) = Box::pin(handle_candidate(
      state,
      &policy,
      &candidate,
      now,
      quiet_before,
      limit,
      &mut run,
    ))
    .await
    {
      warn!(%error, conversation_id = %candidate.id, "the inactivity policy failed on a channel");
    }
  }
  if policy.maybe_delete_after_archived_days.is_some() {
    Box::pin(delete_step(state, &policy, now, limit, &mut run)).await?;
  }
  if run.is_limited {
    audit::record(
      state,
      None,
      "inactivity_run_limited",
      None,
      &json!({
        "max_per_run": policy.max_per_run,
        "noticed": run.noticed,
        "archived": run.archived,
        "delete_warned": run.delete_warned,
        "deleted": run.deleted,
      }),
    )
    .await?;
  }
  if run != InactivityRun::default() {
    info!(
      noticed = run.noticed,
      archived = run.archived,
      delete_warned = run.delete_warned,
      deleted = run.deleted,
      "ran the inactivity policy"
    );
  }
  Ok(run)
}

/// The newest sign of life of a channel: its newest message, else its creation.
fn last_activity(candidate: &InactivityCandidate) -> OffsetDateTime {
  let created = candidate.created_at;
  candidate
    .maybe_last_message_millis
    .and_then(|millis| OffsetDateTime::from_unix_timestamp_nanos(i128::from(millis) * 1_000_000).ok())
    .map_or(created, |message| message.max(created))
}

async fn handle_candidate(
  state: &AppState,
  policy: &InactivityPolicy,
  candidate: &InactivityCandidate,
  now: OffsetDateTime,
  quiet_before: OffsetDateTime,
  limit: usize,
  run: &mut InactivityRun,
) -> Result<(), ApiError> {
  let activity = last_activity(candidate);
  let Some(notified_at) = candidate.maybe_notified_at else {
    if activity > quiet_before {
      return Ok(());
    }
    if run.noticed >= limit {
      run.is_limited = true;
      return Ok(());
    }
    if send_notice(state, policy, candidate.id, activity, now, None).await? {
      run.noticed += 1;
    }
    return Ok(());
  };
  if activity > notified_at {
    // Written in since: the quiet spell is over.
    state.database.clear_inactivity_notice(candidate.id).await?;
    return Ok(());
  }
  if !policy.archives() {
    return Ok(());
  }
  let due = notified_at + Duration::days(i64::from(policy.archive_after_more_days));
  if due > now {
    return Ok(());
  }
  if now > due + stale_grace(policy.archive_after_more_days) {
    if run.noticed >= limit {
      run.is_limited = true;
    } else if send_notice(state, policy, candidate.id, activity, now, Some(notified_at)).await? {
      run.noticed += 1;
    }
    return Ok(());
  }
  if run.archived >= limit {
    run.is_limited = true;
    return Ok(());
  }
  if archive(state, candidate.id, notified_at, activity, now).await? {
    run.archived += 1;
  }
  Ok(())
}

fn stale_grace(period_days: u32) -> Duration {
  Duration::days(i64::from(period_days).max(MIN_STALE_GRACE_DAYS))
}

/// Tells the owners that the channel is quiet and records it, if at least one of them could be
/// reached; a channel nobody can be told about is never archived. `false`: nobody was told, or the
/// channel changed meanwhile.
async fn send_notice(
  state: &AppState,
  policy: &InactivityPolicy,
  conversation: ConversationId,
  activity: OffsetDateTime,
  now: OffsetDateTime,
  maybe_expected: Option<OffsetDateTime>,
) -> Result<bool, ApiError> {
  let settings = state.settings.get();
  let config = &settings.inactivity;
  let body = if policy.archives() {
    &config.notice_body
  } else {
    &config.notice_body_no_archive
  };
  let mailed = mail_owners(
    state,
    conversation,
    &config.notice_subject,
    body,
    policy.archive_after_more_days,
  )
  .await?;
  if mailed == 0 {
    return Ok(false);
  }
  if !state
    .database
    .record_inactivity_notice(conversation, now, maybe_expected)
    .await?
  {
    return Ok(false);
  }
  audit::record(
    state,
    None,
    "channel_inactivity_notice",
    Some(AuditTarget::new("conversation", conversation)),
    &json!({
      "quiet_days": (now - activity).whole_days(),
      "archive_in_days": policy.archives().then_some(policy.archive_after_more_days),
      "mailed": mailed,
    }),
  )
  .await?;
  Ok(true)
}

/// Archives a noticed channel that stayed quiet, and tells its members the usual way.
async fn archive(
  state: &AppState,
  conversation: ConversationId,
  notified_at: OffsetDateTime,
  activity: OffsetDateTime,
  now: OffsetDateTime,
) -> Result<bool, ApiError> {
  if !state
    .database
    .archive_inactive_channel(conversation, notified_at, now)
    .await?
  {
    return Ok(false);
  }
  audit::record(
    state,
    None,
    "channel_auto_archived",
    Some(AuditTarget::new("conversation", conversation)),
    &json!({
      "quiet_days": (now - activity).whole_days(),
      "notified_at": unix_millis(notified_at).0,
    }),
  )
  .await?;
  system_events::emit(state, conversation, SystemEventKind::Archived, None, None).await;
  let members = state.database.conversation_member_ids(conversation).await?;
  state.hub.publish(
    members,
    &GatewayEvent::ConversationUpdate {
      conversation_id: conversation,
    },
  );
  Ok(true)
}

/// Warns the owners of channels the policy archived that they will be deleted, and deletes them once
/// the warning has stood for its time. Only with `delete_after_archived_days` in the policy.
async fn delete_step(
  state: &AppState,
  policy: &InactivityPolicy,
  now: OffsetDateTime,
  limit: usize,
  run: &mut InactivityRun,
) -> Result<(), ApiError> {
  let Some(after_days) = policy.maybe_delete_after_archived_days else {
    return Ok(());
  };
  let after = Duration::days(i64::from(after_days));
  let lead = Duration::days(i64::from(INACTIVITY_DELETE_NOTICE_DAYS));
  let channels = state
    .database
    .list_policy_archived_channels(now, now - (after - lead), ARCHIVED_LIMIT)
    .await?;
  for channel in channels {
    let outcome = Box::pin(delete_one(state, &channel, after, lead, now, limit, run)).await;
    if let Err(error) = outcome {
      warn!(%error, conversation_id = %channel.id, "the inactivity policy failed on an archived channel");
    }
  }
  Ok(())
}

async fn delete_one(
  state: &AppState,
  channel: &PolicyArchivedChannel,
  after: Duration,
  lead: Duration,
  now: OffsetDateTime,
  limit: usize,
  run: &mut InactivityRun,
) -> Result<(), ApiError> {
  let Some(warned_at) = channel.maybe_notified_at else {
    if run.delete_warned >= limit {
      run.is_limited = true;
    } else if warn_of_deletion(state, channel, after, now, None).await? {
      run.delete_warned += 1;
    }
    return Ok(());
  };
  let due = (warned_at + lead).max(channel.archived_at + after);
  if due > now {
    return Ok(());
  }
  if now > warned_at + lead + stale_grace(u32::try_from(lead.whole_days()).unwrap_or(0)) {
    if run.delete_warned >= limit {
      run.is_limited = true;
    } else if warn_of_deletion(state, channel, after, now, Some(warned_at)).await? {
      run.delete_warned += 1;
    }
    return Ok(());
  }
  if run.deleted >= limit {
    run.is_limited = true;
    return Ok(());
  }
  if delete_channel(state, channel, warned_at).await? {
    run.deleted += 1;
  }
  Ok(())
}

async fn warn_of_deletion(
  state: &AppState,
  channel: &PolicyArchivedChannel,
  after: Duration,
  now: OffsetDateTime,
  maybe_expected: Option<OffsetDateTime>,
) -> Result<bool, ApiError> {
  let settings = state.settings.get();
  let config = &settings.inactivity;
  let mailed = mail_owners(
    state,
    channel.id,
    &config.delete_notice_subject,
    &config.delete_notice_body,
    INACTIVITY_DELETE_NOTICE_DAYS,
  )
  .await?;
  if mailed == 0 {
    return Ok(false);
  }
  if !state
    .database
    .record_inactivity_notice(channel.id, now, maybe_expected)
    .await?
  {
    return Ok(false);
  }
  audit::record(
    state,
    None,
    "channel_inactivity_delete_notice",
    Some(AuditTarget::new("conversation", channel.id)),
    &json!({
      "delete_in_days": INACTIVITY_DELETE_NOTICE_DAYS,
      "archived_at": unix_millis(channel.archived_at).0,
      "delete_after_days": after.whole_days(),
      "mailed": mailed,
    }),
  )
  .await?;
  Ok(true)
}

/// Deletes a channel the policy archived, as `delete_channel` does for an admin, with an audit entry
/// without an actor. Refused (`false`) if somebody restored or imported into it meanwhile.
async fn delete_channel(
  state: &AppState,
  channel: &PolicyArchivedChannel,
  warned_at: OffsetDateTime,
) -> Result<bool, ApiError> {
  let id = state
    .ids
    .next_snowflake()
    .map_err(|error| ApiError::Internal(error.to_string()))?;
  let target = AuditTarget::new("conversation", channel.id);
  let detail = json!({
    "archived_at": unix_millis(channel.archived_at).0,
    "notified_at": unix_millis(warned_at).0,
  })
  .to_string();
  let audit = NewAuditEntry {
    id,
    maybe_actor: None,
    action: "channel_inactivity_deleted",
    maybe_target_kind: Some(target.kind),
    maybe_target_id: Some(target.id),
    detail: &detail,
  };
  let outcome = state
    .database
    .delete_channel_locked(channel.id, &audit, None, |facts, _epoch| {
      if !facts.is_channel || !facts.is_archived_by_policy {
        return Err(ApiError::Conflict("the channel was restored".into()));
      }
      if facts.maybe_importer.is_some() {
        return Err(ApiError::ImportInProgress);
      }
      Ok(())
    })
    .await;
  let members = match outcome {
    Ok(Some(members)) => members,
    Ok(None) | Err(ApiError::Conflict(_) | ApiError::ImportInProgress) => return Ok(false),
    Err(error) => return Err(error),
  };
  conversations::announce_deleted(state, channel.id, &members, None).await;
  crate::webhooks::emit_for_conversation(
    state,
    WebhookEventKind::ChannelDeleted,
    channel.id,
    None,
    None,
    None,
  )
  .await;
  Ok(true)
}

/// Mails the owners of a channel (its channel admins, else its space's admins). The mail names no
/// channel. Returns how many mails went out.
async fn mail_owners(
  state: &AppState,
  conversation: ConversationId,
  subject: &str,
  body: &str,
  days: u32,
) -> Result<usize, ApiError> {
  let recipients = state.database.inactivity_notice_recipients(conversation).await?;
  if recipients.is_empty() {
    return Ok(0);
  }
  let emails = state.database.get_user_emails(&recipients).await?;
  let settings = state.settings.get();
  let brands = state.brands.current();
  let mut sent = 0;
  for user in recipients {
    let Some(address) = emails.get(&user) else {
      continue;
    };
    let brand = brands.for_email(address);
    let Some(mailer) = state.mailer_for(brand) else {
      continue;
    };
    let url = brand.base_url(&settings.mail.public_base_url);
    let mail = OutgoingMail {
      to_address: address.clone(),
      maybe_to_name: None,
      subject: subject.to_owned(),
      text: body.replace("{days}", &days.to_string()).replace("{url}", &url),
      method: Method::Request,
      ics: String::new(),
      maybe_from_name: None,
      maybe_from_address: None,
    }
    .as_brand(brand);
    match mailer.send(&mail).await {
      Ok(()) => sent += 1,
      Err(error) => warn!(%error, user_id = %user, "mailing an inactivity notice failed"),
    }
  }
  Ok(sent)
}

/// Keeps a channel open: nothing is noticed, archived or deleted for the days asked (the policy's
/// `snooze_days` by default), and a notice already sent is forgotten, so a channel still quiet
/// afterwards is noticed afresh and gets its full time. Whoever manages the channel's properties may
/// (channel admins, space admins, workspace admins and owners).
pub async fn snooze(
  state: &AppState,
  caller: &AuthenticatedUser,
  id: ConversationId,
  request: SnoozeInactivityRequest,
) -> Result<ChannelInactivityStatus, ApiError> {
  let access = load_conversation(state, caller, id).await?;
  if access.view.conversation.kind != ConversationKind::Channel {
    return Err(ApiError::NotAChannel);
  }
  require(
    state,
    caller,
    Permission::ManageChannelProperties,
    &access.target(),
  )?;
  let policy = state.policy.get().inactivity.clone();
  let days = request.maybe_days.unwrap_or(policy.snooze_days);
  if !(1..=MAX_INACTIVITY_SNOOZE_DAYS).contains(&days) {
    return Err(ApiError::InvalidRequest(format!(
      "days must be between 1 and {MAX_INACTIVITY_SNOOZE_DAYS}"
    )));
  }
  let until = OffsetDateTime::now_utc() + Duration::days(i64::from(days));
  if !state.database.snooze_inactivity(id, until).await? {
    return Err(ApiError::conversation_not_found());
  }
  audit::record(
    state,
    Some(caller.user.id),
    "channel_inactivity_snoozed",
    Some(AuditTarget::new("conversation", id)),
    &json!({"days": days}),
  )
  .await?;
  Ok(ChannelInactivityStatus {
    maybe_snoozed_until: Some(unix_millis(until)),
    is_archived_by_policy: false,
    ..ChannelInactivityStatus::default()
  })
}

/// Snoozes a channel that was just restored, so a quiet one isn't archived again at the next run.
/// Nothing while the policy is off.
pub async fn snooze_restored(state: &AppState, conversation: ConversationId) -> Result<(), ApiError> {
  let policy = state.policy.get().inactivity.clone();
  if policy.is_enabled {
    let until = OffsetDateTime::now_utc() + Duration::days(i64::from(policy.snooze_days));
    state.database.snooze_inactivity(conversation, until).await?;
  }
  Ok(())
}

/// What the admin console shows of a channel's inactivity state; `None` when there is nothing.
pub fn status_of(
  policy: &InactivityPolicy,
  row: &teamcraft_db::AdminConversationRow,
) -> Option<ChannelInactivityStatus> {
  let archive_at = (policy.archives() && !row.is_archived)
    .then_some(row.maybe_inactivity_notified_at)
    .flatten()
    .map(|notified| unix_millis(notified + Duration::days(i64::from(policy.archive_after_more_days))));
  let delete_at = row
    .is_archived_by_policy
    .then_some(row.maybe_archived_at)
    .flatten()
    .zip(
      policy
        .maybe_delete_after_archived_days
        .filter(|_| policy.is_enabled),
    )
    .map(|(archived, days)| unix_millis(archived + Duration::days(i64::from(days))));
  let status = ChannelInactivityStatus {
    maybe_notified_at: row.maybe_inactivity_notified_at.map(unix_millis),
    maybe_archive_at: archive_at,
    maybe_snoozed_until: row.maybe_inactivity_snoozed_until.map(unix_millis),
    is_archived_by_policy: row.is_archived_by_policy,
    maybe_delete_at: delete_at,
  };
  (status != ChannelInactivityStatus::default()).then_some(status)
}

/// Deactivates local guest accounts that belonged to no space for the signed policy's period
/// (`guests.deactivate_accounts_after_days`). Reversible: an admin reactivates the account. The clock
/// starts when the account has no space left, and stops if it gets one. At most
/// `[guests] max_deactivations_per_run` per run. Off while the policy has no period. Returns how many.
pub async fn deactivate_idle_guest_accounts(
  state: &AppState,
  now: OffsetDateTime,
) -> Result<usize, ApiError> {
  let Some(days) = state.policy.get().guests.maybe_deactivate_accounts_after_days else {
    return Ok(0);
  };
  state.database.track_memberless_guest_accounts(now).await?;
  let limit = i64::from(state.settings.get().guests.max_deactivations_per_run);
  let idle = state
    .database
    .list_idle_guest_accounts(now - Duration::days(i64::from(days)), limit)
    .await?;
  let mut deactivated = 0;
  for user in idle {
    let note = format!("local guest account without a space for {days} days");
    match deactivate_user(
      state,
      None,
      user,
      DeactivationReason::Admin,
      &note,
      AfterDeactivation::KeepChats,
    )
    .await
    {
      Ok(true) => deactivated += 1,
      Ok(false) => {},
      Err(error) => warn!(%error, user_id = %user, "deactivating an idle guest account failed"),
    }
  }
  Ok(deactivated)
}
