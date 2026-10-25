//! Desktop shell. It hosts the webview and owns sign-in: the OIDC flow, the keyring and the
//! authenticated API calls live here, so the UI never holds a token. The UI itself is
//! `teamcraft-ui`; client logic will move into `teamcraft-core`.

// Hides the console window on Windows release builds; harmless on macOS and Linux.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "linux")]
mod appindicator;
mod attention;
mod auth;
mod autostart;
mod calls;
mod core;
#[cfg(target_os = "linux")]
mod file_portal;
#[cfg(target_os = "linux")]
mod freedesktop;
mod notifications;
mod ringtone;
mod shell_prefs;
mod single_instance;
mod tray;
mod updates;
#[cfg(target_os = "linux")]
mod webview_memory;
mod window_chrome;
#[cfg(windows)]
mod windows_shell;

use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tauri::path::BaseDirectory;
use tauri::webview::NewWindowResponse;
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_window_state::StateFlags;
use tracing_subscriber::EnvFilter;

use teamcraft_client_api::server_entry::ServerProfile;

use crate::auth::servers::{ManagedServer, ServerDirectory, ServerState};
use crate::auth::{AuthService, DesktopConfig, KeyringStore};
use crate::calls::{BUNDLED_MODEL, CALL_CHANGED_EVENT, CallHost, MediaFiles};
use crate::core::TokenCache;
use crate::core::commands::CoreHost;
use crate::shell_prefs::ShellPrefsState;
use crate::updates::UpdateHost;
use crate::window_chrome::WindowChromeState;

/// How often the login session's access token is checked and refreshed before it runs out.
const TOKEN_REFRESH: Duration = Duration::from_secs(2);

fn main() -> anyhow::Result<()> {
  // Before GTK starts: file pickers go through xdg-desktop-portal (the desktop's own dialog).
  #[cfg(target_os = "linux")]
  file_portal::ensure_portal_choosers();
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
    .init();

  // No server yet: the account's server is set in `setup`, once the data directory is known.
  let service = Arc::new(AuthService::new(None, KeyringStore).context("could not set up sign-in")?);
  // The core reads the session's current token on every request; the loop below refreshes it.
  let tokens = TokenCache::live(service.clone());
  keep_tokens_fresh(service.clone());

  // Registered first: the single-instance plugin must see a second launch before anything else.
  let builder = tauri::Builder::default().plugin(single_instance::plugin());
  #[cfg(windows)]
  let builder = windows_shell::plugins(builder);
  builder
    .plugin(tauri_plugin_opener::init())
    .plugin(tauri_plugin_global_shortcut::Builder::new().build())
    .plugin(tauri_plugin_notification::init())
    // The shell drives it (updates.rs); the webview gets no updater permission.
    .plugin(tauri_plugin_updater::Builder::new().build())
    // Size, position and maximized state; visibility is ours (start minimized to the tray), and so
    // are the decorations (the title bar setting, window_chrome.rs).
    .plugin(
      tauri_plugin_window_state::Builder::default()
        .with_state_flags(StateFlags::all() - StateFlags::VISIBLE - StateFlags::DECORATIONS)
        .build(),
    )
    .manage(service.clone())
    .manage(attention::OsProbe::default())
    .manage(notifications::ShownNotifications::default())
    .manage(ringtone::RingtoneState::default())
    .manage(calls::push_to_talk::PushToTalkState::default())
    .manage(CoreHost::new(tokens))
    .setup(move |app| {
      let servers = Arc::new(ServerState::new(
        ServerDirectory::load(&core::commands::base_dir(app.handle())?),
        ManagedServer::load(),
      ));
      match servers.startup_account() {
        Ok(Some(profile)) => {
          let config = DesktopConfig::from_profile(&profile);
          if let Err(error) = tauri::async_runtime::block_on(service.forget_origin_only_token(&config)) {
            tracing::warn!(%error, "could not delete an outdated token entry");
          }
          tauri::async_runtime::block_on(service.set_server(Some(config)));
        },
        Ok(None) => {},
        Err(forbidden) => {
          let config = DesktopConfig::from_profile(&forbidden);
          if let Err(error) = tauri::async_runtime::block_on(service.forget_tokens(&config)) {
            tracing::warn!(%error, "could not delete the token of a forbidden server");
          }
        },
      }
      clean_up_legacy_layout(core::commands::base_dir(app.handle())?, servers.directory.profiles());
      app.manage(servers);
      let emitter = app.handle().clone();
      let notify: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if let Err(error) = emitter.emit(CALL_CHANGED_EVENT, ()) {
          tracing::debug!(%error, "could not notify the webview");
        }
      });
      let files = MediaFiles {
        codec_directory: app.path().app_data_dir()?.join("codecs"),
        maybe_segmentation_model: app.path().resolve(BUNDLED_MODEL, BaseDirectory::Resource).ok(),
      };
      app.manage(Arc::new(CallHost::new(notify, files)));
      app.manage(ShellPrefsState::load(app.handle()));
      app.manage(Arc::new(UpdateHost::load(app.handle())));
      UpdateHost::spawn(app.handle().clone());
      // Without a tray (some Linux desktops) the window must never be hidden.
      if let Err(error) = tray::install(app) {
        tracing::warn!(%error, "no tray icon");
      }
      create_main_window(app)?;
      notifications::prepare();
      #[cfg(target_os = "linux")]
      notifications::watch_actions(app.handle());
      #[cfg(target_os = "linux")]
      watch_wake(app.handle());
      #[cfg(windows)]
      windows_shell::forward_deep_links(app.handle());
      Ok(())
    })
    .on_window_event(|window, event| {
      if let WindowEvent::Resized(_) = event
        && window.label() == tray::MAIN_WINDOW
      {
        window_chrome::on_resized(window);
      }
      // Closing keeps the app running in the tray (notifications keep coming) when the user wants it.
      // Without a tray icon it closes as usual, which quits the app.
      if let WindowEvent::CloseRequested { api, .. } = event
        && window.label() == tray::MAIN_WINDOW
        && window.state::<ShellPrefsState>().get().is_close_to_tray
        && tray::has_tray(window.app_handle())
      {
        api.prevent_close();
        if let Err(error) = window.hide() {
          tracing::debug!(%error, "could not hide the window");
        }
      }
    })
    .invoke_handler(tauri::generate_handler![
      auth::commands::auth_restore,
      auth::commands::auth_login,
      auth::commands::auth_login_local,
      auth::commands::auth_cancel,
      auth::commands::auth_logout,
      auth::commands::api_get,
      auth::commands::api_request,
      auth::commands::api_request_bytes,
      auth::commands::public_get,
      auth::commands::public_asset,
      auth::commands::avatar_download,
      auth::commands::avatar_upload,
      auth::commands::server_state,
      auth::commands::server_check,
      auth::commands::server_suggest,
      auth::commands::server_confirm,
      auth::commands::server_forget,
      core::commands::core_start,
      core::commands::core_status,
      core::commands::core_connection,
      core::commands::core_network_changed,
      core::commands::core_focus,
      core::commands::core_enroll,
      core::commands::core_snapshot,
      core::commands::core_snapshot_changes,
      core::commands::core_link_start,
      core::commands::core_link_status,
      core::commands::core_link_cancel,
      core::commands::core_approval_start,
      core::commands::core_approval_confirm,
      core::commands::core_approval_cancel,
      core::commands::core_messages,
      core::commands::core_thread,
      core::commands::core_action,
      core::commands::core_query,
      core::commands::core_attachment,
      core::commands::core_stage_file,
      core::commands::core_stop,
      core::commands::core_reset_store,
      calls::commands::call_snapshot,
      calls::commands::call_media_devices,
      calls::commands::call_action,
      calls::commands::call_watch_video,
      calls::commands::call_background_preview,
      calls::commands::call_unwatch_video,
      calls::commands::call_watch_camera,
      calls::commands::call_unwatch_camera,
      notifications::notify_user,
      notifications::withdraw_notifications,
      notifications::request_attention,
      ringtone::set_ringtone,
      attention::os_attention,
      autostart::autostart_get,
      autostart::autostart_set,
      shell_prefs::shell_prefs_get,
      shell_prefs::shell_prefs_set,
      tray::tray_update,
      tray::set_zoom,
      updates::update_status,
      updates::update_check,
      updates::update_set_preference,
      updates::update_download,
      updates::update_restart,
      updates::update_with_package_manager,
      window_chrome::window_chrome,
      window_chrome::window_action,
    ])
    .build(tauri::generate_context!())
    .context("failed to build the TeamCraft desktop shell")?
    .run(|app, event| match event {
      // A downloaded update is installed when the app quits (docs/updates.md §4.3).
      tauri::RunEvent::Exit => app.state::<Arc<UpdateHost>>().install_on_exit(app),
      // macOS: clicking the dock icon brings back a window that was closed to the tray.
      #[cfg(target_os = "macos")]
      tauri::RunEvent::Reopen { .. } => tray::show_main_window(app),
      _ => {},
    });
  Ok(())
}

/// The window is created here, not in `tauri.conf.json`, so it can start hidden in the tray. Debug
/// builds take `TEAMCRAFT_BACKEND=server|mock` to pick the data source of a UI built with the `demo`
/// feature (the UI reads it from `localStorage`); release builds never offer demo data.
fn create_main_window(app: &tauri::App) -> tauri::Result<()> {
  let prefs = app.state::<ShellPrefsState>().get();
  // Without a tray icon a hidden window couldn't come back: start minimized to the taskbar instead.
  // Linux asks D-Bus for the icon's host in the background; a window hidden before the answer is
  // brought back by `tray::reveal_without_host` if there is none.
  let has_tray = tray::may_have_tray(app.handle());
  let is_hidden = prefs.is_start_minimized && has_tray;
  // The title stays set without a title bar: the taskbar, Alt+Tab and the dock show it.
  let window = WebviewWindowBuilder::new(app, tray::MAIN_WINDOW, WebviewUrl::default())
    .title("TeamCraft")
    .visible(!is_hidden)
    .inner_size(1360.0, 860.0)
    .min_inner_size(720.0, 520.0)
    // Files dragged onto the window reach the page as HTML5 drag and drop (#484), the same path as
    // in the browser: the UI gets `File` objects and attaches them like the + button. Tauri's own
    // handler would swallow the drop on every platform and only report file paths.
    .disable_drag_drop_handler()
    .on_new_window({
      let app = app.handle().clone();
      move |url, _features| {
        if is_allowed_new_window(&url) {
          return NewWindowResponse::Allow;
        }
        // A link (target="_blank" in a message, a preview card, "what's new"): the system browser.
        if is_external_link(&url)
          && let Err(error) = app.opener().open_url(url.as_str(), None::<&str>)
        {
          tracing::warn!(%error, "could not open a link in the browser");
        }
        NewWindowResponse::Deny
      }
    });
  let window = window_chrome::configure(window, prefs.title_bar);
  app.manage(WindowChromeState::new(prefs.title_bar));
  #[cfg(debug_assertions)]
  let window = match std::env::var("TEAMCRAFT_BACKEND") {
    Ok(mode) if matches!(mode.as_str(), "server" | "mock") => window.initialization_script(format!(
      "try {{ localStorage.setItem('teamcraft.backend', '{mode}'); }} catch (_) {{}}"
    )),
    _ => window,
  };
  let window = window.build()?;
  #[cfg(target_os = "linux")]
  if let Err(error) = window.with_webview(|webview| webview_memory::configure(&webview.inner())) {
    tracing::warn!(%error, "could not set the webview's memory caches");
  }
  tray::reveal_without_host(app.handle());
  if prefs.is_start_minimized
    && !has_tray
    && let Err(error) = window.minimize()
  {
    tracing::debug!(%error, "could not minimize the window");
  }
  Ok(())
}

/// `window.open` from the UI: only the blank window the image viewer fills from memory ("open in a
/// new window") may open; it never loads anything, so decrypted files stay in the app. Links go
/// through the system browser instead.
fn is_allowed_new_window(url: &tauri::Url) -> bool {
  url.as_str() == "about:blank"
}

/// Links the system may open: web pages and mail addresses, never local files or app URLs.
fn is_external_link(url: &tauri::Url) -> bool {
  matches!(url.scheme(), "https" | "http" | "mailto")
}

/// The client core reads its access token synchronously; this keeps a copy current (the login code
/// refreshes it before it expires) and clears it on sign-out.
/// Resuming from suspend reconnects the gateway at once (logind). macOS and Windows have no such
/// hook without native code; there the core notices the wall-clock jump at its next check (within
/// seconds) and the webview reports the network coming back.
#[cfg(target_os = "linux")]
fn watch_wake(app: &tauri::AppHandle) {
  let app = app.clone();
  tauri::async_runtime::spawn(async move {
    let buses = app.state::<attention::OsProbe>().buses();
    let handle = app.clone();
    buses
      .watch_wake(move || {
        tracing::info!("resumed from suspend");
        handle.state::<CoreHost>().network_changed();
        handle.state::<Arc<CallHost>>().network_changed();
      })
      .await;
  });
}

/// Revokes and deletes what builds from before the server entry left behind (#223), in the
/// background: the keyring and the identity provider may be slow.
fn clean_up_legacy_layout(base_dir: std::path::PathBuf, profiles: Vec<ServerProfile>) {
  tauri::async_runtime::spawn(async move {
    match auth::legacy::HttpRevoker::new() {
      Ok(revoker) => {
        let keys = auth::legacy::KeyringDatabaseKeys;
        auth::legacy::clean_up_once(&base_dir, &profiles, &KeyringStore, &keys, &revoker).await;
      },
      Err(error) => tracing::warn!(%error, "could not clean up the old layout"),
    }
  });
}

fn keep_tokens_fresh(service: Arc<AuthService<KeyringStore>>) {
  tauri::async_runtime::spawn(async move {
    loop {
      // Refreshes the token when it is close to expiry; the core reads the result from the session.
      // Signed out is the normal case here, not an error.
      drop(service.server_bearer_token().await);
      tokio::time::sleep(TOKEN_REFRESH).await;
    }
  });
}

#[cfg(test)]
mod tests {
  use super::{is_allowed_new_window, is_external_link};

  #[test]
  fn only_web_and_mail_links_go_to_the_system() {
    let external = |url: &str| url.parse().is_ok_and(|url| is_external_link(&url));
    for opened in [
      "https://example.com/a",
      "http://example.com/",
      "mailto:a@example.com",
    ] {
      assert!(external(opened), "{opened}");
    }
    for refused in [
      "file:///etc/passwd",
      "tauri://localhost/",
      "about:blank",
      "javascript:alert(1)",
    ] {
      assert!(!external(refused), "{refused}");
    }
  }

  #[test]
  fn only_the_blank_viewer_window_may_open() {
    let allowed = |url: &str| url.parse().is_ok_and(|url| is_allowed_new_window(&url));
    assert!(allowed("about:blank"));
    for refused in [
      "https://example.com/",
      "about:blank#x",
      "file:///etc/passwd",
      "tauri://localhost/",
    ] {
      assert!(!allowed(refused), "{refused}");
    }
  }

  /// Tauri refuses an app command that has no permission once the app has an ACL manifest, and
  /// nothing fails at build time: every registered command must be in `build.rs` and allowed in
  /// the capability.
  #[test]
  fn every_registered_command_is_listed_and_allowed() {
    let main = include_str!("main.rs");
    let build = include_str!("../build.rs");
    let capability: serde_json::Value =
      serde_json::from_str(include_str!("../capabilities/default.json")).unwrap_or_default();
    let allowed: Vec<String> = capability["permissions"]
      .as_array()
      .map(|list| {
        list
          .iter()
          .filter_map(|value| value.as_str())
          .map(str::to_owned)
          .collect()
      })
      .unwrap_or_default();

    let handler_start = main.find("generate_handler![").unwrap_or(0);
    let handler = &main[handler_start..];
    let handler = &handler[..handler.find(']').unwrap_or(handler.len())];
    let registered: Vec<&str> = handler
      .trim_start_matches("generate_handler![")
      .split(',')
      .filter_map(|path| path.trim().rsplit("::").next())
      .filter(|name| !name.is_empty())
      .collect();
    assert!(registered.len() > 10, "could not read the handler list");

    for command in registered {
      assert!(
        build.contains(&format!("\"{command}\"")),
        "{command} is missing from build.rs"
      );
      let permission = format!("allow-{}", command.replace('_', "-"));
      assert!(
        allowed.contains(&permission),
        "{permission} is missing from capabilities/default.json"
      );
    }
  }
}
