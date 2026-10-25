//! One-shot cleanup of what builds from before the server entry (#222) left behind (#223). They
//! had one identity provider configured (`TEAMCRAFT_OIDC_ISSUER`) and kept the refresh token as
//! `oidc-refresh-token@<issuer>` in the keyring, the database key as `<issuer>|<user>` in the
//! service `de.zeitgleich.teamcraft.db` and the store as `core/<user>.db`. The server entry keys
//! all of that by server, so the old state is never used again. The refresh token is revoked at its
//! provider (best effort, `teamcraft-client-api` `legacy_session` decides where it may go), then
//! everything is deleted.
//!
//! The keyring can't be listed, so the old issuer is guessed: the environment variable the old
//! builds read, their built-in default, and the issuer of every server the user entered since
//! (most move on to the same server). Only old refresh tokens are looked up under all of them: their
//! entry name can't be one of the new layout. Old database keys share their keyring service with
//! the new ones (`<server>|<user>`), so they are deleted only under the old builds' own issuer, and
//! never under a name a known server's current key has (an identity provider at a server's root).

use std::future::Future;
use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use teamcraft_client_api::legacy_session::{self, LegacyProvider};
use teamcraft_client_api::server_entry::ServerProfile;
use teamcraft_core::store::run_outside_runtime;
use teamcraft_proto::UserId;

use super::{AuthError, HTTP_POLICY, HTTP_TIMEOUT, TokenStore};

/// Notes in the data directory what the cleanup covered already.
const RECORD_FILE: &str = "legacy-cleanup.json";
/// The issuer and client old builds used without the environment variables (the dev stack).
const OLD_DEFAULT_ISSUER: &str = "http://127.0.0.1:8080/realms/teamcraft-dev";
const OLD_DEFAULT_CLIENT_ID: &str = "teamcraft";
const OLD_ISSUER_ENV: &str = "TEAMCRAFT_OIDC_ISSUER";
const OLD_CLIENT_ID_ENV: &str = "TEAMCRAFT_OIDC_CLIENT_ID";
/// Where old builds kept the database keys.
const DB_KEY_SERVICE: &str = "de.zeitgleich.teamcraft.db";
/// Old stores sat directly in this folder of the data directory; new ones are in a folder per server.
const CORE_DIR: &str = "core";

/// Deletes database keys of the old layout.
pub trait LegacyDatabaseKeys: Send + Sync {
  fn delete(&self, account: String) -> impl Future<Output = Result<(), AuthError>> + Send;
}

/// Revokes an old refresh token at its identity provider.
pub trait LegacyRevoker: Send + Sync {
  fn revoke(
    &self,
    provider: &LegacyProvider,
    refresh_token: &SecretString,
  ) -> impl Future<Output = Result<(), String>> + Send;
}

/// The keyring entries of old database keys.
pub struct KeyringDatabaseKeys;

/// Revokes over HTTPS: discovery of the old issuer, then the revocation `legacy_session` allows.
pub struct HttpRevoker {
  http: reqwest::Client,
}

/// Which names of old database keys may be deleted: only those under the old builds' issuer, and
/// none that a known server's current key has (`<server>|<user>`, commands.rs).
struct DatabaseKeyNames<'a> {
  maybe_issuer: Option<&'a str>,
  profiles: &'a [ServerProfile],
}

/// What earlier runs covered: the issuers whose old entries are gone, and the users whose old
/// stores were found (their database keys are named by issuer, which may only become known when
/// the user enters a server of that provider later).
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct CleanupRecord {
  #[serde(default)]
  issuers: Vec<String>,
  #[serde(default)]
  users: Vec<UserId>,
}

/// Cleans up for every candidate issuer earlier runs didn't cover. `profiles`: the servers the
/// user entered. Cheap when nothing is new: no keyring call, one small file read.
pub async fn clean_up_once<S: TokenStore>(
  base_dir: &Path,
  profiles: &[ServerProfile],
  tokens: &S,
  keys: &impl LegacyDatabaseKeys,
  revoker: &impl LegacyRevoker,
) {
  clean_up_once_for(environment_provider(), base_dir, profiles, tokens, keys, revoker).await;
}

/// [`clean_up_once`] with the old builds' provider given, so tests don't depend on the process
/// environment.
async fn clean_up_once_for<S: TokenStore>(
  environment: LegacyProvider,
  base_dir: &Path,
  profiles: &[ServerProfile],
  tokens: &S,
  keys: &impl LegacyDatabaseKeys,
  revoker: &impl LegacyRevoker,
) {
  let maybe_key_issuer = environment.maybe_issuer.clone();
  let record_path = base_dir.join(RECORD_FILE);
  let mut record: CleanupRecord = std::fs::read(&record_path)
    .ok()
    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    .unwrap_or_default();
  let providers: Vec<LegacyProvider> = candidate_providers(environment, profiles)
    .into_iter()
    .filter(|provider| {
      provider
        .maybe_issuer
        .as_ref()
        .is_some_and(|issuer| !record.issuers.contains(issuer))
    })
    .collect();
  let core_dir = base_dir.join(CORE_DIR);
  let stores = legacy_stores(&core_dir);
  if providers.is_empty() && stores.is_empty() {
    return;
  }
  for (user, _) in &stores {
    if !record.users.contains(user) {
      record.users.push(*user);
    }
  }
  let key_names = DatabaseKeyNames {
    maybe_issuer: maybe_key_issuer.as_deref(),
    profiles,
  };
  match clean_up(&providers, &key_names, &record, &stores, tokens, keys, revoker).await {
    Ok(()) => {
      record
        .issuers
        .extend(providers.into_iter().filter_map(|provider| provider.maybe_issuer));
      let saved = serde_json::to_vec(&record)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(&record_path, bytes));
      if let Err(error) = saved {
        tracing::warn!(%error, "could not note the cleanup of the old layout");
      }
    },
    // Tried again on the next start.
    Err(error) => tracing::warn!(%error, "could not clean up everything of the old layout"),
  }
}

/// Revokes and deletes each old refresh token, deletes the old database keys and stores.
async fn clean_up<S: TokenStore>(
  providers: &[LegacyProvider],
  key_names: &DatabaseKeyNames<'_>,
  record: &CleanupRecord,
  stores: &[(UserId, Vec<PathBuf>)],
  tokens: &S,
  keys: &impl LegacyDatabaseKeys,
  revoker: &impl LegacyRevoker,
) -> Result<(), AuthError> {
  for provider in providers {
    let Some(issuer) = &provider.maybe_issuer else {
      continue;
    };
    // `TokenStore` keys by `oidc-refresh-token@<key>`: the old entry is the one keyed by issuer.
    if let Some(token) = tokens.load(issuer).await? {
      if let Err(error) = revoker.revoke(provider, &token).await {
        // The token expires on its own; deleting it locally is what matters.
        tracing::info!(%error, "could not revoke an old refresh token");
      }
      tokens.clear(issuer).await?;
    }
    for user in &record.users {
      if let Some(account) = key_names.old_account(issuer, *user) {
        keys.delete(account).await?;
      }
    }
  }
  // Every key of a store is gone once each issuer ran; the store is useless without it already.
  for (_, files) in stores {
    for file in files {
      std::fs::remove_file(file)
        .or_else(|error| match error.kind() {
          std::io::ErrorKind::NotFound => Ok(()),
          _ => Err(error),
        })
        .map_err(|error| AuthError::Unsupported(format!("old store: {error}")))?;
    }
  }
  if !stores.is_empty() {
    tracing::info!(stores = stores.len(), "removed the stores of the old layout");
  }
  Ok(())
}

impl DatabaseKeyNames<'_> {
  /// The old key's account name of `user` under `issuer`, or `None` when it isn't one to delete.
  fn old_account(&self, issuer: &str, user: UserId) -> Option<String> {
    if self.maybe_issuer != Some(issuer) {
      return None;
    }
    // Some keyrings (Windows) compare names without case; a trailing slash is the same root.
    let root = issuer.trim_end_matches('/');
    let is_current_key = self
      .profiles
      .iter()
      .any(|profile| profile.origin.as_str().eq_ignore_ascii_case(root));
    if is_current_key {
      tracing::info!("kept a database key whose name is a known server's");
      return None;
    }
    Some(format!("{issuer}|{user}"))
  }
}

/// The old builds' provider as their environment variables (or defaults) name it.
fn environment_provider() -> LegacyProvider {
  let var = |name: &str, default: &str| {
    std::env::var(name)
      .ok()
      .filter(|value| !value.is_empty())
      .unwrap_or_else(|| default.to_owned())
  };
  LegacyProvider {
    maybe_issuer: Some(var(OLD_ISSUER_ENV, OLD_DEFAULT_ISSUER)),
    maybe_token_endpoint: None,
    client_id: var(OLD_CLIENT_ID_ENV, OLD_DEFAULT_CLIENT_ID),
  }
}

/// Every provider an old token may be stored under, each issuer once.
fn candidate_providers(environment: LegacyProvider, profiles: &[ServerProfile]) -> Vec<LegacyProvider> {
  let mut providers = vec![environment];
  for profile in profiles {
    let is_known = providers
      .iter()
      .any(|known| known.maybe_issuer.as_deref() == Some(profile.issuer.as_str()));
    if !is_known {
      providers.push(LegacyProvider {
        maybe_issuer: Some(profile.issuer.clone()),
        maybe_token_endpoint: None,
        client_id: profile.client_id.clone(),
      });
    }
  }
  providers
}

/// The old stores in `core_dir`: files named `<user>.db` and their sidecars (`-wal`, `-shm`,
/// `.lock`). The folders of the new layout and anything else stay.
fn legacy_stores(core_dir: &Path) -> Vec<(UserId, Vec<PathBuf>)> {
  let Ok(entries) = std::fs::read_dir(core_dir) else {
    return Vec::new();
  };
  let mut files: Vec<PathBuf> = entries
    .flatten()
    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
    .map(|entry| entry.path())
    .collect();
  files.sort();
  let mut stores: Vec<(UserId, Vec<PathBuf>)> = Vec::new();
  for file in files {
    let Some(user) = file
      .file_name()
      .and_then(|name| name.to_str())
      .and_then(store_owner)
    else {
      continue;
    };
    match stores.iter_mut().find(|(owner, _)| *owner == user) {
      Some((_, owned)) => owned.push(file),
      None => stores.push((user, vec![file])),
    }
  }
  stores
}

/// The user of an old store file name: `<user>.db` with an optional sidecar suffix.
fn store_owner(name: &str) -> Option<UserId> {
  let (user, rest) = name.split_once(".db")?;
  let is_store_file = matches!(rest, "" | "-wal" | "-shm" | "-journal" | ".lock");
  if !is_store_file {
    return None;
  }
  user.parse().ok()
}

impl LegacyDatabaseKeys for KeyringDatabaseKeys {
  async fn delete(&self, account: String) -> Result<(), AuthError> {
    // Keyring calls block (D-Bus on Linux); keep them off the async executor, and out of any
    // runtime context, where zbus' blocking API panics (run_outside_runtime).
    tauri::async_runtime::spawn_blocking(move || {
      run_outside_runtime(move || {
        let entry = keyring::Entry::new(DB_KEY_SERVICE, &account)?;
        match entry.delete_credential() {
          Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
          Err(error) => Err(error),
        }
      })
    })
    .await
    .map_err(|error| AuthError::Unsupported(error.to_string()))?
    .map_err(|error| AuthError::Unsupported(format!("keyring: {error}")))?
    .map_err(|error| AuthError::Unsupported(format!("keyring: {error}")))
  }
}

impl HttpRevoker {
  pub fn new() -> Result<Self, AuthError> {
    let http = reqwest::Client::builder()
      .timeout(HTTP_TIMEOUT)
      // A redirect would carry the token to wherever a hostile answer points.
      .redirect(reqwest::redirect::Policy::none())
      .build()
      .map_err(|error| AuthError::Unsupported(error.to_string()))?;
    Ok(Self { http })
  }
}

impl LegacyRevoker for HttpRevoker {
  async fn revoke(&self, provider: &LegacyProvider, refresh_token: &SecretString) -> Result<(), String> {
    let issuer = provider
      .maybe_issuer
      .as_deref()
      .ok_or_else(|| "no issuer".to_owned())?;
    let response = self
      .http
      .get(legacy_session::discovery_url(issuer))
      .send()
      .await
      .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
      return Err(format!("discovery returned HTTP {}", response.status().as_u16()));
    }
    let document = response.bytes().await.map_err(|error| error.to_string())?;
    let revocation = legacy_session::token_revocation(
      provider,
      issuer,
      &document,
      refresh_token.expose_secret(),
      HTTP_POLICY,
    )
    .map_err(|error| error.to_string())?;
    let answer = self
      .http
      .post(&revocation.url)
      .form(&revocation.form)
      .send()
      .await
      .map_err(|error| error.to_string())?;
    if answer.status().is_success() {
      Ok(())
    } else {
      Err(format!("revocation returned HTTP {}", answer.status().as_u16()))
    }
  }
}

#[cfg(test)]
mod tests {
  use std::sync::Mutex;

  use super::*;
  use crate::auth::MemoryStore;
  use crate::auth::test_dir::TempDir;

  const OLD_ISSUER: &str = "https://id.example.com/realms/old";
  const USER: &str = "123456789";
  const SERVER_URL: &str = "https://chat.example.com";

  #[tokio::test]
  async fn the_old_token_is_revoked_and_everything_old_deleted() {
    let dir = TempDir::new("legacy-all");
    let core = dir.path().join(CORE_DIR);
    std::fs::create_dir_all(core.join("example.com")).unwrap();
    for name in [
      "123456789.db",
      "123456789.db-wal",
      "123456789.db.lock",
      "notes.txt",
    ] {
      std::fs::write(core.join(name), b"x").unwrap();
    }
    std::fs::write(core.join("example.com").join("123456789.db"), b"new").unwrap();
    let tokens = MemoryStore::default();
    tokens.save(OLD_ISSUER, "old-refresh").await.unwrap();
    tokens
      .save(
        "https://chat.example.com|https://id.example.com/realms/old|teamcraft",
        "new-refresh",
      )
      .await
      .unwrap();
    let keys = FakeKeys::default();
    let revoker = FakeRevoker::default();

    // The user entered a server of the same identity provider since.
    clean_up_once_for(
      provider(OLD_ISSUER),
      dir.path(),
      &[profile(OLD_ISSUER)],
      &tokens,
      &keys,
      &revoker,
    )
    .await;

    assert_eq!(
      revoker.revoked(),
      vec![(OLD_ISSUER.to_owned(), "old-refresh".to_owned())]
    );
    assert!(tokens.load(OLD_ISSUER).await.unwrap().is_none());
    assert!(
      tokens
        .load("https://chat.example.com|https://id.example.com/realms/old|teamcraft")
        .await
        .unwrap()
        .is_some(),
      "the new layout's token stays"
    );
    assert!(keys.deleted().contains(&format!("{OLD_ISSUER}|{USER}")));
    assert!(!core.join("123456789.db").exists());
    assert!(!core.join("123456789.db-wal").exists());
    assert!(!core.join("123456789.db.lock").exists());
    assert!(core.join("notes.txt").exists(), "unrelated files stay");
    assert!(
      core.join("example.com").join("123456789.db").exists(),
      "the new layout stays"
    );
    let record: CleanupRecord =
      serde_json::from_slice(&std::fs::read(dir.path().join(RECORD_FILE)).unwrap()).unwrap();
    assert!(record.issuers.contains(&OLD_ISSUER.to_owned()));
  }

  #[tokio::test]
  async fn a_failed_revocation_still_deletes_the_token() {
    let dir = TempDir::new("legacy-offline");
    std::fs::create_dir_all(dir.path()).unwrap();
    let tokens = MemoryStore::default();
    tokens.save(OLD_ISSUER, "old-refresh").await.unwrap();
    let revoker = FakeRevoker {
      is_offline: true,
      ..FakeRevoker::default()
    };
    clean_up_once_for(
      provider(OLD_ISSUER),
      dir.path(),
      &[profile(OLD_ISSUER)],
      &tokens,
      &FakeKeys::default(),
      &revoker,
    )
    .await;
    assert!(tokens.load(OLD_ISSUER).await.unwrap().is_none());
  }

  #[tokio::test]
  async fn a_provider_entered_later_is_cleaned_up_then_and_nothing_twice() {
    let dir = TempDir::new("legacy-later");
    let core = dir.path().join(CORE_DIR);
    std::fs::create_dir_all(&core).unwrap();
    std::fs::write(core.join("123456789.db"), b"x").unwrap();
    let tokens = MemoryStore::default();
    tokens.save(OLD_ISSUER, "old-refresh").await.unwrap();
    let keys = FakeKeys::default();
    let revoker = FakeRevoker::default();

    let environment = || provider(OLD_DEFAULT_ISSUER);

    // First start: no server entered yet; the store and its key under the old issuer go.
    clean_up_once_for(environment(), dir.path(), &[], &tokens, &keys, &revoker).await;
    assert!(!core.join("123456789.db").exists());
    assert!(revoker.revoked().is_empty());
    assert_eq!(keys.deleted(), vec![format!("{OLD_DEFAULT_ISSUER}|{USER}")]);

    // The user enters a server of another provider: its old token goes now, but no database key
    // is deleted under an issuer only an entered server names.
    let entered = [profile(OLD_ISSUER)];
    clean_up_once_for(environment(), dir.path(), &entered, &tokens, &keys, &revoker).await;
    assert_eq!(revoker.revoked().len(), 1);
    assert!(!keys.deleted().contains(&format!("{OLD_ISSUER}|{USER}")));

    // Nothing new: no keyring call at all.
    let before = keys.deleted().len();
    clean_up_once_for(environment(), dir.path(), &entered, &tokens, &keys, &revoker).await;
    assert_eq!(keys.deleted().len(), before);
    assert_eq!(revoker.revoked().len(), 1);
  }

  #[tokio::test]
  async fn the_current_database_key_stays_when_the_issuer_is_the_server_url() {
    // An identity provider at the server's root: the old key `<issuer>|<user>` and the current
    // `<server_url>|<user>` (commands.rs) have the same name.
    for issuer in [
      SERVER_URL.to_owned(),
      format!("{SERVER_URL}/"),
      SERVER_URL.to_uppercase(),
    ] {
      let dir = TempDir::new("legacy-root-issuer");
      let core = dir.path().join(CORE_DIR);
      std::fs::create_dir_all(&core).unwrap();
      std::fs::write(core.join("123456789.db"), b"x").unwrap();
      let tokens = MemoryStore::default();
      tokens.save(&issuer, "old-refresh").await.unwrap();
      let keys = FakeKeys::default();
      let revoker = FakeRevoker::default();

      clean_up_once_for(
        provider(&issuer),
        dir.path(),
        &[profile(&issuer)],
        &tokens,
        &keys,
        &revoker,
      )
      .await;

      assert!(keys.deleted().is_empty(), "kept for issuer {issuer}");
      assert_eq!(revoker.revoked().len(), 1, "the old token still goes");
      assert!(tokens.load(&issuer).await.unwrap().is_none());
    }
  }

  #[test]
  fn every_entered_issuer_is_a_candidate_once() {
    let environment = provider(OLD_ISSUER);
    let profiles = [profile(OLD_ISSUER), profile("https://login.example.org")];
    let issuers: Vec<_> = candidate_providers(environment, &profiles)
      .into_iter()
      .map(|provider| provider.maybe_issuer.unwrap())
      .collect();
    assert_eq!(
      issuers,
      vec![OLD_ISSUER.to_owned(), "https://login.example.org".to_owned()]
    );
  }

  #[test]
  fn only_old_store_files_are_recognised() {
    assert!(store_owner("123456789.db").is_some());
    assert!(store_owner("123456789.db-shm").is_some());
    assert!(store_owner("123456789.dbx").is_none());
    assert!(store_owner("not-a-user.db").is_none());
    assert!(store_owner("123456789.txt").is_none());
  }

  #[derive(Default)]
  struct FakeKeys(Mutex<Vec<String>>);

  impl FakeKeys {
    fn deleted(&self) -> Vec<String> {
      self.0.lock().unwrap().clone()
    }
  }

  impl LegacyDatabaseKeys for FakeKeys {
    async fn delete(&self, account: String) -> Result<(), AuthError> {
      self.0.lock().unwrap().push(account);
      Ok(())
    }
  }

  #[derive(Default)]
  struct FakeRevoker {
    is_offline: bool,
    revoked: Mutex<Vec<(String, String)>>,
  }

  impl FakeRevoker {
    fn revoked(&self) -> Vec<(String, String)> {
      self.revoked.lock().unwrap().clone()
    }
  }

  impl LegacyRevoker for FakeRevoker {
    async fn revoke(&self, provider: &LegacyProvider, refresh_token: &SecretString) -> Result<(), String> {
      if self.is_offline {
        return Err("offline".into());
      }
      self.revoked.lock().unwrap().push((
        provider.maybe_issuer.clone().unwrap_or_default(),
        refresh_token.expose_secret().to_owned(),
      ));
      Ok(())
    }
  }

  fn provider(issuer: &str) -> LegacyProvider {
    LegacyProvider {
      maybe_issuer: Some(issuer.to_owned()),
      maybe_token_endpoint: None,
      client_id: OLD_DEFAULT_CLIENT_ID.to_owned(),
    }
  }

  fn profile(issuer: &str) -> ServerProfile {
    ServerProfile {
      origin: teamcraft_client_api::server_entry::ServerOrigin::parse(
        SERVER_URL,
        teamcraft_client_api::server_entry::HttpPolicy::LoopbackOnly,
      )
      .unwrap(),
      issuer: issuer.to_owned(),
      client_id: OLD_DEFAULT_CLIENT_ID.to_owned(),
      scopes: vec!["openid".to_owned()],
      display_name: "Example".to_owned(),
      server_version: "1.0.0".to_owned(),
      has_local_accounts: false,
      is_login_page_always_shown: true,
      maybe_login_hint: None,
    }
  }
}
