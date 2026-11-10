//! A desktop's local secrets in one sealed file (e2ee.md §9.7): refresh tokens, local sessions, DPoP
//! keys and the database and backup keys, under one key that the OS keychain holds.
//!
//! Every keychain access can raise a dialog on macOS, and an ad hoc signed update is a stranger to
//! the entries of the build before it, so the keychain holds one item, the file's key (#1223). It is
//! read once per start and not touched while the app runs. What is in the file is served from memory.
//!
//! - **Tokens and sessions** live only in the file. Their old keychain entries are moved over once,
//!   per entry, the first time each is asked for, and deleted.
//! - **Database and backup keys** live in the file and in a second sealed file of their own, written
//!   only when a key is new or changed, so a damaged secrets file costs a sign-in, never the
//!   database. Their old keychain entries are read once where the files lack the key and deleted once
//!   both files hold it.
//! - **A failed keychain call** (locked, denied, no keychain) wipes nothing and is remembered for
//!   [`RETRY_AFTER_FAILURE`], because the UI asks for the core's status again and again and each
//!   attempt would raise another dialog. The user asking again ([`SecretVault::allow_retry`]) ends
//!   that at once.
//! - **Signing out** ([`SecretVault::purge_after_sign_out`]) gives the files a new key, so an older
//!   copy put back by a process that can write them but not read them doesn't open any more and
//!   brings no signed-out token back (#1155). It also deletes what normally nobody looks at:
//!   set-aside files, and old token entries an older version of the app may have written after the
//!   move.

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
/// Where versions before #1223 kept the database keys; only read and deleted now.
const DATABASE_KEY_SERVICE: &str = "de.zeitgleich.teamcraft.db";
/// e2ee.md §6.5.6; the account name is the database key's. Only read and deleted now, as above.
const BACKUP_KEY_SERVICE: &str = "de.zeitgleich.teamcraft.backup-key";
const VAULT_FILE: &str = "secrets.sealed";
const TEMPORARY_FILE: &str = "secrets.sealed.tmp";
/// A file that could not be opened, kept for a look but never read again.
const SET_ASIDE_FILE: &str = "secrets.sealed.damaged";
const KEYS_FILE: &str = "keys.sealed";
const KEYS_TEMPORARY_FILE: &str = "keys.sealed.tmp";
const KEYS_SET_ASIDE_FILE: &str = "keys.sealed.damaged";
/// A file sealed under the key a sign-out is changing to, waiting for the keychain to have that key.
const STAGED_FILE: &str = "secrets.sealed.next";
const KEYS_STAGED_FILE: &str = "keys.sealed.next";
/// Everything, rewritten at every token refresh.
const SECRETS: SealedFile = SealedFile {
  name: VAULT_FILE,
  temporary: TEMPORARY_FILE,
  set_aside: SET_ASIDE_FILE,
  staged: STAGED_FILE,
};
/// The second copy of the database and backup keys, rewritten only when a key is new or changed:
/// what the keychain entries were for a damaged secrets file before #1223.
const KEYS: SealedFile = SealedFile {
  name: KEYS_FILE,
  temporary: KEYS_TEMPORARY_FILE,
  set_aside: KEYS_SET_ASIDE_FILE,
  staged: KEYS_STAGED_FILE,
};
const DATABASE_KEY_KIND: &str = "database-key";
const BACKUP_KEY_KIND: &str = "backup-key";
/// The old keychain entries of an account's keys, deleted once the files hold the keys.
const OLD_KEY_ENTRIES: [(&str, &str); 2] = [
  (DATABASE_KEY_KIND, DATABASE_KEY_SERVICE),
  (BACKUP_KEY_KIND, BACKUP_KEY_SERVICE),
];
/// Starts that try to delete an old key entry before it is left in place. The entry holds a key the
/// files have, as before #1223; asking at every start would bring a dialog back after a "Deny".
const MAX_DELETE_ATTEMPTS: u32 = 3;
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
  /// A sign-out's new key that the keychain may or may not have taken: it failed the write and
  /// couldn't be read back. Until that is settled every write seals the file under `maybe_key`
  /// and its staged copy under this one, so the files open under whichever key the keychain has.
  maybe_pending_key: Option<SymmetricKey>,
  maybe_contents: Option<Contents>,
  /// The contents changed and the file doesn't have that yet. Stays set when a write fails, so the
  /// next call writes again instead of finding nothing to do.
  is_dirty: bool,
  /// The same for the keys file: its keys are not those of the contents yet.
  are_keys_dirty: bool,
  maybe_blocked: Option<Blocked>,
  /// Keychain clean-ups tried this run. One that fails is retried at the next start, not on every
  /// token refresh.
  tried_this_run: BTreeSet<String>,
}

/// One of the two sealed files, with the names of its temporary, set-aside and staged copies.
struct SealedFile {
  name: &'static str,
  temporary: &'static str,
  set_aside: &'static str,
  /// Sealed under the next key during a sign-out's key change ([`SecretVault::change_file_key`]).
  staged: &'static str,
}

/// What is at a sealed file's path, opened with the key at hand.
enum Sealed {
  Missing,
  Opened(Contents),
  /// Doesn't open: damaged, too big, or sealed under another key.
  Unreadable,
}

/// How the keychain took the files' new key at a sign-out.
enum KeyWrite {
  Taken,
  /// It has the old key (or none): the staged files would never open.
  Refused(KeySourceError),
  /// It failed the write and couldn't be read back: it may have either key.
  Unsure(KeySourceError),
}

/// The last keychain failure, standing until `until`.
struct Blocked {
  until: Instant,
  message: String,
}

/// What a file holds, by item name. The keys file holds only the keys' `secrets`.
///
/// The versions since 1.0.18, which kept copies of the keys in the keychain, read the same layout
/// and find the keys under the same names, so going back to one of them keeps the database. Their
/// field `missing_from_keychain` is ignored.
#[derive(Default, Serialize, Deserialize)]
struct Contents {
  /// Tokens as they are, keys in base64.
  #[serde(default)]
  secrets: BTreeMap<String, String>,
  /// Items whose old keychain entry is known to be gone, so nothing looks there again; also an old
  /// key entry left in place after [`MAX_DELETE_ATTEMPTS`].
  #[serde(default)]
  moved: BTreeSet<String>,
  /// Starts that failed to delete an old key entry, by item name.
  #[serde(default)]
  failed_deletes: BTreeMap<String, u32>,
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

  /// The user signed out of `server` or left it. The files get a new key, so a copy from before
  /// the sign-out that a process of the user puts back doesn't open any more (#1155): one keychain
  /// write, the only one outside a first start. Then it deletes what the files don't speak for. A
  /// set-aside file may hold old tokens and keys; nothing opens it again. An older version of the
  /// app, run after the move, may have written the old token entries again, and nothing looks at
  /// them on a normal start. Deleting an entry that isn't there raises no dialog. The user asked
  /// for this, so a failure still standing doesn't hold it back; a failure now is reported.
  ///
  /// The key goes first: a keychain that refuses deletes (an access list on macOS) must not keep
  /// the old key in place.
  pub fn purge_after_sign_out(&self, token_servers: &[(TokenKind, &str)]) -> Result<(), KeySourceError> {
    let mut state = self.lock();
    state.maybe_blocked = None;
    if let Some(directory) = &state.maybe_directory {
      remove_if_there(&directory.join(SECRETS.set_aside));
      remove_if_there(&directory.join(KEYS.set_aside));
    }
    let mut outcome = self.change_file_key(&mut state);
    // A refused key doesn't hold back the deletes: the user asked for them too.
    state.maybe_blocked = None;
    for (kind, server) in token_servers {
      let name = token_name(*kind, server);
      if let Err(error) = self.call(&mut state, |keystore| keystore.delete(VAULT_SERVICE, &name)) {
        outcome = Err(error);
      }
    }
    outcome
  }

  /// The user removed the account of `server` from this computer (#214). Everything kept for it
  /// goes: its tokens and session notes (named by the server, or by `{server}|…` like the refresh
  /// token's key), and the database and backup keys of each of its users (`{server}|{user}`). Items
  /// of other servers are not touched. Then, as at a sign-out, the files get a new key, so an older
  /// copy put back brings none of it back (#1155); set-aside files, which may hold the items, go,
  /// and so do old keychain entries of the items.
  ///
  /// Answers the accounts whose database key went: what those keys sealed can't be read any more,
  /// and the caller deletes it. `Err` means the items may still be in the files. A new key or an
  /// old entry that the keychain refuses is logged and doesn't fail the call: the items are gone
  /// from the files already.
  pub fn forget_server(&self, server: &str) -> Result<Vec<String>, KeySourceError> {
    if server.is_empty() || server.contains(['@', '|']) {
      return Err(KeySourceError::SecretsFile("not a server origin".to_owned()));
    }
    let mut state = self.lock();
    // The user asked for this: a failure still standing doesn't hold it back.
    state.maybe_blocked = None;
    self.ensure_open(&mut state)?;
    let contents = contents_of(&mut state)?;
    let names: Vec<String> = contents
      .secrets
      .keys()
      .filter(|name| is_item_of(name, server))
      .cloned()
      .collect();
    let mut accounts = BTreeSet::new();
    for name in &names {
      forget_secret(contents, name);
      if let Some(account) = scope_of(name, DATABASE_KEY_KIND).or_else(|| scope_of(name, BACKUP_KEY_KIND)) {
        accounts.insert(account.to_owned());
      }
    }
    contents
      .failed_deletes
      .retain(|name, _| !is_item_of(name, server));
    state.is_dirty |= !names.is_empty();
    state.are_keys_dirty |= names.iter().any(|name| is_key_item(name));
    flush_all(&mut state)?;
    if let Some(directory) = &state.maybe_directory {
      remove_if_there(&directory.join(SECRETS.set_aside));
      remove_if_there(&directory.join(KEYS.set_aside));
    }
    if let Err(error) = self.change_file_key(&mut state) {
      tracing::warn!(%error, "the secrets files keep their key after removing an account");
    }
    state.maybe_blocked = None;
    // Also where the files record an entry as gone: an older version of the app, run after the
    // move, may have written it again. Deleting an entry that isn't there raises no dialog.
    for name in &names {
      let entries: Vec<(&str, &str)> = if is_key_item(name) {
        OLD_KEY_ENTRIES
          .iter()
          .filter_map(|(kind, service)| scope_of(name, kind).map(|account| (*service, account)))
          .collect()
      } else if [TokenKind::RefreshToken, TokenKind::LocalSession]
        .into_iter()
        .any(|kind| scope_of(name, kind.name()).is_some())
      {
        vec![(VAULT_SERVICE, name.as_str())]
      } else {
        // Session notes never had a keychain entry.
        Vec::new()
      };
      for (service, account) in entries {
        if let Err(error) = self.call(&mut state, |keystore| keystore.delete(service, account)) {
          tracing::warn!(%error, "could not delete an old keychain entry of a removed account");
        }
      }
    }
    Ok(accounts.into_iter().collect())
  }

  /// The database key of `account`, creating it on first use. It lives in the sealed files; an
  /// install from before #1223 has it in the keychain, where it is read once and then deleted.
  pub fn database_key(&self, account: &str) -> Result<SymmetricKey, KeySourceError> {
    let mut state = self.lock();
    self.ensure_open(&mut state)?;
    let name = item_name(DATABASE_KEY_KIND, account);
    if let Some(key) = stored_key(contents_of(&mut state)?, &name)? {
      self.retire_old_entries(&mut state, account);
      return Ok(key);
    }
    // Not in the files: an install from before the move, a new account, or a key both files lost.
    let found = self.call(&mut state, |keystore| {
      keystore.read_secret(DATABASE_KEY_SERVICE, account)
    })?;
    match found {
      Some(bytes) => {
        let key = SymmetricKey::from_slice(&bytes).ok_or(KeySourceError::InvalidKey(bytes.len()))?;
        self.take_old_key(&mut state, account, &name, &key)?;
        Ok(key)
      },
      None => create_database_key(&mut state, &name),
    }
  }

  /// Keeps the account's backup key (e2ee.md §6.5.6) in both files; a start with the same key writes
  /// nothing. An error means a file doesn't have it yet, and the next call writes again.
  pub fn keep_backup_key(&self, account: &str, key: &SymmetricKey) -> Result<(), KeySourceError> {
    let mut state = self.lock();
    self.ensure_open(&mut state)?;
    let name = item_name(BACKUP_KEY_KIND, account);
    let contents = contents_of(&mut state)?;
    let is_kept = matches!(stored_key(contents, &name), Ok(Some(kept)) if kept.is_same_key(key));
    if !is_kept {
      replace_secret(contents, &name, encode_key(key));
      state.is_dirty = true;
      state.are_keys_dirty = true;
    }
    flush_all(&mut state)?;
    // An entry from before the move holds this key or the one before it: it goes either way.
    self.retire_old_entries(&mut state, account);
    Ok(())
  }

  /// The kept backup key of `account`, `None` when there is none. Looks in the keychain only when
  /// the files have none, which is on the setup screen and not on a normal start: an install from
  /// before #1223 may keep it only there.
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
    self.take_old_key(&mut state, account, &name, &key)?;
    Ok(Some(key))
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, State> {
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }

  /// Reads the files' key once per run, then the files. Only a keychain that says the entry doesn't
  /// exist makes a new key: any other answer leaves the files alone.
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
      if found.is_none() && has_sealed_files(&directory) {
        // A file says a key exists. Overwriting it is final, so give a keychain that is still
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
    let (mut contents, kept_keys) = match &state.maybe_key {
      Some(key) => (
        read_file(key, &directory, &SECRETS)?,
        read_file(key, &directory, &KEYS)?,
      ),
      None => return Err(KeySourceError::SecretsFile("no key for the file".to_owned())),
    };
    if recover_keys(&mut contents, &kept_keys) {
      tracing::warn!("took keys the secrets file had lost from the keys file");
      state.is_dirty = true;
    }
    state.are_keys_dirty |= !has_same_keys(&contents, &kept_keys);
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

  /// A new key for the files. Files already there were sealed under a key that is gone, and with it
  /// the tokens and the keys of local databases that no old keychain entry still holds.
  fn create_key(&self, state: &mut State, directory: &Path) -> Result<SymmetricKey, KeySourceError> {
    let key = SymmetricKey::generate(&mut OsRng);
    self.call(state, |keystore| {
      keystore.write_secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT, key.expose_secret())
    })?;
    if directory.join(SECRETS.name).exists() {
      tracing::warn!("the key of the secrets file is gone; signing in again is needed");
      set_aside(directory, &SECRETS);
    }
    if directory.join(KEYS.name).exists() {
      tracing::warn!("the key of the keys file is gone; local databases need setting up again");
      set_aside(directory, &KEYS);
    }
    Ok(key)
  }

  /// Seals both files under a new key and puts it in the keychain item in place of the old one
  /// (#1155). The item keeps its name and format, so a normal start still reads it once and an
  /// older version of the app still opens the files.
  ///
  /// The keychain never holds a key that opens neither file. The files sealed under the new key
  /// are staged next to the current ones, then the keychain takes the key, then the staged files
  /// replace the current ones. A start finds each file's staged copy where a crash left one, and
  /// keeps whichever of the two opens under the key the keychain gave (see [`take_staged`]). A
  /// crash before the keychain took the key leaves the old key and files, signed out all the same.
  /// A keychain that fails the write and can't be read back may have either key: then both copies
  /// are kept current until the next sign-out or start settles it ([`State::maybe_pending_key`]).
  fn change_file_key(&self, state: &mut State) -> Result<(), KeySourceError> {
    self.ensure_open(state)?;
    self.settle_pending_key(state)?;
    let directory = state.maybe_directory.clone().ok_or_else(not_open)?;
    let key = SymmetricKey::generate(&mut OsRng);
    let contents = state.maybe_contents.as_ref().ok_or_else(not_open)?;
    let staged = write_staged(&directory, &SECRETS, &key, contents)
      .and_then(|()| write_staged(&directory, &KEYS, &key, &kept_keys_of(contents)));
    if let Err(error) = staged {
      discard_staged(&directory);
      return Err(error);
    }
    match self.put_file_key(state, &key) {
      KeyWrite::Taken => {},
      KeyWrite::Refused(error) => {
        discard_staged(&directory);
        return Err(error);
      },
      KeyWrite::Unsure(error) => {
        // Discarding the staged files could lock the files out; keeping only them could too.
        state.maybe_pending_key = Some(key);
        return Err(error);
      },
    }
    state.maybe_key = Some(key);
    // Whatever was due is in the staged files now.
    state.is_dirty = false;
    state.are_keys_dirty = false;
    for file in [&SECRETS, &KEYS] {
      if let Err(error) = promote_staged(&directory, file) {
        // The keychain has the new key: the next write seals the file under it and the stale
        // staged copy goes, and until then a start promotes it.
        state.is_dirty = true;
        state.are_keys_dirty = true;
        return Err(error);
      }
    }
    Ok(())
  }

  /// Writes the files' new key to the keychain. A keychain may report a failure and still have
  /// taken it (a Secret Service that timed out); then only the staged files open, so one read on
  /// that path says which it was, or that it can't say either.
  fn put_file_key(&self, state: &mut State, key: &SymmetricKey) -> KeyWrite {
    let Err(error) = self.call(state, |keystore| {
      keystore.write_secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT, key.expose_secret())
    }) else {
      return KeyWrite::Taken;
    };
    // Not through `call`: the failure just now stands, and this is the one read that settles it.
    match self.keystore.read_secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT) {
      Ok(Some(bytes)) if is_key(&bytes, key) => {
        tracing::info!("the keychain reported a failure but took the files' new key");
        state.maybe_blocked = None;
        KeyWrite::Taken
      },
      Ok(_) => KeyWrite::Refused(error),
      Err(read_error) => {
        tracing::warn!(%read_error, "can't tell whether the keychain took the files' new key");
        KeyWrite::Unsure(error)
      },
    }
  }

  /// Settles a key change the keychain left unsure ([`State::maybe_pending_key`]) before the next
  /// one: one read says which key it has, and the files are sealed under that key alone. A start
  /// settles it as well, by which file opens ([`take_staged`]).
  fn settle_pending_key(&self, state: &mut State) -> Result<(), KeySourceError> {
    let Some(pending) = state.maybe_pending_key.take() else {
      return Ok(());
    };
    let found = match self.read_file_key(state) {
      Ok(found) => found,
      Err(error) => {
        state.maybe_pending_key = Some(pending);
        return Err(error);
      },
    };
    if found.is_some_and(|bytes| is_key(&bytes, &pending)) {
      state.maybe_key = Some(pending);
      state.is_dirty = true;
      state.are_keys_dirty = true;
      // Seals both files under the key the keychain has and deletes the staged copies.
      return flush_all(state);
    }
    if let Some(directory) = &state.maybe_directory {
      discard_staged(directory);
    }
    Ok(())
  }

  /// Keeps a key found in an old keychain entry. The entry goes once both files have the key; until
  /// then it stays, so a file that can't be written costs a read at the next start, not the key.
  fn take_old_key(
    &self,
    state: &mut State,
    account: &str,
    name: &str,
    key: &SymmetricKey,
  ) -> Result<(), KeySourceError> {
    replace_secret(contents_of(state)?, name, encode_key(key));
    state.is_dirty = true;
    state.are_keys_dirty = true;
    self.retire_old_entries(state, account);
    Ok(())
  }

  /// Deletes the old keychain entries of the account's keys once both files hold the keys (#1223):
  /// then the files' key is the one item a start reads, also after an update with a new signature.
  /// First the files take the keys, then the entry goes, then the file records that it went; a
  /// crash in between deletes again, and deleting an entry that isn't there raises no dialog.
  ///
  /// Tried once per run, never while a keychain failure stands (no dialog in the middle of a run),
  /// and left in place after [`MAX_DELETE_ATTEMPTS`] failed starts. The caller has the key either
  /// way, so nothing here fails the call.
  fn retire_old_entries(&self, state: &mut State, account: &str) {
    if let Err(error) = flush_all(state) {
      tracing::warn!(%error, "could not write the secrets files; old keychain entries stay for now");
      return;
    }
    for (kind, service) in OLD_KEY_ENTRIES {
      let name = item_name(kind, account);
      let was_tried = state.tried_this_run.contains(&name);
      let Ok(contents) = contents_of(state) else {
        return;
      };
      // Only a usable key in the files lets its entry go.
      let has_key = matches!(stored_key(contents, &name), Ok(Some(_)));
      let is_due = has_key && !contents.moved.contains(&name);
      if !is_due || was_tried {
        continue;
      }
      state.tried_this_run.insert(name.clone());
      if self.is_blocked(state) {
        continue;
      }
      let outcome = self.call(state, |keystore| keystore.delete(service, account));
      let Ok(contents) = contents_of(state) else {
        return;
      };
      record_delete(contents, name, outcome);
      state.is_dirty = true;
    }
    if let Err(error) = flush(state) {
      tracing::warn!(%error, "could not record the cleaned-up keychain entries");
    }
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

  /// A keychain failure still stands, so a call now would only repeat it.
  fn is_blocked(&self, state: &State) -> bool {
    state
      .maybe_blocked
      .as_ref()
      .is_some_and(|blocked| (self.now)() < blocked.until)
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

/// The scope of an item of `kind` ([`item_name`]), `None` for an item of another kind.
fn scope_of<'a>(name: &'a str, kind: &str) -> Option<&'a str> {
  name.strip_prefix(kind)?.strip_prefix('@')
}

/// Whether an item belongs to the server with origin `server`: its scope is the origin, or starts
/// with the origin and `|` (a refresh token's key, an account's keys). An origin has neither `@`
/// nor `|`, and the `|` keeps `https://a.example.com:8443` apart from `https://a.example.com`.
fn is_item_of(name: &str, server: &str) -> bool {
  name.split_once('@').is_some_and(|(_, scope)| {
    scope == server
      || scope
        .strip_prefix(server)
        .is_some_and(|rest| rest.starts_with('|'))
  })
}

/// Whether keychain bytes are `key`, compared in constant time.
fn is_key(bytes: &[u8], key: &SymmetricKey) -> bool {
  SymmetricKey::from_slice(bytes).is_some_and(|found| found.is_same_key(key))
}

/// A new database key. It counts only once both files have it: a database whose key is only in
/// memory can't be opened after a restart, so a failed write fails the call and keeps nothing.
fn create_database_key(state: &mut State, name: &str) -> Result<SymmetricKey, KeySourceError> {
  let key = SymmetricKey::generate(&mut OsRng);
  let contents = contents_of(state)?;
  replace_secret(contents, name, encode_key(&key));
  // The keychain just said it has no entry: nothing to clean up there.
  contents.moved.insert(name.to_owned());
  state.is_dirty = true;
  state.are_keys_dirty = true;
  if let Err(error) = flush_all(state) {
    forget_secret(contents_of(state)?, name);
    // The secrets file may have it already; the next write takes it out again.
    state.is_dirty = true;
    return Err(error);
  }
  Ok(key)
}

/// Notes how deleting an old key entry went: gone, to try at the next start, or left in place.
fn record_delete(contents: &mut Contents, name: String, outcome: Result<(), KeySourceError>) {
  let Err(error) = outcome else {
    contents.failed_deletes.remove(&name);
    contents.moved.insert(name);
    return;
  };
  let attempts = contents.failed_deletes.entry(name.clone()).or_insert(0);
  *attempts += 1;
  if *attempts < MAX_DELETE_ATTEMPTS {
    tracing::info!(%error, "could not delete an old keychain entry; trying at the next start");
    return;
  }
  tracing::info!(%error, "left an old keychain entry in place; the secrets files hold its key");
  contents.failed_deletes.remove(&name);
  contents.moved.insert(name);
}

fn contents_of(state: &mut State) -> Result<&mut Contents, KeySourceError> {
  state.maybe_contents.as_mut().ok_or_else(not_open)
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

/// A key in the files; one that doesn't decode counts as not there.
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

/// Whether an item is a database or backup key, which the keys file holds as well.
fn is_key_item(name: &str) -> bool {
  [DATABASE_KEY_KIND, BACKUP_KEY_KIND].into_iter().any(|kind| {
    name
      .strip_prefix(kind)
      .is_some_and(|scope| scope.starts_with('@'))
  })
}

fn key_items(contents: &Contents) -> impl Iterator<Item = (&String, &String)> {
  contents.secrets.iter().filter(|(name, _)| is_key_item(name))
}

/// What the keys file holds: the keys of `contents` and nothing else.
fn kept_keys_of(contents: &Contents) -> Contents {
  let mut kept = Contents::default();
  kept.secrets = key_items(contents)
    .map(|(name, key)| (name.clone(), key.clone()))
    .collect();
  kept
}

/// Takes the keys the secrets file lost (a damaged file, set aside) from the keys file. Where both
/// have a usable key the secrets file wins: it is written last. Says whether it took any.
fn recover_keys(contents: &mut Contents, kept: &Contents) -> bool {
  let mut has_taken = false;
  for (name, key) in key_items(kept) {
    let is_usable = matches!(stored_key(contents, name), Ok(Some(_)));
    if !is_usable && matches!(stored_key(kept, name), Ok(Some(_))) {
      replace_secret(contents, name, key.clone());
      has_taken = true;
    }
  }
  has_taken
}

fn has_same_keys(contents: &Contents, kept: &Contents) -> bool {
  key_items(contents).eq(key_items(kept))
}

fn has_sealed_files(directory: &Path) -> bool {
  [&SECRETS, &KEYS]
    .into_iter()
    .flat_map(|file| [file.name, file.staged])
    .any(|name| directory.join(name).exists())
}

/// A file's contents. A missing one is empty; one that doesn't open is set aside: the secrets file
/// costs a sign-in, the keys file a second copy until the next write. A file that can't be read at
/// all (a disk error) stays where it is and fails the call.
///
/// A copy from before the last sign-out is sealed under a key the keychain no longer has, so it
/// is set aside like a damaged file (#1155).
fn read_file(key: &SymmetricKey, directory: &Path, file: &SealedFile) -> Result<Contents, KeySourceError> {
  let current = open_sealed(key, &directory.join(file.name))?;
  match take_staged(key, directory, file, current)? {
    Sealed::Missing => Ok(Contents::default()),
    Sealed::Opened(contents) => Ok(contents),
    Sealed::Unreadable => {
      tracing::warn!(
        file = file.name,
        "a sealed file of local secrets is damaged or out of date and set aside"
      );
      set_aside(directory, file);
      Ok(Contents::default())
    },
  }
}

/// Settles a staged copy a sign-out's key change left behind ([`SecretVault::change_file_key`]).
/// The keychain's key decides: where the current file opens under it, the change never reached the
/// keychain or was finished, and the staged copy goes; where only the staged copy opens, the
/// keychain took the new key before the crash, and the staged copy replaces the current file.
/// A staged copy that doesn't open is a leftover of an earlier change and goes.
fn take_staged(
  key: &SymmetricKey,
  directory: &Path,
  file: &SealedFile,
  current: Sealed,
) -> Result<Sealed, KeySourceError> {
  let staged_path = directory.join(file.staged);
  if matches!(current, Sealed::Opened(_)) {
    remove_if_there(&staged_path);
    return Ok(current);
  }
  match open_sealed(key, &staged_path)? {
    Sealed::Missing => Ok(current),
    Sealed::Unreadable => {
      remove_if_there(&staged_path);
      Ok(current)
    },
    Sealed::Opened(contents) => {
      match promote_staged(directory, file) {
        Ok(()) => tracing::info!(
          file = file.name,
          "finished a change of the files' key that a crash had stopped"
        ),
        // The staged copy is the file for now: the next write replaces the current one and
        // deletes it, and until then each start takes it again.
        Err(error) => tracing::warn!(%error, file = file.name, "could not put a staged file in place"),
      }
      Ok(Sealed::Opened(contents))
    },
  }
}

/// What is at `path`, opened with `key`. Only a disk error is an error.
fn open_sealed(key: &SymmetricKey, path: &Path) -> Result<Sealed, KeySourceError> {
  let handle = match fs::File::open(path) {
    Ok(handle) => handle,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Sealed::Missing),
    Err(error) => return Err(KeySourceError::SecretsFile(error.to_string())),
  };
  let mut sealed = Vec::new();
  handle
    .take(MAX_FILE_BYTES + 1)
    .read_to_end(&mut sealed)
    .map_err(|error| KeySourceError::SecretsFile(error.to_string()))?;
  let parsed = (sealed.len() as u64 <= MAX_FILE_BYTES)
    .then(|| secret_file::open(key, &sealed).ok())
    .flatten()
    .and_then(|plaintext| serde_json::from_slice(plaintext.expose_secret()).ok());
  Ok(parsed.map_or(Sealed::Unreadable, Sealed::Opened))
}

fn set_aside(directory: &Path, file: &SealedFile) {
  if let Err(error) = fs::rename(directory.join(file.name), directory.join(file.set_aside)) {
    // The next write replaces the file, which nothing can read anyway.
    tracing::warn!(%error, file = file.name, "could not set a sealed file aside");
  }
}

/// Deletes a copy nothing reads any more (set aside or stale staged).
fn remove_if_there(path: &Path) {
  match fs::remove_file(path) {
    Err(error) if error.kind() != io::ErrorKind::NotFound => {
      tracing::warn!(%error, "could not delete a leftover secrets file");
    },
    _ => {},
  }
}

/// Seals `contents` under the files' next key into the file's staged copy.
fn write_staged(
  directory: &Path,
  file: &SealedFile,
  key: &SymmetricKey,
  contents: &Contents,
) -> Result<(), KeySourceError> {
  let sealed = seal(key, contents)?;
  replace_file(directory, file.temporary, file.staged, &sealed)
    .map_err(|error| KeySourceError::SecretsFile(error.to_string()))
}

/// The staged copy replaces the file, in one step.
fn promote_staged(directory: &Path, file: &SealedFile) -> Result<(), KeySourceError> {
  fs::rename(directory.join(file.staged), directory.join(file.name))
    .map_err(|error| KeySourceError::SecretsFile(error.to_string()))?;
  sync_directory(directory);
  Ok(())
}

/// The keychain kept the old key: the staged copies would never open.
fn discard_staged(directory: &Path) {
  for file in [&SECRETS, &KEYS] {
    remove_if_there(&directory.join(file.staged));
  }
}

/// Writes changed contents to the secrets file. A failed write stays due: see [`State::is_dirty`].
fn flush(state: &mut State) -> Result<(), KeySourceError> {
  if !state.is_dirty {
    return Ok(());
  }
  let contents = state.maybe_contents.as_ref().ok_or_else(not_open)?;
  write_file(state, &SECRETS, contents)?;
  state.is_dirty = false;
  Ok(())
}

/// Writes both files where they are behind; the keys file is written only when a key changed.
fn flush_all(state: &mut State) -> Result<(), KeySourceError> {
  flush(state)?;
  if !state.are_keys_dirty {
    return Ok(());
  }
  let keys = kept_keys_of(state.maybe_contents.as_ref().ok_or_else(not_open)?);
  write_file(state, &KEYS, &keys)?;
  state.are_keys_dirty = false;
  Ok(())
}

/// Seals `contents` and replaces the file in one step, so a crash leaves the old file or the new one
/// and never half of either.
/// A staged copy left by a key change whose promotion failed is stale once the file is written.
/// While the keychain's answer to a key change is unsure, the staged copy is written as well, under
/// the new key (see [`State::maybe_pending_key`]).
fn write_file(state: &State, file: &SealedFile, contents: &Contents) -> Result<(), KeySourceError> {
  let directory = state.maybe_directory.as_ref().ok_or_else(not_open)?;
  let key = state.maybe_key.as_ref().ok_or_else(not_open)?;
  let sealed = seal(key, contents)?;
  replace_file(directory, file.temporary, file.name, &sealed)
    .map_err(|error| KeySourceError::SecretsFile(error.to_string()))?;
  match &state.maybe_pending_key {
    Some(pending) => write_staged(directory, file, pending, contents),
    None => {
      remove_if_there(&directory.join(file.staged));
      Ok(())
    },
  }
}

fn seal(key: &SymmetricKey, contents: &Contents) -> Result<Vec<u8>, KeySourceError> {
  let plaintext = serialize(contents)?;
  secret_file::seal(key, &plaintext, &mut OsRng)
    .map_err(|error| KeySourceError::SecretsFile(error.to_string()))
}

fn not_open() -> KeySourceError {
  KeySourceError::SecretsFile("the file is not open".to_owned())
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

fn replace_file(directory: &Path, temporary: &str, target: &str, sealed: &[u8]) -> io::Result<()> {
  fs::create_dir_all(directory)?;
  let temporary = directory.join(temporary);
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
  let mut handle = options.open(&temporary)?;
  handle.write_all(sealed)?;
  handle.sync_all()?;
  drop(handle);
  fs::rename(&temporary, directory.join(target))?;
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
  const SIGN_OUT_ITEMS: [(TokenKind, &str); 2] = [
    (TokenKind::RefreshToken, SERVER_A),
    (TokenKind::LocalSession, SERVER_A),
  ];
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
      assert_eq!(
        fake.dialogs(),
        1,
        "after an update with a new signature: one dialog"
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

    /// Everything an install from before 1.0.18 has, all at once; the counts are what the PR
    /// reports. Afterwards the keychain holds the file's key and nothing else (#1223).
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
      // key. Deletes: the origin-only token, session, refresh token, database and backup keys.
      assert_eq!(
        fake.counts(),
        Counts {
          reads: 4,
          writes: 1,
          deletes: 5
        }
      );
      // Each existing entry is read and deleted once, the backup key's only deleted.
      assert_eq!(fake.dialogs(), 7);
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
      assert_eq!(
        fake.entries(),
        vec![(VAULT_SERVICE.to_owned(), VAULT_KEY_ACCOUNT.to_owned())]
      );
      drop(vault);

      fake.reset_counts();
      let vault = start(&fake, &directory);
      assert_eq!(
        fake.counts(),
        Counts {
          reads: 1,
          writes: 0,
          deletes: 0
        }
      );
      assert_eq!(vault.database_key(ACCOUNT_A).unwrap().expose_secret(), &[3; 32]);
      assert_eq!(
        vault.kept_backup_key(ACCOUNT_A).unwrap().unwrap().expose_secret(),
        &[4; 32]
      );
    }

    /// An install of 1.0.18 to 1.0.21: the file has the tokens and copies of the keys, whose entries
    /// are in the keychain too. Its first start deletes them, once, and reads nothing more.
    #[test]
    fn the_keychain_copies_of_the_keys_go_once() {
      let fake = Fake::new();
      let directory = TestDir::new();
      write_file_of_1_0_18(&fake, &directory);
      fake.put_secret(DATABASE_KEY_SERVICE, ACCOUNT_A, &[3; 32]);
      fake.put_secret(BACKUP_KEY_SERVICE, ACCOUNT_A, &[4; 32]);
      fake.reset_counts();

      let vault = start(&fake, &directory);

      assert_eq!(
        fake.counts(),
        Counts {
          reads: 1,
          writes: 0,
          deletes: 2
        }
      );
      assert_eq!(fake.dialogs(), 3, "the file's key, then two deletes, once");
      assert_eq!(
        fake.entries(),
        vec![(VAULT_SERVICE.to_owned(), VAULT_KEY_ACCOUNT.to_owned())]
      );
      assert_eq!(vault.database_key(ACCOUNT_A).unwrap().expose_secret(), &[3; 32]);
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
      assert_eq!(fake.dialogs(), 1);
      fs::remove_file(directory.path().join(VAULT_FILE)).unwrap();
      let vault = reopen(&fake, &directory);
      assert_eq!(
        vault.database_key(ACCOUNT_A).unwrap().expose_secret(),
        &[3; 32],
        "the keys file had them before the entries went"
      );
      assert_eq!(
        vault.kept_backup_key(ACCOUNT_A).unwrap().unwrap().expose_secret(),
        &[4; 32]
      );
    }

    #[test]
    fn a_second_account_from_before_the_move_costs_one_read_and_one_delete_once() {
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
          deletes: 1
        }
      );
      assert!(!fake.has(DATABASE_KEY_SERVICE, ACCOUNT_B));

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

    /// A crash after the files took a key from the keychain and before its entry went: the next
    /// start finishes the deletion, and the key is never only in the keychain or only in memory.
    #[test]
    fn a_crash_before_an_old_key_entry_is_deleted_finishes_the_next_time() {
      let fake = Fake::new();
      fake.put_secret(DATABASE_KEY_SERVICE, ACCOUNT_A, &[3; 32]);
      let directory = TestDir::new();
      fake.fail_deletes(true);
      let vault = reopen(&fake, &directory);
      assert_eq!(vault.database_key(ACCOUNT_A).unwrap().expose_secret(), &[3; 32]);
      assert!(fake.has(DATABASE_KEY_SERVICE, ACCOUNT_A));
      drop(vault);

      fake.fail_deletes(false);
      reopen(&fake, &directory).database_key(ACCOUNT_A).unwrap();
      assert!(
        !fake.has(DATABASE_KEY_SERVICE, ACCOUNT_A),
        "the next start deletes it"
      );
      fs::remove_file(directory.path().join(VAULT_FILE)).unwrap();
      assert_eq!(
        reopen(&fake, &directory)
          .database_key(ACCOUNT_A)
          .unwrap()
          .expose_secret(),
        &[3; 32],
        "both files had it before anything was deleted"
      );
    }

    /// A refused delete (the user clicked "Deny") is asked again at the next starts, not in the same
    /// run, and after three starts the entry is left: it holds a key the files have.
    #[test]
    fn an_old_key_entry_that_will_not_go_is_left_after_three_starts() {
      let fake = Fake::new();
      fake.put_secret(DATABASE_KEY_SERVICE, ACCOUNT_A, &[3; 32]);
      fake.put_secret(BACKUP_KEY_SERVICE, ACCOUNT_A, &[4; 32]);
      fake.fail_deletes(true);
      let directory = TestDir::new();
      let backup_key = SymmetricKey::from_bytes([4; 32]);
      for _ in 0..MAX_DELETE_ATTEMPTS {
        fake.reset_counts();
        let vault = reopen(&fake, &directory);
        vault.database_key(ACCOUNT_A).unwrap();
        vault.keep_backup_key(ACCOUNT_A, &backup_key).unwrap();
        vault.database_key(ACCOUNT_A).unwrap();
        assert_eq!(
          fake.counts().deletes,
          1,
          "one refusal stands for the rest of the run"
        );
      }

      // The backup key's entry had its turns while the database key's refusal stood.
      for _ in 0..MAX_DELETE_ATTEMPTS {
        reopen(&fake, &directory)
          .keep_backup_key(ACCOUNT_A, &backup_key)
          .unwrap();
      }
      fake.reset_counts();
      let vault = reopen(&fake, &directory);
      vault.database_key(ACCOUNT_A).unwrap();
      vault.keep_backup_key(ACCOUNT_A, &backup_key).unwrap();
      assert_eq!(fake.counts().deletes, 0, "left in place");
      assert!(fake.has(DATABASE_KEY_SERVICE, ACCOUNT_A));
      drop(vault);

      // A left entry is still a way back for a key both files lost.
      fs::remove_file(directory.path().join(VAULT_FILE)).unwrap();
      fs::remove_file(directory.path().join(KEYS_FILE)).unwrap();
      assert_eq!(
        reopen(&fake, &directory)
          .database_key(ACCOUNT_A)
          .unwrap()
          .expose_secret(),
        &[3; 32]
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

    /// Keeping a backup key needs no keychain any more: a keychain that refuses once the file is
    /// open changes nothing.
    #[test]
    fn a_backup_key_is_kept_while_the_keychain_refuses() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.load_token(TokenKind::LocalSession, SERVER_A).unwrap();
      fake.deny(true);
      let key = SymmetricKey::from_bytes([4; 32]);
      vault.keep_backup_key(ACCOUNT_A, &key).unwrap();
      assert!(
        vault
          .kept_backup_key(ACCOUNT_A)
          .unwrap()
          .unwrap()
          .is_same_key(&key)
      );
      assert!(!fake.has(BACKUP_KEY_SERVICE, ACCOUNT_A));
    }

    /// A refused clean-up of an old token entry stands for a minute: the old key entries wait for
    /// the next start instead of raising more dialogs now, and nothing fails meanwhile.
    #[test]
    fn a_standing_failure_only_delays_the_clean_up() {
      let fake = Fake::new();
      fake.put_text(VAULT_SERVICE, OLD_REFRESH_A, "old-refresh");
      fake.put_secret(BACKUP_KEY_SERVICE, ACCOUNT_A, &[1; 32]);
      fake.fail_deletes(true);
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.load_token(TokenKind::RefreshToken, SERVER_A).unwrap();
      fake.reset_counts();
      let key = SymmetricKey::from_bytes([4; 32]);
      vault.keep_backup_key(ACCOUNT_A, &key).unwrap();
      assert_eq!(fake.counts(), Counts::default());
      assert!(fake.has(BACKUP_KEY_SERVICE, ACCOUNT_A));
      drop(vault);

      fake.fail_deletes(false);
      let vault = reopen(&fake, &directory);
      vault.keep_backup_key(ACCOUNT_A, &key).unwrap();
      assert!(!fake.has(BACKUP_KEY_SERVICE, ACCOUNT_A));
      assert!(
        vault
          .kept_backup_key(ACCOUNT_A)
          .unwrap()
          .unwrap()
          .is_same_key(&key),
        "the old entry's older key never wins"
      );
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
      drop(vault);
      fs::remove_file(directory.path().join(VAULT_FILE)).unwrap();
      assert!(
        reopen(&fake, &directory)
          .kept_backup_key(ACCOUNT_A)
          .unwrap()
          .unwrap()
          .is_same_key(&key),
        "the keys file has it too"
      );
    }

    /// A key from an old keychain entry works while the files can't be written; the entry stays
    /// until they have it.
    #[test]
    fn a_database_key_works_without_the_file_and_is_kept_later() {
      let fake = Fake::new();
      fake.put_secret(DATABASE_KEY_SERVICE, ACCOUNT_A, &[3; 32]);
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.load_token(TokenKind::LocalSession, SERVER_A).unwrap();
      block_writes(&directory);
      assert_eq!(vault.database_key(ACCOUNT_A).unwrap().expose_secret(), &[3; 32]);
      assert!(fake.has(DATABASE_KEY_SERVICE, ACCOUNT_A), "not deleted yet");
      allow_writes(&directory);
      vault.save_token(TokenKind::LocalSession, SERVER_A, "s").unwrap();
      drop(vault);
      fake.reset_counts();
      let vault = reopen(&fake, &directory);
      assert_eq!(vault.database_key(ACCOUNT_A).unwrap().expose_secret(), &[3; 32]);
      assert_eq!(
        fake.counts(),
        Counts {
          reads: 1,
          writes: 0,
          deletes: 1
        },
        "the file's key, then the entry goes"
      );
      assert!(!fake.has(DATABASE_KEY_SERVICE, ACCOUNT_A));
    }

    /// A database made with a key only memory has would be lost at the next start.
    #[test]
    fn a_new_database_key_is_handed_out_only_once_both_files_have_it() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.load_token(TokenKind::LocalSession, SERVER_A).unwrap();
      for temporary in [TEMPORARY_FILE, KEYS_TEMPORARY_FILE] {
        block_write_of(&directory, temporary);
        assert!(
          vault.database_key(ACCOUNT_A).is_err(),
          "{temporary} is in the way"
        );
        assert!(
          vault.database_key(ACCOUNT_A).is_err(),
          "and nothing is kept meanwhile"
        );
        allow_write_of(&directory, temporary);
      }
      let key = vault.database_key(ACCOUNT_A).unwrap();
      drop(vault);
      fs::remove_file(directory.path().join(VAULT_FILE)).unwrap();
      assert!(
        reopen(&fake, &directory)
          .database_key(ACCOUNT_A)
          .unwrap()
          .is_same_key(&key)
      );
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
          writes: 1,
          deletes: 2
        },
        "the files' new key is the one write"
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

      fs::write(directory.path().join(KEYS_SET_ASIDE_FILE), b"set aside").unwrap();

      vault.purge_after_sign_out(&ITEMS).unwrap();

      assert!(!directory.path().join(SET_ASIDE_FILE).exists());
      assert!(!directory.path().join(KEYS_SET_ASIDE_FILE).exists());
      assert!(
        directory.path().join(KEYS_FILE).exists(),
        "the keys stay, like the database"
      );
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

  /// Issue #1155: a process of the user that can write the files but not read them puts back a
  /// copy from before a sign-out.
  mod rollback {
    use super::*;

    #[test]
    fn a_copy_from_before_sign_out_brings_no_token_back() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let database_key = vault.database_key(ACCOUNT_A).unwrap();
      let before = fs::read(directory.path().join(VAULT_FILE)).unwrap();
      sign_out(&vault);
      drop(vault);

      fs::write(directory.path().join(VAULT_FILE), before).unwrap();

      let vault = reopen(&fake, &directory);
      assert!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .is_none()
      );
      assert!(
        vault.database_key(ACCOUNT_A).unwrap().is_same_key(&database_key),
        "the keys file still has the database key"
      );
      assert!(directory.path().join(SET_ASIDE_FILE).exists());
    }

    /// Both files put back: no token either. The database key goes with them, as it would if they
    /// were deleted, which such a process can do anyway.
    #[test]
    fn copies_of_both_files_bring_no_token_back() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let before = [VAULT_FILE, KEYS_FILE].map(|name| fs::read(directory.path().join(name)).unwrap());
      sign_out(&vault);
      drop(vault);

      for (name, copy) in [VAULT_FILE, KEYS_FILE].into_iter().zip(before) {
        fs::write(directory.path().join(name), copy).unwrap();
      }

      let vault = reopen(&fake, &directory);
      assert!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .is_none()
      );
      assert!(
        vault
          .load_session_note(SessionNote::TokenSubject, SERVER_A)
          .unwrap()
          .is_none()
      );
    }

    /// The goal of #1150 and #1223 holds after a sign-out: the key changed, the item didn't.
    #[test]
    fn a_normal_start_after_sign_out_still_reads_the_keychain_once() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let database_key = vault.database_key(ACCOUNT_A).unwrap();
      sign_out(&vault);
      drop(vault);
      fake.reset_counts();

      let vault = start(&fake, &directory);

      assert_eq!(
        fake.counts(),
        Counts {
          reads: 1,
          writes: 0,
          deletes: 0
        }
      );
      assert!(vault.database_key(ACCOUNT_A).unwrap().is_same_key(&database_key));
      assert_eq!(
        fake.entries(),
        [(VAULT_SERVICE.to_owned(), VAULT_KEY_ACCOUNT.to_owned())]
      );
    }

    /// Versions from 1.0.18 read the same item as a 32-byte key and the files under it.
    #[test]
    fn an_older_version_still_opens_the_files_after_sign_out() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let old_key = fake.secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT).unwrap();
      sign_out(&vault);
      drop(vault);

      let new_key = fake.secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT).unwrap();
      assert_ne!(new_key, old_key);
      let key = SymmetricKey::from_slice(&new_key).unwrap();
      for name in [VAULT_FILE, KEYS_FILE] {
        let sealed = fs::read(directory.path().join(name)).unwrap();
        assert!(secret_file::open(&key, &sealed).is_ok(), "{name}");
      }
      assert!(!has_staged_files(&directory));
    }

    #[test]
    fn an_install_from_1_0_18_is_protected_from_its_first_sign_out() {
      let fake = Fake::new();
      let directory = TestDir::new();
      write_file_of_1_0_18(&fake, &directory);
      let vault = start(&fake, &directory);
      let before = fs::read(directory.path().join(VAULT_FILE)).unwrap();
      sign_out(&vault);
      drop(vault);

      fs::write(directory.path().join(VAULT_FILE), before).unwrap();

      let vault = reopen(&fake, &directory);
      assert!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .is_none()
      );
      assert_eq!(vault.database_key(ACCOUNT_A).unwrap().expose_secret(), &[3; 32]);
    }

    /// A staged copy only wins where the current file doesn't open: a signed-out copy from the last
    /// change, put at the staged name after a new sign-in, doesn't undo the sign-in.
    #[test]
    fn a_staged_copy_never_wins_over_a_current_file() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let older = fs::read(directory.path().join(VAULT_FILE)).unwrap();
      sign_out(&vault);
      let signed_out = fs::read(directory.path().join(VAULT_FILE)).unwrap();
      vault
        .save_token(TokenKind::RefreshToken, SERVER_A, "signed-in-again")
        .unwrap();
      drop(vault);

      for copy in [older, signed_out] {
        fs::write(directory.path().join(STAGED_FILE), copy).unwrap();
        fs::write(directory.path().join(KEYS_STAGED_FILE), b"not a sealed file").unwrap();

        let vault = reopen(&fake, &directory);
        assert_eq!(
          vault
            .load_token(TokenKind::RefreshToken, SERVER_A)
            .unwrap()
            .unwrap()
            .as_str(),
          "signed-in-again"
        );
        assert!(!has_staged_files(&directory));
      }
    }
  }

  /// A sign-out's key change stopped at each step: never locked out, at worst signed out.
  mod key_change {
    use super::*;

    /// The files were staged under the new key, but the keychain never got it.
    #[test]
    fn a_crash_before_the_keychain_keeps_the_old_key() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let (database_key, old_key, old_files) = sign_out_up_to_the_key_change(&fake, &directory);
      stage_current_files(&directory, &old_files);
      fake.put_secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT, &old_key);

      assert_finished(&fake, &directory, &database_key);
      assert_eq!(fake.secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT).unwrap(), old_key);
    }

    /// The keychain has the new key, the files under it are still staged.
    #[test]
    fn a_crash_after_the_keychain_finishes_the_change() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let (database_key, _, old_files) = sign_out_up_to_the_key_change(&fake, &directory);
      stage_current_files(&directory, &old_files);

      assert_finished(&fake, &directory, &database_key);
      assert_opens_under_the_keychains_key(&fake, &directory);
    }

    /// The secrets file was replaced, the keys file still waits.
    #[test]
    fn a_crash_between_the_files_finishes_the_change() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let (database_key, _, old_files) = sign_out_up_to_the_key_change(&fake, &directory);
      fs::rename(
        directory.path().join(KEYS_FILE),
        directory.path().join(KEYS_STAGED_FILE),
      )
      .unwrap();
      fs::write(directory.path().join(KEYS_FILE), &old_files[1]).unwrap();

      assert_finished(&fake, &directory, &database_key);
      assert_opens_under_the_keychains_key(&fake, &directory);
    }

    #[test]
    fn a_refused_key_keeps_the_old_one_and_says_so() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let database_key = vault.database_key(ACCOUNT_A).unwrap();
      clear_tokens(&vault);
      let old_key = fake.secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT).unwrap();
      fake.refuse_next_write();

      assert!(vault.purge_after_sign_out(&SIGN_OUT_ITEMS).is_err());

      assert_eq!(fake.secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT).unwrap(), old_key);
      assert!(!has_staged_files(&directory));
      drop(vault);
      assert_finished(&fake, &directory, &database_key);
    }

    /// The keychain took the key but reported a failure: then only the new files open.
    #[test]
    fn a_keychain_that_took_the_key_but_said_no_counts_as_done() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let database_key = vault.database_key(ACCOUNT_A).unwrap();
      clear_tokens(&vault);
      fake.lose_next_write_answer();

      vault.purge_after_sign_out(&SIGN_OUT_ITEMS).unwrap();

      drop(vault);
      assert_finished(&fake, &directory, &database_key);
      assert_opens_under_the_keychains_key(&fake, &directory);
    }

    /// The keychain failed the write and the read-back (a Secret Service that hangs): it may have
    /// either key. A start finds the files under both, with what was written since.
    #[test]
    fn an_unsure_keychain_keeps_the_files_open_under_either_key() {
      for keeps_new_key in [true, false] {
        let fake = Fake::new();
        let directory = TestDir::new();
        let vault = start(&fake, &directory);
        let database_key = vault.database_key(ACCOUNT_A).unwrap();
        clear_tokens(&vault);
        let old_key = fake.secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT).unwrap();
        fake.lose_next_write_answer();
        fake.fail_next_read();

        assert!(vault.purge_after_sign_out(&SIGN_OUT_ITEMS).is_err());
        vault
          .save_token(TokenKind::RefreshToken, SERVER_A, "signed-in-again")
          .unwrap();
        drop(vault);
        if !keeps_new_key {
          fake.put_secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT, &old_key);
        }

        let vault = reopen(&fake, &directory);
        assert_eq!(
          vault
            .load_token(TokenKind::RefreshToken, SERVER_A)
            .unwrap()
            .unwrap()
            .as_str(),
          "signed-in-again",
          "the keychain kept the new key: {keeps_new_key}"
        );
        assert!(vault.database_key(ACCOUNT_A).unwrap().is_same_key(&database_key));
        assert!(!has_staged_files(&directory));
      }
    }

    #[test]
    fn the_next_sign_out_settles_an_unsure_key_change() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let database_key = vault.database_key(ACCOUNT_A).unwrap();
      clear_tokens(&vault);
      fake.lose_next_write_answer();
      fake.fail_next_read();
      assert!(vault.purge_after_sign_out(&SIGN_OUT_ITEMS).is_err());

      vault.purge_after_sign_out(&SIGN_OUT_ITEMS).unwrap();

      assert!(!has_staged_files(&directory));
      assert_opens_under_the_keychains_key(&fake, &directory);
      drop(vault);
      assert_finished(&fake, &directory, &database_key);
    }

    /// A failed key change doesn't keep the old token entries from going.
    #[test]
    fn a_failed_key_change_still_lets_the_old_entries_go() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      fake.put_text(VAULT_SERVICE, OLD_REFRESH_A, "from-an-older-app");
      clear_tokens(&vault);
      fake.lose_next_write_answer();
      fake.fail_next_read();

      assert!(vault.purge_after_sign_out(&SIGN_OUT_ITEMS).is_err());

      assert!(!fake.has(VAULT_SERVICE, OLD_REFRESH_A));
    }

    /// The staged file couldn't take the file's place while running: the next write or start does.
    #[test]
    fn a_failed_replacement_is_finished_later() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let database_key = vault.database_key(ACCOUNT_A).unwrap();
      clear_tokens(&vault);
      // A directory with something in it can't be replaced by a file.
      let file = directory.path().join(VAULT_FILE);
      fs::remove_file(&file).unwrap();
      fs::create_dir(&file).unwrap();
      fs::write(file.join("in-the-way"), b"").unwrap();

      assert!(vault.purge_after_sign_out(&SIGN_OUT_ITEMS).is_err());

      fs::remove_dir_all(&file).unwrap();
      vault
        .save_token(TokenKind::RefreshToken, SERVER_A, "signed-in-again")
        .unwrap();
      assert!(!directory.path().join(STAGED_FILE).exists());
      drop(vault);
      let vault = reopen(&fake, &directory);
      assert_eq!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .unwrap()
          .as_str(),
        "signed-in-again"
      );
      assert!(vault.database_key(ACCOUNT_A).unwrap().is_same_key(&database_key));
      assert!(!has_staged_files(&directory));
    }

    /// Signs out and returns the database key, the files' key before the change, and the files
    /// as they were then, already signed out.
    fn sign_out_up_to_the_key_change(
      fake: &Arc<Fake>,
      directory: &TestDir,
    ) -> (SymmetricKey, Vec<u8>, [Vec<u8>; 2]) {
      let vault = start(fake, directory);
      let database_key = vault.database_key(ACCOUNT_A).unwrap();
      clear_tokens(&vault);
      let old_key = fake.secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT).unwrap();
      let old_files = [VAULT_FILE, KEYS_FILE].map(|name| fs::read(directory.path().join(name)).unwrap());
      vault.purge_after_sign_out(&SIGN_OUT_ITEMS).unwrap();
      (database_key, old_key, old_files)
    }

    /// Back to the moment the files were staged: the files under the new key at their staged names,
    /// the old ones in place.
    fn stage_current_files(directory: &TestDir, old_files: &[Vec<u8>; 2]) {
      for (file, old) in [&SECRETS, &KEYS].into_iter().zip(old_files) {
        let path = directory.path().join(file.name);
        fs::rename(&path, directory.path().join(file.staged)).unwrap();
        fs::write(&path, old).unwrap();
      }
    }

    /// A start after the crash: one keychain read, signed out, the database key kept, nothing
    /// staged left.
    fn assert_finished(fake: &Arc<Fake>, directory: &TestDir, database_key: &SymmetricKey) {
      fake.reset_counts();
      let vault = reopen(fake, directory);
      assert!(
        vault
          .load_token(TokenKind::RefreshToken, SERVER_A)
          .unwrap()
          .is_none()
      );
      assert!(vault.database_key(ACCOUNT_A).unwrap().is_same_key(database_key));
      assert_eq!(
        fake.counts(),
        Counts {
          reads: 1,
          writes: 0,
          deletes: 0
        }
      );
      assert!(!has_staged_files(directory));
      assert!(!directory.path().join(SET_ASIDE_FILE).exists());
      assert!(!directory.path().join(KEYS_SET_ASIDE_FILE).exists());
    }

    fn assert_opens_under_the_keychains_key(fake: &Fake, directory: &TestDir) {
      let key = SymmetricKey::from_slice(&fake.secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT).unwrap()).unwrap();
      for name in [VAULT_FILE, KEYS_FILE] {
        let sealed = fs::read(directory.path().join(name)).unwrap();
        assert!(secret_file::open(&key, &sealed).is_ok(), "{name}");
      }
    }
  }

  mod damaged_file {
    use super::*;

    /// What the keychain copies were for before #1223: a damaged secrets file costs a sign-in,
    /// never the database, and now no keychain dialog either.
    #[test]
    fn a_damaged_file_means_signing_in_and_keeps_the_keys() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let database_key = vault.database_key(ACCOUNT_A).unwrap();
      fs::write(directory.path().join(VAULT_FILE), b"not a sealed file").unwrap();
      drop(vault);
      fake.reset_counts();

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
        "the database key comes back from the keys file"
      );
      assert!(
        vault
          .kept_backup_key(ACCOUNT_A)
          .unwrap()
          .is_some_and(|key| key.expose_secret() == &[4; 32]),
        "and so does the backup key"
      );
      // The lost record of moved entries costs looks at entries that aren't there, which raise
      // no dialog; the files' key is the only entry touched.
      assert_eq!(fake.counts().writes, 0);
      assert_eq!(fake.dialogs(), 1);
      assert_eq!(
        fs::read(directory.path().join(SET_ASIDE_FILE)).unwrap(),
        b"not a sealed file",
        "nothing is wiped"
      );
      drop(vault);
      fs::remove_file(directory.path().join(KEYS_FILE)).unwrap();
      assert!(
        reopen(&fake, &directory)
          .database_key(ACCOUNT_A)
          .unwrap()
          .is_same_key(&database_key),
        "and the secrets file has them again"
      );
    }

    #[test]
    fn a_damaged_keys_file_is_written_again() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let database_key = start(&fake, &directory).database_key(ACCOUNT_A).unwrap();
      fs::write(directory.path().join(KEYS_FILE), b"not a sealed file").unwrap();

      reopen(&fake, &directory).database_key(ACCOUNT_A).unwrap();

      assert!(directory.path().join(KEYS_SET_ASIDE_FILE).exists());
      fs::remove_file(directory.path().join(VAULT_FILE)).unwrap();
      assert!(
        reopen(&fake, &directory)
          .database_key(ACCOUNT_A)
          .unwrap()
          .is_same_key(&database_key)
      );
    }

    /// Where both files have a key, the secrets file is the newer word: the keys file follows it.
    #[test]
    fn the_secrets_file_wins_over_the_keys_file() {
      let mut contents = Contents::default();
      replace_secret(
        &mut contents,
        "database-key@a",
        encode_key(&SymmetricKey::from_bytes([1; 32])),
      );
      replace_secret(&mut contents, "oidc-refresh-token@a", "token".to_owned());
      let mut kept = Contents::default();
      replace_secret(
        &mut kept,
        "database-key@a",
        encode_key(&SymmetricKey::from_bytes([2; 32])),
      );
      replace_secret(
        &mut kept,
        "backup-key@b",
        encode_key(&SymmetricKey::from_bytes([3; 32])),
      );
      replace_secret(&mut kept, "database-key@c", "not a key".to_owned());

      assert!(recover_keys(&mut contents, &kept));

      let key = |name| {
        stored_key(&contents, name)
          .unwrap()
          .map(|key| key.expose_secret()[0])
      };
      assert_eq!(key("database-key@a"), Some(1));
      assert_eq!(key("backup-key@b"), Some(3));
      assert_eq!(key("database-key@c"), None, "an unusable key is not taken");
      assert!(!has_same_keys(&contents, &kept));
      let written = kept_keys_of(&contents);
      assert_eq!(
        written.secrets.keys().collect::<Vec<_>>(),
        ["backup-key@b", "database-key@a"],
        "the keys file holds keys only"
      );
      assert!(has_same_keys(&contents, &written));
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

    /// The cost of one keychain item (#1223): the keys go with the files' key. Both files are set
    /// aside, not deleted, and an old keychain entry not yet deleted still brings its key back.
    #[test]
    fn a_files_key_that_is_gone_sets_both_files_aside() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let database_key = vault.database_key(ACCOUNT_A).unwrap();
      fake.put_secret(DATABASE_KEY_SERVICE, ACCOUNT_B, &[5; 32]);
      fake.fail_deletes(true);
      vault.database_key(ACCOUNT_B).unwrap();
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
      assert!(directory.path().join(KEYS_SET_ASIDE_FILE).exists());
      assert!(!vault.database_key(ACCOUNT_A).unwrap().is_same_key(&database_key));
      assert_eq!(vault.database_key(ACCOUNT_B).unwrap().expose_secret(), &[5; 32]);
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
      vault.database_key(ACCOUNT_A).unwrap();
      for file in [VAULT_FILE, KEYS_FILE] {
        let bytes = fs::read(directory.path().join(file)).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("plain-token-text"));
        assert!(!text.contains("example.com"));
        assert!(!text.contains("database-key"));
      }
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_for_its_owner_only() {
      use std::os::unix::fs::PermissionsExt;

      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.save_token(TokenKind::RefreshToken, SERVER_A, "t").unwrap();
      vault.database_key(ACCOUNT_A).unwrap();
      for file in [VAULT_FILE, KEYS_FILE] {
        let mode = fs::metadata(directory.path().join(file))
          .unwrap()
          .permissions()
          .mode();
        assert_eq!(mode & 0o777, 0o600, "{file}");
      }
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

    /// The keychain holds the files' key and nothing else (#1223).
    #[test]
    fn a_new_database_key_stays_out_of_the_keychain() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      let key = vault.database_key(ACCOUNT_A).unwrap();
      assert_eq!(
        fake.entries(),
        vec![(VAULT_SERVICE.to_owned(), VAULT_KEY_ACCOUNT.to_owned())]
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
      fake.reset_counts();
      vault.keep_backup_key(ACCOUNT_A, &second).unwrap();
      assert_eq!(
        fake.counts(),
        Counts::default(),
        "a changed key touches no keychain"
      );
      assert!(!fake.has(BACKUP_KEY_SERVICE, ACCOUNT_A));
      assert!(
        vault
          .kept_backup_key(ACCOUNT_A)
          .unwrap()
          .unwrap()
          .is_same_key(&second)
      );
      drop(vault);
      fs::remove_file(directory.path().join(VAULT_FILE)).unwrap();
      assert!(
        reopen(&fake, &directory)
          .kept_backup_key(ACCOUNT_A)
          .unwrap()
          .unwrap()
          .is_same_key(&second),
        "the keys file follows"
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
    /// Accesses to an entry that exists: what can raise a dialog on macOS when the app's signature
    /// changed. Creating an entry or looking for one that isn't there raises none.
    dialogs: u32,
    is_denied: bool,
    are_deletes_failing: bool,
    is_next_read_hidden: bool,
    /// The next write is stored, but reported as failed.
    is_next_write_answer_lost: bool,
    is_next_read_failing: bool,
    /// The next write is refused and stores nothing; reads still work.
    is_next_write_refused: bool,
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
      self.with(|inner| {
        inner.counts = Counts::default();
        inner.dialogs = 0;
      });
    }

    fn dialogs(&self) -> u32 {
      self.with(|inner| inner.dialogs)
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

    fn lose_next_write_answer(&self) {
      self.with(|inner| inner.is_next_write_answer_lost = true);
    }

    fn refuse_next_write(&self) {
      self.with(|inner| inner.is_next_write_refused = true);
    }

    fn fail_next_read(&self) {
      self.with(|inner| inner.is_next_read_failing = true);
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

    /// Every entry's service and account, sorted.
    fn entries(&self) -> Vec<(String, String)> {
      let mut entries = self.with(|inner| inner.entries.keys().cloned().collect::<Vec<_>>());
      entries.sort();
      entries
    }
  }

  impl FakeInner {
    fn count_dialog(&mut self, service: &str, account: &str) {
      if self
        .entries
        .contains_key(&(service.to_owned(), account.to_owned()))
      {
        self.dialogs += 1;
      }
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
        inner.count_dialog(service, account);
        if inner.is_denied || std::mem::take(&mut inner.is_next_read_failing) {
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
        inner.count_dialog(service, account);
        if inner.is_denied || std::mem::take(&mut inner.is_next_write_refused) {
          return Err(denied());
        }
        inner
          .entries
          .insert((service.to_owned(), account.to_owned()), secret.to_vec());
        if std::mem::take(&mut inner.is_next_write_answer_lost) {
          return Err(denied());
        }
        Ok(())
      })
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), KeySourceError> {
      self.with(|inner| {
        inner.counts.deletes += 1;
        inner.count_dialog(service, account);
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

  /// Removing an account (#214): the server's items go, every other server's stay.
  mod removing_an_account {
    use super::*;

    /// Starts like A's origin, so only the `|` keeps them apart.
    const SERVER_A_PORT: &str = "https://a.example.com:8443";
    const ACCOUNT_A_PORT: &str = "https://a.example.com:8443|3";
    const TOKEN_KEY_A: &str = "https://a.example.com|https://auth.example.com/realms/a|teamcraft";

    fn token(vault: &SecretVault, kind: TokenKind, scope: &str) -> Option<String> {
      vault
        .load_token(kind, scope)
        .unwrap()
        .map(|token| token.as_str().to_owned())
    }

    fn note(vault: &SecretVault, kind: SessionNote, scope: &str) -> Option<String> {
      vault
        .load_session_note(kind, scope)
        .unwrap()
        .map(|note| note.as_str().to_owned())
    }

    #[test]
    fn the_servers_tokens_notes_and_keys_go_and_nothing_else() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let key_a = vault.database_key(ACCOUNT_A).unwrap();
      vault
        .save_token(TokenKind::RefreshToken, TOKEN_KEY_A, "refresh-a")
        .unwrap();
      vault
        .save_token(TokenKind::LocalSession, SERVER_A, "session-a")
        .unwrap();
      vault
        .save_session_note(SessionNote::DpopKey, SERVER_A, "dpop-a")
        .unwrap();
      vault
        .save_session_note(SessionNote::CachedAccount, SERVER_A, "account-a")
        .unwrap();
      vault
        .save_session_note(SessionNote::TokenSubject, TOKEN_KEY_A, "subject-a")
        .unwrap();
      vault
        .save_token(TokenKind::RefreshToken, SERVER_B, "refresh-b")
        .unwrap();
      vault
        .save_session_note(SessionNote::DpopKey, SERVER_B, "dpop-b")
        .unwrap();
      let key_b = vault.database_key(ACCOUNT_B).unwrap();
      vault
        .keep_backup_key(ACCOUNT_B, &SymmetricKey::from_bytes([7; 32]))
        .unwrap();
      vault
        .save_token(TokenKind::RefreshToken, SERVER_A_PORT, "refresh-a-port")
        .unwrap();
      let key_a_port = vault.database_key(ACCOUNT_A_PORT).unwrap();

      assert_eq!(vault.forget_server(SERVER_A).unwrap(), vec![ACCOUNT_A.to_owned()]);
      drop(vault);

      // A new run reads the files from disk: nothing of A is there any more.
      let vault = reopen(&fake, &directory);
      for (kind, scope) in [
        (TokenKind::RefreshToken, SERVER_A),
        (TokenKind::RefreshToken, TOKEN_KEY_A),
        (TokenKind::LocalSession, SERVER_A),
      ] {
        assert_eq!(token(&vault, kind, scope), None, "{kind:?} of {scope}");
      }
      for (kind, scope) in [
        (SessionNote::DpopKey, SERVER_A),
        (SessionNote::CachedAccount, SERVER_A),
        (SessionNote::TokenSubject, TOKEN_KEY_A),
      ] {
        assert_eq!(note(&vault, kind, scope), None, "{kind:?} of {scope}");
      }
      assert!(vault.kept_backup_key(ACCOUNT_A).unwrap().is_none());
      assert!(
        !vault.database_key(ACCOUNT_A).unwrap().is_same_key(&key_a),
        "the old database key is gone: a new one is made"
      );
      // B and the server on another port keep everything.
      assert_eq!(
        token(&vault, TokenKind::RefreshToken, SERVER_B).as_deref(),
        Some("refresh-b")
      );
      assert_eq!(
        note(&vault, SessionNote::DpopKey, SERVER_B).as_deref(),
        Some("dpop-b")
      );
      assert!(vault.database_key(ACCOUNT_B).unwrap().is_same_key(&key_b));
      assert_eq!(
        vault.kept_backup_key(ACCOUNT_B).unwrap().unwrap().expose_secret(),
        &[7; 32]
      );
      assert_eq!(
        token(&vault, TokenKind::RefreshToken, SERVER_A_PORT).as_deref(),
        Some("refresh-a-port")
      );
      assert!(
        vault
          .database_key(ACCOUNT_A_PORT)
          .unwrap()
          .is_same_key(&key_a_port)
      );
    }

    /// The keys file is a second copy of the keys: it must lose them too, or the next start would
    /// take them back from there.
    #[test]
    fn the_keys_file_loses_the_keys_too() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let key_a = vault.database_key(ACCOUNT_A).unwrap();
      vault.forget_server(SERVER_A).unwrap();
      drop(vault);
      // Only the keys file is left.
      fs::remove_file(directory.path().join(VAULT_FILE)).unwrap();

      let vault = reopen(&fake, &directory);
      assert!(!vault.database_key(ACCOUNT_A).unwrap().is_same_key(&key_a));
    }

    /// A copy of the files from before the removal, put back, doesn't open under the new key.
    #[test]
    fn an_older_copy_put_back_brings_nothing_back() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      let copies: Vec<(PathBuf, Vec<u8>)> = [VAULT_FILE, KEYS_FILE]
        .into_iter()
        .map(|name| {
          let path = directory.path().join(name);
          let bytes = fs::read(&path).unwrap();
          (path, bytes)
        })
        .collect();

      vault.forget_server(SERVER_A).unwrap();
      drop(vault);
      for (path, bytes) in copies {
        fs::write(path, bytes).unwrap();
      }

      let vault = reopen(&fake, &directory);
      assert_eq!(token(&vault, TokenKind::RefreshToken, SERVER_A), None);
    }

    #[test]
    fn old_keychain_entries_of_the_server_go() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = reopen(&fake, &directory);
      vault.database_key(ACCOUNT_A).unwrap();
      vault
        .save_token(TokenKind::RefreshToken, SERVER_A, "refresh-a")
        .unwrap();
      // Written by an older version after the move: the files record them as gone.
      fake.put_secret(DATABASE_KEY_SERVICE, ACCOUNT_A, &[8; 32]);
      fake.put_text(VAULT_SERVICE, OLD_REFRESH_A, "from-an-older-app");
      fake.put_secret(DATABASE_KEY_SERVICE, ACCOUNT_B, &[9; 32]);

      vault.forget_server(SERVER_A).unwrap();

      assert!(!fake.has(VAULT_SERVICE, OLD_REFRESH_A));
      assert!(!fake.has(DATABASE_KEY_SERVICE, ACCOUNT_A));
      assert!(
        fake.has(DATABASE_KEY_SERVICE, ACCOUNT_B),
        "another server's entry stays"
      );
    }

    #[test]
    fn only_an_origin_is_taken() {
      let fake = Fake::new();
      let directory = TestDir::new();
      let vault = start(&fake, &directory);
      for scope in ["", ACCOUNT_A, "https://a.example.com@b"] {
        assert!(vault.forget_server(scope).is_err(), "{scope:?}");
      }
      assert!(token(&vault, TokenKind::RefreshToken, SERVER_A).is_some());
    }
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

  /// What the sign-in service does at sign-out (`auth/mod.rs`), for server A.
  fn sign_out(vault: &SecretVault) {
    clear_tokens(vault);
    vault.purge_after_sign_out(&SIGN_OUT_ITEMS).unwrap();
  }

  fn clear_tokens(vault: &SecretVault) {
    for (kind, server) in SIGN_OUT_ITEMS {
      vault.clear_token(kind, server).unwrap();
    }
  }

  fn has_staged_files(directory: &TestDir) -> bool {
    [STAGED_FILE, KEYS_STAGED_FILE]
      .into_iter()
      .any(|name| directory.path().join(name).exists())
  }

  /// The secrets file as versions 1.0.18 to 1.0.21 left it for one server and account: tokens moved
  /// out of the keychain, and copies of the keys, whose entries the test puts in the keychain.
  fn write_file_of_1_0_18(fake: &Fake, directory: &TestDir) {
    let file_key = SymmetricKey::from_bytes([7; 32]);
    fake.put_secret(VAULT_SERVICE, VAULT_KEY_ACCOUNT, file_key.expose_secret());
    let refresh = token_name(TokenKind::RefreshToken, SERVER_A);
    let session = token_name(TokenKind::LocalSession, SERVER_A);
    let origin_only = token_name(TokenKind::RefreshToken, "https://origin-only.example.com");
    let layout = serde_json::json!({
      "secrets": {
        refresh.clone(): "refresh-a",
        item_name(DATABASE_KEY_KIND, ACCOUNT_A): encode_key(&SymmetricKey::from_bytes([3; 32])),
        item_name(BACKUP_KEY_KIND, ACCOUNT_A): encode_key(&SymmetricKey::from_bytes([4; 32])),
      },
      "moved": [refresh, session, origin_only],
      "missing_from_keychain": [],
    });
    let sealed = secret_file::seal(&file_key, layout.to_string().as_bytes(), &mut OsRng).unwrap();
    fs::write(directory.path().join(VAULT_FILE), sealed).unwrap();
  }

  /// Makes every write of the files fail until [`allow_writes`]: a directory stands where the
  /// temporary files have to go. The vault must have opened the files before.
  fn block_writes(directory: &TestDir) {
    block_write_of(directory, TEMPORARY_FILE);
    block_write_of(directory, KEYS_TEMPORARY_FILE);
  }

  fn allow_writes(directory: &TestDir) {
    allow_write_of(directory, TEMPORARY_FILE);
    allow_write_of(directory, KEYS_TEMPORARY_FILE);
  }

  fn block_write_of(directory: &TestDir, temporary: &str) {
    let path = directory.path().join(temporary);
    drop(fs::remove_file(&path));
    fs::create_dir(&path).unwrap();
  }

  fn allow_write_of(directory: &TestDir, temporary: &str) {
    fs::remove_dir(directory.path().join(temporary)).unwrap();
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
