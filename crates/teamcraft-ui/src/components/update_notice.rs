//! Desktop updates in the UI (docs/updates.md §4.3): the banner above the app and the Updates
//! settings. The desktop shell does the work; this only shows its status and forwards clicks.

use leptos::prelude::*;
use teamcraft_client_api::updates::{
  AvailableAction, Dismissal, InstallKind, InstallPhase, ManualUpdate, PackageManager, PackageManagerHint,
  SelfUpdateStep, TooOldAction, UpdateBanner, UpdateBlocker, UpdatePreference, UpdateProgress, UpdateState,
  UpdateStatus,
};

use crate::areas::{self, Place, lazy_area};
use crate::auth::tauri;
use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::copy_to_clipboard;
use crate::components::pending_button::PendingButton;
use crate::feedback::PendingAction;
use crate::i18n::{I18n, use_i18n};
use crate::reactive::{SetIfAlive, use_interval_while_visible};

/// The shell's event when the status changed (`updates::UPDATE_CHANGED_EVENT`).
const UPDATE_CHANGED_EVENT: &str = "update-changed";
/// Status is read again this often anyway (a call starting or ending changes it too), from the
/// shell over IPC, and only while the window is visible: the notice is all it feeds.
const REFRESH_MS: u64 = 30_000;
const PREFERENCES: [UpdatePreference; 3] = [
  UpdatePreference::Automatic,
  UpdatePreference::NotifyOnly,
  UpdatePreference::Off,
];

/// Above the app: "ready, restart", "available", or "too old". Renders nothing outside the
/// desktop app. How it can be hidden is the status's [`Dismissal`]: "Later" until the next launch,
/// "Dismiss" for good for a version the app can't install (deb, rpm, Flatpak), and not at all when
/// too old or when a self-installed update waits for its restart (#729): it only needs a restart,
/// and the banner keeps asking for it, in the accent colour (the one banner meant to be seen;
/// connection trouble stays quiet, offline_banner.rs). During a call its button waits for the call
/// to end.
#[component]
pub fn UpdateNotice() -> impl IntoView {
  // Only the desktop app loads the banner (`areas::update_notice`); its chunk is on the disk there.
  tauri::is_available().then(|| lazy_area(Place::Inline, || areas::update_notice(())))
}

/// The banner of [`UpdateNotice`]. One banner that stays while the user acts on it and changes
/// its content (#1044): "available" turns into the progress of the download or install the user
/// started, then into "ready"; a failure shows inline, next to the button that tries again.
#[component]
pub fn UpdateNoticeBanner() -> impl IntoView {
  let i18n = use_i18n();
  let status = use_update_status();
  let dismissed = RwSignal::new(None::<String>);
  let error = RwSignal::new(None::<String>);
  // The banner's own click (download, PackageKit, restart) while it runs.
  let action = PendingAction::new();
  let shown = Memo::new(move |_| {
    let current = status.get()?;
    shown_banner(&current, action.is_pending(), dismissed.get().as_deref())
  });
  // The body only changes with what is shown; the percentage alone updates its own text, so the
  // spinner keeps turning.
  let body = Memo::new(move |_| shown.get().map(|banner| without_percent(&banner)));
  let maybe_percent = Memo::new(move |_| match status.get().map(|current| current.state) {
    Some(UpdateState::Downloading { maybe_percent, .. }) => maybe_percent,
    _ => None,
  });
  let install = Memo::new(move |_| status.get().map(|current| current.install));
  let dismissal = Memo::new(move |_| status.get().map(|current| current.dismissal()));
  let is_alert = move || {
    matches!(shown.get(), Some(UpdateBanner::TooOld { .. }))
      || (shown.with(Option::is_none) && error.with(Option::is_some))
  };
  let is_ready = move || matches!(shown.get(), Some(UpdateBanner::Ready { .. }));
  let context = BannerContext {
    i18n,
    status,
    error,
    action,
    maybe_percent,
  };
  view! {
    // A failed click whose banner is gone (a restart that could not install) keeps the banner for
    // its error: no silent failure.
    <Show when=move || shown.with(Option::is_some) || error.with(Option::is_some)>
      <div class="update-banner" class:is-ready=is_ready role=move || if is_alert() { "alert" } else { "status" }>
        {move || view! { <Icon name=if is_alert() { IconName::Alert } else { IconName::Download } /> }}
        {move || {
          body
            .get()
            .zip(install.get())
            .map(|(banner, install)| banner_body(context, banner, install))
        }}
        {move || {
          let version = shown.get().as_ref().and_then(banner_version);
          match dismissal.get() {
            Some(dismissal) if shown.with(Option::is_some) => dismiss_button(i18n, dismissal, version, dismissed, error),
            _ => error.with(Option::is_some).then(|| clear_error_button(i18n, error)),
          }
        }}
        {move || error.get().map(|message| view! { <span class="small update-banner-error">{message}</span> })}
      </div>
    </Show>
  }
}

/// What the banner shows: the status's own banner, or the progress of the user's click that still
/// runs (an automatic download is otherwise quiet); nothing for a version the user hid.
fn shown_banner(
  status: &UpdateStatus,
  is_action_pending: bool,
  maybe_dismissed: Option<&str>,
) -> Option<UpdateBanner> {
  let banner = status
    .banner()
    .or_else(|| is_action_pending.then(|| status.progress_banner()).flatten())?;
  let is_hidden = banner_version(&banner).is_some_and(|version| maybe_dismissed == Some(version.as_str()));
  (!is_hidden).then_some(banner)
}

/// The banner without the download's percentage, which [`BannerContext::maybe_percent`] carries.
fn without_percent(banner: &UpdateBanner) -> UpdateBanner {
  let strip = |progress: UpdateProgress| match progress {
    UpdateProgress::Downloading { .. } => UpdateProgress::Downloading { maybe_percent: None },
    installing @ UpdateProgress::Installing(_) => installing,
  };
  match banner.clone() {
    UpdateBanner::InProgress { version, progress } => UpdateBanner::InProgress {
      version,
      progress: strip(progress),
    },
    UpdateBanner::TooOld {
      required_version,
      action: TooOldAction::InProgress(progress),
    } => UpdateBanner::TooOld {
      required_version,
      action: TooOldAction::InProgress(strip(progress)),
    },
    other => other,
  }
}

/// What the banner's buttons need.
#[derive(Clone, Copy)]
struct BannerContext {
  i18n: I18n,
  status: RwSignal<Option<UpdateStatus>>,
  error: RwSignal<Option<String>>,
  action: PendingAction,
  maybe_percent: Memo<Option<u8>>,
}

/// Settings → Updates (desktop only): the preference, what is going on, and "Check now".
#[component]
pub fn UpdatesSection() -> impl IntoView {
  let i18n = use_i18n();
  let status = use_update_status();
  let error = RwSignal::new(None::<String>);
  let choose = move |preference: UpdatePreference| {
    status.update(|maybe| {
      if let Some(current) = maybe {
        current.preference = preference;
      }
    });
    leptos::task::spawn_local(async move {
      error.set_if_alive(tauri::update_set_preference(preference).await.err());
    });
  };
  let is_managed = move || {
    status.get().is_some_and(|current| {
      matches!(
        current.state,
        UpdateState::Off {
          reason: UpdateBlocker::Policy
            | UpdateBlocker::Machine
            | UpdateBlocker::Unpackaged
            | UpdateBlocker::NotConfigured
        }
      )
    })
  };
  view! {
    <fieldset class="settings-group">
      <legend>{move || i18n.t("settings-updates")}</legend>
      {move || status.get().map(|current| view! {
        <p class="small" translate="no">{format!("TeamCraft {}", current.current_version)}</p>
        <p class="small" role="status">{state_text(i18n, &current)}</p>
      })}
      {PREFERENCES
        .into_iter()
        .map(|preference| {
          let slug = preference_slug(preference);
          view! {
            <label class="radio">
              <input
                type="radio"
                name="update-preference"
                prop:checked=move || status.get().is_some_and(|current| current.preference == preference)
                prop:disabled=is_managed
                on:change=move |_| choose(preference)
              />
              <span>
                {move || i18n.t(&format!("settings-update-{slug}"))}
                <small class="muted-text">{move || i18n.t(&format!("settings-update-{slug}-hint"))}</small>
              </span>
            </label>
          }
        })
        .collect_view()}
      <button
        class="btn"
        prop:disabled=move || is_managed() || status.get().is_some_and(|current| current.preference == UpdatePreference::Off)
        on:click=move |_| {
          leptos::task::spawn_local(async move { error.set_if_alive(tauri::update_check().await.err()) });
        }
      >
        {move || i18n.t("settings-update-check")}
      </button>
      {move || error.get().map(|message| view! { <p class="composer-error" role="alert">{message}</p> })}
    </fieldset>
  }
}

/// The shell's update status, kept current; `None` outside the desktop app.
fn use_update_status() -> RwSignal<Option<UpdateStatus>> {
  let status = RwSignal::new(None::<UpdateStatus>);
  if !tauri::is_available() {
    return status;
  }
  let refresh = move || {
    leptos::task::spawn_local(async move {
      status.set_if_alive(tauri::update_status().await);
    });
  };
  refresh();
  tauri::listen(UPDATE_CHANGED_EVENT, refresh);
  use_interval_while_visible(REFRESH_MS, refresh);
  status
}

/// "Later" (this launch) or "Dismiss" (this version, remembered by the shell); nothing for a banner
/// that has to stay.
fn dismiss_button(
  i18n: I18n,
  dismissal: Dismissal,
  maybe_version: Option<String>,
  dismissed: RwSignal<Option<String>>,
  error: RwSignal<Option<String>>,
) -> Option<AnyView> {
  match dismissal {
    Dismissal::Never => None,
    Dismissal::UntilRelaunch => maybe_version.map(|version| {
      view! {
        <button class="btn btn-small update-banner-quiet" on:click=move |_| dismissed.set(Some(version.clone()))>
          {move || i18n.t("update-later")}
        </button>
      }
      .into_any()
    }),
    Dismissal::ForVersion(version) => Some(
      view! {
        <button
          class="btn btn-small update-banner-quiet"
          on:click=move |_| {
            dismissed.set(Some(version.clone()));
            let version = version.clone();
            leptos::task::spawn_local(async move {
              error.set_if_alive(tauri::update_dismiss(&version).await.err());
            });
          }
        >
          {move || i18n.t("update-dismiss")}
        </button>
      }
      .into_any(),
    ),
  }
}

/// Hides the error of a click whose banner is gone.
fn clear_error_button(i18n: I18n, error: RwSignal<Option<String>>) -> AnyView {
  view! {
    <button class="btn btn-small update-banner-quiet" on:click=move |_| error.set(None)>
      {move || i18n.t("update-dismiss")}
    </button>
  }
  .into_any()
}

fn banner_body(context: BannerContext, banner: UpdateBanner, install: InstallKind) -> AnyView {
  let i18n = context.i18n;
  match banner {
    UpdateBanner::Ready {
      version,
      maybe_notes_url,
      can_restart,
    } => {
      // A package manager installed it (deb/rpm, PackageKit): the files are new, the running app isn't.
      let is_installed = !install.updates_itself();
      let text_key = if is_installed { "update-ready-installed" } else { "update-ready-banner" };
      view! {
        <span class="update-banner-text">{i18n.t_with(text_key, &[("version", &version)])}</span>
        {notes_link(i18n, maybe_notes_url)}
        <PendingButton
          pending=context.action
          class="btn btn-small update-banner-restart"
          is_disabled=Signal::derive(move || !can_restart)
          on_click=move |_| context.run(tauri::update_restart())
        >
          {i18n.t(if can_restart { "update-restart-short" } else { "update-restart-after-call" })}
        </PendingButton>
      }
      .into_any()
    },
    UpdateBanner::Available {
      version,
      maybe_notes_url,
      action,
    } => view! {
      <span>{i18n.t_with("update-available", &[("version", &version)])}</span>
      {notes_link(i18n, maybe_notes_url)}
      {match action {
        AvailableAction::Download { can_download } => download_button(context, can_download),
        AvailableAction::Manual(manual) => manual_update(context, manual, install),
      }}
    }
    .into_any(),
    UpdateBanner::InProgress { version, progress } => {
      let text = match progress {
        UpdateProgress::Installing(InstallPhase::WaitingForAuthorization) => {
          i18n.t_with("update-waiting-authorization-hint", &[("version", &version)])
        },
        _ => i18n.t_with("update-available", &[("version", &version)]),
      };
      view! {
        <span>{text}</span>
        {progress_button(context, version, progress)}
      }
      .into_any()
    },
    UpdateBanner::TooOld {
      required_version,
      action,
    } => view! {
      <span>{i18n.t_with("update-too-old", &[("version", &required_version)])}</span>
      {match action {
        TooOldAction::SelfUpdate(SelfUpdateStep::Restart { can_restart }) => restart_button(context, can_restart),
        TooOldAction::SelfUpdate(SelfUpdateStep::Download { can_download }) => download_button(context, can_download),
        TooOldAction::SelfUpdate(SelfUpdateStep::Wait) => view! {
          <span class="small">{i18n.t("update-too-old-self")}</span>
        }
        .into_any(),
        TooOldAction::Manual(manual) => manual_update(context, manual, install),
        TooOldAction::InProgress(progress) => progress_button(context, required_version, progress),
        TooOldAction::Elsewhere => view! {
          <span class="small">{i18n.t("update-too-old-elsewhere")}</span>
        }
        .into_any(),
      }}
    }
    .into_any(),
  }
}

/// The button of the click that runs, disabled, with a spinner and how far it is.
fn progress_button(context: BannerContext, version: String, progress: UpdateProgress) -> AnyView {
  let i18n = context.i18n;
  let label = move || match progress {
    UpdateProgress::Downloading { .. } => match context.maybe_percent.get() {
      Some(percent) => i18n.t_with("update-downloading-percent", &[("percent", &percent.to_string())]),
      None => i18n.t("update-downloading"),
    },
    UpdateProgress::Installing(InstallPhase::Running) => {
      i18n.t_with("update-installing", &[("version", &version)])
    },
    UpdateProgress::Installing(InstallPhase::WaitingForAuthorization) => {
      i18n.t("update-waiting-authorization")
    },
  };
  view! {
    <button type="button" class="btn btn-primary btn-small" disabled aria-busy="true">
      <span class="spinner" aria-hidden="true"></span>
      <span role="status">{label}</span>
    </button>
  }
  .into_any()
}

fn restart_button(context: BannerContext, can_restart: bool) -> AnyView {
  let i18n = context.i18n;
  view! {
    <PendingButton
      pending=context.action
      class="btn btn-primary btn-small"
      is_disabled=Signal::derive(move || !can_restart)
      on_click=move |_| context.run(tauri::update_restart())
    >
      {i18n.t(if can_restart { "update-restart" } else { "update-restart-after-call" })}
    </PendingButton>
  }
  .into_any()
}

/// Disabled during a call.
fn download_button(context: BannerContext, can_download: bool) -> AnyView {
  let i18n = context.i18n;
  view! {
    <PendingButton
      pending=context.action
      class="btn btn-primary btn-small"
      is_disabled=Signal::derive(move || !can_download)
      on_click=move |_| context.run(tauri::update_download())
    >
      {i18n.t("update-download")}
    </PendingButton>
  }
  .into_any()
}

/// What the user does where the app can't install the update itself (#731).
fn manual_update(context: BannerContext, manual: ManualUpdate, install: InstallKind) -> AnyView {
  let i18n = context.i18n;
  match manual {
    ManualUpdate::PackageManager(hint) => package_manager(context, hint),
    ManualUpdate::Download {
      url,
      maybe_repository_setup_url,
    } => {
      let label = if install == InstallKind::AppImage {
        "update-download-appimage"
      } else {
        "update-download-package"
      };
      view! {
        <a href=url target="_blank" rel="noopener noreferrer" class="btn btn-primary btn-small">
          {move || i18n.t(label)}
        </a>
        {maybe_repository_setup_url.map(|setup| view! {
          <a href=setup target="_blank" rel="noopener noreferrer" class="small">
            {move || i18n.t("update-add-repository")}
          </a>
        })}
      }
      .into_any()
    },
  }
}

/// The command with a copy button, and "Update now" when PackageKit runs.
fn package_manager(context: BannerContext, hint: PackageManagerHint) -> AnyView {
  let i18n = context.i18n;
  let is_copied = RwSignal::new(false);
  let command = hint.command.clone();
  let intro = match hint.manager {
    PackageManager::Flatpak => "update-flatpak",
    PackageManager::Apt | PackageManager::Dnf => "update-package-manager",
  };
  view! {
    <span class="small">{move || i18n.t(intro)}</span>
    <code class="update-command" translate="no">{hint.command}</code>
    <button
      class="btn btn-small"
      on:click=move |_| {
        copy_to_clipboard(&command);
        is_copied.set(true);
      }
    >
      <Icon name=IconName::Copy />
      {move || i18n.t(if is_copied.get() { "update-command-copied" } else { "update-copy-command" })}
    </button>
    {hint.has_package_kit.then(|| view! {
      <PendingButton
        pending=context.action
        class="btn btn-primary btn-small"
        on_click=move |_| context.run(tauri::update_with_package_manager())
      >
        {i18n.t("update-package-kit")}
      </PendingButton>
    })}
  }
  .into_any()
}

fn notes_link(i18n: I18n, maybe_url: Option<String>) -> Option<impl IntoView> {
  maybe_url.map(|url| {
    view! {
      <a href=url target="_blank" rel="noopener noreferrer" class="small">{i18n.t("update-whats-new")}</a>
    }
  })
}

impl BannerContext {
  /// Runs the banner's shell command: its button waits, then the status is read again and an
  /// error shows inline in the banner, where the button can try again.
  fn run(self, command: impl std::future::Future<Output = Result<(), String>> + 'static) {
    self.error.set(None);
    let (error, status) = (self.error, self.status);
    self.action.spawn(async move {
      let result = command.await;
      status.set_if_alive(tauri::update_status().await);
      error.set_if_alive(result.err());
      // Shown in the banner, not as a toast as well.
      Ok(())
    });
  }
}

/// The version a hidden banner stays hidden for this launch; whether it can be hidden at all is the
/// status's [`Dismissal`].
fn banner_version(banner: &UpdateBanner) -> Option<String> {
  match banner {
    UpdateBanner::Ready { version, .. }
    | UpdateBanner::Available { version, .. }
    | UpdateBanner::InProgress { version, .. } => Some(version.clone()),
    UpdateBanner::TooOld { .. } => None,
  }
}

/// One line about the state, for Settings.
fn state_text(i18n: I18n, status: &UpdateStatus) -> String {
  match &status.state {
    UpdateState::Off { reason } => i18n.t(&format!("update-off-{}", blocker_slug(*reason))),
    UpdateState::Idle {
      maybe_checked_at: None,
    } => i18n.t("update-state-unchecked"),
    UpdateState::Idle { .. } => i18n.t("update-state-idle"),
    UpdateState::Checking => i18n.t("update-state-checking"),
    UpdateState::Available { version, .. } => i18n.t_with("update-available", &[("version", version)]),
    UpdateState::Downloading {
      version,
      maybe_percent,
    } => match maybe_percent {
      Some(percent) => i18n.t_with(
        "update-state-downloading-percent",
        &[("version", version), ("percent", &percent.to_string())],
      ),
      None => i18n.t_with("update-state-downloading", &[("version", version)]),
    },
    UpdateState::Installing {
      version,
      phase: InstallPhase::Running,
    } => i18n.t_with("update-state-installing", &[("version", version)]),
    UpdateState::Installing {
      version,
      phase: InstallPhase::WaitingForAuthorization,
    } => i18n.t_with("update-waiting-authorization-hint", &[("version", version)]),
    UpdateState::Ready { version, .. } => i18n.t_with("update-ready", &[("version", version)]),
    UpdateState::Failed { message } => i18n.t_with("update-state-failed", &[("message", message)]),
  }
}

fn preference_slug(preference: UpdatePreference) -> &'static str {
  match preference {
    UpdatePreference::Automatic => "automatic",
    UpdatePreference::NotifyOnly => "notify_only",
    UpdatePreference::Off => "off",
  }
}

fn blocker_slug(blocker: UpdateBlocker) -> &'static str {
  match blocker {
    UpdateBlocker::Policy => "policy",
    UpdateBlocker::Machine => "machine",
    UpdateBlocker::Preference => "preference",
    UpdateBlocker::NotConfigured => "not_configured",
    UpdateBlocker::Unpackaged => "unpackaged",
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  pub(crate) const BLOCKERS: [UpdateBlocker; 5] = [
    UpdateBlocker::Policy,
    UpdateBlocker::Machine,
    UpdateBlocker::Preference,
    UpdateBlocker::NotConfigured,
    UpdateBlocker::Unpackaged,
  ];

  #[test]
  fn every_state_has_a_translated_line() {
    use crate::i18n::{Locale, translate};

    let mut keys: Vec<String> = BLOCKERS
      .into_iter()
      .map(|blocker| format!("update-off-{}", blocker_slug(blocker)))
      .collect();
    for preference in PREFERENCES {
      let slug = preference_slug(preference);
      keys.push(format!("settings-update-{slug}"));
      keys.push(format!("settings-update-{slug}-hint"));
    }
    for key in [
      "update-dismiss",
      "update-flatpak",
      "update-copy-command",
      "update-command-copied",
      "update-download-package",
      "update-download-appimage",
      "update-add-repository",
      "update-state-idle",
      "update-state-unchecked",
      "update-state-checking",
      "update-state-downloading",
      "update-state-downloading-percent",
      "update-state-installing",
      "update-state-failed",
      "update-installing",
      "update-downloading",
      "update-downloading-percent",
      "update-waiting-authorization",
      "update-waiting-authorization-hint",
      "update-ready",
      "update-available",
      "update-restart",
      "update-restart-after-call",
      "update-restart-short",
      "update-ready-banner",
      "update-ready-installed",
      "update-download",
      "update-later",
      "update-whats-new",
      "update-package-manager",
      "update-package-kit",
      "update-too-old",
      "update-too-old-self",
      "update-too-old-elsewhere",
      "settings-updates",
      "settings-update-check",
    ] {
      keys.push(key.to_owned());
    }
    for locale in [Locale::En, Locale::De] {
      for key in &keys {
        assert_ne!(&translate(locale, key, None), key, "{locale:?} lacks {key}");
      }
    }
  }

  mod shown {
    use super::*;

    #[test]
    fn the_banner_stays_while_the_users_click_runs() {
      let mut current = status(UpdateState::Downloading {
        version: "1.0.1".into(),
        maybe_percent: Some(30),
      });
      // An automatic download is quiet...
      assert_eq!(shown_banner(&current, false, None), None);
      // ...but not the one the user clicked "Download" for (in a call it waited for).
      let progress = Some(UpdateBanner::InProgress {
        version: "1.0.1".into(),
        progress: UpdateProgress::Downloading {
          maybe_percent: Some(30),
        },
      });
      assert_eq!(shown_banner(&current, true, None), progress);
      current.state = UpdateState::Ready {
        version: "1.0.1".into(),
        maybe_notes_url: None,
      };
      assert!(matches!(
        shown_banner(&current, true, None),
        Some(UpdateBanner::Ready { .. })
      ));
    }

    #[test]
    fn a_hidden_version_stays_hidden() {
      let current = status(UpdateState::Available {
        version: "1.0.1".into(),
        maybe_notes_url: None,
      });
      assert_eq!(shown_banner(&current, false, Some("1.0.1")), None);
      assert!(shown_banner(&current, false, Some("1.0.0")).is_some());
    }

    #[test]
    fn the_percentage_is_not_part_of_the_body() {
      let downloading = |maybe_percent| UpdateBanner::InProgress {
        version: "1.0.1".into(),
        progress: UpdateProgress::Downloading { maybe_percent },
      };
      assert_eq!(
        without_percent(&downloading(Some(10))),
        without_percent(&downloading(Some(90)))
      );
      let installing = UpdateBanner::InProgress {
        version: "1.0.1".into(),
        progress: UpdateProgress::Installing(InstallPhase::WaitingForAuthorization),
      };
      assert_eq!(without_percent(&installing), installing);
    }

    fn status(state: UpdateState) -> UpdateStatus {
      UpdateStatus {
        current_version: "1.0.0".into(),
        install: InstallKind::AppImage,
        preference: UpdatePreference::Automatic,
        state,
        maybe_required_version: None,
        is_in_call: false,
        maybe_manual: None,
        maybe_dismissed_version: None,
      }
    }
  }

  #[test]
  fn only_versioned_banners_can_be_dismissed() {
    let ready = UpdateBanner::Ready {
      version: "1.0.1".into(),
      maybe_notes_url: None,
      can_restart: true,
    };
    assert_eq!(banner_version(&ready), Some("1.0.1".to_owned()));
    let too_old = UpdateBanner::TooOld {
      required_version: "1.2.0".into(),
      action: TooOldAction::Elsewhere,
    };
    assert_eq!(banner_version(&too_old), None);
  }
}
