//! Local data of a signed-in account in a browser, for timing the web app's start without a server
//! or a sign-in (#1465, docs/performance.md "Web start on phones").
//!
//! Runs in the page of the app's origin, built with wasm-bindgen (`--target web`) and served next to
//! the app (`dev/start-bench`). [`seed`] enrolls a member against the in-memory fake server, has
//! another member create `channels` channels with `messages` messages each and add the first one,
//! and writes the first member's store the way the web app does: the database `teamcraft-<user>`
//! with its key wrapped by Web Crypto ([`browser_database_key`]). It returns the member's account as
//! `GET /api/v1/me` answers it, which the bench page hands the app as a cached sign-in: the app then
//! starts from that account and its local data, as after a reload or the next tap on the icon, and
//! the account check beside it finds no server.
//!
//! Nothing here is used by the app; it only writes test data of a fake server.

#[cfg(target_arch = "wasm32")]
mod seed {
  use std::rc::Rc;

  use teamcraft_core::api::ServerApi;
  use teamcraft_core::api::fake::FakeServer;
  use teamcraft_core::clock::SystemClock;
  use teamcraft_core::store::{
    IndexedDbStore, LocalStore, MemoryStore, MessageContent, StoredMessage, browser_database_key,
  };
  use teamcraft_core::{ClientSettings, EnrollmentOptions, EnrollmentSecret, PendingEnrollment};
  use teamcraft_crypto::backup::Argon2Settings;
  use teamcraft_crypto::secret::SecretBytes;
  use teamcraft_proto::{ClientMessageId, ConversationId, Id, Snowflake, UserId};
  use wasm_bindgen::prelude::*;

  fn options() -> EnrollmentOptions {
    EnrollmentOptions {
      device_name: "Start bench".into(),
      secret: EnrollmentSecret::Passphrase(SecretBytes::new(b"start bench".to_vec())),
      // Cheap Argon2id: the bench times the start, not the enrollment.
      argon2: Argon2Settings {
        memory_kib: 64,
        iterations: 1,
        parallelism: 1,
      },
      maybe_attestation: None,
    }
  }

  fn id<T: Id>(raw: i64) -> Result<T, JsValue> {
    Snowflake::new(raw).map(T::from_snowflake).map_err(failed)
  }

  /// A message from before the bench's sync, in the shape the store keeps it.
  fn older_message(
    channel: ConversationId,
    channel_number: u32,
    number: u32,
  ) -> Result<StoredMessage, JsValue> {
    let unique = i64::from(channel_number) << 32 | i64::from(number);
    let mut client_id = [0_u8; 16];
    client_id[..8].copy_from_slice(&unique.to_le_bytes());
    Ok(StoredMessage {
      conversation_id: channel,
      client_message_id: ClientMessageId::from_bytes(client_id),
      maybe_server_id: Some(id(1 + unique)?),
      sender_user_id: id::<UserId>(1)?,
      maybe_thread_id: None,
      sent_at_ms: 1_700_000_000_000 + i64::from(number),
      content: MessageContent::Text {
        body: format!("An older message, number {number}, with a few words in it to read."),
        mentions: Vec::new(),
        meta: Default::default(),
      },
      is_edited: false,
      edits: Vec::new(),
      maybe_edit_id: None,
      is_pinned: false,
      maybe_verified_commit: None,
      maybe_server_change: None,
      maybe_unreadable: None,
    })
  }

  fn failed(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
  }

  /// Writes the store of a member with `channels` channels of `messages` messages each, plus
  /// `history` older messages per channel written straight into the store (sync'ing them through
  /// the fake server would take minutes for a store of many thousand records); returns the member's
  /// `GetMeResponse` as JSON.
  #[wasm_bindgen]
  pub async fn seed(channels: u32, messages: u32, history: u32) -> Result<String, JsValue> {
    let server = FakeServer::new();
    let alice_api = server.add_user("Alice");
    let mut alice = PendingEnrollment::new()
      .complete(
        alice_api.clone(),
        Rc::new(MemoryStore::new()),
        Box::new(SystemClock),
        ClientSettings::default(),
        options(),
      )
      .await
      .map_err(failed)?;
    let bob_api = server.add_user("Bob");
    let me = bob_api.me().await.map_err(failed)?;
    let name = format!("teamcraft-{}", me.id);
    let key = browser_database_key(&name).await.map_err(failed)?;
    let store = Rc::new(IndexedDbStore::open(&name, &key).await.map_err(failed)?);
    let mut bob = PendingEnrollment::new()
      .complete(
        bob_api,
        store.clone(),
        Box::new(SystemClock),
        ClientSettings::default().with_notes_to_self(),
        options(),
      )
      .await
      .map_err(failed)?;
    let (mut gateway, _) = bob.connect_gateway().await.map_err(failed)?;
    let space = alice_api.list_spaces().await.map_err(failed)?.spaces[0].id;
    for channel_number in 0..channels {
      let channel = alice
        .create_channel(space, &format!("channel-{channel_number}"))
        .await
        .map_err(failed)?;
      alice.add_member(channel, bob.user_id()).await.map_err(failed)?;
      for message_number in 0..messages {
        let text = format!("Message {message_number} in channel {channel_number}: some words to read.");
        alice.send_text(channel, None, &text).await.map_err(failed)?;
      }
      while bob.pump(&mut gateway).await.map_err(failed)? > 0 {}
      for number in 0..history {
        store
          .upsert_message(&older_message(channel, channel_number, number)?)
          .map_err(failed)?;
      }
      store.flush().await.map_err(failed)?;
    }
    store.flush().await.map_err(failed)?;
    serde_json::to_string(&me).map_err(failed)
  }
}

fn main() {}
