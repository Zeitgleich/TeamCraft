//! "Set up your workspace": a short, dismissible checklist on the Chats start page with progress
//! (issue #301). A step is done once the user has taken it; the profile picture also counts when
//! one is already set. Progress is kept with the preferences.

use leptos::prelude::*;

use crate::brand::Brand;
use crate::channel_access::{can_invite_to, is_guest};
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
  // The first space the user is in: where browsing and inviting happen.
  let space = move || {
    state
      .read(|backend| backend.spaces())
      .first()
      .map(|space| space.id)
  };
  // Only the steps the user can take (issue #691): guests browse no channels, and inviting takes
  // a space that lets them.
  let steps = StoredValue::new(offered_steps(
    !is_guest(state),
    untrack(space).is_some_and(|space| can_invite_to(state, space)),
  ));
  let step_count = steps.with_value(Vec::len);
  let done_count = move || steps.with_value(|steps| steps.iter().filter(|step| is_done(**step)).count());
  let mark_done = move |step: SetupStep| {
    prefs.update(|all| {
      if !all.onboarding.done.contains(&step) {
        all.onboarding.done.push(step);
      }
    });
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
  let is_visible = move || !prefs.with(|all| all.onboarding.is_dismissed) && done_count() < step_count;

  view! {
    <Show when=is_visible fallback=|| ()>
      <section class="setup-checklist" aria-labelledby="setup-title">
        <header class="setup-head">
          <h2 id="setup-title">{move || i18n.t("setup-title")}</h2>
          <span class="setup-progress-text">
            {move || i18n.t_with("setup-progress", &[("done", &done_count().to_string()), ("total", &step_count.to_string())])}
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
          aria-valuemax=step_count
          aria-valuenow=done_count
          aria-label=move || i18n.t("setup-title")
        >
          <span class="setup-progress-bar" style:--setup-done=move || format!("{}", done_count() as f64 / step_count.max(1) as f64)></span>
        </div>
        <ul class="setup-steps">
          {steps
            .get_value()
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

/// The checklist's steps for someone who can browse channels and invite people, or not.
fn offered_steps(can_browse_channels: bool, can_invite: bool) -> Vec<SetupStep> {
  STEPS
    .into_iter()
    .filter(|step| match step {
      SetupStep::BrowseChannels => can_browse_channels,
      SetupStep::InvitePeople => can_invite,
      _ => true,
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn guests_get_no_steps_they_cannot_take() {
    assert_eq!(offered_steps(true, true), STEPS);
    assert_eq!(
      offered_steps(false, false),
      [SetupStep::ProfilePicture, SetupStep::InstallApp]
    );
    assert!(!offered_steps(true, false).contains(&SetupStep::InvitePeople));
  }
}
