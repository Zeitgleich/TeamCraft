//! Shared helpers for the fake-server tests.
#![allow(clippy::unwrap_used, dead_code)]

use std::sync::Arc;

use teamcraft_core::api::ServerApi;
use teamcraft_core::api::fake::{FakeGateway, FakeServer, FakeServerApi};
use teamcraft_core::clock::ManualClock;
use teamcraft_core::store::{LocalStore, MemoryStore, MessageContent, MessageFilter};
use teamcraft_core::{Client, ClientSettings, EnrollmentOptions, PendingEnrollment};
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
    let store = Arc::new(MemoryStore::new());
    let mut client = PendingEnrollment::new()
      .complete(
        api.clone(),
        store.clone(),
        clock(),
        settings(),
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
    passphrase: SecretBytes::new(passphrase.as_bytes().to_vec()),
    // Cheap Argon2id so tests stay fast; production uses the default (128 MiB).
    argon2: Argon2Settings {
      memory_kib: 64,
      iterations: 1,
      parallelism: 1,
    },
    maybe_attestation: None,
    maybe_org_recovery: None,
    maybe_linked: None,
  }
}
