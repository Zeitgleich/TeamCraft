//! The servers the user signed in to, and the server a company preconfigured (docs/operations.md
//! "Server address in the apps").
//!
//! Accounts are kept as a list with at most one active entry: several accounts on several servers
//! (#214), one per server origin. The file holds no secrets: tokens and keys are in the sealed file
//! of local secrets, keyed by the server origin, and each server's store has its own directory.
//!
//! Locked to a managed server (`lock_server`, #220), the list holds that server's account only:
//! accounts on other servers are dropped at the start (as the one account was before #214) and no
//! account can be added.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use teamcraft_client_api::server_entry::{ServerOrigin, ServerProfile};

use super::HTTP_POLICY;

/// The account list in the app's config directory.
const ACCOUNTS_FILE: &str = "accounts.json";
/// A preconfigured server for this computer, written by an administrator or a deployment tool.
const MANAGED_FILE: &str = "desktop.json";
/// Environment variable naming a server, e.g. set by an installer or for development.
const SERVER_ENV: &str = "TEAMCRAFT_SERVER_URL";
/// A default server compiled into a self-built app (release builds have none).
const BUILD_DEFAULT: Option<&str> = option_env!("TEAMCRAFT_DEFAULT_SERVER_URL");

/// The accounts on this computer, persisted on every change.
pub struct ServerDirectory {
  path: PathBuf,
  accounts: Mutex<Accounts>,
}

/// The server side of the shell: the accounts, the managed configuration and a checked server
/// waiting for the user's confirmation.
pub struct ServerState {
  pub directory: ServerDirectory,
  pub managed: ManagedServer,
  /// The last server the user checked; only this one can be confirmed.
  pub pending: Mutex<Option<ServerProfile>>,
}

/// A server named by the computer's configuration rather than by the user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ManagedServer {
  /// What the configuration says, as typed there; checked like user input.
  pub maybe_address: Option<String>,
  /// The user may not use another server.
  pub is_locked: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Accounts {
  #[serde(default)]
  accounts: Vec<ServerProfile>,
  #[serde(rename = "active", default, skip_serializing_if = "Option::is_none")]
  maybe_active: Option<ServerOrigin>,
}

/// What an administrator configures, from `desktop.json` or the operating system's policy store:
/// `{ "server_url": "team.example.com", "lock_server": true }`.
#[derive(Debug, Default, Deserialize)]
pub(super) struct ManagedFile {
  #[serde(rename = "server_url", default)]
  pub(super) maybe_server_url: Option<String>,
  #[serde(default)]
  pub(super) lock_server: bool,
}

impl ServerState {
  pub fn new(directory: ServerDirectory, managed: ManagedServer) -> Self {
    Self {
      directory,
      managed,
      pending: Mutex::new(None),
    }
  }

  /// The accounts to start with: the active one, if any, and the accounts the managed configuration
  /// forbids. Those are forgotten here and returned, so their tokens can be deleted; without an
  /// active account the managed server is offered.
  pub fn startup_accounts(&self) -> StartupAccounts {
    let forbidden = match self
      .directory
      .retain(|profile| self.managed.allows(&profile.origin))
    {
      Ok(forbidden) => forbidden,
      Err(error) => {
        tracing::warn!(%error, "could not update the account list");
        Vec::new()
      },
    };
    if !forbidden.is_empty() {
      tracing::info!(
        count = forbidden.len(),
        "the managed configuration does not allow stored servers; forgetting them"
      );
    }
    StartupAccounts {
      maybe_active: self.directory.active(),
      forbidden,
    }
  }

  /// Whether another account may be added: not while the computer is locked to one server.
  pub fn can_add_account(&self) -> bool {
    !self.managed.is_locked
  }
}

/// What [`ServerState::startup_accounts`] found.
#[derive(Debug, Default)]
pub struct StartupAccounts {
  pub maybe_active: Option<ServerProfile>,
  /// Accounts on servers the managed configuration doesn't allow, no longer in the list.
  pub forbidden: Vec<ServerProfile>,
}

impl ServerDirectory {
  /// Reads `accounts.json` in `config_dir`; a missing or unreadable file means no accounts (the
  /// user enters the server again, nothing secret is lost).
  pub fn load(config_dir: &Path) -> Self {
    let path = config_dir.join(ACCOUNTS_FILE);
    let accounts = match std::fs::read(&path) {
      Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        tracing::warn!(%error, "the account list is unreadable; starting without accounts");
        Accounts::default()
      }),
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => Accounts::default(),
      Err(error) => {
        tracing::warn!(%error, "could not read the account list");
        Accounts::default()
      },
    };
    Self {
      path,
      accounts: Mutex::new(accounts),
    }
  }

  /// Every account, in the order they were added; the switcher shows them so.
  pub fn accounts(&self) -> Vec<ServerProfile> {
    self.lock().accounts.clone()
  }

  /// The account in use, if any.
  pub fn active(&self) -> Option<ServerProfile> {
    let accounts = self.lock();
    let active = accounts.maybe_active.as_ref()?;
    accounts
      .accounts
      .iter()
      .find(|profile| &profile.origin == active)
      .cloned()
  }

  /// The stored profile of `origin`, if any.
  pub fn profile(&self, origin: &ServerOrigin) -> Option<ServerProfile> {
    self
      .lock()
      .accounts
      .iter()
      .find(|profile| &profile.origin == origin)
      .cloned()
  }

  /// Stores `profile` (replacing an older one of the same server in its place) and makes it the
  /// active account; a new server's account comes last. The login hint of the older one stays
  /// while the identity provider is the same.
  pub fn activate(&self, mut profile: ServerProfile) -> std::io::Result<()> {
    let mut accounts = self.lock();
    accounts.maybe_active = Some(profile.origin.clone());
    match accounts
      .accounts
      .iter_mut()
      .find(|existing| existing.origin == profile.origin)
    {
      Some(older) => {
        if profile.maybe_login_hint.is_none() && older.issuer == profile.issuer {
          profile.maybe_login_hint = older.maybe_login_hint.take();
        }
        *older = profile;
      },
      None => accounts.accounts.push(profile),
    }
    save(&self.path, &accounts)
  }

  /// Makes the stored account of `origin` the active one; `false` when there is none.
  pub fn switch_to(&self, origin: &ServerOrigin) -> std::io::Result<bool> {
    let mut accounts = self.lock();
    if !accounts.accounts.iter().any(|profile| &profile.origin == origin) {
      return Ok(false);
    }
    if accounts.maybe_active.as_ref() == Some(origin) {
      return Ok(true);
    }
    accounts.maybe_active = Some(origin.clone());
    save(&self.path, &accounts).map(|()| true)
  }

  /// No account is active while another one is added: the list stays, and the next start shows
  /// the server entry with the way back to them.
  pub fn park(&self) -> std::io::Result<()> {
    let mut accounts = self.lock();
    if accounts.maybe_active.take().is_none() {
      return Ok(());
    }
    save(&self.path, &accounts)
  }

  /// Takes the account of `origin` out of the list; answers it, `None` when there was none. When it
  /// was the active one, no account is active afterwards.
  pub fn remove(&self, origin: &ServerOrigin) -> std::io::Result<Option<ServerProfile>> {
    let mut accounts = self.lock();
    let Some(index) = accounts
      .accounts
      .iter()
      .position(|profile| &profile.origin == origin)
    else {
      return Ok(None);
    };
    let removed = accounts.accounts.remove(index);
    if accounts.maybe_active.as_ref() == Some(origin) {
      accounts.maybe_active = None;
    }
    save(&self.path, &accounts).map(|()| Some(removed))
  }

  /// Keeps the accounts `is_kept` takes and answers the others, which are gone from the list.
  fn retain(&self, is_kept: impl Fn(&ServerProfile) -> bool) -> std::io::Result<Vec<ServerProfile>> {
    let mut accounts = self.lock();
    let (kept, dropped): (Vec<_>, Vec<_>) = accounts.accounts.drain(..).partition(|profile| is_kept(profile));
    accounts.accounts = kept;
    if dropped.is_empty() {
      return Ok(dropped);
    }
    if let Some(active) = &accounts.maybe_active
      && !accounts.accounts.iter().any(|profile| &profile.origin == active)
    {
      accounts.maybe_active = None;
    }
    save(&self.path, &accounts).map(|()| dropped)
  }

  /// Remembers the account last signed in with on the server `origin` of `issuer`, for the next
  /// sign-in's login page. Nothing changes when the profile meanwhile names another provider.
  pub fn set_login_hint(&self, origin: &ServerOrigin, issuer: &str, hint: String) -> std::io::Result<()> {
    let mut accounts = self.lock();
    let Some(profile) = accounts
      .accounts
      .iter_mut()
      .find(|profile| &profile.origin == origin && profile.issuer == issuer)
    else {
      return Ok(());
    };
    if profile.maybe_login_hint.as_deref() == Some(hint.as_str()) {
      return Ok(());
    }
    profile.maybe_login_hint = Some(hint);
    save(&self.path, &accounts)
  }

  /// Forgets the active account's server: the next start shows the server entry.
  pub fn forget_active(&self) -> std::io::Result<()> {
    let mut accounts = self.lock();
    if let Some(active) = accounts.maybe_active.take() {
      accounts.accounts.retain(|profile| profile.origin != active);
    }
    save(&self.path, &accounts)
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Accounts> {
    self.accounts.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

impl ManagedServer {
  /// The computer's configuration. This is the one place that says which source wins, first match
  /// first (a source counts when it names a server; the lock comes from that same source):
  ///
  /// 1. the operating system's policy store: Windows registry policy, macOS configuration profile
  ///    ([`super::policy`], #220);
  /// 2. the administrator's `desktop.json` (macOS and Linux);
  /// 3. the `TEAMCRAFT_SERVER_URL` environment variable (never locks);
  /// 4. a default compiled into a self-built app (never locks).
  ///
  /// Only machine-wide locations an ordinary user cannot write are read for 1 and 2, because the
  /// lock is a security control.
  pub fn load() -> Self {
    let from_file = system_config_path()
      .filter(|path| is_admin_owned(path))
      .and_then(|path| std::fs::read(&path).ok().map(|bytes| (path, bytes)))
      .and_then(
        |(path, bytes)| match serde_json::from_slice::<ManagedFile>(&bytes) {
          Ok(file) => Some(file),
          Err(error) => {
            tracing::warn!(path = %path.display(), %error, "ignoring an unreadable managed configuration");
            None
          },
        },
      );
    Self::resolve(
      super::policy::load(),
      from_file,
      std::env::var(SERVER_ENV).ok(),
      BUILD_DEFAULT.map(str::to_owned),
    )
  }

  /// Whether `origin` may be used under this configuration.
  pub fn allows(&self, origin: &ServerOrigin) -> bool {
    if !self.is_locked {
      return true;
    }
    self
      .maybe_address
      .as_deref()
      .and_then(|address| ServerOrigin::parse(address, HTTP_POLICY).ok())
      .is_some_and(|managed| &managed == origin)
  }

  /// The order of [`Self::load`].
  fn resolve(
    maybe_policy: Option<ManagedFile>,
    maybe_file: Option<ManagedFile>,
    maybe_env: Option<String>,
    maybe_build_default: Option<String>,
  ) -> Self {
    let non_empty = |value: Option<String>| value.filter(|text| !text.trim().is_empty());
    for source in [maybe_policy, maybe_file].into_iter().flatten() {
      if let Some(address) = non_empty(source.maybe_server_url) {
        return Self {
          maybe_address: Some(address),
          is_locked: source.lock_server,
        };
      }
    }
    Self {
      maybe_address: non_empty(maybe_env).or_else(|| non_empty(maybe_build_default)),
      is_locked: false,
    }
  }
}

/// Writes the list through a temporary file, so a crash never leaves half a file.
fn save(path: &Path, accounts: &Accounts) -> std::io::Result<()> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  let bytes = serde_json::to_vec_pretty(accounts).map_err(std::io::Error::other)?;
  let temporary = path.with_extension("json.tmp");
  std::fs::write(&temporary, bytes)?;
  std::fs::rename(&temporary, path)
}

/// Where administrators put `desktop.json`. Not on Windows: any user may create folders in
/// `%ProgramData%`, so a file there proves nothing; Windows gets the policy registry key (#220).
fn system_config_path() -> Option<PathBuf> {
  if cfg!(target_os = "windows") {
    None
  } else if cfg!(target_os = "macos") {
    Some(PathBuf::from("/Library/Application Support/TeamCraft").join(MANAGED_FILE))
  } else {
    Some(PathBuf::from("/etc/teamcraft").join(MANAGED_FILE))
  }
}

/// Only a file root owns and only root can change counts: otherwise any local user could send
/// every user of the computer to their server. An untrusted file is ignored with a warning.
#[cfg(unix)]
pub(super) fn is_admin_owned(path: &Path) -> bool {
  use std::os::unix::fs::MetadataExt;

  let trusted = |metadata: &std::fs::Metadata| metadata.uid() == 0 && metadata.mode() & 0o022 == 0;
  let Ok(metadata) = std::fs::metadata(path) else {
    // No file: nothing to trust or warn about.
    return false;
  };
  let is_dir_trusted = path
    .parent()
    .and_then(|dir| std::fs::metadata(dir).ok())
    .is_some_and(|dir| trusted(&dir));
  if trusted(&metadata) && is_dir_trusted {
    true
  } else {
    tracing::warn!(path = %path.display(), "ignoring a managed configuration that other users can change");
    false
  }
}

#[cfg(not(unix))]
fn is_admin_owned(_path: &Path) -> bool {
  false
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::server_entry::HttpPolicy;

  use super::*;
  use crate::auth::test_dir::TempDir;

  #[test]
  fn activating_persists_and_replaces_the_same_server() {
    let dir = TempDir::new("activate");
    let directory = ServerDirectory::load(&dir.0);
    assert!(directory.active().is_none());
    directory
      .activate(profile("team.example.com", "Old name"))
      .unwrap();
    directory.activate(profile("other.example.com", "Other")).unwrap();
    directory
      .activate(profile("team.example.com", "New name"))
      .unwrap();

    let reloaded = ServerDirectory::load(&dir.0);
    let active = reloaded.active().unwrap();
    assert_eq!(active.origin.as_str(), "https://team.example.com");
    assert_eq!(active.display_name, "New name");
    assert_eq!(reloaded.lock().accounts.len(), 2);
  }

  #[test]
  fn forgetting_the_active_server_leads_back_to_the_entry() {
    let dir = TempDir::new("forget");
    let directory = ServerDirectory::load(&dir.0);
    directory.activate(profile("team.example.com", "Team")).unwrap();
    directory.forget_active().unwrap();
    assert!(ServerDirectory::load(&dir.0).active().is_none());
  }

  #[test]
  fn several_accounts_stay_in_their_order_and_one_is_active() {
    let dir = TempDir::new("several");
    let directory = ServerDirectory::load(&dir.0);
    directory.activate(profile("a.example.com", "A")).unwrap();
    directory.activate(profile("b.example.com", "B")).unwrap();
    // Signing in again on A (new scopes) keeps it in its place.
    directory.activate(profile("a.example.com", "A again")).unwrap();
    assert!(directory.switch_to(&origin("b.example.com")).unwrap());
    assert!(!directory.switch_to(&origin("c.example.com")).unwrap());

    let reloaded = ServerDirectory::load(&dir.0);
    let hosts: Vec<_> = reloaded
      .accounts()
      .iter()
      .map(|profile| profile.origin.host().to_owned())
      .collect();
    assert_eq!(hosts, ["a.example.com", "b.example.com"]);
    assert_eq!(reloaded.accounts()[0].display_name, "A again");
    assert_eq!(reloaded.active().unwrap().origin.host(), "b.example.com");
  }

  #[test]
  fn parking_keeps_every_account_and_none_is_active() {
    let dir = TempDir::new("park");
    let directory = ServerDirectory::load(&dir.0);
    directory.activate(profile("a.example.com", "A")).unwrap();
    directory.park().unwrap();
    let reloaded = ServerDirectory::load(&dir.0);
    assert!(reloaded.active().is_none());
    assert_eq!(reloaded.accounts().len(), 1);
    assert!(reloaded.switch_to(&origin("a.example.com")).unwrap());
    assert_eq!(reloaded.active().unwrap().origin.host(), "a.example.com");
  }

  #[test]
  fn removing_an_account_leaves_the_others() {
    let dir = TempDir::new("remove");
    let directory = ServerDirectory::load(&dir.0);
    directory.activate(profile("a.example.com", "A")).unwrap();
    directory.activate(profile("b.example.com", "B")).unwrap();

    let removed = directory.remove(&origin("a.example.com")).unwrap().unwrap();
    assert_eq!(removed.display_name, "A");
    assert_eq!(directory.active().unwrap().origin.host(), "b.example.com");
    assert!(directory.remove(&origin("a.example.com")).unwrap().is_none());

    directory.remove(&origin("b.example.com")).unwrap();
    let reloaded = ServerDirectory::load(&dir.0);
    assert!(reloaded.active().is_none());
    assert!(reloaded.accounts().is_empty());
  }

  /// The file of the version before #214 is read as it is: its account stays the active one, under
  /// the same origin, so its tokens, keys and store directory (all keyed by the origin) are found.
  #[test]
  fn the_single_account_of_an_older_version_is_the_active_account() {
    let dir = TempDir::new("older");
    std::fs::create_dir_all(&dir.0).unwrap();
    let older = r#"{"accounts":[{"origin":"https://team.example.com","issuer":"https://auth.example.com/realms/company","client_id":"teamcraft","scopes":["openid"],"display_name":"Team","server_version":"1.0.0","has_local_accounts":false,"is_login_page_always_shown":true,"login_hint":"me@example.com"}],"active":"https://team.example.com"}"#;
    std::fs::write(dir.0.join(ACCOUNTS_FILE), older).unwrap();

    let directory = ServerDirectory::load(&dir.0);
    let active = directory.active().unwrap();
    assert_eq!(active.origin.as_str(), "https://team.example.com");
    assert_eq!(active.maybe_login_hint.as_deref(), Some("me@example.com"));
    assert_eq!(directory.accounts().len(), 1);
    let config = crate::auth::DesktopConfig::from_profile(&active);
    assert_eq!(config.storage_key().unwrap(), "team.example.com");
    assert_eq!(
      config.token_key(),
      "https://team.example.com|https://auth.example.com/realms/company|teamcraft"
    );

    // Adding a second account writes the same format, with the first one still first.
    directory.activate(profile("other.example.com", "Other")).unwrap();
    let written: serde_json::Value =
      serde_json::from_slice(&std::fs::read(dir.0.join(ACCOUNTS_FILE)).unwrap()).unwrap();
    assert_eq!(written["active"], "https://other.example.com");
    assert_eq!(written["accounts"][0]["origin"], "https://team.example.com");
    assert_eq!(written["accounts"][0]["login_hint"], "me@example.com");
  }

  #[test]
  fn a_lock_drops_the_accounts_on_other_servers_at_the_start() {
    let dir = TempDir::new("lock");
    let directory = ServerDirectory::load(&dir.0);
    directory.activate(profile("team.example.com", "Team")).unwrap();
    directory.activate(profile("other.example.com", "Other")).unwrap();
    let locked = ManagedServer {
      maybe_address: Some("team.example.com".into()),
      is_locked: true,
    };

    let state = ServerState::new(ServerDirectory::load(&dir.0), locked);
    let startup = state.startup_accounts();
    assert!(!state.can_add_account());
    assert!(
      startup.maybe_active.is_none(),
      "the active account was on the other server"
    );
    let forbidden: Vec<_> = startup
      .forbidden
      .iter()
      .map(|profile| profile.origin.host().to_owned())
      .collect();
    assert_eq!(forbidden, ["other.example.com"]);
    let hosts: Vec<_> = state
      .directory
      .accounts()
      .iter()
      .map(|profile| profile.origin.host().to_owned())
      .collect();
    assert_eq!(hosts, ["team.example.com"]);
    assert_eq!(ServerDirectory::load(&dir.0).accounts().len(), 1, "persisted");

    let unlocked = ServerState::new(ServerDirectory::load(&dir.0), ManagedServer::default());
    assert!(unlocked.can_add_account());
    assert!(unlocked.startup_accounts().forbidden.is_empty());
  }

  #[test]
  fn a_corrupt_or_tampered_file_means_no_accounts() {
    let dir = TempDir::new("corrupt");
    std::fs::create_dir_all(&dir.0).unwrap();
    std::fs::write(dir.0.join(ACCOUNTS_FILE), b"{ not json").unwrap();
    assert!(ServerDirectory::load(&dir.0).active().is_none());
    let tampered = r#"{"accounts":[{"origin":"https://team.example.com/evil","issuer":"https://a.example.com","client_id":"x","scopes":["openid"],"display_name":"x","server_version":"1.0.0"}],"active":"https://team.example.com/evil"}"#;
    std::fs::write(dir.0.join(ACCOUNTS_FILE), tampered).unwrap();
    assert!(ServerDirectory::load(&dir.0).active().is_none());
  }

  #[test]
  fn the_administrators_file_wins_over_environment_and_build_default() {
    let file = ManagedFile {
      maybe_server_url: Some("team.example.com".into()),
      lock_server: true,
    };
    let managed = ManagedServer::resolve(
      None,
      Some(file),
      Some("env.example.com".into()),
      Some("build.example.com".into()),
    );
    assert_eq!(managed.maybe_address.as_deref(), Some("team.example.com"));
    assert!(managed.is_locked);

    let managed = ManagedServer::resolve(None, None, Some(" ".into()), Some("build.example.com".into()));
    assert_eq!(managed.maybe_address.as_deref(), Some("build.example.com"));
    assert!(!managed.is_locked);

    let empty_file = ManagedFile {
      maybe_server_url: None,
      lock_server: true,
    };
    let managed = ManagedServer::resolve(None, Some(empty_file), None, None);
    assert_eq!(managed, ManagedServer::default());
  }

  #[test]
  fn the_policy_store_wins_over_the_file_and_locks_only_with_its_own_flag() {
    let policy = || ManagedFile {
      maybe_server_url: Some("policy.example.com".into()),
      lock_server: false,
    };
    let file = || ManagedFile {
      maybe_server_url: Some("file.example.com".into()),
      lock_server: true,
    };
    let managed = ManagedServer::resolve(Some(policy()), Some(file()), Some("env.example.com".into()), None);
    assert_eq!(managed.maybe_address.as_deref(), Some("policy.example.com"));
    assert!(
      !managed.is_locked,
      "the lock of a lower source does not carry over"
    );

    // A policy without a server (only a lock) falls through to the file.
    let lock_only = ManagedFile {
      maybe_server_url: Some("  ".into()),
      lock_server: true,
    };
    let managed = ManagedServer::resolve(Some(lock_only), Some(file()), None, None);
    assert_eq!(managed.maybe_address.as_deref(), Some("file.example.com"));
    assert!(managed.is_locked);

    let managed = ManagedServer::resolve(None, None, Some("env.example.com".into()), None);
    assert_eq!(managed.maybe_address.as_deref(), Some("env.example.com"));
  }

  #[test]
  fn a_locked_configuration_only_allows_its_server() {
    let managed = ManagedServer {
      maybe_address: Some("https://team.example.com/".into()),
      is_locked: true,
    };
    assert!(managed.allows(&origin("team.example.com")));
    assert!(!managed.allows(&origin("other.example.com")));
    assert!(ManagedServer::default().allows(&origin("other.example.com")));
  }

  fn origin(input: &str) -> ServerOrigin {
    ServerOrigin::parse(input, HttpPolicy::LoopbackOnly).unwrap()
  }

  fn profile(host: &str, name: &str) -> ServerProfile {
    ServerProfile {
      origin: origin(host),
      issuer: "https://auth.example.com/realms/company".into(),
      client_id: "teamcraft".into(),
      scopes: vec!["openid".into()],
      display_name: name.into(),
      server_version: "1.0.0".into(),
      has_local_accounts: false,
      is_login_page_always_shown: true,
      maybe_login_hint: None,
    }
  }
}
