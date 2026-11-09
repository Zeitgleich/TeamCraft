//! The web sign-in's tokens across page loads and tabs.
//!
//! A reload restores the session with a refresh-token grant instead of a redirect round trip to the
//! identity provider. The tokens are sealed in IndexedDB under a non-extractable Web Crypto key
//! (`teamcraft_core::store::vault_put`), never in plain text in Web Storage. Every tab of the browser
//! shares them: refreshing happens under one Web Lock, and a tab first looks whether another one has
//! refreshed meanwhile, so a rotated refresh token ("Revoke Refresh Token") is never used twice.

#[cfg(target_arch = "wasm32")]
use std::time::Duration;

use secrecy::zeroize::Zeroizing;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use teamcraft_crypto::dpop::{JwkThumbprint, token_bound_key};
use teamcraft_proto::GetMeResponse;

/// Refresh this long before the access token expires.
pub(super) const REFRESH_MARGIN_MS: i64 = 30_000;
#[cfg(target_arch = "wasm32")]
const VAULT_DATABASE: &str = "teamcraft-session";
#[cfg(target_arch = "wasm32")]
const TOKENS_SLOT: &str = "oidc_tokens";
#[cfg(target_arch = "wasm32")]
const ACCOUNT_SLOT: &str = "cached_account";
/// The Web Lock every tab takes to refresh.
#[cfg(target_arch = "wasm32")]
const REFRESH_LOCK: &str = "teamcraft-auth-refresh";
/// How long a tab waits for another one's refresh before it goes ahead alone: a tab that holds the
/// lock longer hangs, and a stuck sign-in is worse than a rare second refresh.
#[cfg(target_arch = "wasm32")]
const LOCK_PATIENCE: Duration = Duration::from_secs(20);
/// The pauses before reading the vault again after IndexedDB failed. A home-screen web app on iOS
/// that the system ended in the background often gets an IndexedDB error on its first reads after
/// the next launch; taking that for "nothing saved" signed it out on every open.
#[cfg(target_arch = "wasm32")]
const READ_RETRY_PAUSES: [Duration; 3] = [
  Duration::from_millis(150),
  Duration::from_millis(500),
  Duration::from_millis(1_500),
];

/// The vault could not be read right now (IndexedDB refused or failed). Says nothing about the
/// sign-in: the saved tokens may well be there, so nobody is signed out for it.
#[derive(Debug, thiserror::Error)]
#[error("the saved sign-in can't be read now: {0}")]
pub(super) struct VaultUnreadable(String);

#[derive(Clone)]
pub(super) struct Tokens {
  pub access: SecretString,
  pub maybe_refresh: Option<SecretString>,
  pub expires_at_ms: i64,
  /// The account's `sub` at the provider, to tell whether saved tokens are this tab's account.
  pub maybe_subject: Option<String>,
}

/// The account the server confirmed last, for the next start (issue #778), with the sign-in it
/// belongs to: it only counts next to saved tokens of the same provider, client and subject, so
/// one account's record never starts the app for another account's tokens.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct CachedAccount {
  pub issuer: String,
  pub client_id: String,
  pub subject: String,
  pub account: GetMeResponse,
}

/// What a refresh does, decided under the lock.
pub(super) enum RefreshPlan {
  /// Another tab refreshed meanwhile (or the saved access token is still good): use its tokens.
  Adopt(Tokens),
  /// Trade `token` at the provider. `is_shared`: the answer is saved for the other tabs.
  Refresh {
    token: SecretString,
    /// The account `token` belongs to, kept if the answer names none.
    maybe_subject: Option<String>,
    /// The key the session is bound to, by what its access token says: the refresh takes a proof
    /// with this key and no other.
    maybe_bound_key: Option<JwkThumbprint>,
    is_shared: bool,
  },
  NotSignedIn,
}

/// Holds the refresh lock while alive.
pub(super) struct RefreshTurn {
  #[cfg(target_arch = "wasm32")]
  _lock: Option<crate::web_tabs::OwnerLock>,
}

/// The vault's record: the provider and account it belongs to, and the tokens.
#[derive(Deserialize)]
struct Saved {
  issuer: String,
  client_id: String,
  subject: String,
  access: SecretString,
  refresh: SecretString,
  expires_at_ms: i64,
}

#[derive(Serialize)]
struct SavedRef<'a> {
  issuer: &'a str,
  client_id: &'a str,
  subject: &'a str,
  access: &'a str,
  refresh: &'a str,
  expires_at_ms: i64,
}

impl Tokens {
  pub fn new(
    access: SecretString,
    maybe_refresh: Option<SecretString>,
    expires_at_ms: i64,
    maybe_subject: Option<String>,
  ) -> Self {
    Self {
      access,
      maybe_refresh,
      expires_at_ms,
      maybe_subject,
    }
  }

  /// Whether the access token is good for more than [`REFRESH_MARGIN_MS`].
  pub fn is_fresh(&self, now_ms: i64) -> bool {
    self.expires_at_ms - REFRESH_MARGIN_MS > now_ms
  }
}

/// Decides a refresh: saved tokens of the same account win over this tab's own (another tab may
/// have rotated the refresh token this tab still holds). Tokens of another account, or of an
/// account this tab can't name, are never taken over nor overwritten. On start (`maybe_memory`
/// is `None`) the saved tokens are the browser's last sign-in. `is_forced`: tokens that are still
/// fresh won't do, the provider has to answer (for what it would say by now).
pub(super) fn plan_refresh(
  maybe_memory: Option<&Tokens>,
  maybe_saved: Option<Tokens>,
  now_ms: i64,
  is_forced: bool,
) -> RefreshPlan {
  let is_shared = match maybe_saved {
    Some(saved) if is_same_account(maybe_memory, &saved) => {
      if saved.is_fresh(now_ms) && !is_forced {
        return RefreshPlan::Adopt(saved);
      }
      let maybe_bound_key = token_bound_key(saved.access.expose_secret());
      if let Some(token) = saved.maybe_refresh {
        return RefreshPlan::Refresh {
          token,
          maybe_subject: saved.maybe_subject,
          maybe_bound_key,
          is_shared: true,
        };
      }
      true
    },
    Some(_) => false,
    None => maybe_memory.is_some_and(|memory| memory.maybe_subject.is_some()),
  };
  match maybe_memory.and_then(|memory| memory.maybe_refresh.as_ref().map(|token| (memory, token))) {
    Some((memory, token)) => RefreshPlan::Refresh {
      token: token.clone(),
      maybe_subject: memory.maybe_subject.clone(),
      maybe_bound_key: token_bound_key(memory.access.expose_secret()),
      is_shared,
    },
    None => RefreshPlan::NotSignedIn,
  }
}

fn is_same_account(maybe_memory: Option<&Tokens>, saved: &Tokens) -> bool {
  match maybe_memory {
    None => saved.maybe_subject.is_some(),
    Some(memory) => memory.maybe_subject.is_some() && memory.maybe_subject == saved.maybe_subject,
  }
}

/// Waits until no other tab refreshes, at most [`LOCK_PATIENCE`].
pub(super) async fn refresh_turn() -> RefreshTurn {
  #[cfg(target_arch = "wasm32")]
  {
    use futures_util::future::{Either, select};
    let waiting = Box::pin(crate::web_tabs::exclusive(REFRESH_LOCK));
    let patience = Box::pin(teamcraft_core::platform::sleep(LOCK_PATIENCE));
    let maybe_lock = match select(waiting, patience).await {
      Either::Left((maybe_lock, _)) => maybe_lock,
      Either::Right(((), _)) => {
        tracing::warn!("another tab holds the sign-in refresh too long; refreshing anyway");
        None
      },
    };
    RefreshTurn { _lock: maybe_lock }
  }
  #[cfg(not(target_arch = "wasm32"))]
  RefreshTurn {}
}

/// The saved tokens, if they belong to this provider and client. `issuer` and `client_id` come from
/// the server, under the refresh lock: tokens of another provider are of no use any more.
/// [`VaultUnreadable`] when IndexedDB failed even after a few tries: not the same as nothing saved.
pub(super) async fn load(issuer: &str, client_id: &str) -> Result<Option<Tokens>, VaultUnreadable> {
  let Some(saved) = read_saved().await? else {
    return Ok(None);
  };
  if saved.issuer != issuer || saved.client_id != client_id {
    forget().await;
    return Ok(None);
  }
  Ok(Some(tokens_of(saved)))
}

/// [`load`] that changes nothing, for what a cached record claims: without the lock, and a record
/// that doesn't match says nothing against the tokens.
pub(super) async fn peek(issuer: &str, client_id: &str) -> Option<Tokens> {
  let saved = read_saved().await.ok()??;
  (saved.issuer == issuer && saved.client_id == client_id).then(|| tokens_of(saved))
}

async fn read_saved() -> Result<Option<Saved>, VaultUnreadable> {
  Ok(vault_read(TOKENS_SLOT).await?.and_then(|bytes| decode(&bytes)))
}

fn tokens_of(saved: Saved) -> Tokens {
  Tokens::new(
    saved.access,
    Some(saved.refresh),
    saved.expires_at_ms,
    Some(saved.subject),
  )
}

/// Saves `tokens` for the next page load and the other tabs. Tokens without a refresh token can't
/// outlive the access token, and tokens of an account that can't be named could be taken over by
/// another account's tab, so nothing is saved then.
pub(super) async fn save(issuer: &str, client_id: &str, tokens: &Tokens) {
  let (Some(refresh), Some(subject)) = (&tokens.maybe_refresh, &tokens.maybe_subject) else {
    forget().await;
    return;
  };
  let saved = SavedRef {
    issuer,
    client_id,
    subject,
    access: tokens.access.expose_secret(),
    refresh: refresh.expose_secret(),
    expires_at_ms: tokens.expires_at_ms,
  };
  match serde_json::to_vec(&saved) {
    Ok(bytes) => vault_write(TOKENS_SLOT, &Zeroizing::new(bytes)).await,
    Err(error) => tracing::warn!(%error, "could not encode the sign-in tokens"),
  }
}

/// Removes the saved tokens and the account they stand for (sign-out, revoked session).
pub(super) async fn forget() {
  vault_clear(TOKENS_SLOT).await;
  forget_account().await;
}

/// The account saved for the next start, if it reads.
pub(super) async fn load_account() -> Option<CachedAccount> {
  let bytes = vault_read(ACCOUNT_SLOT).await.ok()??;
  serde_json::from_slice(&bytes)
    .inspect_err(|_| tracing::debug!("the cached account does not read"))
    .ok()
}

/// Saves the account for the next start.
pub(super) async fn save_account(account: &CachedAccount) {
  match serde_json::to_vec(account) {
    Ok(bytes) => vault_write(ACCOUNT_SLOT, &Zeroizing::new(bytes)).await,
    Err(error) => tracing::warn!(%error, "could not encode the account for the next start"),
  }
}

/// The next start waits for the server again.
pub(super) async fn forget_account() {
  vault_clear(ACCOUNT_SLOT).await;
}

fn decode(bytes: &[u8]) -> Option<Saved> {
  serde_json::from_slice(bytes)
    .inspect_err(|_| tracing::warn!("the saved sign-in tokens do not read"))
    .ok()
}

/// The sealed value of `slot`, read again after a pause while IndexedDB fails
/// ([`READ_RETRY_PAUSES`]).
#[cfg(target_arch = "wasm32")]
async fn vault_read(slot: &str) -> Result<Option<Zeroizing<Vec<u8>>>, VaultUnreadable> {
  let mut pauses = READ_RETRY_PAUSES.iter();
  loop {
    match teamcraft_core::store::vault_get(VAULT_DATABASE, slot).await {
      Ok(found) => return Ok(found),
      Err(error) => {
        let Some(pause) = pauses.next() else {
          tracing::warn!(%error, "could not read the saved sign-in; keeping it for the next try");
          return Err(VaultUnreadable(error.to_string()));
        };
        tracing::warn!(%error, "could not read the saved sign-in, trying again");
        teamcraft_core::platform::sleep(*pause).await;
      },
    }
  }
}

#[cfg(target_arch = "wasm32")]
async fn vault_write(slot: &str, bytes: &[u8]) {
  if let Err(error) = teamcraft_core::store::vault_put(VAULT_DATABASE, slot, bytes).await {
    // The session still works; only the next reload goes through the provider again.
    tracing::warn!(%error, "could not save the sign-in");
  }
}

#[cfg(target_arch = "wasm32")]
async fn vault_clear(slot: &str) {
  if let Err(error) = teamcraft_core::store::vault_delete(VAULT_DATABASE, slot).await {
    tracing::warn!(%error, "could not remove the saved sign-in");
  }
}

// Native builds (tests, desktop) have no browser vault: BrowserAuth only runs in the page.
#[cfg(not(target_arch = "wasm32"))]
const TOKENS_SLOT: &str = "";
#[cfg(not(target_arch = "wasm32"))]
const ACCOUNT_SLOT: &str = "";

#[cfg(not(target_arch = "wasm32"))]
async fn vault_read(_slot: &str) -> Result<Option<Zeroizing<Vec<u8>>>, VaultUnreadable> {
  Ok(None)
}

#[cfg(not(target_arch = "wasm32"))]
async fn vault_write(_slot: &str, _bytes: &[u8]) {}

#[cfg(not(target_arch = "wasm32"))]
async fn vault_clear(_slot: &str) {}

#[cfg(test)]
mod tests {
  use super::*;

  const NOW_MS: i64 = 1_000_000;

  #[test]
  fn a_fresh_saved_token_is_adopted_without_refreshing() {
    let memory = tokens("alice", "access-1", Some("refresh-1"), NOW_MS);
    let saved = tokens("alice", "access-2", Some("refresh-2"), NOW_MS + 300_000);
    let RefreshPlan::Adopt(adopted) = plan_refresh(Some(&memory), Some(saved), NOW_MS, false) else {
      panic!("expected the saved tokens");
    };
    assert_eq!(adopted.access.expose_secret(), "access-2");
  }

  /// The attribute the ID token should name was just written: only the provider can say so.
  #[test]
  fn a_forced_refresh_asks_the_provider_with_the_newest_refresh_token() {
    let memory = tokens("alice", "access-1", Some("refresh-1"), NOW_MS);
    let saved = tokens("alice", "access-2", Some("refresh-2"), NOW_MS + 300_000);
    let RefreshPlan::Refresh { token, .. } = plan_refresh(Some(&memory), Some(saved), NOW_MS, true) else {
      panic!("expected a refresh");
    };
    assert_eq!(token.expose_secret(), "refresh-2");
  }

  /// Another tab rotated the refresh token: this tab must not use its old one.
  #[test]
  fn an_expired_saved_token_is_refreshed_with_the_saved_refresh_token() {
    let memory = tokens("alice", "access-1", Some("refresh-1"), NOW_MS);
    let saved = tokens("alice", "access-2", Some("refresh-2"), NOW_MS + 10_000);
    assert_refresh(
      plan_refresh(Some(&memory), Some(saved), NOW_MS, false),
      "refresh-2",
      true,
    );
  }

  /// Opaque tokens without an ID token name no account: another account's saved tokens must not
  /// be taken over, and this tab's must not be shared.
  #[test]
  fn tokens_of_an_unnamed_account_are_neither_adopted_nor_shared() {
    let mut memory = tokens("alice", "access-1", Some("refresh-1"), NOW_MS);
    memory.maybe_subject = None;
    let saved = tokens("bob", "access-2", Some("refresh-2"), NOW_MS + 300_000);
    assert_refresh(
      plan_refresh(Some(&memory), Some(saved), NOW_MS, false),
      "refresh-1",
      false,
    );
    assert_refresh(
      plan_refresh(Some(&memory), None, NOW_MS, false),
      "refresh-1",
      false,
    );
  }

  #[test]
  fn on_start_the_saved_tokens_are_used() {
    let saved = tokens("alice", "access-2", Some("refresh-2"), NOW_MS);
    assert_refresh(plan_refresh(None, Some(saved), NOW_MS, false), "refresh-2", true);
    assert!(matches!(
      plan_refresh(None, None, NOW_MS, false),
      RefreshPlan::NotSignedIn
    ));
  }

  #[test]
  fn tokens_of_another_account_are_left_alone() {
    let memory = tokens("alice", "access-1", Some("refresh-1"), NOW_MS);
    let saved = tokens("bob", "access-2", Some("refresh-2"), NOW_MS + 300_000);
    assert_refresh(
      plan_refresh(Some(&memory), Some(saved), NOW_MS, false),
      "refresh-1",
      false,
    );
  }

  #[test]
  fn without_saved_tokens_the_tab_refreshes_its_own_and_shares_them() {
    let memory = tokens("alice", "access-1", Some("refresh-1"), NOW_MS);
    assert_refresh(
      plan_refresh(Some(&memory), None, NOW_MS, false),
      "refresh-1",
      true,
    );
    let memory = tokens("alice", "access-1", None, NOW_MS);
    assert!(matches!(
      plan_refresh(Some(&memory), None, NOW_MS, false),
      RefreshPlan::NotSignedIn
    ));
  }

  #[test]
  fn saved_tokens_round_trip_and_garbage_does_not_read() {
    let bytes = serde_json::to_vec(&SavedRef {
      issuer: "https://auth.example.com/realms/a",
      client_id: "teamcraft",
      subject: "alice",
      access: "access",
      refresh: "refresh",
      expires_at_ms: NOW_MS,
    })
    .unwrap();
    let saved = decode(&bytes).unwrap();
    assert_eq!(saved.issuer, "https://auth.example.com/realms/a");
    assert_eq!(saved.refresh.expose_secret(), "refresh");
    assert_eq!(saved.expires_at_ms, NOW_MS);
    assert!(decode(b"").is_none());
    assert!(decode(br#"{"issuer":"x"}"#).is_none());
  }

  fn assert_refresh(plan: RefreshPlan, expected_token: &str, expected_shared: bool) {
    let RefreshPlan::Refresh { token, is_shared, .. } = plan else {
      panic!("expected a refresh");
    };
    assert_eq!(token.expose_secret(), expected_token);
    assert_eq!(is_shared, expected_shared);
  }

  fn tokens(subject: &str, access: &str, maybe_refresh: Option<&str>, expires_at_ms: i64) -> Tokens {
    Tokens {
      access: SecretString::from(access),
      maybe_refresh: maybe_refresh.map(SecretString::from),
      expires_at_ms,
      maybe_subject: Some(subject.to_owned()),
    }
  }
}
