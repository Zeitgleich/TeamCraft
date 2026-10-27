//! Linux desktop services over D-Bus: idle time and screen lock for attention (notifications.md,
//! "Attention"), and notifications that can be replaced and withdrawn, with buttons and, where the
//! server offers it, an inline reply (the notification plugin can do none of it). Every probe falls
//! back quietly: desktops implement different subsets.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::OnceCell;
use zbus::Connection;
use zbus::zvariant::{OwnedValue, Structure, Value};

/// A D-Bus service that does not answer within this time counts as missing.
const CALL_TIMEOUT: Duration = Duration::from_secs(1);
/// The notification server picks how long a notification stays.
const DEFAULT_EXPIRY: i32 = -1;

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";
const DEFAULT_ACTION: &str = "default";
/// The capability of servers that offer a text field (KDE Plasma); the action key that asks for it.
const INLINE_REPLY: &str = "inline-reply";
/// Urgency hint values of the notification spec.
const URGENCY_NORMAL: u8 = 1;
const URGENCY_CRITICAL: u8 = 2;
/// The picture hint of the notification spec 1.2 (`image_data` in 1.1, `icon_data` in 1.0), which
/// GNOME Shell and KDE Plasma both show in place of the app icon.
const IMAGE_DATA_HINT: &str = "image-data";
/// `image-data` is 8-bit RGBA.
const IMAGE_BITS_PER_SAMPLE: i32 = 8;
const IMAGE_CHANNELS: i32 = 4;

const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";

const NETWORK_MANAGER: &str = "org.freedesktop.NetworkManager";
const NETWORK_MANAGER_PATH: &str = "/org/freedesktop/NetworkManager";
/// `NM_STATE_CONNECTED_SITE`: a default route exists. `CONNECTED_GLOBAL` (70) is above it; it needs
/// NetworkManager's connectivity check, which not every distribution enables.
const NM_STATE_CONNECTED_SITE: u32 = 60;

/// The StatusNotifier (appindicator) watcher; panels that show tray icons register with it as hosts.
const STATUS_NOTIFIER_WATCHER: &str = "org.kde.StatusNotifierWatcher";
const STATUS_NOTIFIER_WATCHER_PATH: &str = "/StatusNotifierWatcher";
const STATUS_NOTIFIER_HOST_SIGNALS: [&str; 2] =
  ["StatusNotifierHostRegistered", "StatusNotifierHostUnregistered"];
const BUS_DAEMON: &str = "org.freedesktop.DBus";

/// The launcher badge API that began with Ubuntu's Unity and that KDE Plasma's task manager, Dash to
/// Dock and others listen to: a broadcast signal, from any object path, naming the app's `.desktop`.
const LAUNCHER_ENTRY: &str = "com.canonical.Unity.LauncherEntry";
const LAUNCHER_ENTRY_PATH: &str = "/de/zeitgleich/teamcraft/LauncherEntry";

/// Session and system bus, connected on first use.
#[derive(Default)]
pub struct Buses {
  session: OnceCell<Option<Connection>>,
  system: OnceCell<Option<Connection>>,
  /// What the notification server supports (`GetCapabilities`), asked once.
  capabilities: OnceCell<Vec<String>>,
}

/// What the desktop says about the user right now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DesktopState {
  pub maybe_idle: Option<Duration>,
  pub is_locked: bool,
}

/// One notification to show.
pub struct DesktopNotification<'a> {
  pub app_name: &'a str,
  pub app_icon: &'a str,
  /// The installed `.desktop` entry of the app, so the server shows its name and icon. `None`
  /// when none is installed: a hint naming a missing entry makes KDE show the raw ID as app name.
  pub maybe_desktop_entry: Option<&'a str>,
  pub title: &'a str,
  pub body: &'a str,
  /// The notification this one replaces (0: none).
  pub replaces_id: u32,
  /// Buttons as (action key, label); a click on the notification itself is always reported.
  pub actions: &'a [(&'a str, &'a str)],
  /// The placeholder of a text field, shown where the server has `inline-reply`.
  pub maybe_reply_placeholder: Option<&'a str>,
  /// `im.received`, `call.incoming`, … (the spec's categories); servers pick sounds and icons by it.
  pub category: &'a str,
  /// A ringing call: critical urgency (stays until withdrawn).
  pub is_ringing: bool,
  /// No server sound: a ringing call (the app rings) or a message whose sound the app plays itself
  /// (or none, the user's choice).
  pub is_quiet: bool,
  /// The sender's or channel's picture (#543).
  pub maybe_image: Option<HintImage<'a>>,
}

/// A square picture for the `image-data` hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HintImage<'a> {
  pub side: u32,
  /// Straight (not premultiplied) RGBA, row by row, `side` × `side`.
  pub rgba: &'a [u8],
}

/// What the notification server reports about a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerEvent {
  /// The user clicked it, a button or sent an inline reply.
  Action(ServerAction),
  /// It is gone: expired, dismissed or closed by the app.
  Closed { id: u32 },
}

/// What the user did with a notification, as the server reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerAction {
  pub id: u32,
  /// The action key (`default` for a click on the notification).
  pub key: String,
  /// The text of an inline reply.
  pub maybe_text: Option<String>,
}

impl Buses {
  /// Idle time from GNOME's idle monitor or the freedesktop screen saver; locked when logind, the
  /// freedesktop or the GNOME screen saver says so.
  pub async fn desktop_state(&self) -> DesktopState {
    let maybe_session = self.session().await;
    let maybe_system = self.system().await;
    let mut maybe_idle = None;
    let mut is_locked = false;
    if let Some(session) = maybe_session {
      maybe_idle = match mutter_idle(session).await {
        Some(idle) => Some(idle),
        None => screensaver_idle(session).await,
      };
      is_locked |= screensaver_active(
        session,
        "org.freedesktop.ScreenSaver",
        "/org/freedesktop/ScreenSaver",
      )
      .await;
      is_locked |= screensaver_active(session, "org.gnome.ScreenSaver", "/org/gnome/ScreenSaver").await;
    }
    if let Some(system) = maybe_system {
      is_locked |= logind_locked_hint(system).await;
    }
    DesktopState {
      maybe_idle,
      is_locked,
    }
  }

  /// Shows a notification; returns its ID, `None` when no notification server answered.
  pub async fn notify(&self, notification: &DesktopNotification<'_>) -> Option<u32> {
    let session = self.session().await?;
    let has_inline_reply = self.has_capability(INLINE_REPLY).await;
    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    if let Some(entry) = notification.maybe_desktop_entry {
      hints.insert("desktop-entry", Value::from(entry));
    }
    hints.insert("category", Value::from(notification.category));
    let urgency = if notification.is_ringing {
      URGENCY_CRITICAL
    } else {
      URGENCY_NORMAL
    };
    hints.insert("urgency", Value::from(urgency));
    if notification.is_quiet {
      hints.insert("suppress-sound", Value::from(true));
    }
    if let Some(image) = notification.maybe_image.and_then(image_data) {
      hints.insert(IMAGE_DATA_HINT, image);
    }
    let actions = server_actions(notification, has_inline_reply);
    let expire_timeout = if notification.is_ringing {
      0
    } else {
      DEFAULT_EXPIRY
    };
    let body = (
      notification.app_name,
      notification.replaces_id,
      notification.app_icon,
      notification.title,
      notification.body,
      actions,
      hints,
      expire_timeout,
    );
    call::<u32, _>(
      session,
      NOTIFICATIONS,
      NOTIFICATIONS_PATH,
      NOTIFICATIONS,
      "Notify",
      &body,
    )
    .await
  }

  /// Calls `on_event` for every click, button, inline reply and closed notification, for as long
  /// as the session bus lives. Returns at once when there is no session bus.
  pub async fn watch_notifications(&self, on_event: impl Fn(ServerEvent) + Send + 'static) {
    let Some(session) = self.session().await else {
      return;
    };
    let rule = zbus::MatchRule::builder()
      .msg_type(zbus::message::Type::Signal)
      .interface(NOTIFICATIONS)
      .map(|builder| builder.build());
    let Ok(rule) = rule else {
      return;
    };
    let Ok(mut stream) = zbus::MessageStream::for_match_rule(rule, session, None).await else {
      tracing::debug!("cannot watch notifications");
      return;
    };
    while let Some(Ok(message)) = stream.next().await {
      let header = message.header();
      let Some(member) = header.member() else {
        continue;
      };
      let body = message.body();
      let event = match member.as_str() {
        "ActionInvoked" => body.deserialize::<(u32, String)>().map(|(id, key)| {
          ServerEvent::Action(ServerAction {
            id,
            key,
            maybe_text: None,
          })
        }),
        "NotificationReplied" => body.deserialize::<(u32, String)>().map(|(id, text)| {
          ServerEvent::Action(ServerAction {
            id,
            key: INLINE_REPLY.to_owned(),
            maybe_text: Some(text),
          })
        }),
        // The second value is the reason, which doesn't matter here.
        "NotificationClosed" => body
          .deserialize::<(u32, u32)>()
          .map(|(id, _reason)| ServerEvent::Closed { id }),
        _ => continue,
      };
      if let Ok(event) = event {
        on_event(event);
      }
    }
  }

  /// Whether the desktop's own do-not-disturb is on: the notification server's `Inhibited`
  /// property (KDE Plasma and others), or GNOME's "show banners" switch.
  pub async fn is_do_not_disturb(&self) -> bool {
    if let Some(session) = self.session().await
      && let Some(value) = call::<OwnedValue, _>(
        session,
        NOTIFICATIONS,
        NOTIFICATIONS_PATH,
        "org.freedesktop.DBus.Properties",
        "Get",
        &(NOTIFICATIONS, "Inhibited"),
      )
      .await
      && bool::try_from(value).unwrap_or(false)
    {
      return true;
    }
    gnome_hides_banners()
  }

  /// Calls `on_wake` every time the computer resumes from suspend (logind `PrepareForSleep(false)`),
  /// for as long as the system bus lives. Returns at once when there is no system bus.
  pub async fn watch_wake(&self, on_wake: impl Fn() + Send + 'static) {
    let Some(system) = self.system().await else {
      return;
    };
    let rule = zbus::MatchRule::builder()
      .msg_type(zbus::message::Type::Signal)
      .sender(LOGIND)
      .and_then(|builder| builder.interface(LOGIND_MANAGER))
      .and_then(|builder| builder.member("PrepareForSleep"))
      .map(|builder| builder.build());
    let Ok(rule) = rule else {
      return;
    };
    let Ok(mut stream) = zbus::MessageStream::for_match_rule(rule, system, None).await else {
      tracing::debug!("cannot watch for suspend and resume");
      return;
    };
    while let Some(Ok(message)) = stream.next().await {
      // `true` before suspending, `false` after resuming.
      if let Ok(is_going_to_sleep) = message.body().deserialize::<bool>()
        && !is_going_to_sleep
      {
        on_wake();
      }
    }
  }

  /// Calls `on_up` every time NetworkManager reports the network back up (`StateChanged` to
  /// connected), for as long as the system bus lives (issue #883: after a wake-up the network often
  /// comes back seconds later). Returns `false` at once when there is no NetworkManager, so the
  /// caller can fall back to probing.
  pub async fn watch_network_up(&self, on_up: impl Fn() + Send + 'static) -> bool {
    let Some(system) = self.system().await else {
      return false;
    };
    let Some(initial) = call::<OwnedValue, _>(
      system,
      NETWORK_MANAGER,
      NETWORK_MANAGER_PATH,
      "org.freedesktop.DBus.Properties",
      "Get",
      &(NETWORK_MANAGER, "State"),
    )
    .await
    .and_then(|value| u32::try_from(value).ok()) else {
      tracing::debug!("no NetworkManager; probing for the network instead");
      return false;
    };
    let rule = zbus::MatchRule::builder()
      .msg_type(zbus::message::Type::Signal)
      .sender(NETWORK_MANAGER)
      .and_then(|builder| builder.path(NETWORK_MANAGER_PATH))
      .and_then(|builder| builder.interface(NETWORK_MANAGER))
      .and_then(|builder| builder.member("StateChanged"))
      .map(|builder| builder.build());
    let Ok(rule) = rule else {
      return false;
    };
    let Ok(mut stream) = zbus::MessageStream::for_match_rule(rule, system, None).await else {
      tracing::debug!("cannot watch NetworkManager");
      return false;
    };
    // The bus daemon's match rule filters broadcasts by sender, but another local process could
    // send the signal to us directly: only NetworkManager's own connection counts.
    let Some(mut owner) = network_manager_owner(system).await else {
      return false;
    };
    let mut previous = initial;
    while let Some(Ok(message)) = stream.next().await {
      let header = message.header();
      let Some(sender) = header.sender().map(|sender| sender.as_str().to_owned()) else {
        continue;
      };
      if sender != owner {
        // NetworkManager may have restarted under a new connection name.
        match network_manager_owner(system).await {
          Some(current) if current == sender => owner = current,
          _ => continue,
        }
      }
      if let Ok(state) = message.body().deserialize::<u32>() {
        if is_network_back(previous, state) {
          on_up();
        }
        previous = state;
      }
    }
    true
  }

  /// Whether a panel shows StatusNotifier icons: the watcher runs and a host is registered with it.
  /// GNOME has neither without an extension, and an appindicator icon is then invisible.
  pub async fn has_status_notifier_host(&self) -> bool {
    let Some(session) = self.session().await else {
      return false;
    };
    call::<OwnedValue, _>(
      session,
      STATUS_NOTIFIER_WATCHER,
      STATUS_NOTIFIER_WATCHER_PATH,
      "org.freedesktop.DBus.Properties",
      "Get",
      &(STATUS_NOTIFIER_WATCHER, "IsStatusNotifierHostRegistered"),
    )
    .await
    .and_then(|value| bool::try_from(value).ok())
    .unwrap_or(false)
  }

  /// Calls `on_change` with `has_status_notifier_host` once subscribed and again whenever the
  /// watcher appears or goes away or a host registers or unregisters (a panel started after the
  /// app, an extension switched on), for as long as the session bus lives. Returns at once when
  /// there is no session bus.
  pub async fn watch_status_notifier_host(&self, on_change: impl Fn(bool) + Send + 'static) {
    let Some(session) = self.session().await else {
      return;
    };
    let owner_rule = zbus::MatchRule::builder()
      .msg_type(zbus::message::Type::Signal)
      .sender(BUS_DAEMON)
      .and_then(|builder| builder.interface(BUS_DAEMON))
      .and_then(|builder| builder.member("NameOwnerChanged"))
      .and_then(|builder| builder.arg(0, STATUS_NOTIFIER_WATCHER))
      .map(|builder| builder.build());
    let host_rule = zbus::MatchRule::builder()
      .msg_type(zbus::message::Type::Signal)
      .interface(STATUS_NOTIFIER_WATCHER)
      .map(|builder| builder.build());
    let (Ok(owner_rule), Ok(host_rule)) = (owner_rule, host_rule) else {
      return;
    };
    let (Ok(owners), Ok(hosts)) = (
      zbus::MessageStream::for_match_rule(owner_rule, session, None).await,
      zbus::MessageStream::for_match_rule(host_rule, session, None).await,
    ) else {
      tracing::debug!("cannot watch for a StatusNotifier host");
      return;
    };
    // Subscribed first, then asked: a host that registers in between is not missed.
    on_change(self.has_status_notifier_host().await);
    let mut changes = futures_util::stream::select(owners, hosts);
    while let Some(Ok(message)) = changes.next().await {
      // The watcher also reports every other app's icons; only hosts matter here.
      let header = message.header();
      let is_host_change = header.member().is_some_and(|member| {
        member.as_str() == "NameOwnerChanged" || STATUS_NOTIFIER_HOST_SIGNALS.contains(&member.as_str())
      });
      if is_host_change {
        on_change(self.has_status_notifier_host().await);
      }
    }
  }

  /// Shows `count` on the app's launcher icon in the task manager or dock (0 hides it).
  /// `desktop_entry` is the installed entry's file name (`TeamCraft.desktop`). Tauri's own badge
  /// needs `libunity` and a running Unity, which KDE Plasma and most distributions don't have.
  pub async fn set_launcher_count(&self, desktop_entry: &str, count: u32) {
    let Some(session) = self.session().await else {
      return;
    };
    let app_uri = launcher_app_uri(desktop_entry);
    let properties = launcher_properties(count);
    if let Err(error) = session
      .emit_signal(
        None::<&str>,
        LAUNCHER_ENTRY_PATH,
        LAUNCHER_ENTRY,
        "Update",
        &(app_uri.as_str(), properties),
      )
      .await
    {
      tracing::debug!(%error, "could not update the launcher badge");
    }
  }

  /// Closes a notification shown earlier (already closed ones are ignored by the server).
  pub async fn close(&self, id: u32) {
    if let Some(session) = self.session().await {
      // A notification the user closed already is no failure worth reporting.
      let _ = call::<(), _>(
        session,
        NOTIFICATIONS,
        NOTIFICATIONS_PATH,
        NOTIFICATIONS,
        "CloseNotification",
        &(id,),
      )
      .await;
    }
  }

  async fn has_capability(&self, capability: &str) -> bool {
    let capabilities = self
      .capabilities
      .get_or_init(|| async {
        match self.session().await {
          Some(session) => call::<Vec<String>, _>(
            session,
            NOTIFICATIONS,
            NOTIFICATIONS_PATH,
            NOTIFICATIONS,
            "GetCapabilities",
            &(),
          )
          .await
          .unwrap_or_default(),
          None => Vec::new(),
        }
      })
      .await;
    capabilities.iter().any(|known| known == capability)
  }

  async fn session(&self) -> Option<&Connection> {
    self
      .session
      .get_or_init(|| async {
        Connection::session()
          .await
          .inspect_err(|error| tracing::debug!(%error, "no D-Bus session bus"))
          .ok()
      })
      .await
      .as_ref()
  }

  async fn system(&self) -> Option<&Connection> {
    self
      .system
      .get_or_init(|| async {
        Connection::system()
          .await
          .inspect_err(|error| tracing::debug!(%error, "no D-Bus system bus"))
          .ok()
      })
      .await
      .as_ref()
  }
}

/// The `actions` list of `Notify`: key and label pairs. "default" (no button) reports a click on
/// the notification; "inline-reply" with the placeholder as label asks for a text field.
fn server_actions<'a>(notification: &DesktopNotification<'a>, has_inline_reply: bool) -> Vec<&'a str> {
  let mut actions = vec![DEFAULT_ACTION, ""];
  for (key, label) in notification.actions {
    actions.extend([*key, *label]);
  }
  if let (true, Some(placeholder)) = (has_inline_reply, notification.maybe_reply_placeholder) {
    actions.extend([INLINE_REPLY, placeholder]);
  }
  actions
}

/// The `app_uri` of a LauncherEntry update.
/// The `image-data` hint: `(iiibiiay)` width, height, row stride, alpha, bits per sample, channels
/// and the pixels. `None` for a picture whose pixels don't match its side.
fn image_data(image: HintImage<'_>) -> Option<Value<'static>> {
  let side = i32::try_from(image.side).ok()?;
  let row_stride = side.checked_mul(IMAGE_CHANNELS)?;
  let expected = usize::try_from(row_stride)
    .ok()?
    .checked_mul(usize::try_from(side).ok()?)?;
  if image.rgba.len() != expected || side == 0 {
    return None;
  }
  let structure = Structure::from((
    side,
    side,
    row_stride,
    true,
    IMAGE_BITS_PER_SAMPLE,
    IMAGE_CHANNELS,
    image.rgba.to_vec(),
  ));
  Some(Value::from(structure))
}

fn launcher_app_uri(desktop_entry: &str) -> String {
  format!("application://{desktop_entry}")
}

/// The properties of a LauncherEntry update: the count, shown only when there is one.
fn launcher_properties(count: u32) -> HashMap<&'static str, Value<'static>> {
  HashMap::from([
    ("count", Value::from(i64::from(count))),
    ("count-visible", Value::from(count > 0)),
  ])
}

/// GNOME's do-not-disturb: `show-banners` off in `org.gnome.desktop.notifications`. Other desktops
/// don't have the schema.
fn gnome_hides_banners() -> bool {
  use gtk::gio;
  use gtk::gio::prelude::SettingsExt;

  const SCHEMA: &str = "org.gnome.desktop.notifications";
  let has_schema = gio::SettingsSchemaSource::default()
    .and_then(|source| source.lookup(SCHEMA, true))
    .is_some_and(|schema| schema.has_key("show-banners"));
  has_schema && !gio::Settings::new(SCHEMA).boolean("show-banners")
}

/// GNOME (Mutter): milliseconds since the last input, on X11 and Wayland.
async fn mutter_idle(session: &Connection) -> Option<Duration> {
  call::<u64, _>(
    session,
    "org.gnome.Mutter.IdleMonitor",
    "/org/gnome/Mutter/IdleMonitor/Core",
    "org.gnome.Mutter.IdleMonitor",
    "GetIdletime",
    &(),
  )
  .await
  .map(Duration::from_millis)
}

/// KDE and others: seconds the session has been idle.
async fn screensaver_idle(session: &Connection) -> Option<Duration> {
  call::<u32, _>(
    session,
    "org.freedesktop.ScreenSaver",
    "/org/freedesktop/ScreenSaver",
    "org.freedesktop.ScreenSaver",
    "GetSessionIdleTime",
    &(),
  )
  .await
  .map(|seconds| Duration::from_secs(u64::from(seconds)))
}

async fn screensaver_active(session: &Connection, service: &str, path: &str) -> bool {
  call::<bool, _>(session, service, path, service, "GetActive", &())
    .await
    .unwrap_or(false)
}

/// logind's `LockedHint` of the session this process runs in.
async fn logind_locked_hint(system: &Connection) -> bool {
  call::<OwnedValue, _>(
    system,
    "org.freedesktop.login1",
    "/org/freedesktop/login1/session/auto",
    "org.freedesktop.DBus.Properties",
    "Get",
    &("org.freedesktop.login1.Session", "LockedHint"),
  )
  .await
  .and_then(|value| bool::try_from(value).ok())
  .unwrap_or(false)
}

/// One method call with a timeout; any failure (service missing, refused, wrong type) is `None`.
/// The unique bus name NetworkManager holds right now.
async fn network_manager_owner(system: &Connection) -> Option<String> {
  call::<String, _>(
    system,
    BUS_DAEMON,
    "/org/freedesktop/DBus",
    BUS_DAEMON,
    "GetNameOwner",
    &(NETWORK_MANAGER,),
  )
  .await
}

/// A NetworkManager state change that brings the network (back) up: from below connected to
/// connected, or from site-only to global connectivity (the captive-portal check passed).
fn is_network_back(previous: u32, state: u32) -> bool {
  state >= NM_STATE_CONNECTED_SITE && state > previous
}

async fn call<T, B>(
  connection: &Connection,
  service: &str,
  path: &str,
  interface: &str,
  method: &str,
  body: &B,
) -> Option<T>
where
  T: for<'de> serde::Deserialize<'de> + zbus::zvariant::Type,
  B: serde::Serialize + zbus::zvariant::DynamicType,
{
  let reply = tokio::time::timeout(
    CALL_TIMEOUT,
    connection.call_method(Some(service), path, Some(interface), method, body),
  )
  .await
  .ok()?
  .inspect_err(|error| tracing::trace!(service, method, %error, "D-Bus call failed"))
  .ok()?;
  reply.body().deserialize::<T>().ok()
}

#[cfg(test)]
mod tests {
  use super::*;

  /// NetworkManager states: 20 disconnected, 40 connecting, 50 local, 60 site, 70 global.
  #[test]
  fn only_a_state_that_brings_the_network_up_counts() {
    assert!(is_network_back(20, 70));
    assert!(is_network_back(40, 60));
    assert!(is_network_back(60, 70), "the connectivity check passed");
    assert!(!is_network_back(20, 40), "still connecting");
    assert!(!is_network_back(70, 70));
    assert!(!is_network_back(70, 60), "lost global connectivity");
    assert!(!is_network_back(70, 20));
  }

  #[test]
  fn buttons_follow_the_click_action_and_the_reply_field_needs_the_capability() {
    let buttons = [("mark-read", "Mark as read")];
    let notification = notification(&buttons, Some("Reply…"));
    assert_eq!(
      server_actions(&notification, false),
      ["default", "", "mark-read", "Mark as read"]
    );
    assert_eq!(
      server_actions(&notification, true),
      [
        "default",
        "",
        "mark-read",
        "Mark as read",
        "inline-reply",
        "Reply…"
      ]
    );
    assert_eq!(
      server_actions(&self::notification(&[], None), true),
      ["default", ""]
    );
  }

  #[test]
  fn a_picture_travels_as_the_image_data_hint() {
    let rgba = vec![7_u8; 2 * 2 * 4];
    let value = image_data(HintImage { side: 2, rgba: &rgba }).expect("a hint");
    assert_eq!(value.value_signature().to_string(), "(iiibiiay)");
    let Value::Structure(structure) = value else {
      panic!("a structure");
    };
    let fields = structure.fields();
    assert_eq!(fields[0], Value::from(2_i32), "width");
    assert_eq!(fields[1], Value::from(2_i32), "height");
    assert_eq!(fields[2], Value::from(8_i32), "row stride");
    assert_eq!(fields[3], Value::from(true), "alpha");
    assert_eq!(fields[4], Value::from(8_i32), "bits per sample");
    assert_eq!(fields[5], Value::from(4_i32), "channels");
    assert_eq!(fields[6], Value::from(rgba.clone()), "pixels");
    assert!(
      image_data(HintImage { side: 3, rgba: &rgba }).is_none(),
      "too few pixels"
    );
    assert!(image_data(HintImage { side: 0, rgba: &[] }).is_none(), "nothing");
  }

  #[test]
  fn the_launcher_badge_names_the_desktop_entry_and_hides_at_zero() {
    assert_eq!(
      launcher_app_uri("TeamCraft.desktop"),
      "application://TeamCraft.desktop"
    );
    let shown = launcher_properties(3);
    assert_eq!(shown["count"], Value::from(3_i64));
    assert_eq!(shown["count-visible"], Value::from(true));
    assert_eq!(launcher_properties(0)["count-visible"], Value::from(false));
  }

  /// Against the session's real notification server: `dbus-run-session` with a server running (CI
  /// has none). `cargo test -p teamcraft-desktop -- --ignored freedesktop`
  #[tokio::test]
  #[ignore = "needs a D-Bus session with a notification server"]
  async fn a_live_server_shows_replaces_and_closes() {
    let buses = Buses::default();
    let buttons = [("accept-audio", "Accept"), ("decline", "Decline")];
    let mut call = notification(&buttons, None);
    call.title = "Incoming call";
    call.body = "Bob is calling you";
    call.category = "call.incoming";
    call.is_ringing = true;
    call.is_quiet = true;
    let id = buses.notify(&call).await.expect("a notification server answers");
    call.replaces_id = id;
    call.body = "Bob is still calling you";
    assert_eq!(buses.notify(&call).await, Some(id), "replaced in place");
    let message = notification(&[("mark-read", "Mark as read")], Some("Reply…"));
    let other = buses.notify(&message).await.expect("a second notification");
    assert_ne!(other, id);
    if std::env::var_os("TEAMCRAFT_KEEP_NOTIFICATIONS").is_none() {
      buses.close(id).await;
      buses.close(other).await;
    }
  }

  fn notification<'a>(
    actions: &'a [(&'a str, &'a str)],
    maybe_reply_placeholder: Option<&'a str>,
  ) -> DesktopNotification<'a> {
    DesktopNotification {
      app_name: "TeamCraft",
      app_icon: "teamcraft-desktop",
      maybe_desktop_entry: None,
      title: "Bob",
      body: "hello",
      replaces_id: 0,
      actions,
      maybe_reply_placeholder,
      category: "im.received",
      is_ringing: false,
      is_quiet: false,
      maybe_image: None,
    }
  }
}
