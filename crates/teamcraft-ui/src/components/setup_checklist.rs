//! "Set up your workspace": a short, dismissible checklist on the Chats start page with progress
//! (issue #301). A step is done once the user has taken it; the profile picture also counts when
//! one is already set. Progress is kept with the preferences.

use leptos::prelude::*;

use crate::brand::Brand;
use crate::components::calls_unavailable::DEFAULT_DOWNLOAD_PAGE_URL;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::prefs::SetupStep;
use crate::profiles::use_profiles;
use crate::state::{Overlay, SettingsTab, use_app_state};

const STEPS: [SetupStep; 4] = [
  SetupStep::ProfilePicture,
  SetupStep::InstallApp,
  SetupStep::BrowseChannels,
  SetupStep::InvitePeople,
];

impl SetupStep {
  fn title_key(self) -> &'static str {
    match self {
      Self::ProfilePicture => "setup-picture",
      Self::InstallApp => "setup-app",
      Self::BrowseChannels => "setup-channels",
      Self::InvitePeople | Self::Unknown => "setup-invite",
    }
  }

  fn hint_key(self) -> &'static str {
    match self {
      Self::ProfilePicture => "setup-picture-hint",
      Self::InstallApp => "setup-app-hint",
      Self::BrowseChannels => "setup-channels-hint",
      Self::InvitePeople | Self::Unknown => "setup-invite-hint",
    }
  }

  fn icon(self) -> IconName {
    match self {
      Self::ProfilePicture => IconName::Camera,
      Self::InstallApp => IconName::Smartphone,
      Self::BrowseChannels => IconName::Hash,
      Self::InvitePeople | Self::Unknown => IconName::UserPlus,
    }
  }
}

#[component]
pub fn SetupChecklist() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let prefs = state.settings.prefs;
  let brand = Brand::use_current().0;
  let download_page = move || {
    brand
      .with(|view| view.links.maybe_download.clone())
      .unwrap_or_else(|| DEFAULT_DOWNLOAD_PAGE_URL.to_owned())
  };
  let me = state.me().id;
  let has_picture = move || use_profiles().is_some_and(|hub| hub.avatar_url(me).is_some());
  let is_done = move |step: SetupStep| {
    (step == SetupStep::ProfilePicture && has_picture())
      || prefs.with(|all| all.onboarding.done.contains(&step))
  };
  let done_count = move || STEPS.iter().filter(|step| is_done(**step)).count();
  let mark_done = move |step: SetupStep| {
    prefs.update(|all| {
      if !all.onboarding.done.contains(&step) {
        all.onboarding.done.push(step);
      }
    });
  };
  // The first space the user is in: where browsing and inviting happen.
  let space = move || {
    state
      .read(|backend| backend.spaces())
      .first()
      .map(|space| space.id)
  };
  let take = move |step: SetupStep| {
    mark_done(step);
    match step {
      SetupStep::ProfilePicture => state.open_settings_at(SettingsTab::Account),
      SetupStep::BrowseChannels => {
        if let Some(space) = space() {
          state.overlay.set(Overlay::BrowseChannels(space));
        }
      },
      SetupStep::InvitePeople => {
        if let Some(space) = space() {
          state.overlay.set(Overlay::InvitePeople(space));
        }
      },
      SetupStep::InstallApp | SetupStep::Unknown => {},
    }
  };
  let is_visible = move || !prefs.with(|all| all.onboarding.is_dismissed) && done_count() < STEPS.len();

  view! {
    <Show when=is_visible fallback=|| ()>
      <section class="setup-checklist" aria-labelledby="setup-title">
        <header class="setup-head">
          <h2 id="setup-title">{move || i18n.t("setup-title")}</h2>
          <span class="setup-progress-text">
            {move || i18n.t_with("setup-progress", &[("done", &done_count().to_string()), ("total", &STEPS.len().to_string())])}
          </span>
          <button
            class="icon-btn"
            aria-label=move || i18n.t("setup-dismiss")
            title=move || i18n.t("setup-dismiss")
            on:click=move |_| prefs.update(|all| all.onboarding.is_dismissed = true)
          >
            <Icon name=IconName::Close />
          </button>
        </header>
        <div
          class="setup-progress"
          role="progressbar"
          aria-valuemin="0"
          aria-valuemax=STEPS.len()
          aria-valuenow=done_count
          aria-label=move || i18n.t("setup-title")
        >
          <span class="setup-progress-bar" style:--setup-done=move || format!("{}", done_count() as f64 / STEPS.len() as f64)></span>
        </div>
        <ul class="setup-steps">
          {STEPS
            .into_iter()
            .map(|step| {
              let body = view! {
                <span class="setup-mark" aria-hidden="true">
                  {move || is_done(step).then(|| view! { <Icon name=IconName::Check /> })}
                </span>
                <span class="setup-step-text">
                  <strong>{move || i18n.t(step.title_key())}</strong>
                  <span class="muted-text">{move || i18n.t(step.hint_key())}</span>
                </span>
                <Icon name=step.icon() />
              };
              let row = if step == SetupStep::InstallApp {
                view! {
                  <a
                    class="setup-step"
                    class:is-done=move || is_done(step)
                    href=download_page
                    target="_blank"
                    rel="noopener noreferrer"
                    on:click=move |_| mark_done(step)
                  >
                    {body}
                  </a>
                }
                .into_any()
              } else {
                view! {
                  <button type="button" class="setup-step" class:is-done=move || is_done(step) on:click=move |_| take(step)>
                    {body}
                  </button>
                }
                .into_any()
              };
              view! { <li>{row}</li> }
            })
            .collect_view()}
        </ul>
      </section>
    </Show>
  }
}
