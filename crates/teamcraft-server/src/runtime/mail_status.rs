//! How a mail server is doing: its delivery status for the admin console (kept in memory only), and
//! the transport every mail goes through, which records it and leaves a server that could not be
//! reached alone for a moment (#743). The instance's mail server and each brand's own use it alike.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use teamcraft_proto::{MailDeliveryStatus, UnixMillis};
use time::OffsetDateTime;
use tokio::time::Instant;

use crate::auth::BoxFuture;
use crate::meetings::mailer::{MailError, Mailer, OutgoingMail};

/// Longest error text kept; mail servers can answer with long multi-line replies.
const MAX_ERROR_CHARS: usize = 500;
/// How long a server that could not be reached is left alone. Queues that cross many recipients
/// (digests, notices) then fail fast instead of waiting a full timeout per mail; the mail keeps its
/// usual failure path.
pub const UNREACHABLE_PAUSE: Duration = Duration::from_secs(60);
/// Longest time one mail may take, in multiples of the transport's network timeout (which only
/// limits each step), so a server that answers a byte at a time cannot hold a queue up.
pub const DEADLINE_TIMEOUTS: u32 = 3;

#[derive(Default)]
pub struct MailStatus {
  inner: Mutex<MailDeliveryStatus>,
}

/// A mail server as the rest of the server sees it. Every mail it takes is recorded in its status,
/// except those refused while it is paused (they say nothing new about the server).
pub struct MailTransport {
  inner: Arc<dyn Mailer>,
  status: Arc<MailStatus>,
  deadline: Duration,
  paused_until: Mutex<Option<Instant>>,
}

impl MailStatus {
  pub fn snapshot(&self) -> MailDeliveryStatus {
    self.inner.lock().unwrap_or_else(PoisonError::into_inner).clone()
  }

  pub fn record(&self, result: &Result<(), MailError>, at: OffsetDateTime) {
    let millis = UnixMillis(i64::try_from(at.unix_timestamp_nanos() / 1_000_000).unwrap_or(i64::MAX));
    let mut status = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
    match result {
      Ok(()) => status.maybe_last_success_at = Some(millis),
      Err(error) => {
        status.maybe_last_failure_at = Some(millis);
        status.maybe_last_error = Some(error.to_string().chars().take(MAX_ERROR_CHARS).collect());
      },
    }
  }

  /// Forgets the history, e.g. when the mail server changes.
  pub fn clear(&self) {
    *self.inner.lock().unwrap_or_else(PoisonError::into_inner) = MailDeliveryStatus::default();
  }
}

impl MailTransport {
  /// `timeout` is the network timeout of the transport behind `inner`.
  pub fn new(inner: Arc<dyn Mailer>, status: Arc<MailStatus>, timeout: Duration) -> Self {
    Self {
      inner,
      status,
      deadline: timeout.saturating_mul(DEADLINE_TIMEOUTS),
      paused_until: Mutex::new(None),
    }
  }

  pub fn status(&self) -> &MailStatus {
    &self.status
  }

  async fn attempt_once(&self, mail: &OutgoingMail) -> Result<(), MailError> {
    let result = match tokio::time::timeout(self.deadline, self.inner.send(mail)).await {
      Ok(result) => result,
      Err(_) => Err(MailError::Unreachable(format!(
        "no answer within {} seconds",
        self.deadline.as_secs()
      ))),
    };
    self.status.record(&result, OffsetDateTime::now_utc());
    let maybe_pause = match &result {
      Ok(()) => Some(None),
      Err(MailError::Unreachable(_)) => Some(Some(Instant::now() + UNREACHABLE_PAUSE)),
      Err(_) => None,
    };
    if let Some(pause) = maybe_pause {
      *self.paused_until.lock().unwrap_or_else(PoisonError::into_inner) = pause;
    }
    result
  }

  fn is_paused(&self) -> bool {
    self
      .paused_until
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .is_some_and(|until| Instant::now() < until)
  }
}

impl Mailer for MailTransport {
  fn send<'a>(&'a self, mail: &'a OutgoingMail) -> BoxFuture<'a, Result<(), MailError>> {
    Box::pin(async move {
      if self.is_paused() {
        return Err(MailError::Paused);
      }
      self.attempt_once(mail).await
    })
  }

  /// One attempt, also while the transport is paused: a test mail always asks the server.
  fn attempt<'a>(&'a self, mail: &'a OutgoingMail) -> BoxFuture<'a, Result<(), MailError>> {
    Box::pin(self.attempt_once(mail))
  }
}

#[cfg(test)]
pub(crate) mod tests {
  use std::sync::atomic::{AtomicUsize, Ordering};

  use super::*;
  use crate::meetings::ical::Method;

  #[test]
  fn failures_keep_the_last_success_and_cap_the_error() {
    let status = MailStatus::default();
    let at = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
    status.record(&Ok(()), at);
    status.record(&Err(MailError::Send("x".repeat(2000))), at);
    let snapshot = status.snapshot();
    assert_eq!(
      snapshot.maybe_last_success_at,
      Some(UnixMillis(1_700_000_000_000))
    );
    assert_eq!(
      snapshot.maybe_last_failure_at,
      Some(UnixMillis(1_700_000_000_000))
    );
    assert_eq!(
      snapshot.maybe_last_error.unwrap().chars().count(),
      MAX_ERROR_CHARS
    );
    status.clear();
    assert_eq!(status.snapshot(), MailDeliveryStatus::default());
  }

  mod outage {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn an_unreachable_server_is_left_alone_for_a_moment() {
      let (transport, scripted) = transport(|| Err(MailError::Unreachable("connect".into())));
      assert!(matches!(
        transport.send(&mail()).await,
        Err(MailError::Unreachable(_))
      ));
      assert!(matches!(transport.send(&mail()).await, Err(MailError::Paused)));
      assert_eq!(scripted.calls.load(Ordering::SeqCst), 1);
      assert_eq!(
        transport.status().snapshot().maybe_last_error.as_deref(),
        Some("the mail server could not be reached: connect"),
        "the pause doesn't replace what the server said"
      );

      // A test mail asks the server all the same.
      assert!(matches!(
        transport.attempt(&mail()).await,
        Err(MailError::Unreachable(_))
      ));
      assert_eq!(scripted.calls.load(Ordering::SeqCst), 2);

      tokio::time::advance(UNREACHABLE_PAUSE + Duration::from_secs(1)).await;
      assert!(matches!(
        transport.send(&mail()).await,
        Err(MailError::Unreachable(_))
      ));
      assert_eq!(scripted.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_answers_again_ends_the_pause() {
      let (transport, scripted) = transport(|| Ok(()));
      *transport.paused_until.lock().unwrap() = Some(Instant::now() + UNREACHABLE_PAUSE);
      assert!(matches!(transport.send(&mail()).await, Err(MailError::Paused)));
      transport.attempt(&mail()).await.unwrap();
      transport.send(&mail()).await.unwrap();
      assert_eq!(scripted.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_mail_does_not_pause_the_server() {
      let (transport, scripted) = transport(|| Err(MailError::Send("550 no such user".into())));
      assert!(matches!(transport.send(&mail()).await, Err(MailError::Send(_))));
      assert!(matches!(transport.send(&mail()).await, Err(MailError::Send(_))));
      assert_eq!(scripted.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_server_that_hangs_runs_into_the_deadline() {
      struct Hanging;
      impl Mailer for Hanging {
        fn send<'a>(&'a self, _mail: &'a OutgoingMail) -> BoxFuture<'a, Result<(), MailError>> {
          Box::pin(std::future::pending())
        }
      }
      let transport = MailTransport::new(
        Arc::new(Hanging),
        Arc::new(MailStatus::default()),
        Duration::from_secs(1),
      );
      let started = Instant::now();
      assert!(matches!(
        transport.send(&mail()).await,
        Err(MailError::Unreachable(_))
      ));
      assert_eq!(
        started.elapsed(),
        Duration::from_secs(u64::from(DEADLINE_TIMEOUTS))
      );
      assert!(transport.status().snapshot().maybe_last_failure_at.is_some());
      assert!(matches!(transport.send(&mail()).await, Err(MailError::Paused)));
    }
  }

  pub(crate) fn mail() -> OutgoingMail {
    OutgoingMail {
      to_address: "a@example.com".into(),
      maybe_to_name: None,
      subject: "s".into(),
      text: "t".into(),
      method: Method::Request,
      ics: String::new(),
      maybe_from_name: None,
      maybe_from_address: None,
    }
  }

  /// Answers as told, counting what reaches the server.
  pub(crate) struct Scripted {
    pub(crate) calls: AtomicUsize,
    answer: fn() -> Result<(), MailError>,
  }

  impl Mailer for Scripted {
    fn send<'a>(&'a self, _mail: &'a OutgoingMail) -> BoxFuture<'a, Result<(), MailError>> {
      Box::pin(async move {
        self.calls.fetch_add(1, Ordering::SeqCst);
        (self.answer)()
      })
    }
  }

  pub(crate) fn scripted(answer: fn() -> Result<(), MailError>) -> Arc<Scripted> {
    Arc::new(Scripted {
      calls: AtomicUsize::new(0),
      answer,
    })
  }

  fn transport(answer: fn() -> Result<(), MailError>) -> (MailTransport, Arc<Scripted>) {
    let scripted = scripted(answer);
    let transport = MailTransport::new(
      scripted.clone(),
      Arc::new(MailStatus::default()),
      Duration::from_secs(20),
    );
    (transport, scripted)
  }
}
