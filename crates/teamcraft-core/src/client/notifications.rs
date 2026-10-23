//! Push registration with the device's push key (e2ee.md §7) and typing indicators.

use std::collections::HashMap;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::push_gateway::seal_bound_token;
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_proto::{
  BOUND_TOKEN_PREFIX, Base64Bytes, ConversationId, GatewayClientMessage, PushProvider, PushWakeUp,
  RegisterPushRequest, UserId,
};

use super::Client;
use crate::api::{ApiError, ServerApi};
use crate::error::CoreError;
use crate::push_preview::{self, PUSH_KEY_RECORD};
use crate::records;
use crate::store::{LocalStore, Table};

/// The algorithm of the token key this client can seal to.
const TOKEN_ALGORITHM: &str = "hpke-x25519-hkdf-sha256-chacha20poly1305";

/// How long a "typing" indicator stays without a new `typing_start` (the server throttles to 3 s).
pub const TYPING_TIMEOUT_MS: i64 = 6_000;

/// Who is typing where, with the time the indicator was last refreshed. Ephemeral, not stored.
#[derive(Debug, Default)]
pub(crate) struct TypingState {
  seen: HashMap<(ConversationId, UserId), i64>,
}

/// How long a sealed device token stays valid at the gateway.
pub const PUSH_TOKEN_LIFETIME_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Record of the registration (the raw token is kept in the encrypted local store only).
const PUSH_REGISTRATION_RECORD: &str = "push_registration";

/// What the app registered with the server, to seal it again and to end it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredPushRegistration {
  provider: PushProvider,
  /// The device token as the platform gave it.
  raw_token: String,
  /// Present when the token was sealed for a gateway.
  maybe_sealed: Option<SealedRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SealedRecord {
  gateway_url: String,
  /// The server's ID at the gateway the token is bound to.
  server_id: String,
  sealed_at_ms: i64,
  /// The blob the server holds (`tcgw1:...` is this, encoded): revoked when it is replaced.
  blob: Base64Bytes,
}

/// What sealing a device token needs: the gateway, the server's ID there, and the gateway's key.
struct GatewayPlan {
  gateway_url: String,
  server_id: String,
  public_key: [u8; 32],
}

impl<A: ServerApi, S: LocalStore> Client<A, S> {
  /// Registers this device's push channel. A fresh random push key is shared with the server only,
  /// so Apple, Google or the push relay see nothing but ciphertext. Where the server sends APNs and
  /// FCM pushes through a push gateway, the device token is sealed for that gateway, bound to this
  /// server (the server never holds a usable token).
  pub async fn register_push(&mut self, provider: PushProvider, token: &str) -> Result<(), CoreError> {
    let push_key = SymmetricKey::generate(&mut OsRng);
    self.register_push_with(provider, token, push_key).await
  }

  /// Seals the device token again when it is due (the gateway or the server changed, or the sealed
  /// token is getting old), so a gateway key rotation and the token's expiry cost nothing. Cheap
  /// when nothing is due: one request. Call it when the app starts or comes to the foreground.
  /// Returns whether the registration was renewed.
  pub async fn refresh_push(&mut self) -> Result<bool, CoreError> {
    let Some(stored) =
      records::get_json::<StoredPushRegistration>(&self.store, Table::Account, PUSH_REGISTRATION_RECORD)?
    else {
      return Ok(false);
    };
    let plan = self.gateway_plan(&stored.provider).await?;
    let now = self.now_ms();
    let is_due = match (&stored.maybe_sealed, &plan) {
      (None, None) => false,
      (Some(sealed), Some(plan)) => {
        sealed.server_id != plan.server_id
          || sealed.gateway_url != plan.gateway_url
          || now.saturating_sub(sealed.sealed_at_ms)
            >= i64::try_from(self.settings.push_reseal_after.as_millis()).unwrap_or(i64::MAX)
      },
      // The server started or stopped using a gateway.
      _ => true,
    };
    if !is_due {
      return Ok(false);
    }
    // The wake-ups in flight stay readable: the push key is kept.
    let push_key = self
      .stored_push_key()?
      .unwrap_or_else(|| SymmetricKey::generate(&mut OsRng));
    self
      .register_push_with(stored.provider, &stored.raw_token, push_key)
      .await?;
    Ok(true)
  }

  async fn register_push_with(
    &mut self,
    provider: PushProvider,
    token: &str,
    push_key: SymmetricKey,
  ) -> Result<(), CoreError> {
    let previous =
      records::get_json::<StoredPushRegistration>(&self.store, Table::Account, PUSH_REGISTRATION_RECORD)?;
    let maybe_plan = self.gateway_plan(&provider).await?;
    let (registered_token, maybe_sealed) = match maybe_plan {
      Some(plan) => {
        let sealed_at_ms = self.now_ms();
        let blob = seal_bound_token(
          &plan.public_key,
          provider.as_str(),
          &plan.server_id,
          u64::try_from(sealed_at_ms + PUSH_TOKEN_LIFETIME_MS).unwrap_or(0),
          token,
        )
        .map_err(|_| CoreError::PushGateway("sealing the device token failed".into()))?;
        let registered = format!("{BOUND_TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(&blob));
        let sealed = SealedRecord {
          gateway_url: plan.gateway_url,
          server_id: plan.server_id,
          sealed_at_ms,
          blob: Base64Bytes::new(blob),
        };
        (registered, Some(sealed))
      },
      None => (token.to_string(), None),
    };
    let request = RegisterPushRequest {
      provider: provider.clone(),
      token: registered_token,
      push_key: Base64Bytes::new(push_key.expose_secret().to_vec()),
    };
    self.api.register_push(self.account.device_id, &request).await?;
    records::put_json(
      &self.store,
      Table::Account,
      PUSH_KEY_RECORD,
      &Base64Bytes::new(push_key.expose_secret().to_vec()),
    )?;
    records::put_json(
      &self.store,
      Table::Account,
      PUSH_REGISTRATION_RECORD,
      &StoredPushRegistration {
        provider,
        raw_token: token.to_string(),
        maybe_sealed: maybe_sealed.clone(),
      },
    )?;
    // The sealed token the server held before is useless from now on: end it at the gateway, so
    // the copy a server (or its backups) may still hold stops working. Best effort.
    if let Some(old) = previous.and_then(|stored| stored.maybe_sealed)
      && maybe_sealed.as_ref().is_none_or(|new| new.blob != old.blob)
    {
      self.revoke_sealed(&old).await;
    }
    Ok(())
  }

  /// Removes this device's push registration and forgets the push key, so wake-ups that still
  /// arrive can't be opened (sign-out). A sealed device token is ended at its gateway as well, so
  /// the server (or a copy of its database) can't push to the device any more.
  /// The push key on this device goes first, also when the server can't be reached (sign-out
  /// offline): wake-ups that still arrive can't be opened any more.
  pub async fn unregister_push(&mut self) -> Result<(), CoreError> {
    self.store.delete(Table::Account, PUSH_KEY_RECORD)?;
    let stored =
      records::get_json::<StoredPushRegistration>(&self.store, Table::Account, PUSH_REGISTRATION_RECORD)?;
    self.store.delete(Table::Account, PUSH_REGISTRATION_RECORD)?;
    if let Some(sealed) = stored.and_then(|stored| stored.maybe_sealed) {
      self.revoke_sealed(&sealed).await;
    }
    self.api.unregister_push(self.account.device_id).await?;
    Ok(())
  }

  /// What sealing a token for the server's gateway needs, or `None` when the provider goes to the
  /// push service directly (or the server is too old to say).
  async fn gateway_plan(&self, provider: &PushProvider) -> Result<Option<GatewayPlan>, CoreError> {
    if !matches!(provider, PushProvider::Apns | PushProvider::Fcm) {
      return Ok(None);
    }
    let binding = match self.api.push_gateway().await {
      Ok(response) => response.maybe_gateway,
      // A server from before gateways knows no such route.
      Err(ApiError::Status { status: 404, .. }) => None,
      Err(error) => return Err(error.into()),
    };
    let Some(binding) = binding.filter(|binding| binding.providers.contains(provider)) else {
      return Ok(None);
    };
    // The gateway the app was built for wins over the one the server names: a server can't make a
    // store app seal its tokens for a gateway of its own.
    let gateway_url = self
      .settings
      .maybe_push_gateway_url
      .clone()
      .unwrap_or_else(|| binding.url.clone());
    let info = self.api.gateway_info(&gateway_url).await?;
    if info.token_key.algorithm != TOKEN_ALGORITHM {
      return Err(CoreError::PushGateway(format!(
        "unsupported token algorithm {}",
        info.token_key.algorithm
      )));
    }
    let public_key = <[u8; 32]>::try_from(info.token_key.public_key.as_slice())
      .map_err(|_| CoreError::PushGateway("the gateway's token key has the wrong length".into()))?;
    Ok(Some(GatewayPlan {
      gateway_url,
      server_id: binding.server_id,
      public_key,
    }))
  }

  async fn revoke_sealed(&self, sealed: &SealedRecord) {
    // Best effort: a gateway that can't be reached now leaves the token to expire.
    if let Err(error) = self
      .api
      .revoke_gateway_token(&sealed.gateway_url, sealed.blob.as_slice())
      .await
    {
      tracing::debug!(%error, "revoking a sealed push token failed");
    }
  }

  fn stored_push_key(&self) -> Result<Option<SymmetricKey>, CoreError> {
    let stored: Option<Base64Bytes> = records::get_json(&self.store, Table::Account, PUSH_KEY_RECORD)?;
    Ok(stored.and_then(|bytes| SymmetricKey::from_slice(bytes.as_slice())))
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
