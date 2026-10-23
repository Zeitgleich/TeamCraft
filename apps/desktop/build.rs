//! Every command the webview may call is listed here and allowed one by one in
//! `capabilities/default.json`. A command that is registered in `main.rs` but missing from either
//! list is refused by Tauri, so a new command is a deliberate decision (docs/security-review.md C3).

/// The commands of the shell, in the order of `main.rs`.
const COMMANDS: &[&str] = &[
  "auth_restore",
  "auth_login",
  "auth_cancel",
  "auth_logout",
  "api_get",
  "api_request",
  "public_get",
  "core_start",
  "core_status",
  "core_enroll",
  "core_snapshot",
  "core_link_start",
  "core_link_status",
  "core_link_cancel",
  "core_approval_start",
  "core_approval_confirm",
  "core_approval_cancel",
  "core_messages",
  "core_thread",
  "core_action",
  "core_query",
  "core_stop",
  "call_snapshot",
  "call_media_devices",
  "call_action",
  "call_watch_video",
  "call_background_preview",
  "call_unwatch_video",
  "notify_user",
  "withdraw_notifications",
  "os_attention",
  "autostart_get",
  "autostart_set",
  "shell_prefs_get",
  "shell_prefs_set",
  "tray_update",
  "set_zoom",
];

fn main() {
  let manifest = tauri_build::AppManifest::new().commands(COMMANDS);
  if let Err(error) = tauri_build::try_build(tauri_build::Attributes::new().app_manifest(manifest)) {
    // A build script has no other way to report; the message is the whole point.
    panic!("failed to run the Tauri build step: {error}");
  }
}
