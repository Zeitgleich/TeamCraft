//! A desktop's local secrets in one sealed file (e2ee.md §9): refresh tokens, local sessions and
//! copies of the database and backup keys, under one key that the OS keychain holds.
//!
//! Every keychain access can raise a dialog on macOS, so the keychain is read once per start, for
//! the file's key, and not touched while the app runs. What is in the file is served from memory.
//!
//! - **Tokens and sessions** live only in the file. Their old keychain entries are moved over once,
//!   per entry, the first time each is asked for, and deleted.
//! - **Database and backup keys** stay in the keychain too: the file is a copy that saves the read.
//!   So a damaged file costs a sign-in, never the database.
//! - **A failed keychain call** (locked, denied, no keychain) wipes nothing and is remembered for
//!   [`RETRY_AFTER_FAILURE`], because the UI asks for the core's status again and again and each
//!   attempt would raise another dialog.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use teamcraft_crypto::secret::SymmetricKey;
use teamcraft_crypto::secret_file;
use zeroize::{Zeroize as _, Zeroizing};

use super::{KeySourceError, run_outside_runtime};

/// Keychain service of the file's key and of the tokens kept before the file existed.
const VAULT_SERVICE: &str = "de.zeitgleich.teamcraft";
const VAULT_KEY_ACCOUNT: &str = "secrets-file-key";
const DATABASE_KEY_SERVICE: &str = "de.zeitgleich.teamcraft.db";
/// e2ee.md §6.5.6; the account name is the database key's.
const BACKUP_KEY_SERVICE: &str = "de.zeitgleich.teamcraft.backup-key";
const VAULT_FILE: &str = "secrets.sealed";
const TEMPORARY_FILE: &str = "secrets.sealed.tmp";
/// A file that could not be opened, kept for a look but never read again.
const SET_ASIDE_FILE: &str = "secrets.sealed.damaged";
const DATABASE_KEY_KIND: &str = "database-key";
const BACKUP_KEY_KIND: &str = "backup-key";
/// How long a failed keychain call stands before the keychain is asked again. Long enough that
/// polling raises no stream of dialogs, short enough that "try again" works after a mistaken "deny".
const RETRY_AFTER_FAILURE: Duration = Duration::from_secs(60);

/// The sealed file of local secrets, shared by the sign-in service and the client core.
pub struct SecretVault {
  keystore: Arc<dyn Keystore>,
  now: Box<dyn Fn() -> Instant + Send + Sync>,
  /// Held across keychain calls on purpose: whoever waits for an answered dialog then reads from
  /// memory instead of raising the next one.
  state: Mutex<State>,
}

/// The OS keychain as the vault uses it. A trait so tests can count the accesses.
pub trait Keystore: Send + Sync {
  /// A secret written as bytes; `None` when the entry doesn't exist.
  fn read_secret(&self, service: &str, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, KeySourceError>;
  /// A secret written as text (the old token entries); `None` when the entry doesn't exist.
  fn read_text(&self, service: &str, account: &str) -> Result<Option<Zeroizing<String>>, KeySourceError>;
  fn write_secret(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), KeySourceError>;
  /// Removes an entry; one that is not there counts as removed.
  fn delete(&self, service: &str, account: &str) -> Result<(), KeySourceError>;
}

/// The real thing via `keyring`: Keychain (macOS), Secret Service (Linux), Credential Manager.
#[derive(Debug, Default)]
pub struct OsKeystore;

/// What a stored token is; each server has one of each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
  RefreshToken,
  LocalSession,
}

#[derive(Default)]
struct State {
  maybe_directory: Option<PathBuf>,
  /// Kept apart from the contents: a file that can't be read right now doesn't cost another
  /// keychain read when it is tried again.
  maybe_key: Option<SymmetricKey>,
  maybe_contents: Option<Contents>,
  maybe_blocked: Option<Blocked>,
  /// Old entries tried this run. One that can't be deleted is retried at the next start, not on
  /// every token refresh.
  tried_old_entries: BTreeSet<String>,
}

/// The last keychain failure, standing until `until`.
struct Blocked {
  until: Instant,
  message: String,
}

/// What the file holds, by item name.
#[derive(Default, Serialize, Deserialize)]
struct Contents {
  /// Tokens as they are, keys in base64.
  #[serde(default)]
  secrets: BTreeMap<String, String>,
  /// Tokens whose old keychain entry is known to be gone, so nothing looks there again.
  #[serde(default)]
  moved: BTreeSet<String>,
}

impl SecretVault {
  pub fn new(keystore: Arc<dyn Keystore>) -> Self {
    Self {
      keystore,
      now: Box::new(Instant::now),
      state: Mutex::new(State::default()),
    }
  }

  /// Where the file lives; known only once the app has its data directory, and set before anything
  /// else asks for a secret.
  pub fn set_directory(&self, directory: &Path) {
    self.lock().maybe_directory = Some(directory.to_owned());
  }

  /// A stored token; an old keychain entry is moved over the first time it is asked for.
  pub fn load_token(
    &self,
    kind: TokenKind,
    server: &str,
  ) -> Result<Option<Zeroizing<String>>, KeySourceError> {
    let mut state = self.lock();
    self.ensure_open(&mut state)?;
    let name = token_name(kind, server);
    self.move_old_entry_once(&mut state, &name)?;
    Ok(
      contents_of(&mut state)?
        .secrets
        .get(&name)
        .map(|token| Zeroizing::new(token.clone())),
    )
  }

  /// Keeps a token. Writes the file only, unless an old keychain entry was still waiting for its
  /// one move: a token refresh doesn't touch the keychain.
  pub fn save_token(&self, kind: TokenKind, server: &str, token: &str) -> Result<(), KeySourceError> {
    let mut state = self.lock();
    self.ensure_open(&mut state)?;
    let name = token_name(kind, server);
    let contents = contents_of(&mut state)?;
    if contents.secrets.get(&name).map(String::as_str) != Some(token) {
      replace_secret(contents, &name, token.to_owned());
      write_file(&state)?;
    }
    // The new token wins over whatever the old entry holds.
    self.move_old_entry_once(&mut state, &name)
  }

  /// Forgets a token, here and in the keychain. An old entry that can't be deleted fails the call
  /// with the token still there, so it never comes back unnoticed at the next start.
  pub fn clear_token(&self, kind: TokenKind, server: &str) -> Result<(), KeySourceError> {
    let mut state = self.lock();
    self.ensure_open(&mut state)?;
    let name = token_name(kind, server);
    let is_moved = contents_of(&mut state)?.moved.contains(&name);
    if !is_moved {
      self.call(&mut state, |keystore| keystore.delete(VAULT_SERVICE, &name))?;
    }
    let contents = contents_of(&mut state)?;
    let had_secret = forget_secret(contents, &name);
    let was_new = contents.moved.insert(name);
    if had_secret || was_new {
      write_file(&state)?;
    }
    Ok(())
  }

  /// The database key of `account`, creating it on first use. The first time on this file it comes
  /// from the keychain; a new key goes into the keychain too, as the copy a damaged file can't take
  /// away.
  pub fn database_key(&self, account: &str) -> Result<SymmetricKey, KeySourceError> {
    let mut state = self.lock();
    self.ensure_open(&mut state)?;
    let name = item_name(DATABASE_KEY_KIND, account);
    if let Some(key) = stored_key(contents_of(&mut state)?, &name)? {
      return Ok(key);
    }
    let found = self.call(&mut state, |keystore| {
      keystore.read_secret(DATABASE_KEY_SERVICE, account)
    })?;
    let key = match found {
      Some(bytes) => SymmetricKey::from_slice(&bytes).ok_or(KeySourceError::InvalidKey(bytes.len()))?,
      None => {
        let key = SymmetricKey::generate(&mut OsRng);
        self.call(&mut state, |keystore| {
          keystore.write_secret(DATABASE_KEY_SERVICE, account, key.expose_secret())
        })?;
        key
      },
    };
    replace_secret(contents_of(&mut state)?, &name, encode_key(&key));
    // The keychain has the key, so a file that can't be written only costs a read next time.
    if let Err(error) = write_file(&state) {
      tracing::warn!(%error, "could not keep the database key in the secrets file");
    }
    Ok(key)
  }

  /// Keeps the account's backup key (e2ee.md §6.5.6); a start with the same key touches nothing.
  /// The file takes it first, so a keychain that refuses is asked once, not at every start.
  pub fn keep_backup_key(&self, account: &str, key: &SymmetricKey) -> Result<(), KeySourceError> {
    let mut state = self.lock();
    self.ensure_open(&mut state)?;
    let name = item_name(BACKUP_KEY_KIND, account);
    let contents = contents_of(&mut state)?;
    let is_kept = matches!(stored_key(contents, &name), Ok(Some(kept)) if kept.is_same_key(key));
    if is_kept {
      return Ok(());
    }
    replace_secret(contents, &name, encode_key(key));
    write_file(&state)?;
    self.call(&mut state, |keystore| {
      keystore.write_secret(BACKUP_KEY_SERVICE, account, key.expose_secret())
    })
  }

  /// The kept backup key of `account`, `None` when there is none. Looks in the keychain only when
  /// the file has none, which is on the setup screen and not on a normal start.
  pub fn kept_backup_key(&self, account: &str) -> Result<Option<SymmetricKey>, KeySourceError> {
    let mut state = self.lock();
    self.ensure_open(&mut state)?;
    let name = item_name(BACKUP_KEY_KIND, account);
    if let Some(key) = stored_key(contents_of(&mut state)?, &name)? {
      return Ok(Some(key));
    }
    let found = self.call(&mut state, |keystore| {
      keystore.read_secret(BACKUP_KEY_SERVICE, account)
    })?;
    let Some(bytes) = found else {
      return Ok(None);
    };
    let key = SymmetricKey::from_slice(&bytes).ok_or(KeySourceError::InvalidKey(bytes.len()))?;
    replace_secret(contents_of(&mut state)?, &name, encode_key(&key));
    if let Err(error) = write_file(&state) {
      tracing::warn!(%error, "could not keep the backup key in the secrets file");
    }
    Ok(Some(key))
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, State> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Reads the file's key once per run, then the file. Only a keychain that says the entry doesn't
  /// exist makes a new key: any other answer leaves the file alone.
  fn ensure_open(&self, state: &mut State) -> Result<(), KeySourceError> {
    if state.maybe_contents.is_some() {
      return Ok(());
    }
    let directory = state
      .maybe_directory
      .clone()
      .ok_or_else(|| KeySourceError::SecretsFile("the data directory is not known yet".to_owned()))?;
    if state.maybe_key.is_none() {
      let found = self.call(state, |keystore| {
        keystore.read_secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT)
      })?;
      state.maybe_key = Some(match found {
        Some(bytes) => match SymmetricKey::from_slice(&bytes) {
          Some(key) => key,
          None => {
            let error = KeySourceError::InvalidKey(bytes.len());
            self.block(state, &error);
            return Err(error);
          },
        },
        None => self.create_key(state, &directory)?,
      });
    }
    let contents = match &state.maybe_key {
      Some(key) => read_file(key, &directory)?,
      None => return Err(KeySourceError::SecretsFile("no key for the file".to_owned())),
    };
    state.maybe_contents = Some(contents);
    Ok(())
  }

  /// A new key for the file. A file already there was sealed under a key that is gone.
  fn create_key(&self, state: &mut State, directory: &Path) -> Result<SymmetricKey, KeySourceError> {
    let key = SymmetricKey::generate(&mut OsRng);
    self.call(state, |keystore| {
      keystore.write_secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT, key.expose_secret())
    })?;
    if directory.join(VAULT_FILE).exists() {
      tracing::warn!("the key of the secrets file is gone; signing in again is needed");
      set_aside(directory);
    }
    Ok(key)
  }

  /// Moves a token's old keychain entry into the file, once per entry: first the file takes the
  /// token, then the entry goes, then the file records that it went. A crash in between finds the
  /// token in the file, and the file's copy wins. With a token at hand a failure waits for the next
  /// start; without one the caller hears of it.
  fn move_old_entry_once(&self, state: &mut State, name: &str) -> Result<(), KeySourceError> {
    let contents = contents_of(state)?;
    if contents.moved.contains(name) {
      return Ok(());
    }
    let has_token = contents.secrets.contains_key(name);
    if has_token && state.tried_old_entries.contains(name) {
      return Ok(());
    }
    state.tried_old_entries.insert(name.to_owned());
    match self.move_old_entry(state, name) {
      Err(KeySourceError::Keystore(message)) if has_token => {
        tracing::info!(%message, "could not clean up an old keychain entry; trying at the next start");
        Ok(())
      },
      outcome => outcome,
    }
  }

  fn move_old_entry(&self, state: &mut State, name: &str) -> Result<(), KeySourceError> {
    let found = self.call(state, |keystore| keystore.read_text(VAULT_SERVICE, name))?;
    if let Some(token) = found {
      let contents = contents_of(state)?;
      if !contents.secrets.contains_key(name) {
        replace_secret(contents, name, token.as_str().to_owned());
        write_file(state)?;
      }
      if let Err(error) = self.call(state, |keystore| keystore.delete(VAULT_SERVICE, name)) {
        // The token is safe in the file: not this caller's problem.
        tracing::info!(%error, "could not delete an old keychain entry; trying at the next start");
        return Ok(());
      }
    }
    contents_of(state)?.moved.insert(name.to_owned());
    write_file(state)
  }

  /// One keychain call, unless the last failure still stands; a new failure then stands too.
  fn call<T>(
    &self,
    state: &mut State,
    action: impl FnOnce(&dyn Keystore) -> Result<T, KeySourceError>,
  ) -> Result<T, KeySourceError> {
    let now = (self.now)();
    if let Some(blocked) = &state.maybe_blocked {
      if now < blocked.until {
        return Err(KeySourceError::Keystore(blocked.message.clone()));
      }
      state.maybe_blocked = None;
    }
    let outcome = action(self.keystore.as_ref());
    if let Err(error) = &outcome {
      self.block(state, error);
    }
    outcome
  }

  fn block(&self, state: &mut State, error: &KeySourceError) {
    let message = match error {
      KeySourceError::Keystore(message) => message.clone(),
      other => other.to_string(),
    };
    state.maybe_blocked = Some(Blocked {
      until: (self.now)() + RETRY_AFTER_FAILURE,
      message,
    });
  }
}

impl OsKeystore {
  /// Runs one keychain call outside any async runtime (see [`run_outside_runtime`]).
  fn run<T: Send>(
    service: &str,
    account: &str,
    action: impl FnOnce(&keyring::Entry) -> Result<T, KeySourceError> + Send,
  ) -> Result<T, KeySourceError> {
    run_outside_runtime(|| {
      let entry = keyring::Entry::new(service, account).map_err(keystore_error)?;
      action(&entry)
    })
    .map_err(|error| KeySourceError::Keystore(error.to_string()))?
  }
}

impl Keystore for OsKeystore {
  fn read_secret(&self, service: &str, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, KeySourceError> {
    Self::run(service, account, |entry| match entry.get_secret() {
      Ok(bytes) => Ok(Some(Zeroizing::new(bytes))),
      Err(keyring::Error::NoEntry) => Ok(None),
      Err(error) => Err(keystore_error(error)),
    })
  }

  fn read_text(&self, service: &str, account: &str) -> Result<Option<Zeroizing<String>>, KeySourceError> {
    Self::run(service, account, |entry| match entry.get_password() {
      Ok(text) => Ok(Some(Zeroizing::new(text))),
      Err(keyring::Error::NoEntry) => Ok(None),
      Err(error) => Err(keystore_error(error)),
    })
  }

  fn write_secret(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), KeySourceError> {
    Self::run(service, account, |entry| {
      entry.set_secret(secret).map_err(keystore_error)
    })
  }

  fn delete(&self, service: &str, account: &str) -> Result<(), KeySourceError> {
    Self::run(service, account, |entry| match entry.delete_credential() {
      Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
      Err(error) => Err(keystore_error(error)),
    })
  }
}

impl TokenKind {
  /// The first part of the item's name, which is also its old keychain account.
  fn name(self) -> &'static str {
    match self {
      Self::RefreshToken => "oidc-refresh-token",
      Self::LocalSession => "local-session",
    }
  }
}

impl Drop for Contents {
  fn drop(&mut self) {
    for secret in self.secrets.values_mut() {
      secret.zeroize();
    }
  }
}

impl fmt::Debug for SecretVault {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("SecretVault([redacted])")
  }
}

fn token_name(kind: TokenKind, server: &str) -> String {
  item_name(kind.name(), server)
}

fn item_name(kind: &str, scope: &str) -> String {
  format!("{kind}@{scope}")
}

fn contents_of(state: &mut State) -> Result<&mut Contents, KeySourceError> {
  state
    .maybe_contents
    .as_mut()
    .ok_or_else(|| KeySourceError::SecretsFile("the file is not open".to_owned()))
}

fn replace_secret(contents: &mut Contents, name: &str, secret: String) {
  forget_secret(contents, name);
  contents.secrets.insert(name.to_owned(), secret);
}

fn forget_secret(contents: &mut Contents, name: &str) -> bool {
  match contents.secrets.remove(name) {
    Some(mut secret) => {
      secret.zeroize();
      true
    },
    None => false,
  }
}

fn encode_key(key: &SymmetricKey) -> String {
  URL_SAFE_NO_PAD.encode(key.expose_secret())
}

/// A key in the file; one that doesn't decode counts as not there, the keychain still has it.
fn stored_key(contents: &Contents, name: &str) -> Result<Option<SymmetricKey>, KeySourceError> {
  let Some(encoded) = contents.secrets.get(name) else {
    return Ok(None);
  };
  let Ok(bytes) = URL_SAFE_NO_PAD.decode(encoded) else {
    return Ok(None);
  };
  let bytes = Zeroizing::new(bytes);
  Ok(SymmetricKey::from_slice(&bytes))
}

/// The file's contents. A missing one is empty; one that doesn't open is set aside, which costs a
/// sign-in. A file that can't be read at all (a disk error) stays where it is and fails the call.
fn read_file(key: &SymmetricKey, directory: &Path) -> Result<Contents, KeySourceError> {
  let sealed = match fs::read(directory.join(VAULT_FILE)) {
    Ok(sealed) => sealed,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Contents::default()),
    Err(error) => return Err(KeySourceError::SecretsFile(error.to_string())),
  };
  let parsed = secret_file::open(key, &sealed)
    .ok()
    .and_then(|plaintext| serde_json::from_slice(plaintext.expose_secret()).ok());
  match parsed {
    Some(contents) => Ok(contents),
    None => {
      tracing::warn!("the secrets file is damaged; signing in again is needed");
      set_aside(directory);
      Ok(Contents::default())
    },
  }
}

fn set_aside(directory: &Path) {
  if let Err(error) = fs::rename(directory.join(VAULT_FILE), directory.join(SET_ASIDE_FILE)) {
    // The next write replaces the file, which nothing can read anyway.
    tracing::warn!(%error, "could not set the secrets file aside");
  }
}

/// Seals the contents and replaces the file in one step, so a crash leaves the old file or the new
/// one and never half of either.
fn write_file(state: &State) -> Result<(), KeySourceError> {
  let not_open = || KeySourceError::SecretsFile("the file is not open".to_owned());
  let directory = state.maybe_directory.as_ref().ok_or_else(not_open)?;
  let key = state.maybe_key.as_ref().ok_or_else(not_open)?;
  let contents = state.maybe_contents.as_ref().ok_or_else(not_open)?;
  let plaintext = Zeroizing::new(
    serde_json::to_vec(contents).map_err(|error| KeySourceError::SecretsFile(error.to_string()))?,
  );
  let sealed = secret_file::seal(key, &plaintext, &mut OsRng)
    .map_err(|error| KeySourceError::SecretsFile(error.to_string()))?;
  replace_file(directory, &sealed).map_err(|error| KeySourceError::SecretsFile(error.to_string()))
}

fn replace_file(directory: &Path, sealed: &[u8]) -> io::Result<()> {
  fs::create_dir_all(directory)?;
  let temporary = directory.join(TEMPORARY_FILE);
  // Left by a crash; created anew below, so its permissions are certainly ours.
  match fs::remove_file(&temporary) {
    Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
    _ => {},
  }
  let mut options = fs::OpenOptions::new();
  options.write(true).create_new(true);
  #[cfg(unix)]
  {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
  }
  let mut file = options.open(&temporary)?;
  file.write_all(sealed)?;
  file.sync_all()?;
  drop(file);
  fs::rename(&temporary, directory.join(VAULT_FILE))?;
  sync_directory(directory);
  Ok(())
}

/// Makes the rename itself durable. Best effort: a crash then leaves the old file, which is still
/// whole. Windows can't open a directory like this and journals renames anyway.
fn sync_directory(directory: &Path) {
  #[cfg(unix)]
  if let Err(error) = fs::File::open(directory).and_then(|handle| handle.sync_all()) {
    tracing::debug!(%error, "could not sync the data directory");
  }
  #[cfg(not(unix))]
  let _ = directory;
}

fn keystore_error(error: keyring::Error) -> KeySourceError {
  KeySourceError::Keystore(error.to_string())
}

#[cfg(test)]
mod tests {
  use std::collections::HashMap;
  use std::sync::atomic::{AtomicU64, Ordering};

  use super::*;

  const SERVER_A: &str = "https://a.example.com";
  const SERVER_B: &str = "https://b.example.com";
  const ACCOUNT_A: &str = "https://a.example.com|1";
  const ACCOUNT_B: &str = "https://b.example.com|2";
  const OLD_REFRESH_A: &str = "oidc-refresh-token@https://a.example.com";
  const OLD_SESSION_A: &str = "local-session@https://a.example.com";
  /// Throwaway keystore entry of the real keychain's tests; deleted again at the end.
  const TEST_SERVICE: &str = "de.zeitgleich.teamcraft.test-keystore-runtime";

  mod access_counts {
    use super::*;

    /// The goal of issue #1150: what a start does, on an install that has migrated.
    #[test]
    fn a_normal_start_reads_the_keychain_once_and_writes_nothing() {
      let fake = Fake::new();
      let directory = TestDir::new();
      start(&fake, &directory);
      fake.reset_counts();

      start(&fake, &directory);

      assert_eq!(
        fake.counts(),
        Counts {
          reads: 1,
          writes: 0,
          deletes: 0
        }
      );
    }

    #[test]
    fn a_token_refresh_does_not_touch_the_keychain() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      fake.reset_counts();

      for round in 0..100 {
        vault
          .save_token(TokenKind::RefreshToken, SERVER_A, &format!("rotated-{round}"))
          .unwrap();
      }

      assert_eq!(fake.counts(), Counts::default());
      assert_eq!(
        reopen(&fake, &directory)
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .unwrap()
          .as_str(),
        "rotated-99"
      );
    }

    /// Everything an existing install has, all at once; the counts are what the PR reports.
    #[test]
    fn the_migration_happens_once() {
      let fake = Fake::new();
      fake.put_text(VAULT_SERVICE, OLD_REFRESH_A, "old-refresh");
      fake.put_text(VAULT_SERVICE, OLD_SESSION_A, "old-session");
      fake.put_secret(DATABASE_KEY_SERVICE, ACCOUNT_A, &[3; 32]);
      fake.put_secret(BACKUP_KEY_SERVICE, ACCOUNT_A, &[4; 32]);
      let directory = TestDir::new();

      let vault = start(&fake, &directory);

      // Reads: the file's key (none yet), session, refresh token, database key. Writes: the file's
      // key, the backup key's copy. Deletes: the origin-only token, session, refresh token.
      assert_eq!(
        fake.counts(),
        Counts {
          reads: 4,
          writes: 2,
          deletes: 3
        }
      );
      assert_eq!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .unwrap()
          .as_str(),
        "start-refresh",
        "the start's refresh wins over the old entry"
      );
      assert_eq!(
        vault
          .load_token(TokenKind::LocalSession, SERVER_A)
          .unwrap()
          .unwrap()
          .as_str(),
        "old-session"
      );
      assert_eq!(vault.database_key(ACCOUNT_A).unwrap().expose_secret(), &[3; 32]);
      assert!(!fake.has(VAULT_SERVICE, OLD_REFRESH_A), "old entries are gone");
      assert!(!fake.has(VAULT_SERVICE, OLD_SESSION_A), "old entries are gone");
      assert!(
        fake.has(DATABASE_KEY_SERVICE, ACCOUNT_A),
        "the database key stays"
      );

      fake.reset_counts();
      start(&fake, &directory);
      assert_eq!(
        fake.counts(),
        Counts {
          reads: 1,
          writes: 0,
          deletes: 0
        }
      );
    }

    #[test]
    fn a_second_account_and_server_cost_one_read_once() {
      let fake = Fake::new();
      fake.put_secret(DATABASE_KEY_SERVICE, ACCOUNT_B, &[5; 32]);
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      fake.reset_counts();

      let key = vault.database_key(ACCOUNT_B).unwrap();
      assert_eq!(key.expose_secret(), &[5; 32]);
      assert_eq!(
        fake.counts(),
        Counts {
          reads: 1,
          writes: 0,
          deletes: 0
        }
      );

      fake.reset_counts();
      vault.database_key(ACCOUNT_B).unwrap();
      assert_eq!(fake.counts(), Counts::default());
    }
  }

  mod migration {
    use super::*;

    #[test]
    fn a_crash_before_the_old_entry_is_deleted_finishes_the_next_time() {
      let fake = Fake::new();
      fake.put_text(VAULT_SERVICE, OLD_REFRESH_A, "old-refresh");
      fake.fail_deletes(true);
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      assert_eq!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .unwrap()
          .as_str(),
        "old-refresh",
        "the token is in the file, so it can be used"
      );
      assert!(fake.has(VAULT_SERVICE, OLD_REFRESH_A));
      vault
        .save_token(TokenKind::RefreshToken, SERVER_A, "newer")
        .unwrap();

      fake.fail_deletes(false);
      let vault = reopen(&fake, &directory);
      assert_eq!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .unwrap()
          .as_str(),
        "newer",
        "the file's copy wins over the entry"
      );
      assert!(!fake.has(VAULT_SERVICE, OLD_REFRESH_A), "and the entry goes");
    }

    #[test]
    fn an_old_entry_that_cannot_go_is_not_retried_on_every_refresh() {
      let fake = Fake::new();
      fake.put_text(VAULT_SERVICE, OLD_REFRESH_A, "old-refresh");
      fake.fail_deletes(true);
      let directory = TestDir::new();
      let clock = TestClock::new();
      let vault = clock.vault(&fake, &directory);
      vault.load_token(TokenKind::RefreshToken, SERVER_A).unwrap();
      fake.reset_counts();

      clock.advance(RETRY_AFTER_FAILURE * 10);
      vault
        .save_token(TokenKind::RefreshToken, SERVER_A, "rotated")
        .unwrap();

      assert_eq!(fake.counts(), Counts::default());
    }

    #[test]
    fn clearing_a_token_not_yet_moved_removes_the_old_entry() {
      let fake = Fake::new();
      fake.put_text(VAULT_SERVICE, OLD_REFRESH_A, "old-refresh");
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.clear_token(TokenKind::RefreshToken, SERVER_A).unwrap();
      assert!(!fake.has(VAULT_SERVICE, OLD_REFRESH_A));
      assert!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .is_none()
      );
    }

    #[test]
    fn signing_in_over_an_old_entry_replaces_it() {
      let fake = Fake::new();
      fake.put_text(VAULT_SERVICE, OLD_REFRESH_A, "old-refresh");
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault
        .save_token(TokenKind::RefreshToken, SERVER_A, "new-refresh")
        .unwrap();
      assert!(!fake.has(VAULT_SERVICE, OLD_REFRESH_A));
      assert_eq!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .unwrap()
          .as_str(),
        "new-refresh"
      );
    }
  }

  mod denied_keychain {
    use super::*;

    #[test]
    fn a_denied_keychain_is_a_clear_error_and_wipes_nothing() {
      let fake = Fake::new();
      let directory = TestDir::new();
      start(&fake, &directory);
      let file_before = fs::read(directory.path().join(VAULT_FILE)).unwrap();

      fake.deny(true);
      let vault = reopen(&fake, &directory);
      assert!(matches!(
        vault.load_token(TokenKind::RefreshToken, SERVER_A),
        Err(KeySourceError::Keystore(_))
      ));
      assert!(matches!(
        vault.database_key(ACCOUNT_A),
        Err(KeySourceError::Keystore(_))
      ));
      assert_eq!(fs::read(directory.path().join(VAULT_FILE)).unwrap(), file_before);

      fake.deny(false);
      let vault = reopen(&fake, &directory);
      assert_eq!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .unwrap()
          .as_str(),
        "start-refresh"
      );
    }

    /// The UI asks for the core's status again and again; a denied dialog must not come back with
    /// each question.
    #[test]
    fn polling_does_not_raise_more_dialogs() {
      let fake = Fake::new();
      let directory = TestDir::new();
      start(&fake, &directory);
      fake.deny(true);
      fake.reset_counts();
      let clock = TestClock::new();
      let vault = clock.vault(&fake, &directory);

      for _ in 0..50 {
        assert!(vault.database_key(ACCOUNT_A).is_err());
        assert!(vault.load_token(TokenKind::RefreshToken, SERVER_A).is_err());
        clock.advance(Duration::from_millis(500));
      }
      assert_eq!(fake.counts().reads, 1);

      clock.advance(RETRY_AFTER_FAILURE);
      assert!(vault.database_key(ACCOUNT_A).is_err());
      assert_eq!(fake.counts().reads, 2, "a later try really tries");

      fake.deny(false);
      clock.advance(RETRY_AFTER_FAILURE);
      assert!(vault.database_key(ACCOUNT_A).is_ok(), "and works once allowed");
    }

    #[test]
    fn a_denied_key_never_makes_a_new_one() {
      let fake = Fake::new();
      fake.deny(true);
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      assert!(vault.database_key(ACCOUNT_A).is_err());
      assert_eq!(fake.counts().writes, 0);
    }

    #[test]
    fn an_old_database_key_that_is_denied_never_makes_a_new_one() {
      let fake = Fake::new();
      fake.put_secret(DATABASE_KEY_SERVICE, ACCOUNT_A, &[3; 32]);
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.load_token(TokenKind::LocalSession, SERVER_B).unwrap();
      fake.deny(true);
      assert!(vault.database_key(ACCOUNT_A).is_err());
      fake.deny(false);
      assert_eq!(fake.secret(DATABASE_KEY_SERVICE, ACCOUNT_A).unwrap(), vec![3; 32]);
    }

    #[test]
    fn a_refused_backup_key_write_is_not_asked_again_at_the_next_start() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      let key = SymmetricKey::from_bytes([4; 32]);
      vault.load_token(TokenKind::LocalSession, SERVER_A).unwrap();
      fake.deny(true);
      assert!(vault.keep_backup_key(ACCOUNT_A, &key).is_err());
      fake.deny(false);
      fake.reset_counts();

      let vault = reopen(&fake, &directory);
      vault.keep_backup_key(ACCOUNT_A, &key).unwrap();

      assert_eq!(
        fake.counts(),
        Counts {
          reads: 1,
          writes: 0,
          deletes: 0
        }
      );
    }
  }

  mod damaged_file {
    use super::*;

    #[test]
    fn a_damaged_file_means_signing_in_and_keeps_the_keys() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let database_key = vault.database_key(ACCOUNT_A).unwrap();
      fs::write(directory.path().join(VAULT_FILE), b"not a sealed file").unwrap();

      let vault = reopen(&fake, &directory);
      assert!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .is_none(),
        "the token is gone, so the user signs in"
      );
      assert!(
        vault.database_key(ACCOUNT_A).unwrap().is_same_key(&database_key),
        "the database key comes back from the keychain"
      );
      assert!(
        vault
          .kept_backup_key(ACCOUNT_A)
          .unwrap()
          .is_some_and(|key| key.expose_secret() == &[4; 32]),
        "and so does the backup key"
      );
      assert_eq!(
        fs::read(directory.path().join(SET_ASIDE_FILE)).unwrap(),
        b"not a sealed file",
        "nothing is wiped"
      );
    }

    #[test]
    fn an_empty_or_truncated_file_counts_as_damaged() {
      let fake = Fake::new();
      let directory = TestDir::new();
      start(&fake, &directory);
      let path = directory.path().join(VAULT_FILE);
      let whole = fs::read(&path).unwrap();
      for length in [0, 1, 20, whole.len() - 1] {
        fs::write(&path, &whole[..length]).unwrap();
        let vault = reopen(&fake, &directory);
        assert!(
          vault
            .load_token(TokenKind::RefreshToken, SERVER_A)
            .unwrap()
            .is_none(),
          "length {length}"
        );
        vault
          .save_token(TokenKind::RefreshToken, SERVER_A, "signed-in-again")
          .unwrap();
      }
    }

    #[test]
    fn a_file_whose_key_is_gone_is_set_aside() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let database_key = vault.database_key(ACCOUNT_A).unwrap();
      fake.remove(VAULT_SERVICE, VAULT_KEY_ACCOUNT);

      let vault = reopen(&fake, &directory);
      assert!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .is_none()
      );
      assert!(directory.path().join(SET_ASIDE_FILE).exists());
      assert!(vault.database_key(ACCOUNT_A).unwrap().is_same_key(&database_key));
    }
  }

  mod file {
    use super::*;

    #[test]
    fn nothing_readable_reaches_the_disk() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault
        .save_token(TokenKind::RefreshToken, SERVER_A, "plain-token-text")
        .unwrap();
      let bytes = fs::read(directory.path().join(VAULT_FILE)).unwrap();
      let text = String::from_utf8_lossy(&bytes);
      assert!(!text.contains("plain-token-text"));
      assert!(!text.contains("example.com"));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_for_its_owner_only() {
      use std::os::unix::fs::PermissionsExt;

      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.save_token(TokenKind::RefreshToken, SERVER_A, "t").unwrap();
      let mode = fs::metadata(directory.path().join(VAULT_FILE))
        .unwrap()
        .permissions()
        .mode();
      assert_eq!(mode & 0o777, 0o600);
    }

    /// A crash after writing the temporary file: the old file is whole, the leftover is ignored and
    /// replaced.
    #[test]
    fn a_crash_mid_write_leaves_the_old_file() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.save_token(TokenKind::RefreshToken, SERVER_A, "t").unwrap();
      fs::write(directory.path().join(TEMPORARY_FILE), b"half a file").unwrap();

      let vault = reopen(&fake, &directory);
      assert_eq!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .unwrap()
          .as_str(),
        "t"
      );
      vault.save_token(TokenKind::RefreshToken, SERVER_A, "u").unwrap();
      assert!(!directory.path().join(TEMPORARY_FILE).exists());
    }

    #[test]
    fn asking_before_the_directory_is_known_is_an_error() {
      let vault = SecretVault::new(Fake::new());
      assert!(matches!(
        vault.load_token(TokenKind::RefreshToken, SERVER_A),
        Err(KeySourceError::SecretsFile(_))
      ));
    }
  }

  mod tokens {
    use super::*;

    #[test]
    fn signing_out_deletes_the_token_for_good() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      fake.reset_counts();
      vault.clear_token(TokenKind::RefreshToken, SERVER_A).unwrap();
      vault.clear_token(TokenKind::LocalSession, SERVER_A).unwrap();
      assert_eq!(fake.counts(), Counts::default(), "signing out raises no dialog");
      let vault = reopen(&fake, &directory);
      assert!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .is_none()
      );
      assert!(
        vault
          .load_token(TokenKind::LocalSession, SERVER_A)
          .unwrap()
          .is_none()
      );
    }

    #[test]
    fn servers_keep_apart() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.save_token(TokenKind::RefreshToken, SERVER_A, "a").unwrap();
      vault.save_token(TokenKind::RefreshToken, SERVER_B, "b").unwrap();
      vault.save_token(TokenKind::LocalSession, SERVER_A, "s").unwrap();
      vault.clear_token(TokenKind::RefreshToken, SERVER_A).unwrap();
      assert_eq!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_B)
          .unwrap()
          .unwrap()
          .as_str(),
        "b"
      );
      assert_eq!(
        vault
          .load_token(TokenKind::LocalSession, SERVER_A)
          .unwrap()
          .unwrap()
          .as_str(),
        "s"
      );
    }

    #[test]
    fn a_new_database_key_goes_to_the_keychain_too() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      let key = vault.database_key(ACCOUNT_A).unwrap();
      assert_eq!(
        fake.secret(DATABASE_KEY_SERVICE, ACCOUNT_A).unwrap(),
        key.expose_secret().to_vec()
      );
      assert!(
        reopen(&fake, &directory)
          .database_key(ACCOUNT_A)
          .unwrap()
          .is_same_key(&key)
      );
    }

    #[test]
    fn a_new_backup_key_replaces_the_kept_one() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      let first = SymmetricKey::from_bytes([1; 32]);
      let second = SymmetricKey::from_bytes([2; 32]);
      vault.keep_backup_key(ACCOUNT_A, &first).unwrap();
      vault.keep_backup_key(ACCOUNT_A, &second).unwrap();
      assert_eq!(fake.secret(BACKUP_KEY_SERVICE, ACCOUNT_A).unwrap(), vec![2; 32]);
      assert!(
        vault
          .kept_backup_key(ACCOUNT_A)
          .unwrap()
          .unwrap()
          .is_same_key(&second)
      );
    }

    #[test]
    fn no_backup_key_is_none() {
      let fake = Fake::new();
      let directory = TestDir::new();
      assert!(
        reopen(&fake, &directory)
          .kept_backup_key(ACCOUNT_A)
          .unwrap()
          .is_none()
      );
    }
  }

  mod real_keychain {
    use super::*;

    /// The desktop core's situation: the keychain is called from inside the core's runtime, with
    /// the desktop's dependency features (zbus with `tokio`, see the dev-dependencies). Before the
    /// fix this panicked even without a Secret Service, because the panic comes before the D-Bus
    /// connection; without a keystore (CI containers) the keystore error is accepted.
    #[tokio::test]
    async fn a_call_from_inside_a_runtime_does_not_panic() {
      let account = format!("runtime-test-{}", std::process::id());
      let outcome = OsKeystore.write_secret(TEST_SERVICE, &account, &[9; 32]);
      match outcome {
        Ok(()) => {
          let read = OsKeystore.read_secret(TEST_SERVICE, &account);
          OsKeystore.delete(TEST_SERVICE, &account).unwrap();
          assert_eq!(read.unwrap().unwrap().as_slice(), &[9; 32]);
          assert!(OsKeystore.read_secret(TEST_SERVICE, &account).unwrap().is_none());
          OsKeystore.delete(TEST_SERVICE, &account).unwrap();
        },
        Err(KeySourceError::Keystore(error)) => assert!(!error.contains("panicked"), "{error}"),
        Err(error) => panic!("{error}"),
      }
    }

    /// The whole story on this machine's real keychain (Secret Service on Linux, the Keychain on a
    /// Mac): old entries move over, then a start is one read and a refresh none. Without a keychain
    /// (CI containers) there is nothing to measure.
    #[test]
    fn the_real_keychain_sees_one_read_per_start() {
      let keystore = Arc::new(CountedOsKeystore {
        prefix: format!("test-{}-", std::process::id()),
        calls: AtomicU64::new(0),
      });
      // Written as text, as the app did: on Windows that is not the same bytes as a secret.
      let old_token = "old-refresh";
      let old_service = format!("{}{VAULT_SERVICE}", keystore.prefix);
      let written = run_outside_runtime(|| {
        keyring::Entry::new(&old_service, OLD_REFRESH_A).and_then(|entry| entry.set_password(old_token))
      })
      .unwrap();
      if written.is_err() {
        return;
      }
      let directory = TestDir::new();
      let open = || {
        let vault = SecretVault::new(Arc::clone(&keystore) as Arc<dyn Keystore>);
        vault.set_directory(directory.path());
        vault
      };

      let vault = open();
      let database_key = vault.database_key(ACCOUNT_A).unwrap();
      assert_eq!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .unwrap()
          .as_str(),
        old_token
      );

      keystore.calls.store(0, Ordering::Relaxed);
      let vault = open();
      assert!(vault.database_key(ACCOUNT_A).unwrap().is_same_key(&database_key));
      vault.load_token(TokenKind::RefreshToken, SERVER_A).unwrap();
      vault
        .save_token(TokenKind::RefreshToken, SERVER_A, "rotated")
        .unwrap();
      let calls = keystore.calls.load(Ordering::Relaxed);

      drop(keystore.delete(VAULT_SERVICE, VAULT_KEY_ACCOUNT));
      drop(keystore.delete(DATABASE_KEY_SERVICE, ACCOUNT_A));
      assert!(
        keystore
          .read_text(VAULT_SERVICE, OLD_REFRESH_A)
          .unwrap()
          .is_none()
      );
      assert_eq!(calls, 1);
    }
  }

  // Helpers

  /// The OS keychain under a test's own service names, counted: entries never meet the app's.
  struct CountedOsKeystore {
    prefix: String,
    calls: AtomicU64,
  }

  impl CountedOsKeystore {
    fn service(&self, service: &str) -> String {
      self.calls.fetch_add(1, Ordering::Relaxed);
      format!("{}{service}", self.prefix)
    }
  }

  impl Keystore for CountedOsKeystore {
    fn read_secret(
      &self,
      service: &str,
      account: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, KeySourceError> {
      OsKeystore.read_secret(&self.service(service), account)
    }

    fn read_text(&self, service: &str, account: &str) -> Result<Option<Zeroizing<String>>, KeySourceError> {
      OsKeystore.read_text(&self.service(service), account)
    }

    fn write_secret(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), KeySourceError> {
      OsKeystore.write_secret(&self.service(service), account, secret)
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), KeySourceError> {
      OsKeystore.delete(&self.service(service), account)
    }
  }

  #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
  struct Counts {
    reads: u32,
    writes: u32,
    deletes: u32,
  }

  /// A keychain in memory that counts every access and can refuse them.
  #[derive(Default)]
  struct Fake {
    inner: Mutex<FakeInner>,
  }

  #[derive(Default)]
  struct FakeInner {
    entries: HashMap<(String, String), Vec<u8>>,
    counts: Counts,
    is_denied: bool,
    are_deletes_failing: bool,
  }

  impl Fake {
    fn new() -> Arc<Self> {
      Arc::new(Self::default())
    }

    fn with<T>(&self, action: impl FnOnce(&mut FakeInner) -> T) -> T {
      action(&mut self.inner.lock().unwrap())
    }

    fn counts(&self) -> Counts {
      self.with(|inner| inner.counts)
    }

    fn reset_counts(&self) {
      self.with(|inner| inner.counts = Counts::default());
    }

    fn deny(&self, is_denied: bool) {
      self.with(|inner| inner.is_denied = is_denied);
    }

    fn fail_deletes(&self, are_failing: bool) {
      self.with(|inner| inner.are_deletes_failing = are_failing);
    }

    fn put_text(&self, service: &str, account: &str, text: &str) {
      self.put_secret(service, account, text.as_bytes());
    }

    fn put_secret(&self, service: &str, account: &str, secret: &[u8]) {
      self.with(|inner| {
        inner
          .entries
          .insert((service.to_owned(), account.to_owned()), secret.to_vec())
      });
    }

    fn secret(&self, service: &str, account: &str) -> Option<Vec<u8>> {
      self.with(|inner| {
        inner
          .entries
          .get(&(service.to_owned(), account.to_owned()))
          .cloned()
      })
    }

    fn has(&self, service: &str, account: &str) -> bool {
      self.secret(service, account).is_some()
    }

    fn remove(&self, service: &str, account: &str) {
      self.with(|inner| inner.entries.remove(&(service.to_owned(), account.to_owned())));
    }
  }

  impl Keystore for Fake {
    fn read_secret(
      &self,
      service: &str,
      account: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, KeySourceError> {
      self.with(|inner| {
        inner.counts.reads += 1;
        if inner.is_denied {
          return Err(denied());
        }
        Ok(
          inner
            .entries
            .get(&(service.to_owned(), account.to_owned()))
            .map(|bytes| Zeroizing::new(bytes.clone())),
        )
      })
    }

    fn read_text(&self, service: &str, account: &str) -> Result<Option<Zeroizing<String>>, KeySourceError> {
      Ok(
        self
          .read_secret(service, account)?
          .map(|bytes| Zeroizing::new(String::from_utf8(bytes.to_vec()).unwrap())),
      )
    }

    fn write_secret(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), KeySourceError> {
      self.with(|inner| {
        inner.counts.writes += 1;
        if inner.is_denied {
          return Err(denied());
        }
        inner
          .entries
          .insert((service.to_owned(), account.to_owned()), secret.to_vec());
        Ok(())
      })
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), KeySourceError> {
      self.with(|inner| {
        inner.counts.deletes += 1;
        if inner.is_denied || inner.are_deletes_failing {
          return Err(denied());
        }
        inner.entries.remove(&(service.to_owned(), account.to_owned()));
        Ok(())
      })
    }
  }

  fn denied() -> KeySourceError {
    KeySourceError::Keystore("the user denied access".to_owned())
  }

  /// What the desktop app asks for on a start, in its order (`main.rs` setup, `restore`, the core).
  fn start(fake: &Arc<Fake>, directory: &TestDir) -> SecretVault {
    let vault = reopen(fake, directory);
    vault
      .clear_token(TokenKind::RefreshToken, "https://origin-only.example.com")
      .unwrap();
    vault.load_token(TokenKind::LocalSession, SERVER_A).unwrap();
    vault.load_token(TokenKind::RefreshToken, SERVER_A).unwrap();
    vault
      .save_token(TokenKind::RefreshToken, SERVER_A, "start-refresh")
      .unwrap();
    vault.database_key(ACCOUNT_A).unwrap();
    vault
      .keep_backup_key(ACCOUNT_A, &SymmetricKey::from_bytes([4; 32]))
      .unwrap();
    vault
  }

  /// The app starting again: a new vault over the same keychain and file.
  fn reopen(fake: &Arc<Fake>, directory: &TestDir) -> SecretVault {
    let vault = SecretVault::new(Arc::clone(fake) as Arc<dyn Keystore>);
    vault.set_directory(directory.path());
    vault
  }

  /// An injected clock for the time a failure stands.
  struct TestClock {
    now: Arc<Mutex<Instant>>,
  }

  impl TestClock {
    fn new() -> Self {
      Self {
        now: Arc::new(Mutex::new(Instant::now())),
      }
    }

    fn advance(&self, by: Duration) {
      *self.now.lock().unwrap() += by;
    }

    fn vault(&self, fake: &Arc<Fake>, directory: &TestDir) -> SecretVault {
      let mut vault = reopen(fake, directory);
      let now = Arc::clone(&self.now);
      vault.now = Box::new(move || *now.lock().unwrap());
      vault
    }
  }

  struct TestDir(PathBuf);

  impl TestDir {
    fn new() -> Self {
      static NEXT: AtomicU64 = AtomicU64::new(0);
      let number = NEXT.fetch_add(1, Ordering::Relaxed);
      let path = std::env::temp_dir().join(format!("teamcraft-secret-vault-{}-{number}", std::process::id()));
      fs::create_dir_all(&path).unwrap();
      Self(path)
    }

    fn path(&self) -> &Path {
      &self.0
    }
  }

  impl Drop for TestDir {
    fn drop(&mut self) {
      // A leftover in the temporary folder is harmless.
      drop(fs::remove_dir_all(&self.0));
    }
  }
}
