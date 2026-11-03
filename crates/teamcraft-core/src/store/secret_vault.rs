//! A desktop's local secrets in one sealed file (e2ee.md §9): refresh tokens, local sessions, DPoP
//! keys and copies of the database and backup keys, under one key that the OS keychain holds.
//!
//! Every keychain access can raise a dialog on macOS, so the keychain is read once per start, for
//! the file's key, and not touched while the app runs. What is in the file is served from memory.
//!
//! - **Tokens and sessions** live only in the file. Their old keychain entries are moved over once,
//!   per entry, the first time each is asked for, and deleted.
//! - **Database and backup keys** stay in the keychain too: the file is a copy that saves the read.
//!   So a damaged file costs a sign-in, never the database. A backup key the keychain refused is
//!   marked and written at a later start, until it is there.
//! - **A failed keychain call** (locked, denied, no keychain) wipes nothing and is remembered for
//!   [`RETRY_AFTER_FAILURE`], because the UI asks for the core's status again and again and each
//!   attempt would raise another dialog. The user asking again ([`SecretVault::allow_retry`]) ends
//!   that at once.
//! - **Signing out** ([`SecretVault::purge_after_sign_out`]) also deletes what normally nobody looks
//!   at: a set-aside file, and old keychain entries an older version of the app may have written
//!   after the move.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, Read as _, Write};
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

use super::{KeySourceError, StoreError, StoreLock, run_outside_runtime};

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
/// Far above what a few tokens and keys per server take; a bigger file counts as damaged, so
/// nobody can make the app read gigabytes at its start.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// How long a key the keychain says is gone gets to come back before the file is given up: a
/// keychain service that is still starting may answer "no such entry" for a moment.
const MISSING_KEY_RECHECK: Duration = Duration::from_secs(2);
/// A second process on the file (the old app during an update) gets this long to end.
const LOCK_ATTEMPTS: u32 = 20;
const LOCK_RETRY: Duration = Duration::from_millis(100);

/// The sealed file of local secrets, shared by the sign-in service and the client core.
pub struct SecretVault {
  keystore: Arc<dyn Keystore>,
  now: Box<dyn Fn() -> Instant + Send + Sync>,
  pause: Box<dyn Fn(Duration) + Send + Sync>,
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

/// What is kept about a server's session besides its tokens, for the start from the cached account
/// (e2ee.md §9.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionNote {
  /// The account the server confirmed last, with the subject it was confirmed for.
  CachedAccount,
  /// The account at the identity provider that the stored refresh token belongs to.
  TokenSubject,
  /// The device's DPoP key for a server, in base64url (e2ee.md §3.1.2). In this file so that using
  /// it never raises a keychain dialog (#1150), and a damaged file costs a sign-in, as for the
  /// refresh token that needs it.
  DpopKey,
  /// The thumbprint of the key the stored refresh token is bound to.
  TokenBinding,
}

impl SessionNote {
  fn name(self) -> &'static str {
    match self {
      Self::CachedAccount => "cached-account",
      Self::TokenSubject => "token-subject",
      Self::DpopKey => "dpop-key",
      Self::TokenBinding => "token-binding",
    }
  }
}

#[derive(Default)]
struct State {
  maybe_directory: Option<PathBuf>,
  /// One process per file: a second one's stale copy would overwrite rotated tokens.
  maybe_lock: Option<StoreLock>,
  /// Kept apart from the contents: a file that can't be read right now doesn't cost another
  /// keychain read when it is tried again.
  maybe_key: Option<SymmetricKey>,
  maybe_contents: Option<Contents>,
  /// The contents changed and the file doesn't have that yet. Stays set when a write fails, so the
  /// next call writes again instead of finding nothing to do.
  is_dirty: bool,
  maybe_blocked: Option<Blocked>,
  /// Keychain clean-ups tried this run. One that fails is retried at the next start, not on every
  /// token refresh.
  tried_this_run: BTreeSet<String>,
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
  /// Backup keys the keychain doesn't have yet.
  #[serde(default)]
  missing_from_keychain: BTreeSet<String>,
}

/// A growing buffer that wipes every buffer it leaves behind, for what is about to be sealed.
struct ZeroizingWriter(Zeroizing<Vec<u8>>);

impl SecretVault {
  pub fn new(keystore: Arc<dyn Keystore>) -> Self {
    Self {
      keystore,
      now: Box::new(Instant::now),
      pause: Box::new(std::thread::sleep),
      state: Mutex::new(State::default()),
    }
  }

  /// Where the file lives; known only once the app has its data directory, and set before anything
  /// else asks for a secret.
  pub fn set_directory(&self, directory: &Path) {
    self.lock().maybe_directory = Some(directory.to_owned());
  }

  /// The user asked to try again: a failure still standing doesn't stand in the way, so a mistaken
  /// "deny" can be corrected at once. Only for what a person did, never for what a timer or a poll
  /// does: that would bring back the stream of dialogs.
  pub fn allow_retry(&self) {
    self.lock().maybe_blocked = None;
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
      state.is_dirty = true;
    }
    flush(&mut state)?;
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
    state.is_dirty |= had_secret || was_new;
    flush(&mut state)
  }

  /// What the sign-in service keeps about a server's session for the next start. Lives in the file
  /// only: it never had a keychain entry to move and never gets one.
  pub fn load_session_note(
    &self,
    kind: SessionNote,
    server: &str,
  ) -> Result<Option<Zeroizing<String>>, KeySourceError> {
    let mut state = self.lock();
    self.ensure_open(&mut state)?;
    Ok(
      contents_of(&mut state)?
        .secrets
        .get(&item_name(kind.name(), server))
        .map(|note| Zeroizing::new(note.clone())),
    )
  }

  /// Keeps a note. Writes the file only, and only when the note changed.
  pub fn save_session_note(&self, kind: SessionNote, server: &str, note: &str) -> Result<(), KeySourceError> {
    let mut state = self.lock();
    self.ensure_open(&mut state)?;
    let name = item_name(kind.name(), server);
    let contents = contents_of(&mut state)?;
    if contents.secrets.get(&name).map(String::as_str) != Some(note) {
      replace_secret(contents, &name, note.to_owned());
      state.is_dirty = true;
    }
    flush(&mut state)
  }

  /// Forgets a note. A failure to write says so: the caller relies on it being gone.
  pub fn clear_session_note(&self, kind: SessionNote, server: &str) -> Result<(), KeySourceError> {
    let mut state = self.lock();
    self.ensure_open(&mut state)?;
    let name = item_name(kind.name(), server);
    let had_note = forget_secret(contents_of(&mut state)?, &name);
    state.is_dirty |= had_note;
    flush(&mut state)
  }

  /// The user signed out of `server` or left it: deletes what the file doesn't speak for. A set-aside
  /// file may hold old tokens and keys. An older version of the app, run after the move, may have
  /// written the old entries again, and nothing looks at them on a normal start. Deleting an entry
  /// that isn't there raises no dialog. The user asked for this, so a failure still standing doesn't
  /// hold it back; a failure now is reported.
  pub fn purge_after_sign_out(&self, token_servers: &[(TokenKind, &str)]) -> Result<(), KeySourceError> {
    let mut state = self.lock();
    state.maybe_blocked = None;
    if let Some(directory) = &state.maybe_directory {
      remove_if_there(&directory.join(SET_ASIDE_FILE));
    }
    let mut outcome = Ok(());
    for (kind, server) in token_servers {
      let name = token_name(*kind, server);
      if let Err(error) = self.call(&mut state, |keystore| keystore.delete(VAULT_SERVICE, &name)) {
        outcome = Err(error);
      }
    }
    outcome
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
    state.is_dirty = true;
    // The keychain has the key, so a file that can't be written only costs a read next time.
    if let Err(error) = flush(&mut state) {
      tracing::warn!(%error, "could not keep the database key in the secrets file");
    }
    Ok(key)
  }

  /// Keeps the account's backup key (e2ee.md §6.5.6); a start with the same key touches nothing.
  /// The file takes it first and marks it as missing from the keychain until the keychain has it,
  /// so a keychain that refuses is asked again at the next start, not again and again in this one.
  pub fn keep_backup_key(&self, account: &str, key: &SymmetricKey) -> Result<(), KeySourceError> {
    let mut state = self.lock();
    self.ensure_open(&mut state)?;
    let name = item_name(BACKUP_KEY_KIND, account);
    let contents = contents_of(&mut state)?;
    let is_kept = matches!(stored_key(contents, &name), Ok(Some(kept)) if kept.is_same_key(key));
    if !is_kept {
      replace_secret(contents, &name, encode_key(key));
      contents.missing_from_keychain.insert(name.clone());
      state.is_dirty = true;
    }
    flush(&mut state)?;
    let is_missing = contents_of(&mut state)?.missing_from_keychain.contains(&name);
    if !is_missing {
      return Ok(());
    }
    // A new key is always tried; the same one once per run.
    if !state.tried_this_run.insert(name.clone()) && is_kept {
      return Ok(());
    }
    self.call(&mut state, |keystore| {
      keystore.write_secret(BACKUP_KEY_SERVICE, account, key.expose_secret())
    })?;
    contents_of(&mut state)?.missing_from_keychain.remove(&name);
    state.is_dirty = true;
    flush(&mut state)
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
    state.is_dirty = true;
    if let Err(error) = flush(&mut state) {
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
    self.ensure_locked(state, &directory)?;
    if state.maybe_key.is_none() {
      let mut found = self.read_file_key(state)?;
      if found.is_none() && directory.join(VAULT_FILE).exists() {
        // The file says a key exists. Overwriting it is final, so give a keychain that is still
        // starting a moment; this path costs a read only when the key really looks gone.
        (self.pause)(MISSING_KEY_RECHECK);
        found = self.read_file_key(state)?;
      }
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

  fn read_file_key(&self, state: &mut State) -> Result<Option<Zeroizing<Vec<u8>>>, KeySourceError> {
    self.call(state, |keystore| {
      keystore.read_secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT)
    })
  }

  /// Takes the file's lock for the rest of the run. Another process on it is an error, after giving
  /// an old app that is ending a moment. The lock is a backstop behind the app's single instance: a
  /// file system that can't lock doesn't stop the app, the data directory's store lock decides there.
  fn ensure_locked(&self, state: &mut State, directory: &Path) -> Result<(), KeySourceError> {
    if state.maybe_lock.is_some() {
      return Ok(());
    }
    fs::create_dir_all(directory).map_err(|error| KeySourceError::SecretsFile(error.to_string()))?;
    let path = directory.join(VAULT_FILE);
    for attempt in 1..=LOCK_ATTEMPTS {
      match StoreLock::acquire(&path) {
        Ok(lock) => {
          state.maybe_lock = Some(lock);
          return Ok(());
        },
        Err(StoreError::InUse) if attempt < LOCK_ATTEMPTS => (self.pause)(LOCK_RETRY),
        Err(StoreError::InUse) => {
          return Err(KeySourceError::SecretsFile(
            "another instance of the app is using the file".to_owned(),
          ));
        },
        Err(error) => {
          tracing::warn!(%error, "could not lock the secrets file; going on without");
          return Ok(());
        },
      }
    }
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
    if has_token && state.tried_this_run.contains(name) {
      return Ok(());
    }
    state.tried_this_run.insert(name.to_owned());
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
        state.is_dirty = true;
        flush(state)?;
      }
      if let Err(error) = self.call(state, |keystore| keystore.delete(VAULT_SERVICE, name)) {
        // The token is safe in the file: not this caller's problem.
        tracing::info!(%error, "could not delete an old keychain entry; trying at the next start");
        return Ok(());
      }
    }
    contents_of(state)?.moved.insert(name.to_owned());
    state.is_dirty = true;
    flush(state)
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

impl Write for ZeroizingWriter {
  fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
    let needed = self.0.len() + bytes.len();
    if needed > self.0.capacity() {
      let mut larger = Vec::with_capacity(needed.max(self.0.capacity() * 2));
      larger.extend_from_slice(&self.0);
      // Dropping the old `Zeroizing` wipes its whole allocation.
      self.0 = Zeroizing::new(larger);
    }
    self.0.extend_from_slice(bytes);
    Ok(bytes.len())
  }

  fn flush(&mut self) -> io::Result<()> {
    Ok(())
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
  let file = match fs::File::open(directory.join(VAULT_FILE)) {
    Ok(file) => file,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Contents::default()),
    Err(error) => return Err(KeySourceError::SecretsFile(error.to_string())),
  };
  let mut sealed = Vec::new();
  file
    .take(MAX_FILE_BYTES + 1)
    .read_to_end(&mut sealed)
    .map_err(|error| KeySourceError::SecretsFile(error.to_string()))?;
  let parsed = (sealed.len() as u64 <= MAX_FILE_BYTES)
    .then(|| secret_file::open(key, &sealed).ok())
    .flatten()
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

fn remove_if_there(path: &Path) {
  match fs::remove_file(path) {
    Err(error) if error.kind() != io::ErrorKind::NotFound => {
      tracing::warn!(%error, "could not delete a set-aside secrets file");
    },
    _ => {},
  }
}

/// Writes changed contents. A failed write stays due: see [`State::is_dirty`].
fn flush(state: &mut State) -> Result<(), KeySourceError> {
  if !state.is_dirty {
    return Ok(());
  }
  write_file(state)?;
  state.is_dirty = false;
  Ok(())
}

/// Seals the contents and replaces the file in one step, so a crash leaves the old file or the new
/// one and never half of either.
fn write_file(state: &State) -> Result<(), KeySourceError> {
  let not_open = || KeySourceError::SecretsFile("the file is not open".to_owned());
  let directory = state.maybe_directory.as_ref().ok_or_else(not_open)?;
  let key = state.maybe_key.as_ref().ok_or_else(not_open)?;
  let contents = state.maybe_contents.as_ref().ok_or_else(not_open)?;
  let plaintext = serialize(contents)?;
  let sealed = secret_file::seal(key, &plaintext, &mut OsRng)
    .map_err(|error| KeySourceError::SecretsFile(error.to_string()))?;
  replace_file(directory, &sealed).map_err(|error| KeySourceError::SecretsFile(error.to_string()))
}

/// The contents as JSON, with no copy of a secret left behind on the heap: `serde_json::to_vec`
/// would free each smaller buffer as it grows, unwiped.
fn serialize(contents: &Contents) -> Result<Zeroizing<Vec<u8>>, KeySourceError> {
  let size: usize = contents
    .secrets
    .iter()
    .map(|(name, secret)| name.len() + secret.len() + 8)
    .sum();
  let mut writer = ZeroizingWriter(Zeroizing::new(Vec::with_capacity(size + 256)));
  serde_json::to_writer(&mut writer, contents)
    .map_err(|error| KeySourceError::SecretsFile(error.to_string()))?;
  Ok(writer.0)
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

    /// What a start reads for the cached account comes from the file that is open anyway (issue
    /// #778).
    #[test]
    fn session_notes_do_not_touch_the_keychain() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      fake.reset_counts();
      let (account, subject) = (SessionNote::CachedAccount, SessionNote::TokenSubject);

      vault.save_session_note(account, SERVER_A, "account-a").unwrap();
      vault.save_session_note(subject, SERVER_A, "subject-a").unwrap();
      let read = |kind, server| {
        vault
          .load_session_note(kind, server)
          .unwrap()
          .map(|note| note.to_string())
      };
      assert_eq!(read(account, SERVER_A).as_deref(), Some("account-a"));
      assert_eq!(read(subject, SERVER_A).as_deref(), Some("subject-a"));
      assert_eq!(read(account, SERVER_B), None);
      vault.clear_session_note(account, SERVER_A).unwrap();
      assert_eq!(read(account, SERVER_A), None);
      assert_eq!(read(subject, SERVER_A).as_deref(), Some("subject-a"));

      assert_eq!(fake.counts(), Counts::default());
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
      drop(vault);
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
      drop(vault);

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
      drop(vault);

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
      drop(vault);

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

    /// The keychain copy is what survives a damaged file, so a refusal is asked again, once per
    /// start, until the keychain has it.
    #[test]
    fn a_refused_backup_key_is_written_at_a_later_start() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      let key = SymmetricKey::from_bytes([4; 32]);
      vault.load_token(TokenKind::LocalSession, SERVER_A).unwrap();
      fake.deny(true);
      assert!(vault.keep_backup_key(ACCOUNT_A, &key).is_err());
      fake.reset_counts();
      for _ in 0..5 {
        vault.keep_backup_key(ACCOUNT_A, &key).unwrap();
      }
      assert_eq!(fake.counts(), Counts::default(), "not again in this run");
      assert!(
        vault.kept_backup_key(ACCOUNT_A).unwrap().is_some(),
        "the file has it"
      );
      drop(vault);
      fake.deny(false);
      assert!(!fake.has(BACKUP_KEY_SERVICE, ACCOUNT_A));

      fake.reset_counts();
      let vault = reopen(&fake, &directory);
      vault.keep_backup_key(ACCOUNT_A, &key).unwrap();
      assert_eq!(
        fake.counts(),
        Counts {
          reads: 1,
          writes: 1,
          deletes: 0
        }
      );
      assert_eq!(fake.secret(BACKUP_KEY_SERVICE, ACCOUNT_A).unwrap(), vec![4; 32]);
      drop(vault);

      fake.reset_counts();
      reopen(&fake, &directory)
        .keep_backup_key(ACCOUNT_A, &key)
        .unwrap();
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
    fn an_unrelated_failure_only_delays_the_backup_key() {
      let fake = Fake::new();
      fake.put_text(VAULT_SERVICE, OLD_REFRESH_A, "old-refresh");
      fake.fail_deletes(true);
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.load_token(TokenKind::RefreshToken, SERVER_A).unwrap();
      let key = SymmetricKey::from_bytes([4; 32]);
      assert!(
        vault.keep_backup_key(ACCOUNT_A, &key).is_err(),
        "the failed delete stands"
      );
      assert!(!fake.has(BACKUP_KEY_SERVICE, ACCOUNT_A));
      drop(vault);

      fake.fail_deletes(false);
      reopen(&fake, &directory)
        .keep_backup_key(ACCOUNT_A, &key)
        .unwrap();
      assert!(fake.has(BACKUP_KEY_SERVICE, ACCOUNT_A));
    }

    #[test]
    fn the_user_trying_again_really_tries() {
      let fake = Fake::new();
      let directory = TestDir::new();
      start(&fake, &directory);
      fake.deny(true);
      fake.reset_counts();
      let vault = reopen(&fake, &directory);
      assert!(vault.database_key(ACCOUNT_A).is_err());
      assert!(vault.database_key(ACCOUNT_A).is_err());
      assert_eq!(fake.counts().reads, 1, "a poll raises nothing");

      fake.deny(false);
      vault.allow_retry();
      assert!(vault.database_key(ACCOUNT_A).is_ok());
      assert_eq!(fake.counts().reads, 2);
    }
  }

  mod failed_writes {
    use super::*;

    /// Memory must not get ahead of the disk: a retry has to write, not find nothing to do.
    #[test]
    fn a_sign_out_is_not_reported_done_until_the_file_has_it() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      block_writes(&directory);
      assert!(vault.clear_token(TokenKind::RefreshToken, SERVER_A).is_err());
      assert!(
        vault.clear_token(TokenKind::RefreshToken, SERVER_A).is_err(),
        "still not on disk"
      );

      allow_writes(&directory);
      vault.clear_token(TokenKind::RefreshToken, SERVER_A).unwrap();
      drop(vault);
      assert!(
        reopen(&fake, &directory)
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .is_none()
      );
    }

    #[test]
    fn a_token_that_could_not_be_written_is_written_by_the_retry() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      block_writes(&directory);
      assert!(
        vault
          .save_token(TokenKind::RefreshToken, SERVER_A, "rotated")
          .is_err()
      );
      assert!(
        vault
          .save_token(TokenKind::RefreshToken, SERVER_A, "rotated")
          .is_err()
      );

      allow_writes(&directory);
      vault
        .save_token(TokenKind::RefreshToken, SERVER_A, "rotated")
        .unwrap();
      drop(vault);
      assert_eq!(
        reopen(&fake, &directory)
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .unwrap()
          .as_str(),
        "rotated"
      );
    }

    #[test]
    fn a_backup_key_is_not_called_kept_until_the_file_has_it() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.load_token(TokenKind::LocalSession, SERVER_A).unwrap();
      let key = SymmetricKey::from_bytes([4; 32]);
      block_writes(&directory);
      assert!(vault.keep_backup_key(ACCOUNT_A, &key).is_err());
      assert!(vault.keep_backup_key(ACCOUNT_A, &key).is_err());

      allow_writes(&directory);
      vault.keep_backup_key(ACCOUNT_A, &key).unwrap();
      assert!(fake.has(BACKUP_KEY_SERVICE, ACCOUNT_A));
    }

    #[test]
    fn a_database_key_works_without_the_file_and_is_kept_later() {
      let fake = Fake::new();
      fake.put_secret(DATABASE_KEY_SERVICE, ACCOUNT_A, &[3; 32]);
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.load_token(TokenKind::LocalSession, SERVER_A).unwrap();
      block_writes(&directory);
      assert_eq!(vault.database_key(ACCOUNT_A).unwrap().expose_secret(), &[3; 32]);
      allow_writes(&directory);
      vault.save_token(TokenKind::LocalSession, SERVER_A, "s").unwrap();
      drop(vault);
      fake.reset_counts();
      let vault = reopen(&fake, &directory);
      assert_eq!(vault.database_key(ACCOUNT_A).unwrap().expose_secret(), &[3; 32]);
      assert_eq!(fake.counts().reads, 1, "only the file's key");
    }
  }

  mod sign_out {
    use super::*;

    const ITEMS: [(TokenKind, &str); 2] = [
      (TokenKind::RefreshToken, SERVER_A),
      (TokenKind::LocalSession, SERVER_A),
    ];

    /// An older version of the app, run after the move, writes the old entry again. Nothing looks
    /// there on a normal start, so signing out has to.
    #[test]
    fn an_entry_written_by_an_older_version_goes_at_sign_out() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      fake.put_text(VAULT_SERVICE, OLD_REFRESH_A, "from-an-older-app");

      vault.clear_token(TokenKind::RefreshToken, SERVER_A).unwrap();
      assert!(
        fake.has(VAULT_SERVICE, OLD_REFRESH_A),
        "not looked at while running"
      );
      fake.reset_counts();
      vault.purge_after_sign_out(&ITEMS).unwrap();

      assert!(!fake.has(VAULT_SERVICE, OLD_REFRESH_A));
      assert_eq!(
        fake.counts(),
        Counts {
          reads: 0,
          writes: 0,
          deletes: 2
        }
      );
    }

    #[test]
    fn a_set_aside_file_goes_at_sign_out() {
      let fake = Fake::new();
      let directory = TestDir::new();
      start(&fake, &directory);
      fs::write(directory.path().join(VAULT_FILE), b"not a sealed file").unwrap();
      let vault = reopen(&fake, &directory);
      vault.load_token(TokenKind::LocalSession, SERVER_A).unwrap();
      assert!(directory.path().join(SET_ASIDE_FILE).exists());

      vault.purge_after_sign_out(&ITEMS).unwrap();

      assert!(!directory.path().join(SET_ASIDE_FILE).exists());
    }

    #[test]
    fn a_second_damaged_file_replaces_the_first() {
      let fake = Fake::new();
      let directory = TestDir::new();
      for content in [&b"first"[..], &b"second"[..]] {
        fs::write(directory.path().join(VAULT_FILE), content).unwrap();
        reopen(&fake, &directory)
          .load_token(TokenKind::LocalSession, SERVER_A)
          .unwrap();
      }
      assert_eq!(
        fs::read(directory.path().join(SET_ASIDE_FILE)).unwrap(),
        b"second"
      );
    }

    #[test]
    fn a_standing_failure_does_not_hold_it_back_and_a_new_one_is_reported() {
      let fake = Fake::new();
      fake.put_text(VAULT_SERVICE, OLD_REFRESH_A, "from-an-older-app");
      let directory = TestDir::new();
      fake.deny(true);
      let vault = reopen(&fake, &directory);
      assert!(vault.load_token(TokenKind::LocalSession, SERVER_A).is_err());
      assert!(
        vault.purge_after_sign_out(&ITEMS).is_err(),
        "still denied: said so"
      );
      fake.deny(false);
      vault.purge_after_sign_out(&ITEMS).unwrap();
      assert!(!fake.has(VAULT_SERVICE, OLD_REFRESH_A));
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
      drop(vault);

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

    /// A keychain service still starting may say "no such entry" for a moment; overwriting the key
    /// then would cost every token.
    #[test]
    fn a_key_that_comes_back_at_once_is_not_replaced() {
      let fake = Fake::new();
      let directory = TestDir::new();
      start(&fake, &directory);
      fake.reset_counts();
      fake.hide_next_read();

      let vault = reopen(&fake, &directory);

      assert_eq!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .unwrap()
          .as_str(),
        "start-refresh"
      );
      assert_eq!(
        fake.counts(),
        Counts {
          reads: 2,
          writes: 0,
          deletes: 0
        }
      );
      assert!(!directory.path().join(SET_ASIDE_FILE).exists());
    }

    #[test]
    fn a_file_whose_key_is_gone_is_set_aside() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let database_key = vault.database_key(ACCOUNT_A).unwrap();
      fake.remove(VAULT_SERVICE, VAULT_KEY_ACCOUNT);
      drop(vault);

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
      drop(vault);

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
    fn a_huge_file_counts_as_damaged() {
      let fake = Fake::new();
      let directory = TestDir::new();
      start(&fake, &directory);
      let path = directory.path().join(VAULT_FILE);
      fs::write(&path, vec![0_u8; usize::try_from(MAX_FILE_BYTES).unwrap() + 1]).unwrap();
      let vault = reopen(&fake, &directory);
      assert!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .is_none()
      );
      assert!(directory.path().join(SET_ASIDE_FILE).exists());
    }

    /// The old app during an update, or a second build: its stale copy would overwrite rotated
    /// tokens, so only one process has the file.
    #[test]
    fn a_second_process_is_refused_until_the_first_ends() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let first = start(&fake, &directory);
      fake.reset_counts();
      let second = reopen(&fake, &directory);
      assert!(matches!(
        second.load_token(TokenKind::RefreshToken, SERVER_A),
        Err(KeySourceError::SecretsFile(_))
      ));
      assert_eq!(fake.counts(), Counts::default(), "and raises no dialog");
      drop(first);
      assert!(second.load_token(TokenKind::RefreshToken, SERVER_A).is_ok());
    }

    #[test]
    fn serializing_grows_past_the_estimate_without_losing_anything() {
      let mut contents = Contents::default();
      contents.secrets.insert("name".to_owned(), "\u{1}".repeat(2000));
      let written = serialize(&contents).unwrap();
      assert_eq!(written.as_slice(), serde_json::to_vec(&contents).unwrap());
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
      assert_eq!(
        fake.counts(),
        Counts::default(),
        "clearing a moved token raises no dialog"
      );
      drop(vault);
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
      drop(vault);
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
      drop(vault);
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
    is_next_read_hidden: bool,
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

    fn hide_next_read(&self) {
      self.with(|inner| inner.is_next_read_hidden = true);
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
        if std::mem::take(&mut inner.is_next_read_hidden) {
          return Ok(None);
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
    let mut vault = SecretVault::new(Arc::clone(fake) as Arc<dyn Keystore>);
    vault.set_directory(directory.path());
    vault.pause = Box::new(|_| {});
    vault
  }

  /// Makes every write of the file fail until [`allow_writes`]: a directory stands where the
  /// temporary file has to go. The vault must have opened the file before.
  fn block_writes(directory: &TestDir) {
    let temporary = directory.path().join(TEMPORARY_FILE);
    drop(fs::remove_file(&temporary));
    fs::create_dir(&temporary).unwrap();
  }

  fn allow_writes(directory: &TestDir) {
    fs::remove_dir(directory.path().join(TEMPORARY_FILE)).unwrap();
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
