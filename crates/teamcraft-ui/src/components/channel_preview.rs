//! Preview of a public channel the user hasn't joined (opened from "Browse channels"): its name,
//! topic and who is in it, with "Details" and "Join channel" at the bottom. The messages stay out
//! of reach until the user joins: they are end-to-end encrypted for the members (e2ee.md), so the
//! preview says that instead of showing an empty feed.

use std::time::Duration;

use leptos::prelude::*;

use crate::components::empty_state::EmptyState;
use crate::components::icons::{Icon, IconName};
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, SpaceId};
use crate::reactive::SetIfAlive;
use crate::state::use_app_state;

/// After this long without the channel showing up, "Join channel" can be pressed again.
const JOIN_RETRY_AFTER: Duration = Duration::from_secs(10);

#[component]
pub fn ChannelPreview(space: SpaceId, channel: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let listing = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .browse_channels(space)
        .into_iter()
        .find(|candidate| candidate.conversation == channel)
    })
  });
  let is_showing_details = RwSignal::new(false);
  let is_joining = RwSignal::new(false);
  // Joined here or somewhere else (another device, a slash command): show the channel itself.
  Effect::new(move |_| {
    if listing.get().is_some_and(|listing| listing.is_member)
      && state.read(|backend| backend.conversation(channel)).is_some()
    {
      state.open_conversation(channel);
    }
  });
  let pending = PendingAction::new();
  let join = move |_| {
    is_joining.set(true);
    state.pending_open.set(Some(channel));
    // A refused join shows its error (issue #322) and forgets the open it prepared: otherwise a
    // later join from elsewhere (slash command, invite) jumps into the channel (issue #328).
    pending.run_or_else(
      || state.write(|backend| backend.join_channel(channel)),
      move |_| {
        state.forget_pending_open(channel);
        is_joining.set_if_alive(false);
      },
    );
    // A join that never lands (offline, refused) must not leave the button stuck. A preview that
    // closed in the meantime has nothing to reset.
    set_timeout(
      move || {
        is_joining.try_set(false);
      },
      JOIN_RETRY_AFTER,
    );
  };

  move || {
    let Some(listing) = listing.get() else {
      return view! {
        <section class="channel-preview">
          <EmptyState icon=IconName::Hash title_key="preview-missing-title" body_key="preview-missing" />
        </section>
      }
      .into_any();
    };
    let name = listing.name.clone();
    let topic = listing.topic.clone();
    let has_topic = !topic.is_empty();
    let is_archived = listing.is_archived;
    let has_external = listing.has_external_people;
    view! {
      <section class="channel-preview" aria-label=move || i18n.t_with("preview-label", &[("name", &listing.name)])>
        <header class="channel-header">
          <div class="header-main">
            <h1 class="header-title" translate="no"><Icon name=IconName::Hash class="header-icon" />{name.clone()}</h1>
            <span class="badge">{move || i18n.t("preview-badge")}</span>
            {has_topic.then(|| view! {
              <span class="header-divider" aria-hidden="true"></span>
              <p class="header-topic" translate="no" title=topic.clone()>{topic.clone()}</p>
            })}
          </div>
        </header>
        <div class="preview-body">
          <EmptyState icon=IconName::Lock title_key="preview-title" body_key="preview-body" />
          <Show when=move || is_showing_details.get() fallback=|| ()>
            <dl class="preview-details" id="preview-details">
              <dt>{move || i18n.t("preview-topic")}</dt>
              <dd translate="no">{if has_topic { topic.clone() } else { i18n.t("preview-no-topic") }}</dd>
              {has_external.then(|| view! {
                <dt>{move || i18n.t("external-badge")}</dt>
                <dd>{move || i18n.t("external-badge-detail")}</dd>
              })}
              {is_archived.then(|| view! {
                <dt>{move || i18n.t("badge-archived")}</dt>
                <dd>{move || i18n.t("preview-archived")}</dd>
              })}
            </dl>
          </Show>
        </div>
        <footer class="preview-bar">
          <span class="preview-bar-text">{move || i18n.t_with("preview-bar", &[("name", &name)])}</span>
          <button
            class="btn"
            aria-expanded=move || is_showing_details.get().to_string()
            aria-controls="preview-details"
            on:click=move |_| is_showing_details.update(|open| *open = !*open)
          >
            {move || i18n.t("preview-details-button")}
          </button>
          <button
            class="btn btn-primary"
            prop:disabled=move || is_archived || is_joining.get()
            on:click=join
          >
            {move || i18n.t(if is_joining.get() { "preview-joining" } else { "preview-join" })}
          </button>
        </footer>
      </section>
    }
    .into_any()
  }
}
