//! The servers the user signed in to, and the server a company preconfigured (docs/operations.md
//! "Server address in the apps").
//!
//! Accounts are kept as a list with one active entry, so several accounts (#214) only add UI. The
//! file holds no secrets: tokens are in the OS keyring, keyed by the server origin.

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

/// The file an administrator writes: `{ "server_url": "team.example.com", "lock_server": true }`.
#[derive(Debug, Default, Deserialize)]
struct ManagedFile {
  #[serde(rename = "server_url", default)]
  maybe_server_url: Option<String>,
  #[serde(default)]
  lock_server: bool,
}

impl ServerState {
  pub fn new(directory: ServerDirectory, managed: ManagedServer) -> Self {
    Self {
      directory,
      managed,
      pending: Mutex::new(None),
    }
  }

  /// The account to start with: the active one, unless the managed configuration forbids its
  /// server (it is forgotten then, and the managed server is offered instead).
  pub fn startup_account(&self) -> Option<ServerProfile> {
    let active = self.directory.active()?;
    if self.managed.allows(&active.origin) {
      return Some(active);
    }
    tracing::info!("the managed configuration does not allow the stored server; forgetting it");
    if let Err(error) = self.directory.forget_active() {
      tracing::warn!(%error, "could not update the account list");
    }
    None
  }
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

  /// Stores `profile` (replacing an older one of the same server) and makes it the active account.
  pub fn activate(&self, profile: ServerProfile) -> std::io::Result<()> {
    let mut accounts = self.lock();
    accounts
      .accounts
      .retain(|existing| existing.origin != profile.origin);
    accounts.maybe_active = Some(profile.origin.clone());
    accounts.accounts.push(profile);
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
  /// The computer's configuration, most specific first: the administrator's file, then the
  /// environment, then a default compiled into a self-built app.
  pub fn load() -> Self {
    let from_file = system_config_path()
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

  fn resolve(
    maybe_file: Option<ManagedFile>,
    maybe_env: Option<String>,
    maybe_build_default: Option<String>,
  ) -> Self {
    let non_empty = |value: Option<String>| value.filter(|text| !text.trim().is_empty());
    if let Some(file) = maybe_file
      && let Some(address) = non_empty(file.maybe_server_url)
    {
      return Self {
        maybe_address: Some(address),
        is_locked: file.lock_server,
      };
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

/// Where administrators put `desktop.json` on each platform.
fn system_config_path() -> Option<PathBuf> {
  if cfg!(target_os = "windows") {
    std::env::var_os("ProgramData").map(|base| PathBuf::from(base).join("TeamCraft").join(MANAGED_FILE))
  } else if cfg!(target_os = "macos") {
    Some(PathBuf::from("/Library/Application Support/TeamCraft").join(MANAGED_FILE))
  } else {
    Some(PathBuf::from("/etc/teamcraft").join(MANAGED_FILE))
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_client_api::server_entry::HttpPolicy;

  use super::*;

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
      Some(file),
      Some("env.example.com".into()),
      Some("build.example.com".into()),
    );
    assert_eq!(managed.maybe_address.as_deref(), Some("team.example.com"));
    assert!(managed.is_locked);

    let managed = ManagedServer::resolve(None, Some(" ".into()), Some("build.example.com".into()));
    assert_eq!(managed.maybe_address.as_deref(), Some("build.example.com"));
    assert!(!managed.is_locked);

    let empty_file = ManagedFile {
      maybe_server_url: None,
      lock_server: true,
    };
    let managed = ManagedServer::resolve(Some(empty_file), None, None);
    assert_eq!(managed, ManagedServer::default());
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
    }
  }

  /// A directory below the build's temp dir, removed again.
  struct TempDir(PathBuf);

  impl TempDir {
    fn new(name: &str) -> Self {
      let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
      Self(std::env::temp_dir().join(format!("teamcraft-servers-{name}-{}-{nanos}", std::process::id())))
    }
  }

  impl Drop for TempDir {
    fn drop(&mut self) {
      let _ = std::fs::remove_dir_all(&self.0);
    }
  }
}
