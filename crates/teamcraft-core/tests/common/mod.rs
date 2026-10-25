//! Shared helpers for the fake-server tests.
#![allow(clippy::unwrap_used, dead_code)]

use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::atomic::{AtomicI64, Ordering};

use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeGateway, FakeServer, FakeServerApi};
use teamcraft_core::clock::Clock;
use teamcraft_core::clock::ManualClock;
use teamcraft_core::store::{LocalStore, MemoryStore, MessageContent, MessageFilter};
use teamcraft_core::{Client, ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
use teamcraft_crypto::backup::Argon2Settings;
use teamcraft_crypto::secret::SecretBytes;
use teamcraft_proto::{ConversationId, SpaceId};

pub const PASSPHRASE: &str = "correct horse battery staple";

pub type TestClient = Client<FakeServerApi, Arc<MemoryStore>>;

pub struct Peer {
  pub name: String,
  pub api: FakeServerApi,
  pub store: Arc<MemoryStore>,
  pub client: TestClient,
  pub gateway: FakeGateway,
}

impl Peer {
  pub async fn enroll(server: &FakeServer, name: &str) -> Self {
    Self::enroll_as(server.add_user(name), name).await
  }

  pub async fn enroll_as(api: FakeServerApi, name: &str) -> Self {
    Self::enroll_with(api, name, settings()).await
  }

  /// Like [`Self::enroll`], with other client settings.
  pub async fn enroll_with_settings(server: &FakeServer, name: &str, settings: ClientSettings) -> Self {
    Self::enroll_with(server.add_user(name), name, settings).await
  }

  /// Enrolls a device of the user behind `api` with other client settings.
  pub async fn enroll_with(api: FakeServerApi, name: &str, client_settings: ClientSettings) -> Self {
    Self::enroll_on_clock(api, name, client_settings, clock()).await
  }

  /// Like [`Self::enroll_with`], on a clock the test moves.
  pub async fn enroll_on_clock(
    api: FakeServerApi,
    name: &str,
    client_settings: ClientSettings,
    clock: Box<dyn Clock>,
  ) -> Self {
    let store = Arc::new(MemoryStore::new());
    let mut client = PendingEnrollment::new()
      .complete(
        api.clone(),
        store.clone(),
        clock,
        client_settings,
        options(PASSPHRASE),
      )
      .await
      .unwrap();
    let (gateway, _) = client.connect_gateway().await.unwrap();
    Self {
      name: name.to_string(),
      api,
      store,
      client,
      gateway,
    }
  }

  pub async fn space(&self) -> SpaceId {
    self.api.list_spaces().await.unwrap().spaces[0].id
  }

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

  /// The content key ID the server stored for the text message `text` this peer has.
  pub async fn content_key_of(&self, conversation: ConversationId, text: &str) -> Vec<u8> {
    let server_id = self
      .store
      .messages(conversation, MessageFilter::TopLevel)
      .unwrap()
      .into_iter()
      .find(|message| matches!(&message.content, MessageContent::Text { body, .. } if body == text))
      .and_then(|message| message.maybe_server_id)
      .unwrap();
    self
      .api
      .list_messages(conversation, &teamcraft_proto::ListMessagesQuery::default())
      .await
      .unwrap()
      .messages
      .into_iter()
      .find(|message| message.id == server_id)
      .unwrap()
      .content_key_id
      .as_slice()
      .to_vec()
  }
}

/// Links a second device of `owner` over the relay (e2ee.md §5) and enrolls it.
pub async fn link_device(owner: &mut Peer) -> TestClient {
  let api = owner.api.clone();
  let offer = teamcraft_core::LinkOffer::generate();
  let code = offer.to_code();
  // Not `tokio::join!`: this module is shared with the browser tests, which have no tokio.
  let (new_side, owner_side) = futures_util::future::join(
    teamcraft_core::client::NewDeviceLink::connect(&api, &offer),
    owner.client.start_link_approval(&code),
  )
  .await;
  // The owner's device succeeds only once the new device confirmed the transfer.
  let (approved, received) = futures_util::future::join(
    owner.client.complete_link_approval(owner_side.unwrap()),
    new_side.unwrap().receive_secrets(),
  )
  .await;
  approved.unwrap();
  let link_options = linked_options(received.unwrap());
  PendingEnrollment::new()
    .complete(
      api,
      Arc::new(MemoryStore::new()),
      clock(),
      settings(),
      link_options,
    )
    .await
    .unwrap()
}

/// A UI adapter over a peer's store, as the desktop shell creates it.
#[cfg(feature = "ui")]
pub fn backend_of(peer: &Peer) -> teamcraft_core::ui_backend::CoreChatBackend<Arc<MemoryStore>> {
  teamcraft_core::ui_backend::CoreChatBackend::new(peer.store.clone(), peer.client.user_id(), clock())
}

/// Runs what the UI adapter queued, as the app's sync task does, then sends the outbox.
#[cfg(feature = "ui")]
pub async fn run_commands(
  peer: &mut Peer,
  backend: &teamcraft_core::ui_backend::CoreChatBackend<Arc<MemoryStore>>,
) {
  for command in backend.drain_commands() {
    peer.client.run_ui_command(command).await.unwrap();
  }
  peer.client.flush_outbox().await.unwrap();
}

/// Lets every client process its gateway frames until nobody receives anything new.
pub async fn settle(peers: &mut [&mut Peer]) {
  for _ in 0..50 {
    let mut handled = 0;
    for peer in peers.iter_mut() {
      handled += peer.client.pump(&mut peer.gateway).await.unwrap();
    }
    if handled == 0 {
      return;
    }
  }
  panic!("clients did not settle");
}

pub fn clock() -> Box<ManualClock> {
  Box::new(ManualClock::new(1_800_000_000_000))
}

pub fn settings() -> ClientSettings {
  ClientSettings {
    key_package_target: 3,
    key_package_low_water: 2,
    ..ClientSettings::default()
  }
}

pub fn options(passphrase: &str) -> EnrollmentOptions {
  EnrollmentOptions {
    device_name: "Test device".into(),
    secret: EnrollmentSecret::Passphrase(SecretBytes::new(passphrase.as_bytes().to_vec())),
    // Cheap Argon2id so tests stay fast; production uses the default (128 MiB).
    argon2: Argon2Settings {
      memory_kib: 64,
      iterations: 1,
      parallelism: 1,
    },
    maybe_attestation: None,
  }
}

/// Enrollment of a linked device with the secrets from the existing one.
pub fn linked_options(secrets: teamcraft_core::client::LinkedSecrets) -> EnrollmentOptions {
  EnrollmentOptions {
    secret: EnrollmentSecret::Linked(secrets),
    ..options(PASSPHRASE)
  }
}

// Native only: the browser tests have no tokio.
#[cfg(not(target_arch = "wasm32"))]
/// Wall-clock time that follows tokio's (paused) clock, so a runner's deadlines and its sleeps
/// agree. The wall clock can also jump ahead on its own, as it does when a laptop wakes up (tokio's
/// clock is monotonic and stands still during sleep).
pub struct TokioClock {
  started: tokio::time::Instant,
  jumped_ms: Arc<AtomicI64>,
}

#[cfg(not(target_arch = "wasm32"))]
/// Moves a [`TokioClock`]'s wall clock ahead.
#[derive(Clone)]
pub struct ClockJump(Arc<AtomicI64>);

#[cfg(not(target_arch = "wasm32"))]
impl TokioClock {
  pub fn new() -> (Self, ClockJump) {
    let jumped_ms = Arc::new(AtomicI64::new(0));
    let clock = Self {
      started: tokio::time::Instant::now(),
      jumped_ms: jumped_ms.clone(),
    };
    (clock, ClockJump(jumped_ms))
  }
}

#[cfg(not(target_arch = "wasm32"))]
impl Clock for TokioClock {
  fn now_ms(&self) -> i64 {
    1_800_000_000_000
      + i64::try_from(self.started.elapsed().as_millis()).unwrap()
      + self.jumped_ms.load(Ordering::SeqCst)
  }
}

#[cfg(not(target_arch = "wasm32"))]
impl ClockJump {
  pub fn forward(&self, duration: std::time::Duration) {
    self
      .0
      .fetch_add(i64::try_from(duration.as_millis()).unwrap(), Ordering::SeqCst);
  }
}
