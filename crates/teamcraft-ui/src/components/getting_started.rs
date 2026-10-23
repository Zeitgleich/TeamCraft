//! First steps for an empty workspace or an empty space: what a new team needs to do before they
//! can chat, each one a button that opens the right dialog.

use leptos::prelude::*;
use teamcraft_proto::Role;

use crate::components::icons::{Icon, IconName};
use crate::components::workspace_dialogs::JoinableSpaceList;
use crate::i18n::use_i18n;
use crate::state::{Overlay, use_app_state};

#[component]
pub fn GettingStarted() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_guest = Memo::new(move |_| state.read(|backend| backend.my_role()) == Role::Guest);
  // The current space, if the user is in any.
  let space = Memo::new(move |_| {
    let current = state.space.get();
    let spaces = state.read(|backend| backend.spaces());
    spaces
      .iter()
      .find(|space| space.id == current)
      .or_else(|| spaces.first())
      .map(|space| (space.id, space.name.clone()))
  });
  let has_joinable = Memo::new(move |_| !state.read(|backend| backend.joinable_spaces()).is_empty());
  let has_channels = Memo::new(move |_| {
    space
      .get()
      .is_some_and(|(id, _)| !state.read(|backend| backend.space_conversations(id)).is_empty())
  });
  // Read reactively: right after sign-in the profile may still be loading.
  let first_name = move || {
    state
      .me()
      .display_name
      .split_whitespace()
      .next()
      .unwrap_or_default()
      .to_owned()
  };

  view! {
    <section class="getting-started" aria-labelledby="getting-started-title">
      <h1 id="getting-started-title">{move || i18n.t_with("start-title", &[("name", &first_name())])}</h1>
      <p class="muted-text">
        {move || match space.get() {
          None if is_guest.get() => i18n.t("start-guest-subtitle"),
          None => i18n.t("start-empty-subtitle"),
          Some((_, name)) => i18n.t_with("start-space-subtitle", &[("name", &name)]),
        }}
      </p>
      // Colleagues usually join a space someone else made, so open spaces come first.
      <Show when=move || has_joinable.get() fallback=|| ()>
        <h2 class="start-heading">{move || i18n.t("start-join-heading")}</h2>
        <JoinableSpaceList />
      </Show>
      <ul class="start-steps">
        {move || {
          let mut steps = Vec::new();
          match space.get() {
            None => {
              if !is_guest.get() {
                steps.push(step(IconName::Plus, "start-create-space", "start-create-space-hint", move || state.overlay.set(Overlay::NewSpace)).into_any());
              }
            },
            Some((id, _)) => {
              if !has_channels.get() && !is_guest.get() {
                steps.push(step(IconName::Hash, "start-create-channel", "start-create-channel-hint", move || state.overlay.set(Overlay::NewChannel(id))).into_any());
              }
              steps.push(step(IconName::List, "start-browse-channels", "start-browse-channels-hint", move || state.overlay.set(Overlay::BrowseChannels(id))).into_any());
              if !is_guest.get() {
                steps.push(step(IconName::Users, "start-invite", "start-invite-hint", move || state.overlay.set(Overlay::SpaceSettings(id))).into_any());
              }
            },
          }
          steps.push(step(IconName::User, "start-find-people", "start-find-people-hint", move || state.open_directory()).into_any());
          steps.collect_view()
        }}
      </ul>
    </section>
  }
}

/// One step: a large button with an icon, a title and a sentence what it does.
fn step(
  icon: IconName,
  title: &'static str,
  hint: &'static str,
  action: impl Fn() + 'static,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <li>
      <button type="button" class="start-step" on:click=move |_| action()>
        <Icon name=icon />
        <span class="start-step-text">
          <strong>{move || i18n.t(title)}</strong>
          <span class="muted-text">{move || i18n.t(hint)}</span>
        </span>
      </button>
    </li>
  }
}
