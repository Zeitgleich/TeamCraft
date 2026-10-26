//! Desktop updates in the UI (docs/updates.md §4.3): the banner above the app and the Updates
//! settings. The desktop shell does the work; this only shows its status and forwards clicks.

use leptos::prelude::*;
use teamcraft_client_api::updates::{
  AvailableAction, Dismissal, InstallKind, ManualUpdate, PackageManager, PackageManagerHint, SelfUpdateStep,
  TooOldAction, UpdateBanner, UpdateBlocker, UpdatePreference, UpdateState, UpdateStatus,
};

use crate::auth::tauri;
use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::copy_to_clipboard;
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
/// too old or when a self-installed update waits for its restart.
#[component]
pub fn UpdateNotice() -> impl IntoView {
  let i18n = use_i18n();
  let status = use_update_status();
  let dismissed = RwSignal::new(None::<String>);
  let error = RwSignal::new(None::<String>);
  move || {
    let current = status.get()?;
    let banner = current.banner()?;
    let version = banner_version(&banner);
    if dismissed
      .get()
      .is_some_and(|hidden| Some(&hidden) == version.as_ref())
    {
      return None;
    }
    let is_alert = matches!(banner, UpdateBanner::TooOld { .. });
    let hide = dismiss_button(i18n, current.dismissal(), version, dismissed, error);
    Some(view! {
      <div class="update-banner" role=if is_alert { "alert" } else { "status" }>
        <Icon name=if is_alert { IconName::Alert } else { IconName::Download } />
        {banner_body(i18n, banner, current.install, status, error)}
        {hide}
        {move || error.get().map(|message| view! { <span class="small composer-error">{message}</span> })}
      </div>
    })
  }
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
        <button class="btn btn-small" on:click=move |_| dismissed.set(Some(version.clone()))>
          {move || i18n.t("update-later")}
        </button>
      }
      .into_any()
    }),
    Dismissal::ForVersion(version) => Some(
      view! {
        <button
          class="btn btn-small"
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

fn banner_body(
  i18n: I18n,
  banner: UpdateBanner,
  install: InstallKind,
  status: RwSignal<Option<UpdateStatus>>,
  error: RwSignal<Option<String>>,
) -> AnyView {
  match banner {
    UpdateBanner::Ready {
      version,
      maybe_notes_url,
      can_restart,
    } => view! {
      <span>{i18n.t_with("update-ready", &[("version", &version)])}</span>
      {notes_link(i18n, maybe_notes_url)}
      {restart_button(i18n, can_restart, status, error)}
    }
    .into_any(),
    UpdateBanner::Available {
      version,
      maybe_notes_url,
      action,
    } => view! {
      <span>{i18n.t_with("update-available", &[("version", &version)])}</span>
      {notes_link(i18n, maybe_notes_url)}
      {match action {
        AvailableAction::Download { can_download } => download_button(i18n, can_download, status, error),
        AvailableAction::Manual(manual) => manual_update(i18n, manual, install, status, error),
      }}
    }
    .into_any(),
    UpdateBanner::TooOld {
      required_version,
      action,
    } => view! {
      <span>{i18n.t_with("update-too-old", &[("version", &required_version)])}</span>
      {match action {
        TooOldAction::SelfUpdate(SelfUpdateStep::Restart { can_restart }) => restart_button(i18n, can_restart, status, error),
        TooOldAction::SelfUpdate(SelfUpdateStep::Download { can_download }) => {
          download_button(i18n, can_download, status, error)
        },
        TooOldAction::SelfUpdate(SelfUpdateStep::Wait) => view! {
          <span class="small">{i18n.t("update-too-old-self")}</span>
        }
        .into_any(),
        TooOldAction::Manual(manual) => manual_update(i18n, manual, install, status, error),
        TooOldAction::Elsewhere => view! {
          <span class="small">{i18n.t("update-too-old-elsewhere")}</span>
        }
        .into_any(),
      }}
    }
    .into_any(),
  }
}

fn restart_button(
  i18n: I18n,
  can_restart: bool,
  status: RwSignal<Option<UpdateStatus>>,
  error: RwSignal<Option<String>>,
) -> AnyView {
  view! {
    <button
      class="btn btn-primary btn-small"
      prop:disabled=!can_restart
      on:click=move |_| run(error, status, tauri::update_restart())
    >
      {i18n.t(if can_restart { "update-restart" } else { "update-restart-after-call" })}
    </button>
  }
  .into_any()
}

/// Disabled during a call.
fn download_button(
  i18n: I18n,
  can_download: bool,
  status: RwSignal<Option<UpdateStatus>>,
  error: RwSignal<Option<String>>,
) -> AnyView {
  view! {
    <button
      class="btn btn-primary btn-small"
      prop:disabled=!can_download
      on:click=move |_| run(error, status, tauri::update_download())
    >
      {i18n.t("update-download")}
    </button>
  }
  .into_any()
}

/// What the user does where the app can't install the update itself (#731).
fn manual_update(
  i18n: I18n,
  manual: ManualUpdate,
  install: InstallKind,
  status: RwSignal<Option<UpdateStatus>>,
  error: RwSignal<Option<String>>,
) -> AnyView {
  match manual {
    ManualUpdate::PackageManager(hint) => package_manager(i18n, hint, status, error),
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
fn package_manager(
  i18n: I18n,
  hint: PackageManagerHint,
  status: RwSignal<Option<UpdateStatus>>,
  error: RwSignal<Option<String>>,
) -> AnyView {
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
      <button
        class="btn btn-primary btn-small"
        on:click=move |_| run(error, status, tauri::update_with_package_manager())
      >
        {i18n.t("update-package-kit")}
      </button>
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

/// Runs a shell command, shows its error and reads the status again.
fn run(
  error: RwSignal<Option<String>>,
  status: RwSignal<Option<UpdateStatus>>,
  command: impl std::future::Future<Output = Result<(), String>> + 'static,
) {
  leptos::task::spawn_local(async move {
    error.set_if_alive(command.await.err());
    status.set_if_alive(tauri::update_status().await);
  });
}

fn banner_version(banner: &UpdateBanner) -> Option<String> {
  match banner {
    UpdateBanner::Ready { version, .. } | UpdateBanner::Available { version, .. } => Some(version.clone()),
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
    UpdateState::Installing { version } => i18n.t_with("update-state-installing", &[("version", version)]),
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
      "update-ready",
      "update-available",
      "update-restart",
      "update-restart-after-call",
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
