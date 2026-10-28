//! Turning on notifications in the browser (issue #532): a banner after sign-in while the browser
//! has not been asked yet, and the allow flow it shares with Settings → Notifications → "This
//! device". Browsers (Safari above all) only show their permission prompt from a click, so the
//! request always runs from a button, never on load, and is started inside the click handler itself
//! (before any `await`), or Safari and Chrome refuse it without a prompt (#893). The banner always
//! says what came of it. The desktop app has OS notifications and never shows the banner.

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
/// How long the confirmation "Notifications are on" stays before the banner goes.
const CONFIRMATION_MS: u64 = 4_000;

/// What came of the user's click on "Turn on".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllowOutcome {
  /// Allowed in the browser and subscribed to Web Push.
  On,
  /// The browser says no (the user declined, or blocked notifications for this site before).
  Blocked,
  /// The browser showed no prompt and the question is still open (a quieter permission UI behind
  /// the address bar's bell, or a browser that blocks prompts).
  NoPrompt,
}

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
  // What the last click came to, shown in the banner in place of the question.
  let maybe_outcome = RwSignal::new(None::<AllowOutcome>);
  let allowing = PendingAction::new();
  let allow = move |_| {
    // Started here, inside the click: the browser only prompts for a request made in this task.
    let request = tauri::request_notification_permission();
    allowing.spawn(async move {
      // A failed subscription is reported (toast); the banner stays so the user can try again.
      let outcome = allow_notifications(me, maybe_session, request).await?;
      maybe_outcome.set_if_alive(Some(outcome));
      if outcome == AllowOutcome::On {
        set_timeout(
          move || {
            let _ = is_shown.try_set(false);
          },
          std::time::Duration::from_millis(CONFIRMATION_MS),
        );
      }
      Ok(())
    });
  };
  let later = move |_| {
    store_string(&snoozed_key, &now_ms().to_string());
    is_shown.set(false);
  };
  let close = move |_| is_shown.set(false);
  move || {
    is_shown.get().then(|| {
      let outcome = maybe_outcome.get();
      let text = move || i18n.t(outcome_text_key(outcome));
      // The question stays (to try again) unless the browser said no or it worked.
      let asks_again = matches!(outcome, None | Some(AllowOutcome::NoPrompt));
      view! {
        <div class="update-banner" role="status">
          <Icon name=IconName::Bell />
          <span>{text}</span>
          {asks_again.then(|| view! {
            <PendingButton pending=allowing class="btn btn-primary btn-small" on_click=allow>
              {move || i18n.t("notif-prompt-allow")}
            </PendingButton>
            <button class="btn btn-small" on:click=later.clone()>
              {move || i18n.t("notif-prompt-later")}
            </button>
          })}
          {(outcome == Some(AllowOutcome::Blocked)).then(|| view! {
            <button class="btn btn-small" on:click=close>
              {move || i18n.t("notif-prompt-close")}
            </button>
          })}
        </div>
      }
    })
  }
}

/// The banner's text: the question, or what came of the click.
fn outcome_text_key(maybe_outcome: Option<AllowOutcome>) -> &'static str {
  match maybe_outcome {
    None => "notif-prompt-text",
    Some(AllowOutcome::On) => "notif-prompt-on",
    Some(AllowOutcome::Blocked) => "notif-prompt-blocked",
    Some(AllowOutcome::NoPrompt) => "notif-prompt-no-prompt",
  }
}

/// Waits for the browser's answer to `request` (started by the caller inside its click handler),
/// then subscribes this browser to Web Push where it was allowed, so notifications also arrive
/// with no page open. Says what came of it; callers read the new permission from
/// [`tauri::notification_permission`].
pub async fn allow_notifications(
  me: UserId,
  maybe_session: Option<SessionControl>,
  request: tauri::NotificationPermissionRequest,
) -> Result<AllowOutcome, ActionFailure> {
  let maybe_permission = request.answer().await;
  if let Some(outcome) = permission_outcome(maybe_permission.as_deref()) {
    return Ok(outcome);
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
  Ok(AllowOutcome::On)
}

/// What the browser's permission says about a click: `None` where it is granted and the push
/// subscription is still to do.
fn permission_outcome(maybe_permission: Option<&str>) -> Option<AllowOutcome> {
  match maybe_permission {
    Some("granted") => None,
    Some("default") => Some(AllowOutcome::NoPrompt),
    // Denied, or a browser without notifications (then there is nothing to ask for).
    _ => Some(AllowOutcome::Blocked),
  }
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
  fn every_answer_of_the_browser_has_an_outcome_the_user_is_told() {
    assert_eq!(
      permission_outcome(Some("granted")),
      None,
      "subscribing is still to do"
    );
    assert_eq!(permission_outcome(Some("denied")), Some(AllowOutcome::Blocked));
    assert_eq!(
      permission_outcome(Some("default")),
      Some(AllowOutcome::NoPrompt),
      "no prompt was shown"
    );
    assert_eq!(permission_outcome(None), Some(AllowOutcome::Blocked));
    let texts = [
      None,
      Some(AllowOutcome::On),
      Some(AllowOutcome::Blocked),
      Some(AllowOutcome::NoPrompt),
    ]
    .map(outcome_text_key);
    assert_eq!(
      texts.len(),
      texts.iter().collect::<std::collections::HashSet<_>>().len()
    );
  }

  #[test]
  fn the_banner_is_translated() {
    for locale in [Locale::En, Locale::De] {
      for key in [
        "notif-prompt-text",
        "notif-prompt-allow",
        "notif-prompt-later",
        "notif-prompt-on",
        "notif-prompt-blocked",
        "notif-prompt-no-prompt",
        "notif-prompt-close",
      ] {
        assert_ne!(translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }
}
