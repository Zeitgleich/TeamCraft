//! The page behind a meeting link (`/meeting/<token>#v=<fingerprint>`, a personal
//! `#invite=<token>` link, or an old `/meeting/<id>#invite=<token>`) for somebody without an
//! account: a guest enters a name, waits in the lobby until the organizer's device lets them in,
//! then is in. No sign-in. The link holds no secret: the page makes the guest's keys itself
//! (e2ee.md §4.6, "Meeting guests").

use std::rc::Rc;
use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::remote::CoreStatus;
use teamcraft_proto::{ConversationId, LobbyState, MeetingId, UserId};

use crate::areas::{self, Place, lazy_area};
use crate::auth::guest::GuestAuth;
use crate::auth::{AppConfig, Authenticator};
use crate::components::guest_meeting::GuestSession;
use crate::components::login::AuthFrame;
use crate::i18n::use_i18n;
use crate::meetings::{GuestClient, GuestLink, MeetingError};
use crate::reactive::{SetIfAlive, pause, use_interval};
use crate::remote_backend;

/// How often the lobby is asked whether the guest was admitted.
const POLL_MS: u64 = 2000;
/// How long the page waits for its client core to open before it gives up.
const CORE_PATIENCE: Duration = Duration::from_secs(20);
const CORE_POLL: Duration = Duration::from_millis(300);
/// The device name a guest's device shows in the member list.
const DEVICE_NAME: &str = "Guest browser";
/// The query a member's "sign in" adds to the meeting link, so the page shows the sign-in instead.
pub const SIGN_IN_QUERY: &str = "sign-in";

#[derive(Clone, Debug, PartialEq, Eq)]
enum GuestPhase {
  AskName,
  Joining,
  /// In the lobby, with the page's device being set up for the encryption.
  Waiting,
  Admitted,
  Denied,
  Failed(FailureKind),
}

/// What went wrong, in terms the guest can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind {
  /// The link was used, expired, cancelled, made anew or is not valid.
  LinkUnusable,
  /// The page could not set up its encryption.
  SetupFailed,
  Network,
}

/// What the page knows about the guest once the server let them into the lobby.
#[derive(Clone)]
struct Joined {
  guest_token: String,
  meeting: MeetingId,
  conversation_id: ConversationId,
  auth: Rc<dyn Authenticator>,
}

#[component]
pub fn GuestJoinPage(link: GuestLink) -> impl IntoView {
  let i18n = use_i18n();
  let config = AppConfig::load();
  let client = GuestClient::new(config.clone());
  let phase = RwSignal::new(GuestPhase::AskName);
  let name = RwSignal::new(String::new());
  // A member opens the same link; "sign in" shows them the sign-in, which comes back to it.
  let sign_in_href = StoredValue::new(
    web_sys::window()
      .and_then(|window| window.location().pathname().ok())
      .map(|path| format!("{path}?{SIGN_IN_QUERY}")),
  );
  let is_one_time = link.maybe_invite_token.is_some();
  let link = StoredValue::new(link);
  let joined = StoredValue::new_local(None::<Joined>);
  let is_device_ready = RwSignal::new(false);
  crate::startup::first_view();
  // What an earlier guest page left in this browser (closed without "Leave") goes now.
  spawn_local(delete_stale_guest_store());

  let join: StoredValue<Rc<dyn Fn(leptos::ev::SubmitEvent)>, LocalStorage> = StoredValue::new_local({
    let client = client.clone();
    Rc::new(move |ev: leptos::ev::SubmitEvent| {
      ev.prevent_default();
      if name.get_untracked().trim().is_empty() {
        return;
      }
      let parts = link.get_value();
      phase.set(GuestPhase::Joining);
      // A one-time token stays in memory from here on, not in the address bar, the history or a
      // bookmark made later.
      strip_link_fragment();
      let (client, config) = (client.clone(), config.clone());
      spawn_local(async move {
        let display_name = name.get_untracked().trim().to_owned();
        let outcome = client.join(&parts, &display_name).await;
        let response = match outcome {
          Ok(response) => response,
          Err(error) => {
            phase.set_if_alive(GuestPhase::Failed(failure_of(&error)));
            return;
          },
        };
        if response.lobby_state == LobbyState::Denied {
          phase.set_if_alive(GuestPhase::Denied);
          return;
        }
        let auth: Rc<dyn Authenticator> =
          Rc::new(GuestAuth::new(config.clone(), response.guest_token.clone()));
        let is_gone = joined
          .try_set_value(Some(Joined {
            guest_token: response.guest_token.clone(),
            meeting: response.meeting_id,
            conversation_id: response.conversation_id,
            auth: auth.clone(),
          }))
          .is_some();
        if is_gone {
          return;
        }
        phase.set_if_alive(GuestPhase::Waiting);
        let setup = prepare_device(
          &client,
          &config,
          auth,
          &response.guest_token,
          response.user_id,
          response.conversation_id,
          response.meeting_id,
          // A link without `v` (mailed before #987) takes the server's word (e2ee.md §4.6).
          parts
            .maybe_organizer_fingerprint
            .as_deref()
            .or(response.maybe_organizer_fingerprint.as_deref()),
        )
        .await;
        match setup {
          Ok(()) => is_device_ready.set_if_alive(true),
          Err(kind) => phase.set_if_alive(GuestPhase::Failed(kind)),
        }
      });
    }) as Rc<dyn Fn(leptos::ev::SubmitEvent)>
  });

  use_interval(POLL_MS, move || {
    if phase.get_untracked() != GuestPhase::Waiting || !is_device_ready.get_untracked() {
      return;
    }
    let Some((guest_token, meeting)) = joined.with_value(|joined| {
      joined
        .as_ref()
        .map(|joined| (joined.guest_token.clone(), joined.meeting))
    }) else {
      return;
    };
    let client = client.clone();
    spawn_local(async move {
      match client.lobby_state(meeting, &guest_token).await {
        Ok(LobbyState::Admitted) => phase.set_if_alive(GuestPhase::Admitted),
        Ok(LobbyState::Denied) => phase.set_if_alive(GuestPhase::Denied),
        Ok(LobbyState::Waiting | LobbyState::Unknown(_)) => {},
        // A hiccup keeps waiting; a refused session ends it.
        Err(MeetingError::Refused { .. }) => {
          phase.set_if_alive(GuestPhase::Failed(FailureKind::LinkUnusable))
        },
        Err(_) => {},
      }
    });
  });

  view! {
    {move || {
      if phase.get() == GuestPhase::Admitted
        && let Some(joined) = joined.get_value()
      {
        let session = GuestSession {
          meeting: joined.meeting,
          conversation: joined.conversation_id,
          guest_token: joined.guest_token,
          auth: joined.auth,
        };
        return lazy_area(Place::Screen, move || areas::guest_meeting(session.clone()));
      }
      view! {
        <AuthFrame>
          {move || match phase.get() {
            GuestPhase::AskName | GuestPhase::Joining => {
              let is_joining = phase.get() == GuestPhase::Joining;
              view! {
                <form class="auth-form" on:submit=move |ev| join.with_value(|join| join(ev))>
                  <h1>{move || i18n.t("guest-title")}</h1>
                  <p class="muted-text">{move || i18n.t("guest-subtitle")}</p>
                  <label for="guest-name">{move || i18n.t("guest-name")}</label>
                  <input
                    id="guest-name"
                    class="input"
                    autocomplete="name"
                    data-autofocus
                    prop:value=move || name.get()
                    on:input=move |ev| name.set(event_target_value(&ev))
                  />
                  <div class="auth-actions">
                    <button type="submit" class="btn btn-primary" prop:disabled=is_joining>
                      {move || i18n.t("guest-join")}
                    </button>
                  </div>
                  {(!is_one_time)
                    .then(|| sign_in_href.get_value())
                    .flatten()
                    .map(|href| view! {
                      <p class="muted-text small">
                        <a href=href>{move || i18n.t("guest-sign-in")}</a>
                      </p>
                    })}
                </form>
              }
              .into_any()
            },
            GuestPhase::Waiting | GuestPhase::Admitted => {
              view! {
                <h1>{move || i18n.t("guest-waiting-title")}</h1>
                <p class="auth-status" role="status">
                  <span class="spinner" aria-hidden="true"></span>
                  <span>{move || i18n.t("guest-waiting")}</span>
                </p>
                <p class="muted-text small">{move || i18n.t("guest-waiting-encryption")}</p>
              }
              .into_any()
            },
            GuestPhase::Denied => {
              view! {
                <h1>{move || i18n.t("guest-denied-title")}</h1>
                <p role="alert">{move || i18n.t("guest-denied")}</p>
              }
              .into_any()
            },
            GuestPhase::Failed(kind) => {
              let key = match kind {
                FailureKind::LinkUnusable => "guest-link-unusable",
                FailureKind::SetupFailed => "guest-setup-failed",
                FailureKind::Network => "guest-network",
              };
              view! {
                <h1>{move || i18n.t("guest-failed-title")}</h1>
                <p role="alert">{move || i18n.t(key)}</p>
              }
              .into_any()
            },
          }}
        </AuthFrame>
      }
      .into_any()
    }}
  }
}

/// Sets up the encryption of the guest's page while they wait: the client core opens and enrolls an
/// ephemeral device (no passphrase, no backup; its keys are the guest's own), keeps the organizer's
/// key from the link (or, for a link without one, the one the server names), and tells the
/// hosts the device is ready, so an organizer's device can let it in (e2ee.md §4.6, "Meeting
/// guests").
#[allow(clippy::too_many_arguments)]
async fn prepare_device(
  client: &GuestClient,
  config: &AppConfig,
  auth: Rc<dyn Authenticator>,
  guest_token: &str,
  user_id: UserId,
  conversation: ConversationId,
  meeting: MeetingId,
  maybe_organizer_fingerprint: Option<&str>,
) -> Result<(), FailureKind> {
  configure_core(auth, config);
  let failed = |step: &str, error: String| {
    tracing::warn!(step, %error, "could not set up the guest's device");
    FailureKind::SetupFailed
  };
  let mut waited = Duration::ZERO;
  loop {
    match remote_backend::core_status(user_id).await {
      Ok(CoreStatus::NeedsEnrollment { .. }) => {
        remote_backend::enroll_guest(DEVICE_NAME)
          .await
          .map_err(|error| failed("enroll", error))?;
        break;
      },
      Ok(CoreStatus::Ready { .. }) => break,
      Ok(CoreStatus::NotStarted) if waited < CORE_PATIENCE => {
        pause(CORE_POLL).await;
        waited += CORE_POLL;
      },
      Ok(other) => return Err(failed("core", format!("{other:?}"))),
      Err(error) => return Err(failed("core", error)),
    }
  }
  // Without any fingerprint the device fails closed: it joins no group (e2ee.md §4.6).
  if let Some(fingerprint) = maybe_organizer_fingerprint {
    remote_backend::guest_anchor(conversation, fingerprint)
      .await
      .map_err(|error| failed("anchor", error))?;
  }
  // Only a nudge: the hosts also see the device on their next reload of the lobby, so a failure
  // here keeps the guest waiting rather than failing the page.
  if let Err(error) = client.device_ready(meeting, guest_token).await {
    tracing::warn!(%error, "could not tell the hosts that the guest's device is ready");
  }
  Ok(())
}

/// Removes the fragment (`#invite=…&v=…`) from the address (history entry replaced, nothing reloads).
fn strip_link_fragment() {
  let Some(window) = web_sys::window() else {
    return;
  };
  let (Ok(history), Ok(path)) = (window.history(), window.location().pathname()) else {
    return;
  };
  // A failure leaves the address as it was; the link is still in memory.
  let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&path));
}

/// The page runs the guest's client core itself, signing with the guest's session token.
#[cfg(target_arch = "wasm32")]
fn configure_core(auth: Rc<dyn Authenticator>, config: &AppConfig) {
  crate::web_core::configure(auth, &config.server_url);
}

#[cfg(target_arch = "wasm32")]
async fn delete_stale_guest_store() {
  crate::web_core::delete_stale_guest_store().await;
}

#[cfg(not(target_arch = "wasm32"))]
async fn delete_stale_guest_store() {}

#[cfg(not(target_arch = "wasm32"))]
fn configure_core(_auth: Rc<dyn Authenticator>, _config: &AppConfig) {}

fn failure_of(error: &MeetingError) -> FailureKind {
  match error {
    MeetingError::Refused { .. } | MeetingError::Unreadable(_) => FailureKind::LinkUnusable,
    MeetingError::Auth(_) => FailureKind::Network,
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::auth::AuthError;

  #[test]
  fn refusals_mean_the_link_cannot_be_used() {
    let refused = MeetingError::Refused {
      status: 401,
      code: "invalid_token".into(),
    };
    assert_eq!(failure_of(&refused), FailureKind::LinkUnusable);
    assert_eq!(
      failure_of(&MeetingError::Auth(AuthError::Network("down".into()))),
      FailureKind::Network
    );
  }
}
