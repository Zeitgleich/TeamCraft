//! E-mail digest of missed mentions (docs/notifications.md). The server sees mentions as metadata
//! (who is mentioned in which message, who has read what), never content, so the mail can only say
//! how many mentions wait. Nothing about channels, senders or text goes into it.

use teamcraft_db::{DueDigest, StoredEmailDigest};
use teamcraft_proto::{EmailDigestResponse, Id, MessageId, SetEmailDigestRequest, UserId};
use time::{Duration, OffsetDateTime};
use tracing::warn;

use super::retention::snowflake_floor;
use crate::auth::AuthenticatedUser;
use crate::brand::Brand;
use crate::config::EmailDigestConfig;
use crate::error::ApiError;
use crate::meetings::ical::Method;
use crate::meetings::mailer::OutgoingMail;
use crate::state::AppState;

/// Digests looked at per scheduler run; a busy instance catches up on the next run.
const BATCH_SIZE: i64 = 100;
/// A mail that could not be sent is tried again after this long (the watermark stays).
const RETRY_AFTER: Duration = Duration::minutes(5);
const MAX_INTERVAL_MINUTES: u32 = 7 * 24 * 60;

pub async fn get_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
) -> Result<EmailDigestResponse, ApiError> {
  let stored = state.database.get_email_digest(caller.user.id).await?;
  response(state, caller.user.id, stored).await
}

/// Replaces the caller's setting. Turning it on needs a server that sends digests and an e-mail
/// address on the account.
pub async fn put_settings(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: SetEmailDigestRequest,
) -> Result<EmailDigestResponse, ApiError> {
  let config = &state.settings.get().notifications.email_digest;
  if let Some(minutes) = request.maybe_interval_minutes {
    let min_minutes = min_interval_minutes(config);
    if minutes < min_minutes || minutes > MAX_INTERVAL_MINUTES {
      return Err(ApiError::InvalidRequest(format!(
        "interval_minutes must be between {min_minutes} and {MAX_INTERVAL_MINUTES}"
      )));
    }
  }
  if request.is_enabled {
    if !is_sending(state) {
      return Err(ApiError::Unavailable(
        "this server does not send e-mail digests".into(),
      ));
    }
    if !has_email(state, caller.user.id).await? {
      return Err(ApiError::Conflict("the account has no e-mail address".into()));
    }
  }
  let stored = StoredEmailDigest {
    is_enabled: request.is_enabled,
    maybe_interval_minutes: request
      .maybe_interval_minutes
      .map(|minutes| i32::try_from(minutes).unwrap_or(i32::MAX)),
  };
  state
    .database
    .set_email_digest(caller.user.id, stored, OffsetDateTime::now_utc())
    .await?;
  response(state, caller.user.id, stored).await
}

/// Looks at every digest that is due: mails the number of mentions that are still unread after the
/// grace period, then waits for the next interval. Returns how many mails were sent.
pub async fn run_due(state: &AppState, now: OffsetDateTime) -> Result<usize, ApiError> {
  let config = &state.settings.get().notifications.email_digest;
  let (true, Some(mailer)) = (config.is_enabled, state.mailer.get()) else {
    return Ok(0);
  };
  let due = state.database.list_due_email_digests(now, BATCH_SIZE).await?;
  if due.is_empty() {
    return Ok(0);
  }
  let users: Vec<UserId> = due.iter().map(|digest| digest.user).collect();
  let emails = state.database.get_user_emails(&users).await?;
  let upto = MessageId::from_snowflake(snowflake_floor(now - Duration::seconds(secs(config.grace_secs))));
  let mut sent = 0;
  for digest in due {
    let next_check_at = now + interval_of(config, &digest);
    let count = state
      .database
      .count_missed_mentions(digest.user, digest.watermark, upto, now)
      .await?;
    let maybe_address = emails.get(&digest.user);
    let (Some(address), true) = (maybe_address, count > 0) else {
      state
        .database
        .finish_email_digest_check(digest.user, upto, next_check_at, None)
        .await?;
      continue;
    };
    // The digest goes out as the recipient's home brand: sender name and link host.
    let brands = state.brands.current();
    let brand = brands.for_email(address);
    let (subject, text) = compose(state, brand, count);
    let mail = OutgoingMail {
      to_address: address.clone(),
      maybe_to_name: None,
      subject,
      text,
      method: Method::Request,
      ics: String::new(),
      maybe_from_name: None,
      maybe_from_address: None,
    }
    .as_brand(brand);
    match mailer.send(&mail).await {
      Ok(()) => {
        state
          .database
          .finish_email_digest_check(digest.user, upto, next_check_at, Some(now))
          .await?;
        sent += 1;
      },
      Err(error) => {
        warn!(%error, user_id = %digest.user, "sending the mention digest failed");
        // The watermark stays, so the same mentions are counted again at the next try.
        state
          .database
          .finish_email_digest_check(digest.user, digest.watermark, now + RETRY_AFTER, None)
          .await?;
      },
    }
  }
  Ok(sent)
}

fn compose(state: &AppState, brand: &Brand, count: u32) -> (String, String) {
  let config = &state.settings.get().notifications.email_digest;
  let (subject, body) = if count == 1 {
    (&config.subject_one, &config.body_one)
  } else {
    (&config.subject_many, &config.body_many)
  };
  let fill = |template: &str| {
    template.replace("{count}", &count.to_string()).replace(
      "{url}",
      &brand.base_url(&state.settings.get().mail.public_base_url),
    )
  };
  (fill(subject), fill(body))
}

async fn response(
  state: &AppState,
  user: UserId,
  stored: StoredEmailDigest,
) -> Result<EmailDigestResponse, ApiError> {
  let config = &state.settings.get().notifications.email_digest;
  let interval_minutes = stored
    .maybe_interval_minutes
    .and_then(|minutes| u32::try_from(minutes).ok())
    .unwrap_or_else(|| u32::try_from(config.default_interval_secs.div_ceil(60)).unwrap_or(u32::MAX));
  Ok(EmailDigestResponse {
    is_enabled: stored.is_enabled,
    interval_minutes,
    min_interval_minutes: min_interval_minutes(config),
    is_available: is_sending(state) && has_email(state, user).await?,
  })
}

fn is_sending(state: &AppState) -> bool {
  state.settings.get().notifications.email_digest.is_enabled && state.mailer.is_set()
}

async fn has_email(state: &AppState, user: UserId) -> Result<bool, ApiError> {
  Ok(state.database.get_user_emails(&[user]).await?.contains_key(&user))
}

fn min_interval_minutes(config: &EmailDigestConfig) -> u32 {
  u32::try_from(config.min_interval_secs.div_ceil(60).max(1)).unwrap_or(u32::MAX)
}

fn interval_of(config: &EmailDigestConfig, digest: &DueDigest) -> Duration {
  match digest.maybe_interval_minutes {
    Some(minutes) => Duration::minutes(i64::from(minutes)),
    None => Duration::seconds(secs(config.default_interval_secs)),
  }
}

fn secs(value: u64) -> i64 {
  i64::try_from(value).unwrap_or(i64::MAX / 4)
}
