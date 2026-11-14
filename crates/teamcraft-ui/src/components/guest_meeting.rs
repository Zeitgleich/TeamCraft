//! What a guest sees after a host let them in: the meeting's call, opened straight away on its
//! pre-join screen (#1929), and the meeting chat behind it; nothing else of the app (no sidebar,
//! spaces, search or settings). The guest's page runs the same client core as a
//! member's, with an ephemeral device (e2ee.md §4.6, "Meeting guests"; docs/meetings.md).

use std::rc::Rc;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::MeetingId;

use crate::areas::{self, Place, lazy_area};
use crate::auth::{AppConfig, Authenticator};
use crate::backend::Backend;
use crate::calls::{CallEndReason, CallKind, CallPhase, CallRequest};
use crate::components::app::connect_calls;
use crate::components::channel_view::ChannelView;
use crate::components::guest_devices::GuestDevices;
use crate::components::guest_end::{GuestEnd, GuestEndScreen};
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
  /// The call module and the choices of the guest's device check (#1928): admission joins the
  /// call with them. `None` in a browser without calls.
  pub maybe_devices: Option<GuestDevices>,
  /// The meeting link to knock again with after leaving (#1933); none for a one-time invitation.
  pub maybe_rejoin: Option<String>,
}

#[component]
pub fn GuestMeeting(session: GuestSession) -> impl IntoView {
  let i18n = use_i18n();
  let backend = RwSignal::new_local(None::<Result<Rc<dyn Backend>, String>>);
  // How the guest's part ended, and the meeting's title for the end screen (#1933).
  let maybe_end = RwSignal::new(None::<(GuestEnd, String)>);
  let settings = session
    .maybe_devices
    .as_ref()
    .map_or_else(Settings::load, |devices| devices.settings);
  let session = StoredValue::new_local(session);

  spawn_local(async move {
    let (auth, maybe_calls) = session.with_value(|session| {
      (
        session.auth.clone(),
        session
          .maybe_devices
          .as_ref()
          .map(|devices| devices.calls.clone()),
      )
    });
    let loaded = async {
      let response = auth
        .api_get("/api/v1/me")
        .await
        .map_err(|error| error.to_string())?;
      let me = interpret_me_response(response).map_err(|problem| format!("{problem:?}"))?;
      load_backend(auth.clone(), &me, maybe_calls)
        .await
        .map_err(|error| error.to_string())
    }
    .await;
    backend.set_if_alive(Some(loaded));
  });

  view! {
    {move || {
      if let Some((end, title)) = maybe_end.get() {
        return view! { <GuestEndScreen end=end maybe_title=title /> }.into_any();
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
        Some(Ok(loaded)) => view! { <GuestShell backend=loaded settings=settings session=session maybe_end=maybe_end /> }.into_any(),
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
  maybe_end: RwSignal<Option<(GuestEnd, String)>>,
) -> impl IntoView {
  let i18n = use_i18n();
  let (conversation, auth, meeting, token, maybe_call, maybe_rejoin) = session.with_value(|session| {
    (
      session.conversation,
      session.auth.clone(),
      session.meeting,
      session.guest_token.clone(),
      session.maybe_devices.as_ref().map(|devices| devices.call),
      session.maybe_rejoin.clone(),
    )
  });
  let change_source = backend.clone();
  // The device check's call state goes on into the call (#1928): its choices are the call's.
  let state = match maybe_call {
    Some(call) => AppState::new(backend, settings, None).with_call(call),
    None => AppState::new(backend, settings, None),
  };
  provide_context(state);
  provide_context(MeetingsHub::for_guest(
    MeetingsClient::new(auth.clone()),
    conversation,
  ));
  provide_context(crate::channel_policy::ChannelPolicyHub::new(
    crate::rest::RestClient::new(auth.clone()),
  ));
  let picture_retries = crate::picture_retries::PictureRetries::new();
  provide_context(picture_retries);
  provide_context(crate::profiles::ProfileHub::new(
    crate::rest::RestClient::new(auth.clone()),
    picture_retries,
  ));
  provide_context(crate::space_images::SpaceImageHub::new(
    auth.clone(),
    picture_retries,
  ));
  crate::startup::first_view();
  // The call's duration ticks for a guest too (the member app starts it in `app.rs`).
  crate::calls::run_call_clock(state.call);
  change_source.set_change_notifier(Rc::new(move || state.backend_changed()));
  crate::feedback::set_failure_sink(move |failure| state.show_failure(i18n, &failure));
  state.open_conversation(conversation);
  if untrack(|| state.read(|backend| backend.has_calls() && !backend.has_simulated_calls())) {
    connect_calls(state);
    // The guest knocked to join the call (#1929): with the choices of their device check, admission
    // joins it at once (#1928); without one, the pre-join screen opens. The chat stays behind it.
    if maybe_call.is_some() {
      state.call.join(CallRequest {
        conversation,
        kind: CallKind::Group,
      });
    } else {
      join_meeting(state, i18n, conversation);
    }
  }
  let call_prefs = Memo::new(move |_| settings.prefs.get().calls);
  // A device check applied the settings already, and the guest changed them since (muted, camera
  // off): they apply again only when they change.
  let applied = maybe_call.map(|_| untrack(|| call_prefs.get()));
  Effect::new(move |previous: Option<crate::prefs::CallPrefs>| {
    let previous = previous.or_else(|| applied.clone());
    let calls = call_prefs.get();
    state.call.phase.track();
    apply_call_prefs(state.call, previous.as_ref(), &calls)
  });

  let title = move || {
    state
      .read(|backend| backend.conversation(conversation))
      .map(|conversation| conversation.name)
      .unwrap_or_default()
  };
  // The guest's part is over (left, ended, removed): the session ends and this device's keys
  // and data go, then the end screen shows (#1933). Once only.
  let is_leaving = RwSignal::new(false);
  let end_session: StoredValue<Rc<dyn Fn(GuestEnd)>, LocalStorage> =
    StoredValue::new_local(Rc::new(move |end| {
      if is_leaving.get_untracked() {
        return;
      }
      is_leaving.set(true);
      let meeting_title = untrack(title);
      let (auth, token) = (auth.clone(), token.clone());
      spawn_local(async move {
        let client = GuestClient::new(AppConfig::load());
        // The server ends the session either way; this device's keys and data go with the page.
        let _ = client.leave(meeting, &token).await;
        crate::guest_resume::StoredGuestSession::forget();
        if let Err(error) = crate::remote_backend::wipe_guest_store().await {
          tracing::warn!(%error, "could not delete the guest device's local data");
        }
        let _ = auth.logout().await;
        maybe_end.set_if_alive(Some((end, meeting_title)));
      });
    }));
  let rejoin = StoredValue::new(maybe_rejoin);
  let leave = move |_| {
    end_session.with_value(|end| {
      end(GuestEnd::Left {
        maybe_rejoin: rejoin.get_value(),
      })
    })
  };
  follow_call_end(state, end_session, rejoin);

  view! {
    <div class="app guest-meeting">
      // The call screen has its own title and hang-up: while the guest is in the call, this bar
      // would only repeat the title and offer a second way out.
      <header class="guest-meeting-bar" prop:hidden=move || state.call.phase.get() != CallPhase::Idle>
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

/// A guest's call ending is their part of the meeting ending (#1933): hanging up, a host ending
/// the meeting for everyone or removing them shows the end screen. A join that failed or a lost
/// connection keeps the guest in the meeting's chat, which says why (`connect_calls`).
fn follow_call_end(
  state: AppState,
  end_session: StoredValue<Rc<dyn Fn(GuestEnd)>, LocalStorage>,
  rejoin: StoredValue<Option<String>>,
) {
  let call = state.call;
  Effect::new(move |previous: Option<(CallPhase, u64)>| {
    let phase = call.phase.get();
    let failures = untrack(|| state.read(|backend| backend.call_join_failures()));
    if let Some((CallPhase::Active, failures_before)) = previous
      && phase == CallPhase::Idle
      && failures == failures_before
    {
      let maybe_reason = untrack(|| state.read(|backend| backend.call_end_reason()));
      if let Some(end) = end_of_call(maybe_reason, rejoin.get_value()) {
        end_session.with_value(|end_session| end_session(end));
      }
    }
    (phase, failures)
  });
}

/// How a guest's call ending ends their part of the meeting: `None` keeps them in the chat (a
/// lost connection, which the chat explains).
fn end_of_call(maybe_reason: Option<CallEndReason>, maybe_rejoin: Option<String>) -> Option<GuestEnd> {
  match maybe_reason {
    Some(CallEndReason::EndedByHost { .. }) => Some(GuestEnd::Ended),
    Some(CallEndReason::RemovedByHost { .. }) => Some(GuestEnd::Removed),
    Some(CallEndReason::ConnectionLost | CallEndReason::OtherSideLeft) => None,
    None => Some(GuestEnd::Left { maybe_rejoin }),
  }
}

#[cfg(test)]
mod tests {
  use crate::model::id_from_raw;

  use super::*;

  const LINK: &str = "/meeting/abc#v=00ff";

  #[test]
  fn hanging_up_is_leaving_with_the_link_to_rejoin() {
    assert_eq!(
      end_of_call(None, Some(LINK.to_owned())),
      Some(GuestEnd::Left {
        maybe_rejoin: Some(LINK.to_owned())
      })
    );
  }

  #[test]
  fn a_host_ending_or_removing_offers_no_rejoin() {
    let by = id_from_raw(7);
    assert_eq!(
      end_of_call(Some(CallEndReason::EndedByHost { by }), Some(LINK.to_owned())),
      Some(GuestEnd::Ended)
    );
    assert_eq!(
      end_of_call(Some(CallEndReason::RemovedByHost { by }), Some(LINK.to_owned())),
      Some(GuestEnd::Removed)
    );
  }

  #[test]
  fn a_lost_connection_keeps_the_guest_in_the_meeting() {
    assert_eq!(end_of_call(Some(CallEndReason::ConnectionLost), None), None);
  }
}
