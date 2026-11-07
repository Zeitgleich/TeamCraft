//! Turning on notifications in the desktop app (#895), as the web app does in the browser
//! ([`super::notification_prompt`]): a banner after sign-in, and the state of "This device" in
//! Settings > Notifications. The shell reads the system's answer (`notification_state`); "Turn on"
//! does the platform's step from the click: macOS shows its permission prompt
//! (`notification_allow`), Windows and Linux, which have none, show a test notification so the
//! user sees how and where they appear. The banner and the settings say what came of it, with
//! where the system's settings are and the app's name there. The state is read again whenever the
//! window comes to the front, so a change in the system's settings shows.

use std::future::Future;

use leptos::prelude::*;
use teamcraft_client_api::os_notify::{
  DesktopNotifyState, NotificationTag, NotifyPermission, NotifyPlatform, OsNotification, OsNotificationKind,
};

use crate::auth::tauri;
use crate::call_alerts::may_name_names;
use crate::clock::now_ms;
use crate::components::icons::{Icon, IconName};
use crate::components::notification_prompt::{SNOOZED_KEY_PREFIX, is_snoozed};
use crate::components::pending_button::PendingButton;
use crate::feedback::{ActionFailure, FailureKind, PendingAction};
use crate::i18n::{I18n, use_i18n};
use crate::model::UserId;
use crate::notification_pictures::picture_of;
use crate::notifier::PictureSubject;
use crate::reactive::{SetIfAlive, is_document_visible};
use crate::settings::{load_string, store_string};
use crate::state::{AppState, use_app_state};

/// The tag of the test notification; a newer test replaces the last one.
pub const TEST_TAG: &str = "settings-test";
/// Storage key prefix of "notifications were turned on here" (Windows, Linux: the test
/// notification went out); the user ID follows.
const CONFIRMED_KEY_PREFIX: &str = "teamcraft.desktop_notify.";
/// How long "Notifications are on" stays before the banner goes.
const CONFIRMATION_MS: u64 = 4_000;
/// How long the test's hint (where to look if it didn't show) stays: it is longer to read.
const TEST_HINT_MS: u64 = 10_000;

/// What came of a click on "Turn on".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopOutcome {
  /// macOS: the user allowed notifications.
  On,
  /// Windows, Linux: the system takes notifications and a test notification went out.
  TestShown,
  /// Turned off in the system's settings (on macOS: the user declined now or before).
  Blocked,
  /// No notifications on this system.
  Unavailable,
}

/// The platform's side of "Turn on", injected so the logic can be tested without a shell.
pub trait NotifyAdapter {
  /// Asks for permission where the system has a prompt; the state after it.
  fn allow(&self) -> impl Future<Output = Result<DesktopNotifyState, ActionFailure>>;
  /// Shows the test notification.
  fn show_test(&self) -> impl Future<Output = Result<(), ActionFailure>>;
}

/// The shell: `notification_allow`, and the test notification through `notify_user`.
struct ShellAdapter {
  test: OsNotification,
}

impl NotifyAdapter for ShellAdapter {
  fn allow(&self) -> impl Future<Output = Result<DesktopNotifyState, ActionFailure>> {
    tauri::desktop_notify_allow()
  }

  fn show_test(&self) -> impl Future<Output = Result<(), ActionFailure>> {
    tauri::try_show_notification(&self.test)
  }
}

/// Does the platform's step of "Turn on" and says what came of it, with the state after it. A
/// failure (the system did not answer, the test notification could not be shown) is an `Err`: the
/// caller reports it and the question stays.
pub async fn turn_on(
  adapter: &impl NotifyAdapter,
) -> Result<(DesktopOutcome, DesktopNotifyState), ActionFailure> {
  let state = adapter.allow().await?;
  let outcome = match (state.permission, state.platform) {
    (NotifyPermission::Blocked, _) => DesktopOutcome::Blocked,
    (NotifyPermission::Unavailable, _) => DesktopOutcome::Unavailable,
    (NotifyPermission::Allowed, NotifyPlatform::MacOs) => DesktopOutcome::On,
    (NotifyPermission::NotAsked, _) => {
      return Err(ActionFailure::new(
        FailureKind::Other,
        "the system did not answer the permission request",
      ));
    },
    // Windows and Linux have no prompt (and the system's answer may be unknown): the test shows
    // whether and where notifications appear.
    (NotifyPermission::Allowed | NotifyPermission::Unknown, _) => {
      adapter.show_test().await?;
      DesktopOutcome::TestShown
    },
  };
  Ok((outcome, state))
}

/// The banner asks after sign-in while turning notifications on is still to do: on macOS while
/// the system has not asked; on Windows and Linux (no prompt) until the user turned them on here
/// with the test notification. Never where they are blocked (Settings says where to change it) or
/// missing, and not within a week of "Not now".
pub fn is_offered(
  state: &DesktopNotifyState,
  is_confirmed: bool,
  maybe_snoozed_at: Option<i64>,
  now: i64,
) -> bool {
  let is_open = match state.platform {
    NotifyPlatform::MacOs => state.permission == NotifyPermission::NotAsked,
    _ => state.permission == NotifyPermission::Allowed && !is_confirmed,
  };
  is_open && !is_snoozed(maybe_snoozed_at, now)
}

/// Settings > Notifications > "This device": what the system says. `None` while it is read (or
/// where the shell can't say).
pub fn device_status_key(maybe_state: Option<&DesktopNotifyState>) -> &'static str {
  let Some(state) = maybe_state else {
    return "notif-device-desktop";
  };
  match state.permission {
    NotifyPermission::Allowed | NotifyPermission::Unknown => "notif-desktop-allowed",
    NotifyPermission::Blocked => blocked_key(state.platform),
    NotifyPermission::NotAsked => "notif-desktop-not-asked",
    NotifyPermission::Unavailable => "notif-desktop-unavailable",
  }
}

/// The banner's text after a click.
pub fn outcome_key(outcome: DesktopOutcome, platform: NotifyPlatform) -> &'static str {
  match outcome {
    DesktopOutcome::On => "notif-prompt-on",
    DesktopOutcome::TestShown if platform == NotifyPlatform::Windows => "notif-desktop-test-shown-windows",
    DesktopOutcome::TestShown => "notif-desktop-test-shown-linux",
    DesktopOutcome::Blocked => blocked_key(platform),
    DesktopOutcome::Unavailable => "notif-desktop-unavailable",
  }
}

/// Where the system's notification settings are, with the app's name there.
fn blocked_key(platform: NotifyPlatform) -> &'static str {
  match platform {
    NotifyPlatform::MacOs => "notif-desktop-blocked-mac",
    NotifyPlatform::Windows => "notif-desktop-blocked-windows",
    NotifyPlatform::Linux | NotifyPlatform::Other => "notif-desktop-blocked-linux",
  }
}

/// Whether the test may go out: the system allows notifications, or can't say.
pub fn can_test(maybe_state: Option<&DesktopNotifyState>) -> bool {
  maybe_state.is_none_or(|state| {
    matches!(
      state.permission,
      NotifyPermission::Allowed | NotifyPermission::Unknown
    )
  })
}

/// Whether "Turn on" in Settings has something to do: macOS has not asked yet.
pub fn can_turn_on(maybe_state: Option<&DesktopNotifyState>) -> bool {
  maybe_state.is_some_and(|state| state.permission == NotifyPermission::NotAsked)
}

/// What the system says about the app's notifications, read when the component mounts and again
/// whenever the window comes to the front. Stays `None` in the browser.
pub fn use_desktop_notify_state() -> RwSignal<Option<DesktopNotifyState>> {
  let state = RwSignal::new(None);
  if !tauri::is_available() {
    return state;
  }
  let refresh = move || {
    leptos::task::spawn_local(async move {
      let maybe_state = tauri::desktop_notify_state().await;
      state.set_if_alive(maybe_state);
    });
  };
  refresh();
  let on_focus = window_event_listener_untyped("focus", move |_| refresh());
  // `visibilitychange` bubbles from the document to the window: back from the tray or the Dock.
  let on_shown = window_event_listener_untyped("visibilitychange", move |_| {
    if is_document_visible() {
      refresh();
    }
  });
  on_cleanup(move || {
    on_focus.remove();
    on_shown.remove();
  });
  state
}

/// The test notification: the user's own picture, as a direct message shows its sender's (#543),
/// where names may show.
pub fn test_notification(state: AppState, i18n: I18n) -> OsNotification {
  let maybe_picture = state.read(|backend| {
    may_name_names(&backend.push_content_mode())
      .then(|| picture_of(backend, PictureSubject::Person(backend.me().id)))
      .flatten()
  });
  OsNotification {
    tag: NotificationTag::reminder(TEST_TAG),
    kind: OsNotificationKind::Reminder,
    title: i18n.t("notif-test-title"),
    maybe_body: Some(i18n.t("notif-test-body")),
    actions: Vec::new(),
    maybe_reply: None,
    maybe_picture,
    sound: Default::default(),
  }
}

/// Runs "Turn on" from a click: the platform's step, then remembers that it was done here.
pub async fn turn_on_from_click(
  me: UserId,
  test: OsNotification,
) -> Result<(DesktopOutcome, DesktopNotifyState), ActionFailure> {
  let result = turn_on(&ShellAdapter { test }).await?;
  if matches!(result.0, DesktopOutcome::On | DesktopOutcome::TestShown) {
    store_string(&confirmed_key(me), "1");
  }
  Ok(result)
}

fn confirmed_key(me: UserId) -> String {
  format!("{CONFIRMED_KEY_PREFIX}{me}")
}

/// The text of a key that may name the app.
pub fn text_with_app(i18n: I18n, key: &str, maybe_state: Option<&DesktopNotifyState>) -> String {
  let app = maybe_state.map_or("TeamCraft", |state| state.app_name.as_str());
  i18n.t_with(key, &[("app", app)])
}

/// The banner of the desktop app: "Turn on" / "Not now", then what came of it.
#[component]
pub fn DesktopNotificationPrompt() -> impl IntoView {
  let i18n = use_i18n();
  let app_state = use_app_state();
  let me = app_state.me().id;
  let device = use_desktop_notify_state();
  let snoozed_key = format!("{SNOOZED_KEY_PREFIX}{me}");
  let maybe_snoozed_at = RwSignal::new(load_string(&snoozed_key).and_then(|at| at.parse::<i64>().ok()));
  let is_confirmed = RwSignal::new(load_string(&confirmed_key(me)).is_some());
  let is_closed = RwSignal::new(false);
  let maybe_outcome = RwSignal::new(None::<DesktopOutcome>);
  let allowing = PendingAction::new();

  let is_shown = move || {
    !is_closed.get()
      && (maybe_outcome.get().is_some()
        || device
          .get()
          .is_some_and(|state| is_offered(&state, is_confirmed.get(), maybe_snoozed_at.get(), now_ms())))
  };
  let allow = move |_| {
    // Built here, inside the reactive owner; macOS's prompt comes from this click.
    let test = test_notification(app_state, i18n);
    allowing.spawn(async move {
      // A failure is reported (toast); the banner stays so the user can try again.
      let (outcome, state) = turn_on_from_click(me, test).await?;
      device.set_if_alive(Some(state));
      maybe_outcome.set_if_alive(Some(outcome));
      let maybe_linger = match outcome {
        DesktopOutcome::On => Some(CONFIRMATION_MS),
        DesktopOutcome::TestShown => Some(TEST_HINT_MS),
        DesktopOutcome::Blocked | DesktopOutcome::Unavailable => None,
      };
      if let Some(linger) = maybe_linger {
        is_confirmed.set_if_alive(true);
        set_timeout(
          move || {
            let _ = is_closed.try_set(true);
          },
          std::time::Duration::from_millis(linger),
        );
      }
      Ok(())
    });
  };
  let later = move |_| {
    let now = now_ms();
    store_string(&snoozed_key, &now.to_string());
    maybe_snoozed_at.set(Some(now));
  };
  let close = move |_| is_closed.set(true);

  move || {
    is_shown().then(|| {
      let outcome = maybe_outcome.get();
      let text = move || {
        let state = device.get();
        match outcome {
          None => i18n.t("notif-desktop-prompt-text"),
          Some(outcome) => {
            let platform = state
              .as_ref()
              .map_or(NotifyPlatform::Other, |state| state.platform);
            text_with_app(i18n, outcome_key(outcome, platform), state.as_ref())
          },
        }
      };
      view! {
        <div class="update-banner" role="status">
          <Icon name=IconName::Bell />
          <span>{text}</span>
          {outcome.is_none().then(|| view! {
            <PendingButton pending=allowing class="btn btn-primary btn-small" on_click=allow>
              {move || i18n.t("notif-prompt-allow")}
            </PendingButton>
            <button class="btn btn-small" on:click=later.clone()>
              {move || i18n.t("notif-prompt-later")}
            </button>
          })}
          {outcome.is_some().then(|| view! {
            <button class="btn btn-small" on:click=close>
              {move || i18n.t("notif-prompt-close")}
            </button>
          })}
        </div>
      }
    })
  }
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;
  use std::pin::pin;
  use std::task::{Context, Poll, Waker};

  use super::*;
  use crate::components::notification_prompt::SNOOZE_MS;
  use crate::i18n::{Locale, translate};

  fn translate_with(locale: Locale, key: &str, args: &[(&str, &str)]) -> String {
    let mut fluent_args = fluent_bundle::FluentArgs::new();
    for (name, value) in args {
      fluent_args.set(name.to_string(), value.to_string());
    }
    translate(locale, key, Some(&fluent_args))
  }

  const NOW: i64 = 1_792_540_800_000;

  fn state(permission: NotifyPermission, platform: NotifyPlatform) -> DesktopNotifyState {
    DesktopNotifyState {
      permission,
      platform,
      app_name: "TeamCraft".to_owned(),
    }
  }

  /// A platform that answers `allow` with `answer` and fails the test where told to.
  struct FakePlatform {
    answer: Result<DesktopNotifyState, ActionFailure>,
    is_test_failing: bool,
    tests: Cell<u32>,
  }

  impl FakePlatform {
    fn new(answer: Result<DesktopNotifyState, ActionFailure>) -> Self {
      Self {
        answer,
        is_test_failing: false,
        tests: Cell::new(0),
      }
    }
  }

  impl NotifyAdapter for FakePlatform {
    fn allow(&self) -> impl Future<Output = Result<DesktopNotifyState, ActionFailure>> {
      std::future::ready(self.answer.clone())
    }

    fn show_test(&self) -> impl Future<Output = Result<(), ActionFailure>> {
      self.tests.set(self.tests.get() + 1);
      std::future::ready(if self.is_test_failing {
        Err(ActionFailure::new(FailureKind::Other, "toast failed"))
      } else {
        Ok(())
      })
    }
  }

  /// The fakes answer at once, so one poll finishes.
  fn run<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    match future.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
      Poll::Ready(value) => value,
      Poll::Pending => panic!("the fake platform answers at once"),
    }
  }

  fn outcome(platform: &FakePlatform) -> Result<DesktopOutcome, ActionFailure> {
    run(turn_on(platform)).map(|(outcome, _)| outcome)
  }

  #[test]
  fn macos_asks_and_says_what_the_user_answered() {
    let allowed = FakePlatform::new(Ok(state(NotifyPermission::Allowed, NotifyPlatform::MacOs)));
    assert_eq!(outcome(&allowed), Ok(DesktopOutcome::On));
    assert_eq!(allowed.tests.get(), 0, "the prompt was the step, no test needed");
    let declined = FakePlatform::new(Ok(state(NotifyPermission::Blocked, NotifyPlatform::MacOs)));
    assert_eq!(outcome(&declined), Ok(DesktopOutcome::Blocked));
    let unanswered = FakePlatform::new(Ok(state(NotifyPermission::NotAsked, NotifyPlatform::MacOs)));
    assert!(outcome(&unanswered).is_err(), "the banner stays to try again");
    let failed = FakePlatform::new(Err(ActionFailure::new(FailureKind::Other, "no answer")));
    assert!(outcome(&failed).is_err());
  }

  #[test]
  fn windows_and_linux_show_a_test_notification() {
    for platform in [NotifyPlatform::Windows, NotifyPlatform::Linux] {
      let allowed = FakePlatform::new(Ok(state(NotifyPermission::Allowed, platform)));
      assert_eq!(outcome(&allowed), Ok(DesktopOutcome::TestShown));
      assert_eq!(allowed.tests.get(), 1);
      let blocked = FakePlatform::new(Ok(state(NotifyPermission::Blocked, platform)));
      assert_eq!(outcome(&blocked), Ok(DesktopOutcome::Blocked));
      assert_eq!(blocked.tests.get(), 0, "no test into a blocked system");
      let missing = FakePlatform::new(Ok(state(NotifyPermission::Unavailable, platform)));
      assert_eq!(outcome(&missing), Ok(DesktopOutcome::Unavailable));
    }
  }

  #[test]
  fn a_failing_test_notification_is_an_error_and_the_question_stays() {
    let mut failing = FakePlatform::new(Ok(state(NotifyPermission::Allowed, NotifyPlatform::Windows)));
    failing.is_test_failing = true;
    assert!(outcome(&failing).is_err());
  }

  #[test]
  fn the_banner_asks_until_notifications_were_turned_on() {
    let mac = |permission| state(permission, NotifyPlatform::MacOs);
    assert!(is_offered(&mac(NotifyPermission::NotAsked), false, None, NOW));
    assert!(
      !is_offered(&mac(NotifyPermission::Allowed), false, None, NOW),
      "asked before"
    );
    assert!(!is_offered(&mac(NotifyPermission::Blocked), false, None, NOW));
    assert!(
      !is_offered(&mac(NotifyPermission::Unavailable), false, None, NOW),
      "an unbundled development build"
    );
    for platform in [NotifyPlatform::Windows, NotifyPlatform::Linux] {
      let allowed = state(NotifyPermission::Allowed, platform);
      assert!(is_offered(&allowed, false, None, NOW));
      assert!(!is_offered(&allowed, true, None, NOW), "turned on here already");
      assert!(!is_offered(
        &state(NotifyPermission::Blocked, platform),
        false,
        None,
        NOW
      ));
      assert!(!is_offered(
        &state(NotifyPermission::Unavailable, platform),
        false,
        None,
        NOW
      ));
    }
  }

  #[test]
  fn not_now_waits_a_week() {
    let open = state(NotifyPermission::NotAsked, NotifyPlatform::MacOs);
    assert!(!is_offered(&open, false, Some(NOW - SNOOZE_MS + 1), NOW));
    assert!(is_offered(&open, false, Some(NOW - SNOOZE_MS), NOW));
  }

  #[test]
  fn settings_show_the_real_state() {
    assert_eq!(device_status_key(None), "notif-device-desktop");
    let mac = |permission| state(permission, NotifyPlatform::MacOs);
    assert_eq!(
      device_status_key(Some(&mac(NotifyPermission::Allowed))),
      "notif-desktop-allowed"
    );
    assert_eq!(
      device_status_key(Some(&mac(NotifyPermission::Blocked))),
      "notif-desktop-blocked-mac"
    );
    assert_eq!(
      device_status_key(Some(&mac(NotifyPermission::NotAsked))),
      "notif-desktop-not-asked"
    );
    assert_eq!(
      device_status_key(Some(&state(NotifyPermission::Blocked, NotifyPlatform::Windows))),
      "notif-desktop-blocked-windows"
    );
    assert!(can_turn_on(Some(&mac(NotifyPermission::NotAsked))));
    assert!(!can_turn_on(Some(&mac(NotifyPermission::Allowed))));
    assert!(can_test(Some(&mac(NotifyPermission::Allowed))));
    assert!(!can_test(Some(&mac(NotifyPermission::Blocked))));
    assert!(!can_test(Some(&mac(NotifyPermission::NotAsked))));
    assert!(can_test(None), "the shell can't say: the test checks it");
  }

  #[test]
  fn every_text_is_translated_and_names_the_app() {
    let platforms = [
      NotifyPlatform::MacOs,
      NotifyPlatform::Windows,
      NotifyPlatform::Linux,
    ];
    let mut keys = vec![
      "notif-desktop-prompt-text",
      "notif-desktop-allowed",
      "notif-desktop-not-asked",
    ];
    for platform in platforms {
      for outcome in [
        DesktopOutcome::On,
        DesktopOutcome::TestShown,
        DesktopOutcome::Blocked,
        DesktopOutcome::Unavailable,
      ] {
        keys.push(outcome_key(outcome, platform));
      }
    }
    for locale in [Locale::En, Locale::De] {
      for key in &keys {
        assert_ne!(
          translate_with(locale, key, &[("app", "TeamCraft Beta")]),
          *key,
          "{locale:?} lacks {key}"
        );
      }
      for platform in platforms {
        let text = translate_with(locale, blocked_key(platform), &[("app", "TeamCraft Beta")]);
        assert!(text.contains("TeamCraft Beta"), "{locale:?}: {text}");
      }
    }
  }
}
