//! The mail servers brands have of their own (admin.md §6.2). Each is a transport of its own: its
//! own connections, its own delivery status, and nothing shared with the other brands or the
//! instance's server, so a broken one only fails its own mail. Replaced as a whole when an admin
//! changes one.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use futures_util::future::join_all;
use teamcraft_proto::{BrandMailServer, MailDeliveryStatus, SmtpSecurityMode};
use time::OffsetDateTime;
use tokio::time::Instant;

use super::mail_status::MailStatus;
use crate::auth::BoxFuture;
use crate::meetings::mailer::{MailError, Mailer, OutgoingMail};

/// How long a server that could not be reached is left alone. Queues that cross many recipients
/// then fail fast instead of waiting a full timeout per mail; the mail keeps its usual failure path.
const UNREACHABLE_PAUSE: Duration = Duration::from_secs(60);
/// Longest time one mail may take, in multiples of the transport's network timeout (which only
/// limits each step), so a server that answers a byte at a time cannot hold a queue up.
const DEADLINE_TIMEOUTS: u32 = 3;

/// The transports in force by brand id.
#[derive(Default)]
pub struct BrandMailServers {
  current: RwLock<Arc<HashMap<String, Arc<BrandTransport>>>>,
  /// Kept across rebuilds, so changing one brand's sender doesn't forget its delivery history.
  statuses: Mutex<HashMap<String, Arc<MailStatus>>>,
}

/// One brand's own mail server, as the console shows it (no password).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrandServerView {
  pub host: String,
  pub port: u16,
  pub security: SmtpSecurityMode,
  pub maybe_username: Option<String>,
  pub has_password: bool,
}

/// Which mail server a mail goes through.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MailRoute {
  Instance,
  Brand(String),
}

/// A brand's transport. Every mail it takes is recorded in its status, except those refused while
/// it is paused (they say nothing new about the server).
pub struct BrandTransport {
  view: BrandServerView,
  inner: Arc<dyn Mailer>,
  status: Arc<MailStatus>,
  deadline: Duration,
  paused_until: Mutex<Option<Instant>>,
}

/// Stands in for a brand's server when its settings can't be used (a password that can't be
/// decrypted, unusable settings): its mail fails and says why, it never goes elsewhere.
pub struct BrokenMailer {
  reason: String,
}

impl BrandMailServers {
  /// Puts the given transports in force.
  pub fn install(&self, transports: HashMap<String, Arc<BrandTransport>>) {
    self
      .statuses
      .lock()
      .unwrap_or_else(PoisonError::into_inner)
      .retain(|id, _| transports.contains_key(id));
    *self.current.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(transports);
  }

  /// The delivery status of a brand's server; the same one across rebuilds.
  pub fn status_of(&self, brand_id: &str) -> Arc<MailStatus> {
    Arc::clone(
      self
        .statuses
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(brand_id.to_owned())
        .or_default(),
    )
  }

  pub fn transport(&self, brand_id: &str) -> Option<Arc<BrandTransport>> {
    self.snapshot().get(brand_id).cloned()
  }

  pub fn mailer_for(&self, brand_id: &str) -> Option<Arc<dyn Mailer>> {
    self
      .transport(brand_id)
      .map(|transport| transport as Arc<dyn Mailer>)
  }

  pub fn route_of(&self, brand_id: &str) -> MailRoute {
    if self.snapshot().contains_key(brand_id) {
      MailRoute::Brand(brand_id.to_owned())
    } else {
      MailRoute::Instance
    }
  }

  pub fn has_any(&self) -> bool {
    !self.snapshot().is_empty()
  }

  fn snapshot(&self) -> Arc<HashMap<String, Arc<BrandTransport>>> {
    Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner))
  }
}

impl BrandServerView {
  fn to_response(&self, status: MailDeliveryStatus) -> BrandMailServer {
    BrandMailServer {
      host: self.host.clone(),
      port: self.port,
      security: self.security.clone(),
      maybe_username: self.maybe_username.clone(),
      has_password: self.has_password,
      status,
    }
  }
}

impl BrandTransport {
  /// `timeout` is the network timeout of the transport behind `inner`.
  pub fn new(
    view: BrandServerView,
    inner: Arc<dyn Mailer>,
    status: Arc<MailStatus>,
    timeout: Duration,
  ) -> Self {
    Self {
      view,
      inner,
      status,
      deadline: timeout.saturating_mul(DEADLINE_TIMEOUTS),
      paused_until: Mutex::new(None),
    }
  }

  /// What the console shows about this server.
  pub fn to_response(&self) -> BrandMailServer {
    self.view.to_response(self.status.snapshot())
  }

  /// One attempt, also while the transport is paused: a test mail always asks the server.
  pub async fn attempt(&self, mail: &OutgoingMail) -> Result<(), MailError> {
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

impl Mailer for BrandTransport {
  fn send<'a>(&'a self, mail: &'a OutgoingMail) -> BoxFuture<'a, Result<(), MailError>> {
    Box::pin(async move {
      if self.is_paused() {
        return Err(MailError::Paused);
      }
      self.attempt(mail).await
    })
  }
}

impl BrokenMailer {
  pub fn new(reason: impl Into<String>) -> Self {
    Self {
      reason: reason.into(),
    }
  }
}

impl Mailer for BrokenMailer {
  fn send<'a>(&'a self, _mail: &'a OutgoingMail) -> BoxFuture<'a, Result<(), MailError>> {
    Box::pin(async move { Err(MailError::Build(self.reason.clone())) })
  }
}

/// Runs `run` on the items of each route side by side: a job that mails many people must not let
/// a server that hangs hold up the mail of the other brands. Inside a route the order stays.
pub async fn run_per_route<T, R, F, Fut>(items: Vec<(MailRoute, T)>, run: F) -> Vec<R>
where
  F: Fn(Vec<T>) -> Fut,
  Fut: Future<Output = R>,
{
  let mut queues: HashMap<MailRoute, Vec<T>> = HashMap::new();
  for (route, item) in items {
    queues.entry(route).or_default().push(item);
  }
  join_all(queues.into_values().map(|queue| Box::pin(run(queue)))).await
}

#[cfg(test)]
mod tests {
  use std::sync::atomic::{AtomicUsize, Ordering};

  use super::*;
  use crate::meetings::ical::Method;

  fn mail() -> OutgoingMail {
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

  fn view() -> BrandServerView {
    BrandServerView {
      host: "smtp.example.com".into(),
      port: 587,
      security: SmtpSecurityMode::StartTls,
      maybe_username: None,
      has_password: false,
    }
  }

  /// Answers as told, counting what reaches the server.
  struct Scripted {
    calls: AtomicUsize,
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

  fn transport(answer: fn() -> Result<(), MailError>) -> (BrandTransport, Arc<Scripted>) {
    let scripted = Arc::new(Scripted {
      calls: AtomicUsize::new(0),
      answer,
    });
    let transport = BrandTransport::new(
      view(),
      scripted.clone(),
      Arc::new(MailStatus::default()),
      Duration::from_secs(20),
    );
    (transport, scripted)
  }

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
      transport.to_response().status.maybe_last_error.as_deref(),
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
    let transport = BrandTransport::new(
      view(),
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
    assert!(transport.to_response().status.maybe_last_failure_at.is_some());
  }

  #[tokio::test]
  async fn a_broken_transport_fails_and_says_why() {
    let error = BrokenMailer::new("no sender").send(&mail()).await.unwrap_err();
    assert!(error.to_string().contains("no sender"));
  }

  #[tokio::test]
  async fn the_status_stays_across_rebuilds_until_the_brand_has_no_server() {
    let servers = BrandMailServers::default();
    let status = servers.status_of("a");
    status.record(&Err(MailError::Send("x".into())), OffsetDateTime::now_utc());
    let (transport_a, _) = transport(|| Ok(()));
    servers.install(HashMap::from([("a".to_owned(), Arc::new(transport_a))]));
    assert!(servers.status_of("a").snapshot().maybe_last_error.is_some());
    assert_eq!(servers.route_of("a"), MailRoute::Brand("a".into()));
    assert_eq!(servers.route_of("b"), MailRoute::Instance);
    servers.install(HashMap::new());
    assert!(servers.status_of("a").snapshot().maybe_last_error.is_none());
    assert!(!servers.has_any());
  }

  #[tokio::test]
  async fn one_route_does_not_wait_for_another() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let items = vec![
      (MailRoute::Brand("a".into()), "a"),
      (MailRoute::Instance, "instance"),
    ];
    // Route "a" finishes only after the instance's route has run.
    let done = run_per_route(items, |queue: Vec<&str>| {
      let gate = Arc::clone(&gate);
      async move {
        if queue.contains(&"a") {
          gate.notified().await;
        } else {
          gate.notify_one();
        }
        queue.len()
      }
    });
    let results = tokio::time::timeout(Duration::from_secs(5), done).await.unwrap();
    assert_eq!(results.iter().sum::<usize>(), 2);
  }
}
