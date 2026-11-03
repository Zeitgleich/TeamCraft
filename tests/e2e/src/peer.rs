//! A headless client: a real `teamcraft-core` client with its gateway connection, enrolled as a dev user.

use std::sync::Arc;
use std::time::{Duration, Instant};

use teamcraft_core::api::{GatewayConnection, HttpGateway, HttpServerApi};
use teamcraft_core::clock::SystemClock;
use teamcraft_core::store::{LocalStore, MemoryStore, MessageContent, MessageFilter};
use teamcraft_core::workspace::RosterRole;
use teamcraft_core::{Client, ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::{ConversationId, GatewayClientMessage, ThreadId, UserId, WorkspacePolicy};

use crate::stack::Stack;
use crate::tokens::UserTokens;

pub type DevClient<S = Arc<MemoryStore>> = Client<HttpServerApi<UserTokens>, S>;

/// The recovery passphrase every test account uses (also typed into the web client).
pub const PASSPHRASE: &str = "e2e recovery passphrase";
const WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// `S` is the client's store: in memory unless a test needs the disk (`Peer::enroll_with_store`).
pub struct Peer<S = Arc<MemoryStore>> {
  pub name: String,
  pub client: DevClient<S>,
  pub store: S,
  pub gateway: HttpGateway,
  pub tokens: UserTokens,
}

impl Peer {
  /// Enrolls the dev user's first device and connects its gateway.
  pub async fn enroll(stack: &Stack, user: &str) -> Peer {
    Self::enroll_device(stack, user, &format!("{user} e2e device")).await
  }

  pub async fn enroll_device(stack: &Stack, user: &str, device_name: &str) -> Peer {
    Peer::enroll_with_store(stack, user, device_name, Arc::new(MemoryStore::new())).await
  }
}

impl<S: LocalStore + Clone> Peer<S> {
  /// Enrolls the dev user's first device on `store` and connects its gateway.
  pub async fn enroll_with_store(stack: &Stack, user: &str, device_name: &str, store: S) -> Peer<S> {
    let tokens = stack.tokens(user);
    let api = HttpServerApi::new(stack.base_url.clone(), tokens.clone());
    let mut client = PendingEnrollment::new()
      .complete(
        api,
        store.clone(),
        Box::new(SystemClock),
        ClientSettings::default(),
        enrollment_options(device_name),
      )
      .await
      .unwrap_or_else(|error| {
        let log = stack.server_log();
        let tail: Vec<&str> = log.lines().rev().take(25).collect();
        panic!(
          "enrolling {user}: {error}\nserver log (newest first):\n{}",
          tail.join("\n")
        )
      });
    let (gateway, _) = client.connect_gateway().await.expect("gateway connects");
    Peer {
      name: user.to_owned(),
      client,
      store,
      gateway,
      tokens,
    }
  }

  pub fn user_id(&self) -> UserId {
    self.client.user_id()
  }

  /// Owner: edits the signed workspace policy (founding the signed roster with this owner first
  /// if there is none yet). The server enforces its parts from the moment the entry is stored.
  pub async fn set_policy(&mut self, edit: impl FnOnce(&mut WorkspacePolicy)) {
    self
      .client
      .sync_workspace()
      .await
      .expect("syncing the workspace log");
    if self
      .client
      .workspace()
      .expect("workspace state")
      .maybe_root
      .is_none()
    {
      let roles = [(self.user_id(), RosterRole::Owner)];
      self
        .client
        .publish_roster(&roles)
        .await
        .expect("founding the roster");
    }
    let mut policy = self.client.workspace().expect("workspace state").policy;
    edit(&mut policy);
    // A policy that requires a second factor takes effect with its own request: the follow-up call
    // already gets "MFA required" (the dev realm has no second factor).
    if let Err(error) = self.client.publish_policy(policy).await {
      assert!(
        format!("{error:?}").contains("MfaRequired"),
        "publishing the policy: {error}"
      );
    }
  }

  /// Applies the frames that are waiting on the gateway.
  pub async fn pump(&mut self) {
    self
      .client
      .pump(&mut self.gateway)
      .await
      .unwrap_or_else(|error| panic!("{} pumps its gateway: {error}", self.name));
  }

  /// Keeps the gateway session alive (the server closes connections without heartbeats).
  pub async fn heartbeat(&mut self) {
    let _ = self.gateway.send(&GatewayClientMessage::Heartbeat).await;
  }

  /// Like [`Self::pump`], but reconnects when the server closed the connection for lack of
  /// heartbeats (a peer idles while a browser test drives a page).
  pub async fn pump_reconnecting(&mut self) {
    if self.client.pump(&mut self.gateway).await.is_err() {
      let (gateway, _) = self
        .client
        .connect_gateway()
        .await
        .unwrap_or_else(|error| panic!("{} reconnects its gateway: {error}", self.name));
      self.gateway = gateway;
    }
  }

  /// Pumps the gateway until `done` holds; fails after 30 s.
  pub async fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
    let deadline = Instant::now() + WAIT_TIMEOUT;
    loop {
      self.pump().await;
      if done(self) {
        return;
      }
      assert!(
        Instant::now() < deadline,
        "{}: timed out waiting until {what}",
        self.name
      );
      tokio::time::sleep(Duration::from_millis(100)).await;
    }
  }

  /// Waits until this peer is a member of the conversation's encrypted group (a welcome arrived).
  pub async fn wait_joined(&mut self, conversation: ConversationId) {
    self
      .wait_until(&format!("joined {conversation}"), |peer| {
        peer.client.has_group(conversation).unwrap()
      })
      .await;
  }

  /// Waits until a top-level text message with exactly this body is decrypted.
  pub async fn wait_for_text(&mut self, conversation: ConversationId, body: &str) {
    self
      .wait_until(&format!("{body:?} arrives in {conversation}"), |peer| {
        peer.texts(conversation).iter().any(|text| text == body)
      })
      .await;
  }

  /// Decrypted top-level texts of a conversation, oldest first.
  pub fn texts(&self, conversation: ConversationId) -> Vec<String> {
    self
      .store
      .messages(conversation, MessageFilter::TopLevel)
      .unwrap()
      .into_iter()
      .filter_map(|message| match message.content {
        MessageContent::Text { body, .. } => Some(body),
        _ => None,
      })
      .collect()
  }

  /// Decrypted replies of a thread, oldest first.
  pub fn thread_texts(&self, conversation: ConversationId, thread: ThreadId) -> Vec<String> {
    self
      .store
      .messages(conversation, MessageFilter::Thread(thread))
      .unwrap()
      .into_iter()
      .filter_map(|message| match message.content {
        MessageContent::Text { body, .. } => Some(body),
        _ => None,
      })
      .collect()
  }
}

/// Lets several peers exchange what is in flight: pumps each of them for `duration`.
pub async fn settle(peers: &mut [&mut Peer], duration: Duration) {
  let deadline = Instant::now() + duration;
  while Instant::now() < deadline {
    for peer in peers.iter_mut() {
      peer.pump().await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
  }
}

pub fn enrollment_options(device_name: &str) -> EnrollmentOptions {
  EnrollmentOptions {
    device_name: device_name.into(),
    secret: EnrollmentSecret::Passphrase(SecretBytes::new(PASSPHRASE.as_bytes().to_vec())),
    // Cheap key stretching: the tests enroll many devices.
    argon2: Argon2Settings {
      memory_kib: 1024,
      iterations: 1,
      parallelism: 1,
    },
    maybe_attestation: None,
  }
}
