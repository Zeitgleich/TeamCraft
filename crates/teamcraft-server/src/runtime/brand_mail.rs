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

use super::mail_status::{MailStatus, MailTransport};
use crate::auth::BoxFuture;
use crate::meetings::mailer::{MailError, Mailer, OutgoingMail};

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

/// A brand's transport: its server's settings for the console and the shared [`MailTransport`].
pub struct BrandTransport {
  view: BrandServerView,
  transport: MailTransport,
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
      transport: MailTransport::new(inner, status, timeout),
    }
  }

  /// What the console shows about this server.
  pub fn to_response(&self) -> BrandMailServer {
    self.view.to_response(self.transport.status().snapshot())
  }
}

impl Mailer for BrandTransport {
  fn send<'a>(&'a self, mail: &'a OutgoingMail) -> BoxFuture<'a, Result<(), MailError>> {
    self.transport.send(mail)
  }

  fn attempt<'a>(&'a self, mail: &'a OutgoingMail) -> BoxFuture<'a, Result<(), MailError>> {
    self.transport.attempt(mail)
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
  use time::OffsetDateTime;

  use super::*;
  use crate::runtime::mail_status::tests::{mail, scripted};

  fn view() -> BrandServerView {
    BrandServerView {
      host: "smtp.example.com".into(),
      port: 587,
      security: SmtpSecurityMode::StartTls,
      maybe_username: None,
      has_password: false,
    }
  }

  fn transport() -> BrandTransport {
    BrandTransport::new(
      view(),
      scripted(|| Ok(())),
      Arc::new(MailStatus::default()),
      Duration::from_secs(20),
    )
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
    servers.install(HashMap::from([("a".to_owned(), Arc::new(transport()))]));
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
