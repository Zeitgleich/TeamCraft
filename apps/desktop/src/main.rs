//! Desktop shell. It hosts the webview and owns sign-in: the OIDC flow, the keyring and the
//! authenticated API calls live here, so the UI never holds a token. The UI itself is
//! `teamcraft-ui`; client logic will move into `teamcraft-core`.

// Hides the console window on Windows release builds; harmless on macOS and Linux.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod app_menu;
#[cfg(target_os = "linux")]
mod appindicator;
mod attention;
mod auth;
mod autostart;
mod call_window;
mod calls;
mod clipboard;
mod clipboard_paste;
mod core;
#[cfg(target_os = "linux")]
mod desktop_entry;
mod dropped_files;
mod external_links;
#[cfg(target_os = "linux")]
mod freedesktop;
mod message_sound;
mod notifications;
mod reconnect;
mod ringtone;
mod shell_prefs;
mod single_instance;
#[cfg(target_os = "linux")]
mod startup_env;
mod tray;
mod updates;
mod webview_downloads;
#[cfg(target_os = "linux")]
mod webview_memory;
mod webview_menu;
#[cfg(target_os = "linux")]
mod webview_scrolling;
mod window_chrome;
#[cfg(windows)]
mod windows_shell;

use std::sync::Arc;

use anyhow::Context;
use tauri::path::BaseDirectory;
use tauri::webview::NewWindowResponse;
use tauri::{Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_window_state::{AppHandleExt, StateFlags};
use teamcraft_client_api::remote::CORE_FAILED_EVENT;
use tracing_subscriber::EnvFilter;

use crate::auth::servers::{ManagedServer, ServerDirectory, ServerState};
use crate::auth::{AuthService, DesktopConfig, KeyringStore};
use crate::calls::{BUNDLED_MODEL, CALL_CHANGED_EVENT, CallHost, FailureReporter, MediaFiles};
use crate::core::TokenCache;
use crate::core::commands::CoreHost;
use crate::dropped_files::DroppedFilesState;
use crate::shell_prefs::ShellPrefsState;
use crate::updates::UpdateHost;
use crate::window_chrome::WindowChromeState;

fn main() -> anyhow::Result<()> {
  // Before GTK starts: file pickers through xdg-desktop-portal, WebKitGTK without the DMABUF
  // renderer that flickers on some GPUs (startup_env.rs).
  #[cfg(target_os = "linux")]
  startup_env::ensure_startup_environment();
  tracing_subscriber::fmt()
    .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
    .init();

  // No server yet: the account's server is set in `setup`, once the data directory is known.
  let service = Arc::new(AuthService::new(None, KeyringStore).context("could not set up sign-in")?);
  // The core reads the session's current token on every request; the loop below refreshes it.
  let tokens = TokenCache::live(service.clone());
  let refresh_now = reconnect::RefreshNow::default();
  reconnect::keep_tokens_fresh(service.clone(), refresh_now.clone());

  // Registered first: the single-instance plugin must see a second launch before anything else.
  let builder = tauri::Builder::default().plugin(single_instance::plugin());
  #[cfg(windows)]
  let builder = windows_shell::plugins(builder);
  // "Start at login" as a LaunchAgent that starts the app in the tray (autostart.rs).
  #[cfg(target_os = "macos")]
  let builder = builder.plugin(tauri_plugin_autostart::init(
    tauri_plugin_autostart::MacosLauncher::LaunchAgent,
    Some(vec![autostart::AUTOSTART_ARG]),
  ));
  // The menu bar, in the UI's language once it sends its texts (app_menu.rs, #712).
  #[cfg(target_os = "macos")]
  let builder = builder.menu(app_menu::initial);
  // Pictures pasted from the clipboard when the webview's paste event carries none (#761).
  #[cfg(any(target_os = "macos", windows))]
  let builder = builder.plugin(tauri_plugin_clipboard_manager::init());
  builder
    .plugin(tauri_plugin_opener::init())
    // The save and folder dialogs of "Save…" and "Download all" (core_save_attachment[s]); the webview gets no dialog permission.
    .plugin(tauri_plugin_dialog::init())
    .plugin(tauri_plugin_global_shortcut::Builder::new().build())
    .plugin(tauri_plugin_notification::init())
    // The shell drives it (updates.rs); the webview gets no updater permission.
    .plugin(tauri_plugin_updater::Builder::new().build())
    // Size, position and maximized state; visibility is ours (start minimized to the tray), and so
    // are the decorations (the title bar setting, window_chrome.rs).
    .plugin(
      tauri_plugin_window_state::Builder::default()
        .with_state_flags(window_state_flags())
        .build(),
    )
    .manage(service.clone())
    .manage(refresh_now)
    .manage(attention::OsProbe::default())
    .manage(notifications::ShownNotifications::default())
    .manage(notifications::pictures::NotificationPictures::default())
    .manage(call_window::CallWindowState::default())
    .manage(ringtone::RingtoneState::default())
    .manage(message_sound::MessageSoundState::default())
    .manage(calls::push_to_talk::PushToTalkState::default())
    .manage(CoreHost::new(tokens))
    .manage(Arc::new(DroppedFilesState::default()))
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
      let emitter = app.handle().clone();
      let report_failure: FailureReporter = Arc::new(move |failure| {
        if let Err(error) = emitter.emit(CORE_FAILED_EVENT, failure) {
          tracing::debug!(%error, "could not tell the webview about a failure");
        }
      });
      app.manage(Arc::new(CallHost::new(notify, report_failure, files)));
      app.manage(ShellPrefsState::load(app.handle()));
      app.manage(Arc::new(UpdateHost::load(app.handle())));
      UpdateHost::spawn(app.handle().clone());
      // Without a tray (some Linux desktops) the window must never be hidden.
      if let Err(error) = tray::install(app) {
        tracing::warn!(%error, "no tray icon");
      }
      create_main_window(app)?;
      match app.path().app_cache_dir() {
        Ok(dir) => app
          .state::<notifications::pictures::NotificationPictures>()
          .use_dir(dir.join(notifications::pictures::PICTURES_DIR)),
        Err(error) => tracing::info!(%error, "no cache folder: notifications show no pictures on Windows and macOS"),
      }
      notifications::prepare(app.handle());
      #[cfg(target_os = "linux")]
      notifications::watch_notifications(app.handle());
      #[cfg(target_os = "linux")]
      watch_wake(app.handle());
      #[cfg(target_os = "linux")]
      watch_network(app.handle());
      #[cfg(any(target_os = "macos", windows))]
      watch_native_wake(app.handle());
      #[cfg(any(target_os = "macos", windows))]
      watch_native_network(app.handle());
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
      // The plugin writes the window's size, place and maximized state only when the app exits;
      // a session that ends with a logout (SIGTERM) never gets there. Written also when the window
      // goes to the background or is closed, while it is still shown (#721).
      if matches!(event, WindowEvent::Focused(false) | WindowEvent::CloseRequested { .. })
        && window.label() == tray::MAIN_WINDOW
        && window.is_visible().unwrap_or(false)
        && let Err(error) = window.app_handle().save_window_state(window_state_flags())
      {
        tracing::debug!(%error, "could not save the window's size and place");
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
      core::commands::core_phone_link_start,
      core::commands::core_phone_link_status,
      core::commands::core_approval_confirm,
      core::commands::core_approval_cancel,
      core::commands::core_link_announce,
      core::commands::core_link_requests,
      core::commands::core_link_request_review,
      core::commands::core_link_request_reject,
      core::commands::core_messages,
      core::commands::core_thread,
      core::commands::core_action,
      core::commands::core_query,
      core::commands::core_attachment,
      core::commands::core_save_attachment,
      core::commands::core_save_attachments,
      core::commands::core_stage_file,
      core::commands::core_stop,
      core::media::core_media_get,
      core::media::core_media_put,
      core::media::core_media_remove,
      core::media::core_media_clear,
      core::commands::core_reset_store,
      dropped_files::dropped_files_take,
      dropped_files::dropped_file_read,
      clipboard_paste::clipboard_paste_files,
      clipboard_paste::clipboard_read_image,
      calls::commands::call_snapshot,
      calls::commands::call_media_devices,
      calls::commands::call_action,
      calls::commands::call_watch_video,
      calls::commands::call_background_preview,
      calls::commands::call_unwatch_video,
      calls::commands::call_request_keyframe,
      calls::commands::call_video_stats,
      calls::commands::call_watch_camera,
      calls::commands::call_unwatch_camera,
      calls::commands::call_size_camera,
      calls::commands::call_size_video,
      calls::commands::call_watch_level,
      calls::commands::call_unwatch_level,
      external_links::open_external,
      external_links::open_screen_recording_settings,
      clipboard::clipboard_write_image,
      notifications::notify_user,
      notifications::withdraw_notifications,
      notifications::request_attention,
      call_window::call_window_show,
      call_window::call_window_close,
      call_window::call_window_info,
      call_window::call_window_ready,
      call_window::call_window_action,
      ringtone::set_ringtone,
      message_sound::message_sound_play,
      message_sound::message_sound_pick,
      message_sound::message_sound_status,
      message_sound::message_sound_remove,
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
      updates::update_dismiss,
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

/// What the window-state plugin keeps of the main window: size, position, maximized and full
/// screen. Visibility is ours (start minimized to the tray), and so are the decorations (the title
/// bar setting, window_chrome.rs).
fn window_state_flags() -> StateFlags {
  StateFlags::all() - StateFlags::VISIBLE - StateFlags::DECORATIONS
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
  // Started at login (autostart.rs): in the tray, like "start minimized".
  let is_start_minimized = prefs.is_start_minimized || autostart::is_login_launch();
  let is_hidden = is_start_minimized && has_tray;
  // The title stays set without a title bar: the taskbar, Alt+Tab and the dock show it.
  let window = WebviewWindowBuilder::new(app, tray::MAIN_WINDOW, WebviewUrl::default())
    .title("TeamCraft")
    .visible(!is_hidden)
    .inner_size(1360.0, 860.0)
    .min_inner_size(720.0, 520.0)
    // Files dragged onto the window reach the page as HTML5 drag and drop (#484), the same path as
    // in the browser: the UI gets `File` objects and attaches them like the + button. Tauri's own
    // handler would swallow the drop on every platform and only report file paths. WebKitGTK hands
    // the page no files, so on Linux the shell reads them for it (dropped_files.rs, #651).
    .disable_drag_drop_handler()
    // A download the page starts itself lands in the downloads folder and is reported (#719).
    .on_download(webview_downloads::on_download)
    .on_new_window({
      let app = app.handle().clone();
      move |url, _features| {
        if is_allowed_new_window(&url) {
          return NewWindowResponse::Allow;
        }
        // Clicks on links reach `open_external` from the page; this is the fallback for whatever
        // still asks for a new window (`window.open`, a link the page did not route).
        if let Err(error) = external_links::open(&app, url.as_str()) {
          tracing::warn!(%error, "could not open a link in the browser");
        }
        NewWindowResponse::Deny
      }
    })
    // The app never leaves its own pages (#651): a file dropped beside the drop zones, or a link the
    // page did not route, would replace the whole app. Web links go to the browser instead.
    .on_navigation({
      let app = app.handle().clone();
      let maybe_dev_url = cfg!(debug_assertions)
        .then(|| app.config().build.dev_url.clone())
        .flatten();
      move |url| {
        if is_app_navigation(url, maybe_dev_url.as_ref()) {
          return true;
        }
        // The scheme only: a dropped file's address names the file.
        tracing::warn!(scheme = url.scheme(), "refused to navigate away from the app");
        if matches!(url.scheme(), "http" | "https")
          && let Err(error) = external_links::open(&app, url.as_str())
        {
          tracing::warn!(%error, "could not open a link in the browser");
        }
        false
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
  // `TEAMCRAFT_DEVTOOLS=1`: the webview's own context menu everywhere (webview_menu.rs).
  let window = match webview_menu::is_devtools_requested() {
    true => window.initialization_script(webview_menu::NATIVE_MENU_SCRIPT),
    false => window,
  };
  let window = window.build()?;
  #[cfg(target_os = "linux")]
  {
    let dropped = app.state::<Arc<DroppedFilesState>>().inner().clone();
    let handle = app.handle().clone();
    let configured = window.with_webview(move |webview| {
      let webview = webview.inner();
      webview_memory::configure(&webview);
      webview_scrolling::configure(&webview);
      webview_menu::filter_menus(&webview);
      dropped_files::watch(&webview, dropped, handle);
    });
    if let Err(error) = configured {
      tracing::warn!(%error, "could not set up the webview's memory caches, scrolling, menus and file drops");
    }
  }
  tray::reveal_without_host(app.handle());
  if is_start_minimized
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

/// Navigation the webview may do: within the app's own pages (`tauri://localhost` on Linux and macOS,
/// `http(s)://tauri.localhost` on Windows, the dev server in debug builds) and the in-page documents
/// frames load (`about:`, `blob:`, `data:`). Never a `file:` or another site.
fn is_app_navigation(url: &tauri::Url, maybe_dev_url: Option<&tauri::Url>) -> bool {
  match url.scheme() {
    "tauri" => url.host_str() == Some("localhost"),
    "http" | "https" => {
      url.host_str() == Some("tauri.localhost")
        || maybe_dev_url.is_some_and(|dev| dev.origin() == url.origin())
    },
    "about" | "blob" | "data" => true,
    _ => false,
  }
}

/// The client core reads its access token synchronously; this keeps a copy current (the login code
/// refreshes it before it expires) and clears it on sign-out.
/// Resuming from suspend reconnects the gateway at once (logind). macOS and Windows use
/// `watch_native_wake`; should that hook fail, the core notices the wall-clock jump at its next
/// check (within seconds) and the webview reports the network coming back.
#[cfg(target_os = "linux")]
fn watch_wake(app: &tauri::AppHandle) {
  let app = app.clone();
  tauri::async_runtime::spawn(async move {
    let buses = app.state::<attention::OsProbe>().buses();
    let handle = app.clone();
    buses
      .watch_wake(move || {
        tracing::info!("resumed from suspend");
        reconnect::network_back(&handle);
      })
      .await;
  });
}

/// NetworkManager reports the network back up (it often is only seconds after a wake-up): the
/// gateway reconnects at once (issue #883). Without NetworkManager a probe stands in for it.
#[cfg(target_os = "linux")]
fn watch_network(app: &tauri::AppHandle) {
  let app = app.clone();
  tauri::async_runtime::spawn(async move {
    let buses = app.state::<attention::OsProbe>().buses();
    let handle = app.clone();
    let has_network_manager = buses
      .watch_network_up(move || {
        tracing::info!("the network is up");
        reconnect::network_back(&handle);
      })
      .await;
    if !has_network_manager {
      reconnect::probe_while_offline(app);
    }
  });
}

/// macOS (`NSWorkspaceDidWakeNotification`) and Windows (`PBT_APMRESUMEAUTOMATIC`) resume hook.
#[cfg(any(target_os = "macos", windows))]
fn watch_native_wake(app: &tauri::AppHandle) {
  let handle = app.clone();
  let result = teamcraft_desktop_native::watch_resume(move || {
    tracing::info!("resumed from suspend");
    reconnect::network_back(&handle);
  });
  if let Err(error) = result {
    tracing::warn!(%error, "no resume notification; reconnecting at the next check");
  }
}

/// macOS (Network.framework's path monitor) and Windows (connectivity hint) report the network back
/// up: the gateway reconnects at once (issue #883). Should the hook fail, a probe stands in for it.
#[cfg(any(target_os = "macos", windows))]
fn watch_native_network(app: &tauri::AppHandle) {
  let handle = app.clone();
  let result = teamcraft_desktop_native::watch_network_up(move || {
    tracing::info!("the network is up");
    reconnect::network_back(&handle);
  });
  if let Err(error) = result {
    tracing::warn!(%error, "no network notification; probing for the network instead");
    reconnect::probe_while_offline(app.clone());
  }
}

#[cfg(test)]
mod tests {
  use super::{is_allowed_new_window, is_app_navigation};

  #[test]
  fn the_window_stays_on_the_app() {
    let dev: tauri::Url = "http://127.0.0.1:8082".parse().expect("valid URL");
    let allowed = |url: &str, maybe_dev: Option<&tauri::Url>| {
      url.parse().is_ok_and(|url| is_app_navigation(&url, maybe_dev))
    };
    for app in [
      "tauri://localhost/",
      "tauri://localhost/s/1/c/13",
      "http://tauri.localhost/",
      "https://tauri.localhost/settings",
      "about:blank",
      "blob:tauri://localhost/1b4c",
    ] {
      assert!(allowed(app, None), "{app}");
    }
    assert!(allowed("http://127.0.0.1:8082/c/1", Some(&dev)));
    for refused in [
      "file:///home/user/Pictures/beach.jpg",
      "https://example.com/",
      "tauri://example.com/",
      "http://127.0.0.1:8082/",
      "http://127.0.0.1:9000/",
    ] {
      let maybe_dev = refused.contains("9000").then_some(&dev);
      assert!(!allowed(refused, maybe_dev), "{refused}");
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
