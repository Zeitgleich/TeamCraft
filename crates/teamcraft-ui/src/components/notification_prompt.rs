//! Turning on notifications in the browser (issue #532): a banner after sign-in while the browser
//! has not been asked yet, and the allow flow it shares with Settings → Notifications → "This
//! device". Browsers (Safari above all) only show their permission prompt from a click, so the
//! request always runs from a button, never on load. The desktop app has OS notifications and never
//! shows the banner.

use leptos::prelude::*;

use crate::auth::tauri;
use crate::clock::now_ms;
use crate::components::icons::{Icon, IconName};
use crate::components::pending_button::PendingButton;
use crate::feedback::{ActionFailure, FailureKind, PendingAction};
use crate::i18n::use_i18n;
use crate::model::UserId;
use crate::reactive::SetIfAlive;
use crate::session::SessionControl;
use crate::settings::{load_string, store_string};
use crate::state::use_app_state;

/// Storage key prefix of "this browser is subscribed to Web Push"; the user ID follows.
const PUSH_KEY_PREFIX: &str = "teamcraft.web_push.";
/// Storage key prefix of when the user last said "Not now"; the user ID follows.
const SNOOZED_KEY_PREFIX: &str = "teamcraft.notification_prompt.";
/// How long "Not now" hides the banner before it asks once more.
const SNOOZE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Asks above the app to turn on notifications, while this browser's permission is undecided and
/// the user has not said "Not now" in the last [`SNOOZE_MS`].
#[component]
pub fn NotificationPrompt() -> impl IntoView {
  let i18n = use_i18n();
  let me = use_app_state().me().id;
  // Read here, inside the reactive owner: the spawned future has no context to find it in.
  let maybe_session = crate::session::use_session();
  let snoozed_key = format!("{SNOOZED_KEY_PREFIX}{me}");
  let maybe_snoozed_at = load_string(&snoozed_key).and_then(|at| at.parse().ok());
  let is_shown = RwSignal::new(
    !tauri::is_available()
      && is_offered(
        tauri::notification_permission().as_deref(),
        maybe_snoozed_at,
        now_ms(),
      ),
  );
  let allowing = PendingAction::new();
  let allow = move |_| {
    allowing.spawn(async move {
      let result = allow_notifications(me, maybe_session).await;
      // Whatever the browser answered, the question is settled: granted, or denied (Settings
      // explains how to unblock it).
      if tauri::notification_permission().as_deref() != Some("default") {
        is_shown.set_if_alive(false);
      }
      result.map(|_| ())
    });
  };
  let later = move |_| {
    store_string(&snoozed_key, &now_ms().to_string());
    is_shown.set(false);
  };
  move || {
    is_shown.get().then(|| {
      view! {
        <div class="update-banner" role="status">
          <Icon name=IconName::Bell />
          <span>{move || i18n.t("notif-prompt-text")}</span>
          <PendingButton pending=allowing class="btn btn-primary btn-small" on_click=allow>
            {move || i18n.t("notif-prompt-allow")}
          </PendingButton>
          <button class="btn btn-small" on:click=later.clone()>
            {move || i18n.t("notif-prompt-later")}
          </button>
        </div>
      }
    })
  }
}

/// Asks the browser for permission (from a click) where it wasn't granted yet, then subscribes this
/// browser to Web Push, so notifications also arrive with no page open. Returns whether it is
/// subscribed now; `false` when the user said no in the browser's prompt. Callers read the new
/// permission from [`tauri::notification_permission`].
pub async fn allow_notifications(
  me: UserId,
  maybe_session: Option<SessionControl>,
) -> Result<bool, ActionFailure> {
  if tauri::notification_permission().as_deref() != Some("granted") {
    tauri::request_notification_permission().await;
  }
  if tauri::notification_permission().as_deref() != Some("granted") {
    return Ok(false);
  }
  let Some(session) = maybe_session else {
    return Err(ActionFailure::new(
      FailureKind::Other,
      "no session to subscribe to push with",
    ));
  };
  crate::remote_backend::enable_web_push(session.authenticator().as_ref())
    .await
    .map_err(|error| ActionFailure::new(FailureKind::Other, error))?;
  store_string(&push_key(me), "1");
  Ok(true)
}

/// Whether this browser is subscribed to Web Push for `me`.
pub fn is_push_on(me: UserId) -> bool {
  load_string(&push_key(me)).is_some()
}

fn push_key(me: UserId) -> String {
  format!("{PUSH_KEY_PREFIX}{me}")
}

/// The banner asks only while the browser's answer is open (`default`: never when granted, never
/// when denied, not where there are no notifications) and "Not now" is older than [`SNOOZE_MS`].
fn is_offered(maybe_permission: Option<&str>, maybe_snoozed_at: Option<i64>, now: i64) -> bool {
  maybe_permission == Some("default")
    && maybe_snoozed_at.is_none_or(|snoozed_at| now.saturating_sub(snoozed_at) >= SNOOZE_MS)
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::i18n::{Locale, translate};

  const NOW: i64 = 1_792_540_800_000;

  #[test]
  fn asks_only_while_the_browser_has_not_been_answered() {
    assert!(is_offered(Some("default"), None, NOW));
    assert!(!is_offered(Some("granted"), None, NOW));
    assert!(!is_offered(Some("denied"), None, NOW));
    assert!(!is_offered(None, None, NOW), "no notifications in this browser");
  }

  #[test]
  fn not_now_waits_a_week_before_asking_again() {
    assert!(!is_offered(Some("default"), Some(NOW - SNOOZE_MS + 1), NOW));
    assert!(is_offered(Some("default"), Some(NOW - SNOOZE_MS), NOW));
    assert!(
      !is_offered(Some("default"), Some(NOW + 60_000), NOW),
      "a snooze from a clock that was ahead still hides it"
    );
  }

  #[test]
  fn the_banner_is_translated() {
    for locale in [Locale::En, Locale::De] {
      for key in ["notif-prompt-text", "notif-prompt-allow", "notif-prompt-later"] {
        assert_ne!(translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }
}
