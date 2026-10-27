//! Admins' requests to join a private channel the user owns (admin.md §2.4, issue #776): above the
//! entries in Activity and in the channel settings, each with Approve and Decline.
//!
//! Whether "Approve" is offered comes from the core, which checks the person against the roster
//! this device verified; the server's word that they are an admin is not enough (admin.md §2.3).

use leptos::prelude::*;
use teamcraft_client_api::activity::{JoinRequest, JoinRequestStanding};

use crate::clock::{format_date_time, format_relative, now_ms};
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::pending_button::PendingButton;
use crate::components::workspace_dialogs::FailureNote;
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::model::ConversationId;
use crate::state::use_app_state;

/// The requests waiting for the user, above the activity entries; nothing when there are none.
#[component]
pub fn ActivityJoinRequests() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let requests = Memo::new(move |_| state.read(|backend| backend.join_requests()));
  view! {
    <Show when=move || requests.with(|list| !list.is_empty()) fallback=|| ()>
      <section class="join-requests" aria-labelledby="activity-join-requests">
        <h3 id="activity-join-requests" class="join-requests-title">{move || i18n.t("join-requests-title")}</h3>
        <ul class="join-request-list">
          <For each=move || requests.get() key=|request| (request.conversation, request.user, request.requested_at, request.standing) let:request>
            <JoinRequestRow request=request names_channel=true />
          </For>
        </ul>
      </section>
    </Show>
  }
}

/// The requests to join this channel, in its settings; nothing when there are none.
#[component]
pub fn ChannelJoinRequests(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let requests = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .join_requests()
        .into_iter()
        .filter(|request| request.conversation == conversation)
        .collect::<Vec<_>>()
    })
  });
  view! {
    <Show when=move || requests.with(|list| !list.is_empty()) fallback=|| ()>
      <section class="join-requests" aria-labelledby="channel-join-requests">
        <h4 id="channel-join-requests" class="join-requests-title">{move || i18n.t("join-requests-title")}</h4>
        <ul class="join-request-list">
          <For each=move || requests.get() key=|request| (request.user, request.requested_at, request.standing) let:request>
            <JoinRequestRow request=request names_channel=false />
          </For>
        </ul>
      </section>
    </Show>
  }
}

/// Who asks, where and when, what approving means, and the two decisions. A request that only the
/// server calls an admin's can only be declined.
#[component]
fn JoinRequestRow(request: JoinRequest, names_channel: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let (conversation, user) = (request.conversation, request.user);
  let maybe_user = state.read(|backend| backend.user(user));
  let name = maybe_user
    .as_ref()
    .map(|known| known.display_name.clone())
    .unwrap_or_else(|| request.user_name.clone());
  let channel = state
    .read(|backend| backend.conversation(conversation))
    .map(|info| format!("#{}", info.name))
    .unwrap_or_default();
  let summary = move || {
    if names_channel {
      i18n.t_with("join-request-summary", &[("name", &name), ("channel", &channel)])
    } else {
      i18n.t_with("join-request-summary-here", &[("name", &name)])
    }
  };
  let is_approvable = request.standing == JoinRequestStanding::Approvable;
  let relative = format_relative(request.requested_at, now_ms(), locale);
  let absolute = format_date_time(request.requested_at, locale);
  let approving = PendingAction::new();
  let declining = PendingAction::new();
  let is_busy = Signal::derive(move || approving.is_pending() || declining.is_pending());

  view! {
    <li class="join-request">
      <span class="join-request-avatar" aria-hidden="true">
        {match maybe_user {
          Some(known) => view! { <Avatar user=known size=AvatarSize::Small /> }.into_any(),
          None => view! { <Icon name=IconName::Lock /> }.into_any(),
        }}
      </span>
      <div class="join-request-body">
        <span class="join-request-summary" translate="no">{summary}</span>
        <time class="join-request-time" title=absolute>{relative}</time>
        <p class="join-request-hint">
          {move || if is_approvable { i18n.t("join-request-hint") } else { i18n.t("join-request-not-admin") }}
        </p>
        <div class="join-request-actions">
          {is_approvable.then(|| view! {
            <PendingButton
              pending=approving
              class="btn btn-small btn-primary"
              is_disabled=is_busy
              on_click=move |_| approving.run(|| state.write(|backend| backend.approve_admin_join(conversation, user)))
            >
              {move || i18n.t("join-request-approve")}
            </PendingButton>
          })}
          <PendingButton
            pending=declining
            class="btn btn-small"
            is_disabled=is_busy
            on_click=move |_| declining.run(|| state.write(|backend| backend.decline_admin_join(conversation, user)))
          >
            {move || i18n.t("join-request-decline")}
          </PendingButton>
        </div>
        <FailureNote pending=approving />
        <FailureNote pending=declining />
      </div>
    </li>
  }
}
