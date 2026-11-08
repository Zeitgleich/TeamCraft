//! Bot accounts (ARCHITECTURE.md §4.5): created by admins, authenticated by a token.
//!
//! A bot is an ordinary user of kind `bot` with the permissions of a guest: it sees only the
//! conversations it was added to. It has its own device and keys like any client, so integration
//! channels stay end-to-end encrypted. The token is 32 random bytes (`tcb_` + base64url); only its
//! SHA-256 is stored, so a database leak does not yield working tokens, and tokens can be rotated
//! and revoked without an identity provider.

use std::time::Duration;

use aws_lc_rs::digest;
use axum::body::Bytes;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::json;
use teamcraft_db::StoredBot;
use teamcraft_proto::{
  BotResponse, BotTokenResponse, CreateBotRequest, DeactivationReason, ListBotsResponse, Permission, Target,
  UserId,
};

use super::access::require;
use super::admin::{AfterDeactivation, deactivate_user};
use super::audit::{self, Target as AuditTarget};
use super::convert::unix_millis;
use super::profiles;
use super::space_images::{run_limited, sanitize};
use super::validate_name;
use crate::auth::AuthenticatedUser;
use crate::error::ApiError;
use crate::state::AppState;

/// Prefix that tells bot tokens from identity provider tokens.
pub const TOKEN_PREFIX: &str = "tcb_";
const TOKEN_BYTES: usize = 32;
const MAX_NAME_CHARS: usize = 100;

pub async fn create_bot(
  state: &AppState,
  caller: &AuthenticatedUser,
  request: CreateBotRequest,
) -> Result<BotTokenResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  let name = validate_name(&request.name, "name", MAX_NAME_CHARS)?;
  let maybe_description = request
    .maybe_description
    .as_deref()
    .map(str::trim)
    .filter(|text| !text.is_empty());
  if let Some(text) = maybe_description {
    super::validate_text(text, "description", state.settings.get().limits.max_text_length)?;
  }
  let (token, hash) = new_token()?;
  let user: UserId = state.next_id()?;
  let bot = state
    .database
    .create_bot(user, &name, maybe_description, &hash, caller.user.id)
    .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "bot_created",
    Some(AuditTarget::new("user", user)),
    &json!({"name": name}),
  )
  .await?;
  Ok(BotTokenResponse {
    bot: bot_response(&bot),
    token,
  })
}

pub async fn list_bots(state: &AppState, caller: &AuthenticatedUser) -> Result<ListBotsResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  let bots = state.database.list_bots().await?;
  Ok(ListBotsResponse {
    bots: bots.iter().map(bot_response).collect(),
  })
}

/// Issues a new token; the old one stops working at once.
pub async fn rotate_token(
  state: &AppState,
  caller: &AuthenticatedUser,
  bot: UserId,
) -> Result<BotTokenResponse, ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  let (token, hash) = new_token()?;
  if !state
    .database
    .replace_bot_token(bot, &hash, caller.user.id)
    .await?
  {
    return Err(ApiError::bot_not_found());
  }
  let stored = state
    .database
    .get_bot(bot)
    .await?
    .ok_or_else(ApiError::bot_not_found)?;
  audit::record(
    state,
    Some(caller.user.id),
    "bot_token_rotated",
    Some(AuditTarget::new("user", bot)),
    &json!({}),
  )
  .await?;
  Ok(BotTokenResponse {
    bot: bot_response(&stored),
    token,
  })
}

/// Revokes the bot: its token stops working, the account is deactivated and removed from all chats
/// (devices removed, sessions ended, groups left).
pub async fn revoke_bot(state: &AppState, caller: &AuthenticatedUser, bot: UserId) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  if !state.database.revoke_bot(bot).await? {
    return Err(ApiError::bot_not_found());
  }
  deactivate_user(
    state,
    Some(caller.user.id),
    bot,
    DeactivationReason::Admin,
    "bot revoked",
    AfterDeactivation::RemoveFromChats,
  )
  .await?;
  audit::record(
    state,
    Some(caller.user.id),
    "bot_revoked",
    Some(AuditTarget::new("user", bot)),
    &json!({}),
  )
  .await?;
  Ok(())
}

/// The active bot a presented token belongs to.
pub async fn find_by_token(state: &AppState, token: &str) -> Result<Option<StoredBot>, ApiError> {
  if !token.starts_with(TOKEN_PREFIX) {
    return Ok(None);
  }
  Ok(state.database.find_bot_by_token_hash(&hash_token(token)).await?)
}

fn new_token() -> Result<(String, Vec<u8>), ApiError> {
  let mut bytes = [0_u8; TOKEN_BYTES];
  aws_lc_rs::rand::fill(&mut bytes).map_err(|_| ApiError::Internal("random source failed".into()))?;
  let token = format!("{TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes));
  let hash = hash_token(&token);
  Ok((token, hash))
}

fn hash_token(token: &str) -> Vec<u8> {
  digest::digest(&digest::SHA256, token.as_bytes())
    .as_ref()
    .to_vec()
}

/// Sets a bot's picture (docs/bridge.md, "Bots"): a profile picture like anybody's, directory data
/// the server sees, re-encoded as PNG here. Its sources show it when they have none of their own.
pub async fn put_bot_avatar(
  state: &AppState,
  caller: &AuthenticatedUser,
  bot: UserId,
  declared_type: &str,
  bytes: Bytes,
) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  ensure_active_bot(state, bot).await?;
  let (max_bytes, timeout) = {
    let limits = &state.settings.get().limits;
    (
      limits.max_avatar_bytes,
      Duration::from_secs(limits.image_decode_timeout_secs),
    )
  };
  if bytes.is_empty() {
    return Err(ApiError::InvalidRequest("the picture is empty".into()));
  }
  if bytes.len() as u64 > max_bytes {
    return Err(ApiError::PayloadTooLarge(format!(
      "avatars are limited to {max_bytes} bytes"
    )));
  }
  let declared_type = declared_type.to_owned();
  let png = run_limited(&state.image_decodes, timeout, move || {
    sanitize(&bytes, &declared_type)
  })
  .await?
  .map_err(|error| ApiError::InvalidRequest(error.to_string()))?;
  profiles::store_avatar(state, bot, png).await?;
  audit::record(
    state,
    Some(caller.user.id),
    "bot_picture_set",
    Some(AuditTarget::new("user", bot)),
    &json!({}),
  )
  .await
}

/// Takes a bot's picture away.
pub async fn delete_bot_avatar(
  state: &AppState,
  caller: &AuthenticatedUser,
  bot: UserId,
) -> Result<(), ApiError> {
  require(state, caller, Permission::ManageUsers, &Target::Instance)?;
  ensure_active_bot(state, bot).await?;
  profiles::remove_avatar(state, bot).await?;
  audit::record(
    state,
    Some(caller.user.id),
    "bot_picture_removed",
    Some(AuditTarget::new("user", bot)),
    &json!({}),
  )
  .await
}

async fn ensure_active_bot(state: &AppState, bot: UserId) -> Result<(), ApiError> {
  match state.database.get_bot(bot).await? {
    Some(stored) if !stored.is_revoked => Ok(()),
    _ => Err(ApiError::bot_not_found()),
  }
}

fn bot_response(bot: &StoredBot) -> BotResponse {
  BotResponse {
    id: bot.user,
    name: bot.name.clone(),
    maybe_description: bot.maybe_description.clone(),
    created_at: unix_millis(bot.created_at),
    is_revoked: bot.is_revoked,
    is_paused: bot.is_paused && !bot.is_revoked,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn tokens_are_prefixed_long_and_hashed_deterministically() {
    let (token, hash) = new_token().unwrap();
    assert!(token.starts_with(TOKEN_PREFIX) && token.len() >= 40);
    assert_eq!(hash, hash_token(&token));
    assert_eq!(hash.len(), 32);
    assert_ne!(new_token().unwrap().0, token);
  }
}
