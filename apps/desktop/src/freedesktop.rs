//! Linux desktop services over D-Bus: idle time and screen lock for attention (notifications.md,
//! "Attention"), and notifications that can be replaced and withdrawn, with buttons and, where the
//! server offers it, an inline reply (the notification plugin can do none of it). Every probe falls
//! back quietly: desktops implement different subsets.

use std::collections::HashMap;
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::OnceCell;
use zbus::Connection;
use zbus::zvariant::{OwnedValue, Value};

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

const LOGIND: &str = "org.freedesktop.login1";
const LOGIND_MANAGER: &str = "org.freedesktop.login1.Manager";

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
  /// The `.desktop` entry of the app, so the server shows its name and icon.
  pub desktop_entry: &'a str,
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
  /// A ringing call: critical urgency (stays until withdrawn), no server sound (the app rings).
  pub is_ringing: bool,
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
    hints.insert("desktop-entry", Value::from(notification.desktop_entry));
    hints.insert("category", Value::from(notification.category));
    let urgency = if notification.is_ringing {
      URGENCY_CRITICAL
    } else {
      URGENCY_NORMAL
    };
    hints.insert("urgency", Value::from(urgency));
    if notification.is_ringing {
      hints.insert("suppress-sound", Value::from(true));
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

  /// Calls `on_action` for every click, button and inline reply on a notification, for as long as
  /// the session bus lives. Returns at once when there is no session bus.
  pub async fn watch_actions(&self, on_action: impl Fn(ServerAction) + Send + 'static) {
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
      tracing::debug!("cannot watch notification actions");
      return;
    };
    while let Some(Ok(message)) = stream.next().await {
      let header = message.header();
      let Some(member) = header.member() else {
        continue;
      };
      let Ok((id, value)) = message.body().deserialize::<(u32, String)>() else {
        continue;
      };
      let action = match member.as_str() {
        "ActionInvoked" => ServerAction {
          id,
          key: value,
          maybe_text: None,
        },
        "NotificationReplied" => ServerAction {
          id,
          key: INLINE_REPLY.to_owned(),
          maybe_text: Some(value),
        },
        _ => continue,
      };
      on_action(action);
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

  fn notification<'a>(
    actions: &'a [(&'a str, &'a str)],
    maybe_reply_placeholder: Option<&'a str>,
  ) -> DesktopNotification<'a> {
    DesktopNotification {
      app_name: "TeamCraft",
      app_icon: "teamcraft-desktop",
      desktop_entry: "de.zeitgleich.teamcraft",
      title: "Bob",
      body: "hello",
      replaces_id: 0,
      actions,
      maybe_reply_placeholder,
      category: "im.received",
      is_ringing: false,
    }
  }
}
