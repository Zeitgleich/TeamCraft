//! Linux desktop services over D-Bus: idle time and screen lock for attention (notifications.md,
//! "Attention"), and notifications that can be replaced and withdrawn (the notification plugin can
//! neither). Every probe falls back quietly: desktops implement different subsets.

use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::OnceCell;
use zbus::Connection;
use zbus::zvariant::{OwnedValue, Value};

/// A D-Bus service that does not answer within this time counts as missing.
const CALL_TIMEOUT: Duration = Duration::from_secs(1);
/// The notification server picks how long a notification stays.
const DEFAULT_EXPIRY: i32 = -1;

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";

/// Session and system bus, connected on first use.
#[derive(Default)]
pub struct Buses {
  session: OnceCell<Option<Connection>>,
  system: OnceCell<Option<Connection>>,
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
  /// The `.desktop` entry of the app, so the server shows its name and icon.
  pub desktop_entry: &'a str,
  pub title: &'a str,
  pub body: &'a str,
  /// The notification this one replaces (0: none).
  pub replaces_id: u32,
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
    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    hints.insert("desktop-entry", Value::from(notification.desktop_entry));
    let actions: Vec<&str> = Vec::new();
    let body = (
      notification.app_name,
      notification.replaces_id,
      notification.app_icon,
      notification.title,
      notification.body,
      actions,
      hints,
      DEFAULT_EXPIRY,
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
