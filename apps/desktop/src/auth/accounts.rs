//! Several accounts in one app (#214): the list, switching, adding and removing.
//!
//! Each account is one server origin, and everything local is keyed by it already: its refresh
//! token, local session, DPoP key and session notes in the sealed file of local secrets, the database
//! and backup keys of its users (`{origin}|{user}`), and its store directory (`core/{storage key}`,
//! with the media cache and upload staging). One account is active: it has the sign-in service's
//! session and the client core. The others keep their stored session and show an unread badge
//! ([`super::background`]).
//!
//! - **Switching** stores the composers' drafts, stops the core, lets the sign-in service go of the
//!   old server and takes the new one; the webview then loads afresh, so nothing of the old account
//!   stays in its memory.
//! - **Adding** parks the active account (signed in, not active) and leads to the server entry.
//! - **Removing** ends the session at the identity provider (or the server, for a local account),
//!   deletes every secret of the server from the sealed files and gives them a new key
//!   ([`SecretVault::forget_server`]), then deletes the server's store directory. Other accounts'
//!   secrets and directories are not touched.
//!
//! A computer locked to a managed server (`lock_server`, #220) keeps one account, on that server:
//! adding is refused, and accounts on other servers are dropped at the start
//! ([`ServerState::startup_accounts`]).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use teamcraft_client_api::server_entry::{HttpPolicy, ServerOrigin, ServerProfile};
use teamcraft_core::store::SecretVault;
use teamcraft_proto::UserId;

use super::background::{ACCOUNTS_CHANGED_EVENT, BackgroundAccounts, BackgroundState};
use super::commands::{CommandError, Service};
use super::servers::ServerState;
use super::{AuthService, DesktopConfig, VaultStore};
use crate::calls::CallHost;
use crate::core::commands::{CoreHost, core_dir};
use crate::notifications::pictures::NotificationPictures;

/// A store directory being deleted carries this prefix; a start finishes what a crash left.
const REMOVED_PREFIX: &str = ".removed-";
/// How long removing a background account may wait for its identity provider.
const SIGN_OUT_TIMEOUT: Duration = Duration::from_secs(20);

/// One account as the switcher shows it.
#[derive(Debug, Serialize)]
pub struct AccountView {
  pub origin: String,
  pub host: String,
  pub display_name: String,
  pub is_active: bool,
  /// The unread state of an account that is not active; unknown for the active one, whose counts
  /// the UI has itself.
  pub background: BackgroundState,
}

/// The accounts on this computer.
#[derive(Debug, Serialize)]
pub struct AccountsView {
  pub accounts: Vec<AccountView>,
  /// Another account may be added (not on a computer locked to one server).
  pub can_add: bool,
}

/// What removing an account deleted.
#[derive(Debug, Serialize)]
pub struct RemovedAccount {
  /// The users whose data on this computer is gone, so the webview forgets what it kept of them.
  pub user_ids: Vec<UserId>,
  /// It was the active account: the webview loads afresh for the next one, or the server entry.
  pub was_active: bool,
}

#[tauri::command(rename_all = "snake_case")]
pub fn accounts_list(
  servers: State<'_, Arc<ServerState>>,
  background: State<'_, Arc<BackgroundAccounts>>,
) -> AccountsView {
  accounts_view(&servers, &background)
}

/// Makes the account of `origin` the active one. The webview loads afresh afterwards.
#[tauri::command(rename_all = "snake_case")]
pub async fn account_switch(
  origin: String,
  app: AppHandle,
  servers: State<'_, Arc<ServerState>>,
  service: State<'_, Arc<Service>>,
  background: State<'_, Arc<BackgroundAccounts>>,
  host: State<'_, CoreHost>,
  calls: State<'_, Arc<CallHost>>,
) -> Result<(), CommandError> {
  let profile = listed(&servers, &origin)?;
  if !servers.managed.allows(&profile.origin) {
    return Err(locked());
  }
  let maybe_previous = servers.directory.active();
  if maybe_previous
    .as_ref()
    .is_some_and(|previous| previous.origin == profile.origin)
  {
    return Ok(());
  }
  refuse_during_call(&calls)?;
  leave_active(&app, &host, true).await;
  // Its watcher holds the session until its round ends; only then the active service takes it.
  background.stop(profile.origin.as_str()).await;
  service
    .set_server(Some(DesktopConfig::from_profile(&profile)))
    .await;
  servers.directory.switch_to(&profile.origin).map_err(not_saved)?;
  if let Some(previous) = maybe_previous {
    background.watch(DesktopConfig::from_profile(&previous));
  }
  tracing::info!("switched to another account");
  Ok(())
}

/// Leaves the active account signed in but not active, for the server entry of another one. The
/// webview loads afresh afterwards.
#[tauri::command(rename_all = "snake_case")]
pub async fn account_add(
  app: AppHandle,
  servers: State<'_, Arc<ServerState>>,
  service: State<'_, Arc<Service>>,
  background: State<'_, Arc<BackgroundAccounts>>,
  host: State<'_, CoreHost>,
  calls: State<'_, Arc<CallHost>>,
) -> Result<(), CommandError> {
  if !servers.can_add_account() {
    return Err(locked());
  }
  let Some(previous) = servers.directory.active() else {
    return Ok(());
  };
  refuse_during_call(&calls)?;
  leave_active(&app, &host, true).await;
  service.set_server(None).await;
  servers.directory.park().map_err(not_saved)?;
  background.watch(DesktopConfig::from_profile(&previous));
  Ok(())
}

/// Removes the account of `origin` from this computer: its session ends, its secrets and local data
/// are deleted. Another account becomes active when it was the active one.
#[tauri::command(rename_all = "snake_case")]
pub async fn account_remove(
  origin: String,
  app: AppHandle,
  servers: State<'_, Arc<ServerState>>,
  service: State<'_, Arc<Service>>,
  background: State<'_, Arc<BackgroundAccounts>>,
  host: State<'_, CoreHost>,
  calls: State<'_, Arc<CallHost>>,
) -> Result<RemovedAccount, CommandError> {
  let profile = listed(&servers, &origin)?;
  let config = DesktopConfig::from_profile(&profile);
  let vault = Arc::clone(app.state::<Arc<SecretVault>>().inner());
  let was_active = servers
    .directory
    .active()
    .is_some_and(|active| active.origin == profile.origin);
  if was_active {
    refuse_during_call(&calls)?;
    leave_active(&app, &host, false).await;
    if let Err(error) = service.logout().await {
      tracing::warn!(%error, "the removed account's session may not have ended everywhere");
    }
    service.set_server(None).await;
  } else {
    background.stop(profile.origin.as_str()).await;
    sign_out_elsewhere(Arc::clone(&vault), &config).await;
  }
  // The secrets first: while they can't be deleted the account stays listed, to try again.
  let server = config.server_url.clone();
  let forgotten = tauri::async_runtime::spawn_blocking(move || vault.forget_server(&server))
    .await
    .map_err(|error| error.to_string())
    .and_then(|outcome| outcome.map_err(|error| error.to_string()));
  let accounts = match forgotten {
    Ok(accounts) => accounts,
    Err(error) => {
      // Still listed and still the active one, signed out: the user can try again.
      if was_active {
        service.set_server(Some(config.clone())).await;
      }
      return Err(CommandError::new("account-not-removed", error));
    },
  };
  let storage_key = config
    .storage_key()
    .map_err(|error| CommandError::new("account-not-removed", error))?;
  let core_dir = core_dir(&app).map_err(|error| CommandError::new("account-not-removed", error))?;
  tauri::async_runtime::spawn_blocking(move || remove_server_data(&core_dir, &storage_key))
    .await
    .map_err(|error| CommandError::new("account-not-removed", error.to_string()))?;
  servers.directory.remove(&profile.origin).map_err(not_saved)?;
  if was_active && let Some(next) = servers.directory.accounts().into_iter().next() {
    background.stop(next.origin.as_str()).await;
    service.set_server(Some(DesktopConfig::from_profile(&next))).await;
    servers.directory.switch_to(&next.origin).map_err(not_saved)?;
  }
  tracing::info!("removed an account from this computer");
  if let Err(error) = app.emit(ACCOUNTS_CHANGED_EVENT, ()) {
    tracing::debug!(%error, "could not tell the webview about the accounts");
  }
  Ok(RemovedAccount {
    user_ids: user_ids(&accounts, &config.server_url),
    was_active,
  })
}

/// Deletes store directories that a removal renamed but couldn't delete (a crash, a file in use).
pub fn finish_pending_removals(core_dir: &Path) {
  let Ok(entries) = std::fs::read_dir(core_dir) else {
    return;
  };
  for entry in entries.flatten() {
    if entry.file_name().to_string_lossy().starts_with(REMOVED_PREFIX)
      && let Err(error) = std::fs::remove_dir_all(entry.path())
    {
      tracing::warn!(%error, "the data of a removed account is not deleted yet; trying at the next start");
    }
  }
}

fn accounts_view(servers: &ServerState, background: &BackgroundAccounts) -> AccountsView {
  let maybe_active = servers.directory.active().map(|profile| profile.origin);
  let accounts = servers
    .directory
    .accounts()
    .into_iter()
    .map(|profile| {
      let is_active = Some(&profile.origin) == maybe_active.as_ref();
      AccountView {
        background: if is_active {
          BackgroundState::Unknown
        } else {
          background.state(profile.origin.as_str())
        },
        origin: profile.origin.as_str().to_owned(),
        host: profile.origin.host().to_owned(),
        display_name: profile.display_name,
        is_active,
      }
    })
    .collect();
  AccountsView {
    accounts,
    can_add: servers.can_add_account(),
  }
}

/// The stored account of `origin` as the webview names it.
fn listed(servers: &ServerState, origin: &str) -> Result<ServerProfile, CommandError> {
  ServerOrigin::parse(origin, HttpPolicy::Allowed)
    .ok()
    .filter(|parsed| parsed.as_str() == origin)
    .and_then(|parsed| servers.directory.profile(&parsed))
    .ok_or_else(|| CommandError::new("account-unknown", "no such account".to_owned()))
}

/// One call at a time, and it belongs to the active account: switching would end it unseen.
fn refuse_during_call(calls: &CallHost) -> Result<(), CommandError> {
  if calls.snapshot().maybe_active.is_some() {
    return Err(CommandError::new(
      "account-in-call",
      "leave the call before changing the account".to_owned(),
    ));
  }
  Ok(())
}

/// The active account's core and what the shell keeps of its session on screen go. With
/// `is_kept`, the composers store their drafts first: the account stays.
async fn leave_active(app: &AppHandle, host: &CoreHost, is_kept: bool) {
  if is_kept {
    crate::draft_flush::persist_from(app);
    tokio::time::sleep(crate::draft_flush::CLOSE_GRACE).await;
  }
  crate::voice::stop_for_sign_out(app);
  app.state::<NotificationPictures>().forget_all();
  // Closed now, even while a command still holds the runtime: the store is not in use afterwards.
  if let Ok(runtime) = host.current() {
    runtime.shutdown();
  }
  if let Err(error) = host.stop() {
    tracing::warn!(%error, "could not stop the client core");
  }
}

/// Ends the session of an account that isn't active, with a sign-in service of its own: a refresh
/// first (it learns the provider's endpoints), then the sign-out. Offline it only forgets the
/// tokens; the provider's session then runs out on its own.
async fn sign_out_elsewhere(vault: Arc<SecretVault>, config: &DesktopConfig) {
  let service = match AuthService::new(Some(config.clone()), VaultStore::new(vault)) {
    Ok(service) => service,
    Err(error) => {
      tracing::warn!(%error, "could not end the removed account's session");
      return;
    },
  };
  let sign_out = async {
    if let Err(error) = service.restore().await {
      tracing::debug!(%error, "no session of the removed account to end at the provider");
    }
    service.logout().await
  };
  match tokio::time::timeout(SIGN_OUT_TIMEOUT, sign_out).await {
    Ok(Ok(())) => {},
    Ok(Err(error)) => tracing::warn!(%error, "the removed account's session may not have ended everywhere"),
    Err(_) => tracing::warn!("the removed account's identity provider did not answer in time"),
  }
}

/// Deletes a server's store directory: renamed first, so nothing opens it under its name again,
/// then deleted; what can't be deleted now goes at the next start ([`finish_pending_removals`]).
/// Its keys are gone already, so anything left is unreadable.
fn remove_server_data(core_dir: &Path, storage_key: &str) {
  let dir = core_dir.join(storage_key);
  if !dir.exists() {
    return;
  }
  let nanos = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|elapsed| elapsed.as_nanos())
    .unwrap_or_default();
  let removed = core_dir.join(format!("{REMOVED_PREFIX}{storage_key}-{nanos}"));
  let target = match std::fs::rename(&dir, &removed) {
    Ok(()) => removed,
    Err(error) => {
      tracing::warn!(%error, "could not set the removed account's data aside; deleting it in place");
      dir
    },
  };
  if let Err(error) = std::fs::remove_dir_all(&target) {
    tracing::warn!(%error, "the data of a removed account is not deleted yet");
  }
}

/// The users of `server` among the accounts (`{server}|{user}`) whose keys went.
fn user_ids(accounts: &[String], server: &str) -> Vec<UserId> {
  accounts
    .iter()
    .filter_map(|account| account.strip_prefix(server)?.strip_prefix('|')?.parse().ok())
    .collect()
}

fn locked() -> CommandError {
  CommandError::new(
    "server-entry-locked",
    "the managed configuration allows one server only".to_owned(),
  )
}

fn not_saved(error: std::io::Error) -> CommandError {
  CommandError::new("server-entry-not-saved", error.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::auth::servers::{ManagedServer, ServerDirectory};
  use crate::auth::test_dir::TempDir;

  const ORIGIN_A: &str = "https://a.example.com";
  const ORIGIN_B: &str = "https://b.example.com";

  fn profile(origin: &str) -> ServerProfile {
    ServerProfile {
      origin: ServerOrigin::parse(origin, HttpPolicy::Allowed).unwrap(),
      issuer: "https://auth.example.com/realms/company".into(),
      client_id: "teamcraft".into(),
      scopes: vec!["openid".into()],
      display_name: origin.trim_start_matches("https://").into(),
      server_version: "1.0.0".into(),
      has_local_accounts: false,
      is_login_page_always_shown: true,
      maybe_login_hint: None,
    }
  }

  #[test]
  fn only_a_stored_account_named_exactly_is_taken() {
    let dir = TempDir::new("listed");
    let state = ServerState::new(ServerDirectory::load(&dir.0), ManagedServer::default());
    state.directory.activate(profile(ORIGIN_A)).unwrap();
    assert!(listed(&state, ORIGIN_A).is_ok());
    for other in [
      ORIGIN_B,
      "a.example.com",
      "https://a.example.com/",
      "https://A.example.com",
    ] {
      assert!(listed(&state, other).is_err(), "{other}");
    }
  }

  /// The removed account's directory goes; the other server's directory beside it stays whole.
  #[test]
  fn removing_one_servers_data_leaves_the_others() {
    let dir = TempDir::new("remove-data");
    let (key_a, key_b) = ("a.example.com", "b.example.com");
    for key in [key_a, key_b] {
      let media = dir.0.join(key).join("media-cache").join("1");
      std::fs::create_dir_all(&media).unwrap();
      std::fs::write(dir.0.join(key).join("1.db"), b"sealed").unwrap();
      std::fs::write(media.join("picture"), b"sealed").unwrap();
    }

    remove_server_data(&dir.0, key_a);

    assert!(!dir.0.join(key_a).exists());
    assert!(dir.0.join(key_b).join("1.db").exists());
    assert!(dir.0.join(key_b).join("media-cache/1/picture").exists());
    let left: Vec<_> = std::fs::read_dir(&dir.0)
      .unwrap()
      .flatten()
      .map(|entry| entry.file_name().to_string_lossy().into_owned())
      .collect();
    assert_eq!(left, [key_b]);
  }

  #[test]
  fn a_removal_a_crash_interrupted_is_finished_at_the_start() {
    let dir = TempDir::new("pending-removal");
    let pending = dir.0.join(format!("{REMOVED_PREFIX}a.example.com-1"));
    std::fs::create_dir_all(pending.join("media-cache")).unwrap();
    std::fs::create_dir_all(dir.0.join("b.example.com")).unwrap();

    finish_pending_removals(&dir.0);

    assert!(!pending.exists());
    assert!(dir.0.join("b.example.com").exists());
  }

  #[test]
  fn the_users_of_the_server_are_named() {
    let accounts = vec![
      format!("{ORIGIN_A}|123"),
      format!("{ORIGIN_A}|not-a-user"),
      format!("{ORIGIN_B}|456"),
    ];
    let users = user_ids(&accounts, ORIGIN_A);
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].to_string(), "123");
  }

  #[test]
  fn the_view_lists_every_account_and_marks_the_active_one() {
    let dir = TempDir::new("view");
    let state = ServerState::new(ServerDirectory::load(&dir.0), ManagedServer::default());
    state.directory.activate(profile(ORIGIN_A)).unwrap();
    state.directory.activate(profile(ORIGIN_B)).unwrap();
    let background = BackgroundAccounts::new_for_tests();

    let view = accounts_view(&state, &background);
    assert!(view.can_add);
    let shown: Vec<_> = view
      .accounts
      .iter()
      .map(|account| (account.host.as_str(), account.is_active))
      .collect();
    assert_eq!(shown, [("a.example.com", false), ("b.example.com", true)]);

    let locked = ServerState::new(
      ServerDirectory::load(&dir.0),
      ManagedServer {
        maybe_address: Some(ORIGIN_A.into()),
        is_locked: true,
      },
    );
    assert!(!accounts_view(&locked, &background).can_add);
  }
}
