//! Push registration with the device's push key (e2ee.md §7) and typing indicators.

use std::collections::HashMap;

use rand_core::OsRng;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  Base64Bytes, ConversationId, GatewayClientMessage, PushProvider, PushWakeUp, RegisterPushRequest, UserId,
};

use super::Client;
use crate::api::ServerApi;
use crate::error::CoreError;
use crate::push_preview::{self, PUSH_KEY_RECORD};
use crate::records;
use crate::store::{LocalStore, Table};

/// How long a "typing" indicator stays without a new `typing_start` (the server throttles to 3 s).
pub const TYPING_TIMEOUT_MS: i64 = 6_000;

/// Who is typing where, with the time the indicator was last refreshed. Ephemeral, not stored.
#[derive(Debug, Default)]
pub(crate) struct TypingState {
  seen: HashMap<(ConversationId, UserId), i64>,
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Registers this device's push channel. A fresh random push key is shared with the server only,
  /// so Apple, Google or the push relay see nothing but ciphertext.
  pub async fn register_push(&mut self, provider: PushProvider, token: &str) -> Result<(), CoreError> {
    let push_key = SymmetricKey::generate(&mut OsRng);
    let request = RegisterPushRequest {
      provider,
      token: token.to_string(),
      push_key: Base64Bytes::new(push_key.expose_secret().to_vec()),
    };
    self.api.register_push(self.account.device_id, &request).await?;
    records::put_json(
      &self.store,
      Table::Account,
      PUSH_KEY_RECORD,
      &Base64Bytes::new(push_key.expose_secret().to_vec()),
    )
  }

  /// Removes this device's push registration and forgets the push key, so wake-ups that still
  /// arrive can't be opened (sign-out).
  /// The push key on this device goes first, also when the server can't be reached (sign-out
  /// offline): wake-ups that still arrive can't be opened any more.
  pub async fn unregister_push(&mut self) -> Result<(), CoreError> {
    self.store.delete(Table::Account, PUSH_KEY_RECORD)?;
    self.api.unregister_push(self.account.device_id).await?;
    Ok(())
  }

  /// Opens a sealed push payload (the app or its notification extension), naming the conversation
  /// and message to fetch and decrypt.
  pub fn open_push(&self, sealed: &[u8]) -> Result<PushWakeUp, CoreError> {
    push_preview::open_push_wakeup(&self.store, sealed)
  }

  /// The gateway frame announcing that the user types in a conversation (send it on the open
  /// gateway connection; the server throttles).
  pub fn typing_frame(conversation: ConversationId) -> GatewayClientMessage {
    GatewayClientMessage::Typing {
      conversation_id: conversation,
    }
  }

  /// Other users currently typing in a conversation.
  pub fn typing_users(&self, conversation: ConversationId) -> Vec<UserId> {
    let now = self.now_ms();
    let mut users: Vec<UserId> = self
      .typing
      .seen
      .iter()
      .filter(|((typed_in, user), at)| {
        *typed_in == conversation && *user != self.account.user_id && now - **at < TYPING_TIMEOUT_MS
      })
      .map(|((_, user), _)| *user)
      .collect();
    users.sort();
    users
  }

  pub(crate) fn note_typing(&mut self, conversation: ConversationId, user: UserId) {
    let now = self.now_ms();
    self.typing.seen.insert((conversation, user), now);
    self.typing.seen.retain(|_, at| now - *at < TYPING_TIMEOUT_MS);
  }

  /// A message from someone ends their typing indicator.
  pub(crate) fn clear_typing(&mut self, conversation: ConversationId, user: UserId) {
    self.typing.seen.remove(&(conversation, user));
  }
}
