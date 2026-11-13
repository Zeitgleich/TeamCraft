//! The page behind a meeting link (`/meeting/<token>#v=<fingerprint>`, a personal
//! `#invite=<token>` link, or an old `/meeting/<id>#invite=<token>`) for somebody without an
//! account: a guest enters a name, waits in the lobby until the organizer's device lets them in,
//! then is in. No sign-in. The link holds no secret: the page makes the guest's keys itself
//! (e2ee.md §4.6, "Meeting guests"). Once in the lobby, the page moves to the guest's own address
//! (`#r=<key>`), which brings them back after a reload without knocking again (`crate::guest_resume`).

use std::rc::Rc;
use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::remote::CoreStatus;
use teamcraft_crypto::guest_link::code_of_fingerprint;
use teamcraft_proto::{ConversationId, LobbyState, MeetingId, UserId};

use crate::areas::{self, Place, lazy_area};
use crate::auth::guest::GuestAuth;
use crate::auth::{AppConfig, Authenticator};
use crate::components::guest_devices::{FORM_ID, GuestDevices, GuestForm};
use crate::components::guest_meeting::GuestSession;
use crate::components::login::AuthFrame;
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::guest_resume::StoredGuestSession;
use crate::i18n::use_i18n;
use crate::meetings::{GuestClient, GuestLink, MeetingError};
use crate::reactive::{SetIfAlive, pause, use_interval};
use crate::remote_backend;

/// How often the lobby is asked whether the guest was admitted.
const NAME_ID: &str = "guest-name";
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

/// How far the page got: the name and devices, the lobby, or in the meeting. The view is built
/// anew only when this changes, so the device check keeps running while the knock is sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
  Ask,
  Lobby,
  In,
}

/// Whether the page has its call module for the device check (#1928).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DevicesState {
  Loading,
  Ready,
  /// No calls in this browser (or the module did not load): the name alone, as before.
  Unavailable,
}

/// What went wrong, in terms the guest can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind {
  /// The link was used, expired, cancelled, made anew or is not valid.
  LinkUnusable,
  /// The page could not set up its encryption.
  SetupFailed,
  Network,
  /// A resumed session whose device database is gone; the page starts over.
  DeviceLost,
}

/// Whether the page's session is new or a reloaded one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionOrigin {
  Joined,
  Resumed,
}

/// The page's state that the join, a resumed session and the lobby poll share.
#[derive(Clone, Copy)]
struct LobbyPage {
  phase: RwSignal<GuestPhase>,
  joined: StoredValue<Option<Joined>, LocalStorage>,
  is_device_ready: RwSignal<bool>,
  /// The codes the guest may compare out of band (#987 review): their own key's, which the hosts
  /// see on the lobby entry, and the organizer's from the link, which the organizer's app shows.
  codes: RwSignal<Option<(String, String)>>,
  is_organizer_app_outdated: RwSignal<bool>,
  /// The fingerprint still to report with the knock, when the first report failed.
  unreported: StoredValue<Option<String>>,
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
  // A reload of the guest's own address carries on with the session this browser keeps.
  let maybe_resumed = link
    .maybe_resume_key
    .as_deref()
    .and_then(|key| StoredGuestSession::load(key));
  let phase = RwSignal::new(if maybe_resumed.is_some() {
    GuestPhase::Waiting
  } else {
    GuestPhase::AskName
  });
  let name = RwSignal::new(String::new());
  let errors = FormErrors::new();
  let name_errors = move || {
    let mut found = FieldErrors::new();
    if name.with(|name| name.trim().is_empty()) {
      found.push(NAME_ID, i18n.t("form-error-name"));
    }
    found
  };
  Effect::new(move |_| errors.follow(name_errors()));
  // A member opens the same link; "sign in" shows them the sign-in, which comes back to it.
  let sign_in_href = StoredValue::new(
    web_sys::window()
      .and_then(|window| window.location().pathname().ok())
      .map(|path| format!("{path}?{SIGN_IN_QUERY}")),
  );
  let is_one_time = link.maybe_invite_token.is_some();
  let link = StoredValue::new(link);
  let page = LobbyPage {
    phase,
    joined: StoredValue::new_local(None::<Joined>),
    is_device_ready: RwSignal::new(false),
    codes: RwSignal::new(None),
    is_organizer_app_outdated: RwSignal::new(false),
    unreported: StoredValue::new(None),
  };
  let LobbyPage {
    codes,
    is_organizer_app_outdated,
    ..
  } = page;
  crate::startup::first_view();
  // The device check (#1928) runs on the page's call module, which comes with the call screens'
  // module: the meeting needs both next anyway.
  let devices = StoredValue::new_local(None::<GuestDevices>);
  let devices_state = RwSignal::new(DevicesState::Loading);
  spawn_local(async move {
    let state = match areas::guest_devices(()).await {
      Ok(Some(found)) => {
        // Gone with the page: nothing left to set up.
        let _ = devices.try_set_value(Some(found));
        DevicesState::Ready
      },
      Ok(None) => DevicesState::Unavailable,
      Err(error) => {
        tracing::warn!(%error, "the guest's device check did not load");
        DevicesState::Unavailable
      },
    };
    devices_state.set_if_alive(state);
  });
  match maybe_resumed {
    Some(session) => spawn_local(enter(
      session,
      SessionOrigin::Resumed,
      client.clone(),
      config.clone(),
      page,
    )),
    // What an earlier guest page left in this browser (closed without "Leave") goes now.
    None => spawn_local(delete_stale_guest_store()),
  }

  let join: StoredValue<Rc<dyn Fn(leptos::ev::SubmitEvent)>, LocalStorage> = StoredValue::new_local({
    let client = client.clone();
    Rc::new(move |ev: leptos::ev::SubmitEvent| {
      ev.prevent_default();
      let found = untrack(name_errors);
      if !found.is_empty() {
        errors.refuse(FORM_ID, found);
        return;
      }
      let parts = link.get_value();
      phase.set(GuestPhase::Joining);
      // A new knock in this tab replaces whatever session it kept.
      StoredGuestSession::forget();
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
        let session = StoredGuestSession {
          meeting: response.meeting_id,
          conversation: response.conversation_id,
          user_id: response.user_id,
          guest_token: response.guest_token,
          // A link without `v` (mailed before #987) takes the server's word (e2ee.md §4.6).
          maybe_organizer_fingerprint: parts
            .maybe_organizer_fingerprint
            .clone()
            .or(response.maybe_organizer_fingerprint),
        };
        if let Some(address) = session.keep(&parts.handle) {
          replace_address(&address);
        }
        enter(session, SessionOrigin::Joined, client, config, page).await;
      });
    }) as Rc<dyn Fn(leptos::ev::SubmitEvent)>
  });

  use_interval(POLL_MS, move || {
    if phase.get_untracked() != GuestPhase::Waiting || !page.is_device_ready.get_untracked() {
      return;
    }
    spawn_local(check_lobby(client.clone(), page));
  });

  let stage = Memo::new(move |_| match phase.get() {
    GuestPhase::AskName | GuestPhase::Joining => Stage::Ask,
    GuestPhase::Admitted => Stage::In,
    GuestPhase::Waiting | GuestPhase::Denied | GuestPhase::Failed(_) => Stage::Lobby,
  });
  let is_joining = Signal::derive(move || phase.get() == GuestPhase::Joining);
  let maybe_sign_in = StoredValue::new((!is_one_time).then(|| sign_in_href.get_value()).flatten());

  view! {
    {move || {
      match (stage.get(), devices_state.get()) {
        (Stage::In, _) => {
          if let Some(joined) = page.joined.get_value() {
            let session = GuestSession {
              meeting: joined.meeting,
              conversation: joined.conversation_id,
              guest_token: joined.guest_token,
              auth: joined.auth,
              maybe_devices: devices.get_value(),
            };
            return lazy_area(Place::Screen, move || areas::guest_meeting(session.clone()));
          }
        },
        (Stage::Ask, DevicesState::Loading) => {
          return view! {
            <AuthFrame>
              <p class="auth-status auth-status-quiet" role="status">
                <span class="spinner" aria-hidden="true"></span>
                <span>{move || i18n.t("guest-preparing")}</span>
              </p>
            </AuthFrame>
          }
          .into_any();
        },
        (Stage::Ask, DevicesState::Ready) => {
          if let Some(found) = devices.get_value() {
            let form = GuestForm {
              devices: found,
              name,
              errors,
              is_joining,
              submit: join,
              maybe_sign_in: maybe_sign_in.get_value(),
            };
            return lazy_area(Place::Screen, move || areas::guest_device_setup(form.clone()));
          }
        },
        _ => {},
      }
      view! {
        <AuthFrame>
          {move || match phase.get() {
            GuestPhase::AskName | GuestPhase::Joining => {
              let is_joining = phase.get() == GuestPhase::Joining;
              view! {
                <form id=FORM_ID class="auth-form" on:submit=move |ev| join.with_value(|join| join(ev))>
                  <h1>{move || i18n.t("guest-title")}</h1>
                  <p class="muted-text">{move || i18n.t("guest-subtitle")}</p>
                  <GuestNameField name=name errors=errors />
                  <div class="auth-actions">
                    <button type="submit" class="btn btn-primary" prop:disabled=is_joining>
                      {move || i18n.t("guest-join")}
                    </button>
                  </div>
                  {maybe_sign_in
                    .get_value()
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
                // A small preview while waiting; the choices stay changeable (#1928).
                {devices.get_value().map(|found| {
                  lazy_area(Place::Inline, move || areas::guest_waiting_devices(found.clone(), name))
                })}
                {move || is_organizer_app_outdated.get().then(|| view! {
                  <p role="alert">{move || i18n.t("guest-organizer-app-outdated")}</p>
                })}
                <p class="muted-text small">{move || i18n.t("guest-waiting-encryption")}</p>
                {move || codes.get().map(|(own, organizer)| view! {
                  <dl class="guest-codes">
                    <dt>{move || i18n.t("guest-own-code")}</dt>
                    <dd class="mono" translate="no">{own}</dd>
                    <dt>{move || i18n.t("guest-organizer-code")}</dt>
                    <dd class="mono" translate="no">{organizer}</dd>
                  </dl>
                  <p class="muted-text small">{move || i18n.t("guest-codes-hint")}</p>
                })}
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
                FailureKind::SetupFailed | FailureKind::DeviceLost => "guest-setup-failed",
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

/// The guest's name: what the hosts see in the lobby and everyone in the meeting.
#[component]
pub(crate) fn GuestNameField(name: RwSignal<String>, errors: FormErrors) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <label for=NAME_ID>{move || i18n.t("guest-name")}</label>
    <input
      id=NAME_ID
      class="input"
      autocomplete="name"
      data-autofocus
      aria-required="true"
      aria-invalid=errors.invalid(NAME_ID)
      aria-describedby=errors.described(NAME_ID)
      prop:value=move || name.get()
      on:input=move |ev| name.set(event_target_value(&ev))
    />
    <FieldError errors=errors field=NAME_ID />
  }
}

/// Carries the guest on from the lobby entry, after joining or after a reload: the page's session
/// signs with the guest's token, the device is set up (or opened again), and the lobby is asked
/// at once whether the guest was let in meanwhile.
async fn enter(
  session: StoredGuestSession,
  origin: SessionOrigin,
  client: GuestClient,
  config: AppConfig,
  page: LobbyPage,
) {
  let auth: Rc<dyn Authenticator> = Rc::new(GuestAuth::new(config.clone(), session.guest_token.clone()));
  let is_gone = page
    .joined
    .try_set_value(Some(Joined {
      guest_token: session.guest_token.clone(),
      meeting: session.meeting,
      conversation_id: session.conversation,
      auth: auth.clone(),
    }))
    .is_some();
  if is_gone {
    return;
  }
  page.phase.set_if_alive(GuestPhase::Waiting);
  let organizer_fingerprint = session.maybe_organizer_fingerprint.as_deref();
  let setup = prepare_device(
    &client,
    &config,
    auth,
    &session.guest_token,
    session.user_id,
    session.conversation,
    session.meeting,
    organizer_fingerprint,
    origin,
  )
  .await;
  match setup {
    Ok((own_code, maybe_unreported)) => {
      page.unreported.try_set_value(maybe_unreported);
      let organizer_code = organizer_fingerprint.map(code_of_fingerprint).unwrap_or_default();
      page.codes.set_if_alive(Some((own_code, organizer_code)));
      page.is_device_ready.set_if_alive(true);
      check_lobby(client, page).await;
    },
    // The device's database is gone (site data cleared): its knock can't be completed, so the
    // page starts over at the plain link and the guest knocks as new.
    Err(FailureKind::DeviceLost) => {
      StoredGuestSession::forget();
      start_over();
    },
    Err(kind) => page.phase.set_if_alive(GuestPhase::Failed(kind)),
  }
}

/// Asks the lobby whether the guest was let in, reporting the device first if that failed before.
/// A refusal or a turned-away guest ends the session this browser keeps.
async fn check_lobby(client: GuestClient, page: LobbyPage) {
  let Some((guest_token, meeting)) = page.joined.with_value(|joined| {
    joined
      .as_ref()
      .map(|joined| (joined.guest_token.clone(), joined.meeting))
  }) else {
    return;
  };
  if let Some(fingerprint) = page.unreported.get_value()
    && report_device(&client, meeting, &guest_token, &fingerprint).await
  {
    page.unreported.try_set_value(None);
  }
  match client.lobby_state(meeting, &guest_token).await {
    Ok(answer) => {
      page
        .is_organizer_app_outdated
        .set_if_alive(answer.is_organizer_app_outdated);
      match answer.state {
        LobbyState::Admitted => page.phase.set_if_alive(GuestPhase::Admitted),
        LobbyState::Denied => {
          StoredGuestSession::forget();
          page.phase.set_if_alive(GuestPhase::Denied);
        },
        LobbyState::Waiting | LobbyState::Unknown(_) => {},
      }
    },
    // A hiccup keeps waiting; a refused session (left, ended, removed) ends it.
    Err(MeetingError::Refused { .. }) => {
      StoredGuestSession::forget();
      page
        .phase
        .set_if_alive(GuestPhase::Failed(FailureKind::LinkUnusable));
    },
    Err(_) => {},
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
  origin: SessionOrigin,
) -> Result<(String, Option<String>), FailureKind> {
  configure_core(auth, config);
  let failed = |step: &str, error: String| {
    tracing::warn!(step, %error, "could not set up the guest's device");
    FailureKind::SetupFailed
  };
  let mut waited = Duration::ZERO;
  loop {
    match remote_backend::core_status(user_id).await {
      Ok(CoreStatus::NeedsEnrollment { .. }) if origin == SessionOrigin::Resumed => {
        return Err(FailureKind::DeviceLost);
      },
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
    remote_backend::guest_anchor(conversation, meeting, fingerprint)
      .await
      .map_err(|error| failed("anchor", error))?;
  }
  let own = remote_backend::own_key()
    .await
    .map_err(|error| failed("own key", error))?;
  // Reports the key's fingerprint with the knock; the organizer's device admits only that key. A
  // failure keeps the guest waiting and is retried on the next poll, rather than failing the page.
  let is_reported = report_device(client, meeting, guest_token, &own.fingerprint).await;
  Ok((own.code, (!is_reported).then_some(own.fingerprint)))
}

/// Reports the guest's key fingerprint with the knock; whether the server took it.
async fn report_device(
  client: &GuestClient,
  meeting: MeetingId,
  guest_token: &str,
  fingerprint: &str,
) -> bool {
  match client.device_ready(meeting, guest_token, fingerprint).await {
    Ok(()) => true,
    Err(error) => {
      tracing::warn!(%error, "could not tell the hosts that the guest's device is ready");
      false
    },
  }
}

/// Loads the plain meeting link again (without the resume key), so the guest knocks as new.
fn start_over() {
  let Some(location) = web_sys::window().map(|window| window.location()) else {
    return;
  };
  if let Ok(path) = location.pathname() {
    // A failure leaves the page as it is; the guest can reload by hand.
    let _ = location.replace(&path);
  }
}

/// Moves the page to `address` (history entry replaced, nothing reloads).
fn replace_address(address: &str) {
  let Some(history) = web_sys::window().and_then(|window| window.history().ok()) else {
    return;
  };
  // A failure leaves the address as it was: the page works, a reload knocks anew.
  let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(address));
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
  crate::web_core::configure(auth, &config.server_url, crate::web_core::PageKind::Guest);
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
