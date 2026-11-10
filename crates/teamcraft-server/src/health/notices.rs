//! Alerts passed on outside the app (issue #213, docs/operations.md "Health and alerts"): an e-mail
//! to each owner and admin with an address, and an event for the webhooks subscribed to health
//! alerts (docs/webhooks.md).
//!
//! The evaluation only queues; a task of its own delivers, so a mail server that hangs never holds
//! up the checks. What fails is tried again with backoff, from memory: the database may be what
//! failed, so the addresses are the last ones read and the queue is not in it. Bounded by count and
//! by age, so an outage that lasts a week does not grow it.
//!
//! A notice names the check, whether it was raised or resolved, and when: nothing else.

use std::collections::VecDeque;

use teamcraft_proto::{HealthCheckId, WebhookEventData, WebhookEventKind};
use time::{Duration, OffsetDateTime};
use tracing::{info, warn};

use super::tracker::AlertChangeKind;
use crate::meetings::ical::Method;
use crate::meetings::mailer::OutgoingMail;
use crate::state::AppState;
use crate::webhooks;

/// Most deliveries waiting at once; the oldest go first when more come.
const MAX_PENDING: usize = 200;
/// A delivery that could not be made for this long is given up.
const MAX_AGE: Duration = Duration::hours(24);
/// The first retry, doubled per failure in a row of the same delivery.
const FIRST_RETRY: Duration = Duration::seconds(30);
const MAX_RETRY: Duration = Duration::hours(1);

/// One alert raised or resolved, as it is passed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
  pub check: HealthCheckId,
  pub kind: AlertChangeKind,
  pub at: OffsetDateTime,
}

/// Where a notice goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Channel {
  /// One owner's or admin's address.
  Mail { address: String },
  /// Every webhook subscribed to the event: one queued event, the webhook queue does the rest.
  Webhook,
}

/// A notice on its way to one channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
  pub notice: Notice,
  pub channel: Channel,
  queued_at: OffsetDateTime,
  next_attempt_at: OffsetDateTime,
  failures: u32,
}

/// What became of one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
  Delivered,
  /// Failed for now: tried again later.
  Retry,
  /// Cannot be delivered (no mail server for the address's brand): dropped.
  Dropped,
}

/// The deliveries waiting, oldest first.
#[derive(Debug, Default)]
pub struct Outbox {
  pending: VecDeque<Pending>,
}

impl Outbox {
  /// Queues a notice for a channel. A full outbox drops its oldest delivery.
  pub fn push(&mut self, notice: Notice, channel: Channel, now: OffsetDateTime) {
    if self.pending.len() >= MAX_PENDING
      && let Some(dropped) = self.pending.pop_front()
    {
      warn!(check = %dropped.notice.check, "health alert outbox full; the oldest delivery is dropped");
    }
    self.pending.push_back(Pending {
      notice,
      channel,
      queued_at: now,
      next_attempt_at: now,
      failures: 0,
    });
  }

  /// Takes the deliveries due at `now` and drops the ones too old to matter.
  pub fn take_due(&mut self, now: OffsetDateTime) -> Vec<Pending> {
    let before = self.pending.len();
    self.pending.retain(|pending| now - pending.queued_at < MAX_AGE);
    if self.pending.len() < before {
      warn!(
        dropped = before - self.pending.len(),
        "health alert deliveries given up after a day"
      );
    }
    let (due, waiting): (Vec<Pending>, Vec<Pending>) = self
      .pending
      .drain(..)
      .partition(|pending| pending.next_attempt_at <= now);
    self.pending = waiting.into();
    due
  }

  /// Puts a delivery that failed back, to be tried again after its backoff.
  pub fn retry_later(&mut self, mut pending: Pending, now: OffsetDateTime) {
    pending.failures = pending.failures.saturating_add(1);
    pending.next_attempt_at = now + backoff(pending.failures);
    // Before the newer ones, so the order of the notices holds where it can.
    let position = self
      .pending
      .iter()
      .position(|other| other.queued_at > pending.queued_at)
      .unwrap_or(self.pending.len());
    self.pending.insert(position, pending);
    while self.pending.len() > MAX_PENDING {
      self.pending.pop_front();
    }
  }
}

/// Makes one attempt.
pub async fn deliver(state: &AppState, pending: &Pending) -> Outcome {
  match &pending.channel {
    Channel::Webhook => deliver_webhook(state, &pending.notice).await,
    Channel::Mail { address } => deliver_mail(state, &pending.notice, address).await,
  }
}

async fn deliver_webhook(state: &AppState, notice: &Notice) -> Outcome {
  let kind = match notice.kind {
    AlertChangeKind::Raised => WebhookEventKind::HealthAlertRaised,
    AlertChangeKind::Resolved => WebhookEventKind::HealthAlertResolved,
  };
  let data = WebhookEventData {
    maybe_check: Some(notice.check.clone()),
    ..WebhookEventData::default()
  };
  match webhooks::try_emit(state, kind, data).await {
    Ok(()) => Outcome::Delivered,
    Err(error) => {
      warn!(%error, check = %notice.check, "queueing the health alert for the webhooks failed; tried again later");
      Outcome::Retry
    },
  }
}

async fn deliver_mail(state: &AppState, notice: &Notice, address: &str) -> Outcome {
  let brands = state.brands.current();
  let brand = brands.for_email(address);
  let Some(mailer) = state.mailer_for(brand) else {
    return Outcome::Dropped;
  };
  let base_url = brand.base_url(&state.settings.get().mail.public_base_url);
  let (subject, text) = compose(notice, &base_url);
  let mail = OutgoingMail {
    to_address: address.to_owned(),
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
      info!(check = %notice.check, "health alert mailed");
      Outcome::Delivered
    },
    Err(error) => {
      // The address stays out of the log.
      warn!(%error, check = %notice.check, "mailing a health alert failed; tried again later");
      Outcome::Retry
    },
  }
}

/// Subject and text of the mail. Operator mail in English, like the server's other system mail:
/// it names the check, its state and the time, and where to look.
pub fn compose(notice: &Notice, base_url: &str) -> (String, String) {
  let check = notice.check.as_str();
  let at = utc_minutes(notice.at);
  match notice.kind {
    AlertChangeKind::Raised => (
      format!("TeamCraft health alert: {check}"),
      format!(
        "The health check \"{check}\" of {base_url} has been failing long enough to be an alert ({at}).\n\n\
         Administration > System health shows what it measures and since when.\n\n\
         You get this mail as an owner or admin. Health alert mail can be turned off under \
         Administration > Settings > Health alerts.\n"
      ),
    ),
    AlertChangeKind::Resolved => (
      format!("TeamCraft health alert resolved: {check}"),
      format!(
        "The health check \"{check}\" of {base_url} is fine again; its alert ended ({at}).\n\n\
         You get this mail as an owner or admin. Health alert mail can be turned off under \
         Administration > Settings > Health alerts.\n"
      ),
    ),
  }
}

fn utc_minutes(at: OffsetDateTime) -> String {
  let at = at.to_offset(time::UtcOffset::UTC);
  format!(
    "{:04}-{:02}-{:02} {:02}:{:02} UTC",
    at.year(),
    u8::from(at.month()),
    at.day(),
    at.hour(),
    at.minute()
  )
}

fn backoff(failures: u32) -> Duration {
  let factor = 2_i32.saturating_pow(failures.saturating_sub(1).min(16));
  (FIRST_RETRY * factor).min(MAX_RETRY)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn the_mail_names_the_check_the_time_and_nothing_else() {
    let (subject, text) = compose(&notice(AlertChangeKind::Raised, 0), "https://chat.example.com");
    assert_eq!(subject, "TeamCraft health alert: backup");
    assert!(text.contains("\"backup\""), "{text}");
    assert!(text.contains("https://chat.example.com"), "{text}");
    assert!(text.contains("2023-11-14 22:13 UTC"), "{text}");
    let (subject, text) = compose(&notice(AlertChangeKind::Resolved, 0), "https://chat.example.com");
    assert_eq!(subject, "TeamCraft health alert resolved: backup");
    assert!(text.contains("fine again"), "{text}");
  }

  mod outbox {
    use super::*;

    #[test]
    fn a_failed_delivery_waits_longer_each_time_up_to_the_cap() {
      let mut outbox = Outbox::default();
      outbox.push(notice(AlertChangeKind::Raised, 0), Channel::Webhook, at(0));
      let mut now = at(0);
      let mut waits = Vec::new();
      for _ in 0..10 {
        let mut due = outbox.take_due(now);
        assert_eq!(due.len(), 1);
        outbox.retry_later(due.remove(0), now);
        assert!(outbox.take_due(now).is_empty(), "not again at once");
        let next = outbox.pending[0].next_attempt_at;
        waits.push((next - now).whole_seconds());
        now = next;
      }
      assert_eq!(&waits[..4], [30, 60, 120, 240]);
      assert_eq!(*waits.last().unwrap(), 3600);
    }

    #[test]
    fn deliveries_older_than_a_day_are_given_up() {
      let mut outbox = Outbox::default();
      outbox.push(notice(AlertChangeKind::Raised, 0), Channel::Webhook, at(0));
      outbox.push(notice(AlertChangeKind::Resolved, 0), Channel::Webhook, at(3600));
      let due = outbox.take_due(at(24 * 3600));
      assert_eq!(due.len(), 1);
      assert_eq!(due[0].notice.kind, AlertChangeKind::Resolved);
      assert!(outbox.pending.is_empty());
    }

    #[test]
    fn a_full_outbox_drops_the_oldest() {
      let mut outbox = Outbox::default();
      for second in 0..=MAX_PENDING {
        let address = format!("admin{second}@example.com");
        outbox.push(
          notice(AlertChangeKind::Raised, 0),
          Channel::Mail { address },
          at(i64::try_from(second).unwrap()),
        );
      }
      assert_eq!(outbox.pending.len(), MAX_PENDING);
      let due = outbox.take_due(at(1000));
      assert_eq!(
        due[0].channel,
        Channel::Mail {
          address: "admin1@example.com".into()
        }
      );
    }

    #[test]
    fn a_retry_keeps_its_place_before_newer_notices() {
      let mut outbox = Outbox::default();
      outbox.push(notice(AlertChangeKind::Raised, 0), Channel::Webhook, at(0));
      let first = outbox.take_due(at(0)).remove(0);
      outbox.push(notice(AlertChangeKind::Resolved, 0), Channel::Webhook, at(10));
      outbox.retry_later(first, at(10));
      let due = outbox.take_due(at(3600));
      let kinds: Vec<AlertChangeKind> = due.iter().map(|pending| pending.notice.kind).collect();
      assert_eq!(kinds, [AlertChangeKind::Raised, AlertChangeKind::Resolved]);
    }
  }

  fn notice(kind: AlertChangeKind, seconds: i64) -> Notice {
    Notice {
      check: HealthCheckId::Backup,
      kind,
      at: at(seconds),
    }
  }

  fn at(seconds: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_700_000_000 + seconds).unwrap()
  }
}
