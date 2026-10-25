//! The page behind a personal meeting link (`/meeting/<id>#invite=<token>`): a guest enters a name,
//! waits in the lobby until a host lets them in, then is in. No account, no sign-in. The token is in
//! the URL fragment, which the browser never sends to a server.

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_proto::{LobbyState, MeetingId};

use crate::auth::AppConfig;
use crate::components::login::AuthFrame;
use crate::i18n::use_i18n;
use crate::meetings::{GuestClient, MeetingError};
use crate::reactive::{SetIfAlive, use_interval};

/// How often the lobby is asked whether the guest was admitted.
const POLL_MS: u64 = 2000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum GuestPhase {
  AskName,
  Joining,
  /// In the lobby, with the session token of the guest.
  Waiting {
    guest_token: String,
  },
  Admitted,
  Denied,
  Failed(FailureKind),
}

/// What went wrong, in terms the guest can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailureKind {
  /// The link was used, expired, cancelled or is not valid.
  LinkUnusable,
  Network,
}

#[component]
pub fn GuestJoinPage(meeting: MeetingId, invite_token: String) -> impl IntoView {
  let i18n = use_i18n();
  let client = GuestClient::new(AppConfig::load());
  let phase = RwSignal::new(GuestPhase::AskName);
  let name = RwSignal::new(String::new());
  let invite_token = StoredValue::new(invite_token);
  crate::startup::first_view();

  let join = {
    let client = client.clone();
    move |ev: leptos::ev::SubmitEvent| {
      ev.prevent_default();
      if name.get_untracked().trim().is_empty() {
        return;
      }
      phase.set(GuestPhase::Joining);
      let client = client.clone();
      spawn_local(async move {
        let outcome = client
          .join(&invite_token.get_value(), &name.get_untracked())
          .await;
        phase.set_if_alive(match outcome {
          Ok(joined) => match joined.lobby_state {
            LobbyState::Admitted => GuestPhase::Admitted,
            LobbyState::Denied => GuestPhase::Denied,
            LobbyState::Waiting | LobbyState::Unknown(_) => GuestPhase::Waiting {
              guest_token: joined.guest_token,
            },
          },
          Err(error) => GuestPhase::Failed(failure_of(&error)),
        });
      });
    }
  };

  use_interval(POLL_MS, move || {
    let GuestPhase::Waiting { guest_token } = phase.get_untracked() else {
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
    <AuthFrame>
      {move || match phase.get() {
        GuestPhase::AskName | GuestPhase::Joining => {
          let is_joining = phase.get() == GuestPhase::Joining;
          view! {
            <form class="auth-form" on:submit=join.clone()>
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
            </form>
          }
          .into_any()
        },
        GuestPhase::Waiting { .. } => {
          view! {
            <h1>{move || i18n.t("guest-waiting-title")}</h1>
            <p class="auth-status" role="status">
              <span class="spinner" aria-hidden="true"></span>
              <span>{move || i18n.t("guest-waiting")}</span>
            </p>
          }
          .into_any()
        },
        GuestPhase::Admitted => {
          view! {
            <h1>{move || i18n.t("guest-admitted-title")}</h1>
            <p role="status">{move || i18n.t("guest-admitted")}</p>
            <p class="muted-text small">{move || i18n.t("guest-admitted-note")}</p>
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
}

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
