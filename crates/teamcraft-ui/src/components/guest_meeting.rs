//! What a guest sees after a host let them in: the meeting chat and its call, nothing else of the
//! app (no sidebar, spaces, search or settings). The guest's page runs the same client core as a
//! member's, with an ephemeral device (e2ee.md §4.6, "Meeting guests"; docs/meetings.md).

use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::MeetingId;

use crate::areas::{self, Place, lazy_area};
use crate::auth::{AppConfig, Authenticator};
use crate::backend::Backend;
use crate::calls::CallPhase;
use crate::components::app::connect_calls;
use crate::components::channel_view::ChannelView;
use crate::components::login::{AuthFrame, load_backend};
use crate::components::meetings_view::join_meeting;
use crate::components::settings_tabs::apply_call_prefs;
use crate::i18n::use_i18n;
use crate::meetings::{GuestClient, MeetingsClient, MeetingsHub};
use crate::model::ConversationId;
use crate::reactive::SetIfAlive;
use crate::session::interpret_me_response;
use crate::settings::Settings;
use crate::state::{AppState, ToastTone};

/// A guest whose host let them in.
#[derive(Clone)]
pub struct GuestSession {
  pub meeting: MeetingId,
  pub conversation: ConversationId,
  pub guest_token: String,
  pub auth: Rc<dyn Authenticator>,
}

#[component]
pub fn GuestMeeting(session: GuestSession) -> impl IntoView {
  let i18n = use_i18n();
  let backend = RwSignal::new_local(None::<Result<Rc<dyn Backend>, String>>);
  let has_left = RwSignal::new(false);
  let settings = Settings::load();
  let session = StoredValue::new_local(session);

  spawn_local(async move {
    let auth = session.with_value(|session| session.auth.clone());
    let loaded = async {
      let response = auth
        .api_get("/api/v1/me")
        .await
        .map_err(|error| error.to_string())?;
      let me = interpret_me_response(response).map_err(|problem| format!("{problem:?}"))?;
      load_backend(auth.clone(), &me)
        .await
        .map_err(|error| error.to_string())
    }
    .await;
    backend.set_if_alive(Some(loaded));
  });

  view! {
    {move || {
      if has_left.get() {
        return view! {
          <AuthFrame>
            <h1>{move || i18n.t("guest-left-title")}</h1>
            <p role="status">{move || i18n.t("guest-left")}</p>
          </AuthFrame>
        }
        .into_any();
      }
      match backend.get() {
        None => view! {
          <AuthFrame>
            <p class="auth-status" role="status">
              <span class="spinner" aria-hidden="true"></span>
              <span>{move || i18n.t("guest-securing")}</span>
            </p>
          </AuthFrame>
        }
        .into_any(),
        Some(Ok(loaded)) => view! { <GuestShell backend=loaded settings=settings session=session has_left=has_left /> }.into_any(),
        Some(Err(detail)) => {
          tracing::warn!(%detail, "could not open the guest's meeting");
          view! {
            <AuthFrame>
              <h1>{move || i18n.t("guest-failed-title")}</h1>
              <p role="alert">{move || i18n.t("guest-setup-failed")}</p>
            </AuthFrame>
          }
          .into_any()
        },
      }
    }}
  }
}

#[component]
fn GuestShell(
  backend: Rc<dyn Backend>,
  settings: Settings,
  session: StoredValue<GuestSession, LocalStorage>,
  has_left: RwSignal<bool>,
) -> impl IntoView {
  let i18n = use_i18n();
  let (conversation, auth, meeting, token) = session.with_value(|session| {
    (
      session.conversation,
      session.auth.clone(),
      session.meeting,
      session.guest_token.clone(),
    )
  });
  let change_source = backend.clone();
  let state = AppState::new(backend, settings, None);
  provide_context(state);
  provide_context(MeetingsHub::new(MeetingsClient::new(auth.clone())));
  provide_context(crate::groups::GroupsHub::new(crate::rest::RestClient::new(
    auth.clone(),
  )));
  provide_context(crate::channel_policy::ChannelPolicyHub::new(
    crate::rest::RestClient::new(auth.clone()),
  ));
  provide_context(crate::profiles::ProfileHub::new(crate::rest::RestClient::new(
    auth.clone(),
  )));
  provide_context(crate::space_images::SpaceImageHub::new(auth.clone()));
  crate::startup::first_view();
  change_source.set_change_notifier(Rc::new(move || state.backend_changed()));
  crate::feedback::set_failure_sink(move |failure| {
    state.show_error(crate::feedback::failure_message(i18n, &failure), failure.detail);
  });
  state.open_conversation(conversation);
  if untrack(|| state.read(|backend| backend.has_calls() && !backend.has_simulated_calls())) {
    connect_calls(state);
  }
  let call_prefs = Memo::new(move |_| settings.prefs.get().calls);
  Effect::new(move |_| apply_call_prefs(state.call, &call_prefs.get()));

  let title = move || {
    state
      .read(|backend| backend.conversation(conversation))
      .map(|conversation| conversation.name)
      .unwrap_or_default()
  };
  let is_leaving = RwSignal::new(false);
  let leave = move |_| {
    is_leaving.set(true);
    let (auth, token) = (auth.clone(), token.clone());
    spawn_local(async move {
      let client = GuestClient::new(AppConfig::load());
      // The server ends the session either way; this device's keys and data go with the page.
      let _ = client.leave(meeting, &token).await;
      if let Err(error) = crate::remote_backend::wipe_guest_store().await {
        tracing::warn!(%error, "could not delete the guest device's local data");
      }
      let _ = auth.logout().await;
      has_left.set_if_alive(true);
    });
  };

  view! {
    <div class="app guest-meeting">
      <header class="guest-meeting-bar">
        <h1 class="guest-meeting-title" translate="no">{title}</h1>
        <span class="grow"></span>
        <button class="btn btn-primary" on:click=move |_| join_meeting(state, i18n, conversation)>
          {move || i18n.t("guest-join-call")}
        </button>
        <button class="btn" prop:disabled=move || is_leaving.get() on:click=leave>
          {move || i18n.t("guest-leave")}
        </button>
      </header>
      <main id="main-content" class="main" tabindex="-1">
        <ChannelView conversation=conversation />
      </main>
      {move || match state.call.phase.get() {
        CallPhase::PreJoin => state
          .call
          .maybe_request
          .get()
          .map(|request| lazy_area(Place::Screen, move || areas::call_prejoin(request))),
        CallPhase::Active if !state.call.is_minimized.get() => state
          .call
          .maybe_request
          .get()
          .map(|request| lazy_area(Place::Screen, move || areas::call_screen(request))),
        _ => None,
      }}
      <div class="toasts" role="status">
        <For each=move || state.toasts.get() key=|toast| toast.id let:toast>
          {
            let id = toast.id;
            let is_error = matches!(toast.tone, ToastTone::Error { .. });
            view! {
              <div class=if is_error { "toast toast-error" } else { "toast" } role=is_error.then_some("alert")>
                <div class="toast-body"><span>{toast.text}</span></div>
                <button class="btn btn-small" on:click=move |_| state.dismiss_toast(id)>
                  {move || i18n.t("close")}
                </button>
              </div>
            }
          }
        </For>
      </div>
    </div>
  }
}
