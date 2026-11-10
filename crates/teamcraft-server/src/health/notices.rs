//! Alerts passed on outside the app (issue #213, docs/operations.md "Health and alerts"): an e-mail
//! to each owner and admin with an address, and an event for the webhooks subscribed to health
//! alerts (docs/webhooks.md).
//!
//! The evaluation only queues; a task of its own delivers, so a mail server that hangs never holds
//! up the checks. What fails is tried again with backoff, from memory: the database may be what
//! failed, so the addresses are the last ones read and the queue works without it. The queue is
//! written to the database behind it (#1646, [`super::persistence`]), so a restart neither loses
//! what was waiting nor sends again what went out. Bounded by count and by age, so an outage that
//! lasts a week does not grow it.
//!
//! A notice names the check, whether it was raised or resolved, and when: nothing else.

use std::collections::{HashSet, VecDeque};

use teamcraft_db::{HealthNoticeChannel, HealthNoticeKind, StoredHealthNotice};
use teamcraft_proto::{HealthCheckId, HealthNoticeId, WebhookEventData, WebhookEventKind};
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
pub const MAX_AGE: Duration = Duration::hours(24);
/// The first retry, doubled per failure in a row of the same delivery.
const FIRST_RETRY: Duration = Duration::seconds(30);
const MAX_RETRY: Duration = Duration::hours(1);
/// Most finished deliveries remembered until they are written: while the database is down for long,
/// the oldest are forgotten, and such a notice may go out once more after a restart.
const MAX_UNWRITTEN_REMOVALS: usize = MAX_PENDING * 5;

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
  pub id: HealthNoticeId,
  pub notice: Notice,
  pub channel: Channel,
  queued_at: OffsetDateTime,
  next_attempt_at: OffsetDateTime,
  failures: u32,
  /// Taken by the delivery task and not back yet: neither due again nor given up meanwhile.
  is_in_flight: bool,
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

/// The deliveries waiting, oldest first, and what changed since they were last written to the
/// database (#1646).
#[derive(Debug, Default)]
pub struct Outbox {
  pending: VecDeque<Pending>,
  /// Queued or changed since the last write.
  changed: HashSet<HealthNoticeId>,
  /// Delivered or given up since the last write, oldest first.
  removed: VecDeque<HealthNoticeId>,
}

/// What the outbox has to write.
#[derive(Debug, Default)]
pub struct OutboxChanges {
  pub changed: Vec<Pending>,
  pub removed: Vec<HealthNoticeId>,
}

impl Outbox {
  /// Queues a notice for a channel. A full outbox drops its oldest delivery.
  pub fn push(&mut self, id: HealthNoticeId, notice: Notice, channel: Channel, now: OffsetDateTime) {
    if self.pending.len() >= MAX_PENDING
      && let Some(dropped) = self.pending.pop_front()
    {
      warn!(check = %dropped.notice.check, "health alert outbox full; the oldest delivery is dropped");
      self.forget(dropped.id);
    }
    self.pending.push_back(Pending {
      id,
      notice,
      channel,
      queued_at: now,
      next_attempt_at: now,
      failures: 0,
      is_in_flight: false,
    });
    self.changed.insert(id);
  }

  /// Takes the deliveries due at `now` and drops the ones too old to matter. What it takes stays
  /// in the outbox, in flight, until [`Self::finish`] or [`Self::retry_later`].
  pub fn take_due(&mut self, now: OffsetDateTime) -> Vec<Pending> {
    let expired: Vec<HealthNoticeId> = self
      .pending
      .iter()
      .filter(|pending| !pending.is_in_flight && now - pending.queued_at >= MAX_AGE)
      .map(|pending| pending.id)
      .collect();
    if !expired.is_empty() {
      warn!(
        dropped = expired.len(),
        "health alert deliveries given up after a day"
      );
    }
    for id in expired {
      self.finish(id);
    }
    self
      .pending
      .iter_mut()
      .filter(|pending| !pending.is_in_flight && pending.next_attempt_at <= now)
      .map(|pending| {
        pending.is_in_flight = true;
        pending.clone()
      })
      .collect()
  }

  /// A delivery was made or cannot be made: it leaves the outbox.
  pub fn finish(&mut self, id: HealthNoticeId) {
    if let Some(position) = self.pending.iter().position(|pending| pending.id == id) {
      self.pending.remove(position);
      self.forget(id);
    }
  }

  /// A delivery failed: tried again after its backoff, in its place before newer notices.
  pub fn retry_later(&mut self, id: HealthNoticeId, now: OffsetDateTime) {
    let Some(pending) = self.pending.iter_mut().find(|pending| pending.id == id) else {
      return;
    };
    pending.is_in_flight = false;
    pending.failures = pending.failures.saturating_add(1);
    pending.next_attempt_at = now + backoff(pending.failures);
    self.changed.insert(id);
  }

  /// Adds the deliveries kept from before a restart, oldest first, to what was queued since.
  pub fn restore(&mut self, kept: Vec<Pending>) {
    for pending in kept {
      if self.pending.iter().all(|other| other.id != pending.id) {
        self.pending.push_back(pending);
      }
    }
    self
      .pending
      .make_contiguous()
      .sort_by_key(|pending| pending.queued_at);
    while self.pending.len() > MAX_PENDING {
      if let Some(dropped) = self.pending.pop_front() {
        self.forget(dropped.id);
      }
    }
  }

  pub fn has_changes(&self) -> bool {
    !self.changed.is_empty() || !self.removed.is_empty()
  }

  /// What changed since the last write; the outbox counts it as written from now on.
  pub fn take_changes(&mut self) -> OutboxChanges {
    let changed = std::mem::take(&mut self.changed);
    OutboxChanges {
      changed: self
        .pending
        .iter()
        .filter(|pending| changed.contains(&pending.id))
        .cloned()
        .collect(),
      removed: self.removed.drain(..).collect(),
    }
  }

  /// A write failed: what it carried is written with the next one.
  pub fn return_changes(&mut self, changes: OutboxChanges) {
    self
      .changed
      .extend(changes.changed.iter().map(|pending| pending.id));
    for id in changes.removed.into_iter().rev() {
      self.removed.push_front(id);
    }
    self.bound_removed();
  }

  /// Notes a delivery that left, so the next write deletes its row.
  fn forget(&mut self, id: HealthNoticeId) {
    self.changed.remove(&id);
    self.removed.push_back(id);
    self.bound_removed();
  }

  fn bound_removed(&mut self) {
    while self.removed.len() > MAX_UNWRITTEN_REMOVALS {
      self.removed.pop_front();
    }
  }
}

impl Pending {
  /// The row the database keeps of it.
  pub fn to_stored(&self) -> StoredHealthNotice {
    StoredHealthNotice {
      id: self.id,
      check: self.notice.check.clone(),
      kind: match self.notice.kind {
        AlertChangeKind::Raised => HealthNoticeKind::Raised,
        AlertChangeKind::Resolved => HealthNoticeKind::Resolved,
      },
      changed_at: self.notice.at,
      channel: match &self.channel {
        Channel::Mail { address } => HealthNoticeChannel::Mail {
          address: address.clone(),
        },
        Channel::Webhook => HealthNoticeChannel::Webhook,
      },
      queued_at: self.queued_at,
      next_attempt_at: self.next_attempt_at,
      failures: self.failures,
    }
  }

  /// A delivery as the database kept it, waiting again.
  pub fn from_stored(stored: StoredHealthNotice) -> Self {
    Self {
      id: stored.id,
      notice: Notice {
        check: stored.check,
        kind: match stored.kind {
          HealthNoticeKind::Raised => AlertChangeKind::Raised,
          HealthNoticeKind::Resolved => AlertChangeKind::Resolved,
        },
        at: stored.changed_at,
      },
      channel: match stored.channel {
        HealthNoticeChannel::Mail { address } => Channel::Mail { address },
        HealthNoticeChannel::Webhook => Channel::Webhook,
      },
      queued_at: stored.queued_at,
      next_attempt_at: stored.next_attempt_at,
      failures: stored.failures,
      is_in_flight: false,
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
  use teamcraft_proto::Id;

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
      outbox.push(id(1), notice(AlertChangeKind::Raised, 0), Channel::Webhook, at(0));
      let mut now = at(0);
      let mut waits = Vec::new();
      for _ in 0..10 {
        let due = outbox.take_due(now);
        assert_eq!(due.len(), 1);
        outbox.retry_later(due[0].id, now);
        assert!(outbox.take_due(now).is_empty(), "not again at once");
        let next = outbox.pending[0].next_attempt_at;
        waits.push((next - now).whole_seconds());
        now = next;
      }
      assert_eq!(&waits[..4], [30, 60, 120, 240]);
      assert_eq!(*waits.last().unwrap(), 3600);
    }

    #[test]
    fn a_delivery_in_flight_is_not_due_again_until_it_is_back() {
      let mut outbox = Outbox::default();
      outbox.push(id(1), notice(AlertChangeKind::Raised, 0), Channel::Webhook, at(0));
      assert_eq!(outbox.take_due(at(0)).len(), 1);
      assert!(outbox.take_due(at(3600)).is_empty());
      assert_eq!(
        outbox.pending.len(),
        1,
        "kept, so a write meanwhile keeps its row"
      );
      outbox.finish(id(1));
      assert!(outbox.pending.is_empty());
    }

    #[test]
    fn deliveries_older_than_a_day_are_given_up() {
      let mut outbox = Outbox::default();
      outbox.push(id(1), notice(AlertChangeKind::Raised, 0), Channel::Webhook, at(0));
      outbox.push(
        id(2),
        notice(AlertChangeKind::Resolved, 0),
        Channel::Webhook,
        at(3600),
      );
      let due = outbox.take_due(at(24 * 3600));
      assert_eq!(due.len(), 1);
      assert_eq!(due[0].notice.kind, AlertChangeKind::Resolved);
      assert_eq!(outbox.take_changes().removed, [id(1)], "its row goes too");
    }

    #[test]
    fn a_full_outbox_drops_the_oldest() {
      let mut outbox = Outbox::default();
      for second in 0..=MAX_PENDING {
        let address = format!("admin{second}@example.com");
        let seconds = i64::try_from(second).unwrap();
        outbox.push(
          id(seconds + 1),
          notice(AlertChangeKind::Raised, 0),
          Channel::Mail { address },
          at(seconds),
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
      outbox.push(id(1), notice(AlertChangeKind::Raised, 0), Channel::Webhook, at(0));
      let first = outbox.take_due(at(0)).remove(0);
      outbox.push(
        id(2),
        notice(AlertChangeKind::Resolved, 0),
        Channel::Webhook,
        at(10),
      );
      outbox.retry_later(first.id, at(10));
      let due = outbox.take_due(at(3600));
      let kinds: Vec<AlertChangeKind> = due.iter().map(|pending| pending.notice.kind).collect();
      assert_eq!(kinds, [AlertChangeKind::Raised, AlertChangeKind::Resolved]);
    }
  }

  mod writing {
    use super::*;

    #[test]
    fn changes_are_handed_out_once_and_back_after_a_failed_write() {
      let mut outbox = Outbox::default();
      outbox.push(id(1), notice(AlertChangeKind::Raised, 0), Channel::Webhook, at(0));
      outbox.push(id(2), notice(AlertChangeKind::Raised, 0), Channel::Webhook, at(0));
      outbox.take_due(at(0));
      outbox.finish(id(1));
      let changes = outbox.take_changes();
      assert_eq!(ids(&changes.changed), [id(2)]);
      assert_eq!(changes.removed, [id(1)]);
      assert!(!outbox.has_changes());

      outbox.return_changes(changes);
      assert!(outbox.has_changes());
      let again = outbox.take_changes();
      assert_eq!(ids(&again.changed), [id(2)]);
      assert_eq!(again.removed, [id(1)]);
    }

    #[test]
    fn a_retry_is_a_change_and_a_row_round_trips() {
      let mut outbox = Outbox::default();
      let address = "admin@example.com".to_owned();
      outbox.push(
        id(1),
        notice(AlertChangeKind::Resolved, 0),
        Channel::Mail { address },
        at(0),
      );
      outbox.take_changes();
      outbox.take_due(at(0));
      outbox.retry_later(id(1), at(0));
      let changes = outbox.take_changes();
      assert_eq!(changes.changed[0].failures, 1);
      let row = changes.changed[0].to_stored();
      assert_eq!(Pending::from_stored(row), changes.changed[0]);
    }

    #[test]
    fn a_restore_adds_what_was_kept_in_order_without_doubles() {
      let mut outbox = Outbox::default();
      outbox.push(
        id(3),
        notice(AlertChangeKind::Raised, 0),
        Channel::Webhook,
        at(30),
      );
      let mut kept = Outbox::default();
      kept.push(id(1), notice(AlertChangeKind::Raised, 0), Channel::Webhook, at(0));
      kept.push(
        id(3),
        notice(AlertChangeKind::Raised, 0),
        Channel::Webhook,
        at(30),
      );
      let rows: Vec<Pending> = kept.pending.iter().cloned().collect();
      outbox.restore(rows);
      let order: Vec<HealthNoticeId> = outbox.pending.iter().map(|pending| pending.id).collect();
      assert_eq!(order, [id(1), id(3)]);
    }

    #[test]
    fn the_removals_waiting_for_a_write_are_bounded() {
      let mut outbox = Outbox::default();
      for n in 1..=i64::try_from(MAX_UNWRITTEN_REMOVALS * 2).unwrap() {
        outbox.push(id(n), notice(AlertChangeKind::Raised, 0), Channel::Webhook, at(0));
        outbox.finish(id(n));
      }
      assert_eq!(outbox.take_changes().removed.len(), MAX_UNWRITTEN_REMOVALS);
    }

    fn ids(pending: &[Pending]) -> Vec<HealthNoticeId> {
      pending.iter().map(|pending| pending.id).collect()
    }
  }

  fn id(n: i64) -> HealthNoticeId {
    HealthNoticeId::from_snowflake(teamcraft_proto::Snowflake::new(n).unwrap())
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
