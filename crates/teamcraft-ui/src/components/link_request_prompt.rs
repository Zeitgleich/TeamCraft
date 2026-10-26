//! The prompt for a new device that asks to be linked, shown on every signed-in device of the account
//! (e2ee.md §5, "Approve prompt"). It sits in a corner and never takes focus, never approves by
//! itself and never shows the code: the user types the code the *new* device shows, and the core
//! checks it (docs/security-review.md C3), so only someone looking at the new device can approve.

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::remote::LinkRequestView;
use teamcraft_proto::DeviceLinkPlatform;

use crate::components::icons::{Icon, IconName};
use crate::i18n::{I18n, use_i18n};
use crate::reactive::{SetIfAlive, UpdateIfAlive};
use crate::remote_backend;
use crate::state::use_app_state;

/// Re-reads the list at the latest this long after the first request would expire, so a prompt of
/// a request nobody answered disappears without another event.
const EXPIRY_SLACK_MS: u32 = 1_000;

/// Where one request stands on this device.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
  /// Taking it up (joining the relay).
  Joining,
  /// Waiting for the code of the new device; `maybe_error` after a wrong one.
  Typing {
    maybe_error: Option<String>,
  },
  Sending,
  Done,
  Failed(String),
}

#[component]
pub fn LinkRequestPrompt() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let requests = RwSignal::new(Vec::<LinkRequestView>::new());
  // The request this device took up: it leaves the core's list at once, so it is kept here.
  let active = RwSignal::new(None::<(LinkRequestView, Step)>);

  let mark = Memo::new(move |_| state.read(|backend| backend.link_request_mark()));
  let reload = move || {
    spawn_local(async move {
      if let Ok(list) = remote_backend::link_requests().await {
        let maybe_soonest = list.iter().map(|request| request.seconds_left).min();
        requests.set_if_alive(list);
        if let Some(soonest) = maybe_soonest {
          let wait_ms = u32::try_from(soonest.saturating_mul(1000)).unwrap_or(u32::MAX);
          set_timeout(
            move || spawn_local(async move { reload_after_expiry(requests).await }),
            std::time::Duration::from_millis(u64::from(wait_ms.saturating_add(EXPIRY_SLACK_MS))),
          );
        }
      }
    });
  };
  Effect::new(move |_| {
    // Read on every move of the mark: the first value too, since a request may be open already.
    mark.track();
    reload();
  });

  let review = move |request: LinkRequestView| {
    active.set(Some((request.clone(), Step::Joining)));
    spawn_local(async move {
      let step = match remote_backend::link_request_review(&request.id).await {
        Ok(()) => Step::Typing { maybe_error: None },
        Err(message) => Step::Failed(message),
      };
      active.update_if_alive(|current| {
        if let Some((_, slot)) = current {
          *slot = step;
        }
      });
    });
  };
  let deny = move |id: String| {
    spawn_local(async move {
      // A failure means the request is gone already (taken up elsewhere, expired): same result.
      drop(remote_backend::link_request_reject(&id).await);
      requests.update_if_alive(|list| list.retain(|request| request.id != id));
    });
  };
  view! {
    <div class="link-prompts" role="region" aria-label=move || i18n.t("link-request-region")>
      {move || {
        let taken = active.get().map(|(request, _)| request.id);
        requests
          .get()
          .into_iter()
          .filter(|request| Some(&request.id) != taken.as_ref())
          .map(|request| {
            let id = request.id.clone();
            let for_review = request.clone();
            let description = describe(&i18n, &request);
            view! {
              <section class="link-prompt" role="alert">
                <h2>{move || i18n.t("link-request-title")}</h2>
                <p translate="no">{description}</p>
                <p class="no-content-note small">
                  <Icon name=IconName::Alert />
                  <span>{move || i18n.t("link-request-warning")}</span>
                </p>
                <div class="link-prompt-actions">
                  <button class="btn" on:click=move |_| deny(id.clone())>{move || i18n.t("link-request-deny")}</button>
                  <button class="btn btn-primary" on:click=move |_| review(for_review.clone())>
                    {move || i18n.t("link-request-review")}
                  </button>
                </div>
              </section>
            }
          })
          .collect_view()
      }}
      {move || {
        let (request, step) = active.get()?;
        Some(view! { <ActivePrompt request step active /> })
      }}
    </div>
  }
}

async fn reload_after_expiry(requests: RwSignal<Vec<LinkRequestView>>) {
  if let Ok(list) = remote_backend::link_requests().await {
    requests.set_if_alive(list);
  }
}

/// The request this device took up: type the code, send the keys.
#[component]
fn ActivePrompt(
  request: LinkRequestView,
  step: Step,
  active: RwSignal<Option<(LinkRequestView, Step)>>,
) -> impl IntoView {
  let i18n = use_i18n();
  let close = move || {
    // Leaving before the keys were sent drops the attempt, so nothing stays half open.
    spawn_local(async { drop(remote_backend::approval_cancel().await) });
    active.set_if_alive(None);
  };
  let typed = RwSignal::new(String::new());
  let set_step = move |step: Step| {
    active.update_if_alive(|current| {
      if let Some((_, slot)) = current {
        *slot = step;
      }
    });
  };
  let approve = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let short_code = typed.get_untracked();
    set_step(Step::Sending);
    spawn_local(async move {
      match remote_backend::approval_confirm(&short_code).await {
        Ok(()) => set_step(Step::Done),
        // A wrong code keeps the attempt open for a retry; after a few misses the core drops it and
        // the next call fails with "no device waiting", which ends here as well.
        Err(message) if message.contains("did not match the one") => {
          typed.set_if_alive(String::new());
          set_step(Step::Typing {
            maybe_error: Some(message),
          });
        },
        Err(message) => set_step(Step::Failed(message)),
      }
    });
  };
  let description = describe(&i18n, &request);
  view! {
    <section class="link-prompt" role="alert">
      <h2>{move || i18n.t("link-request-title")}</h2>
      <p translate="no">{description}</p>
      {move || match step.clone() {
        Step::Joining => {
          view! { <p role="status">{move || i18n.t("link-request-joining")}</p> }.into_any()
        },
        Step::Typing { maybe_error } => {
          view! {
            <form on:submit=approve>
              <p>{move || i18n.t("link-request-type-code")}</p>
              <label class="visually-hidden" for="link-request-short">{move || i18n.t("approve-link-short")}</label>
              <input
                id="link-request-short"
                class="input mono"
                inputmode="numeric"
                autocomplete="off"
                prop:value=move || typed.get()
                on:input=move |ev| typed.set(event_target_value(&ev))
              />
              {maybe_error.map(|_| view! { <p class="field-error" role="alert">{move || i18n.t("approve-link-wrong")}</p> })}
              <div class="link-prompt-actions">
                <button type="button" class="btn" on:click=move |_| close()>{move || i18n.t("cancel")}</button>
                <button type="submit" class="btn btn-primary" prop:disabled=move || typed.get().trim().is_empty()>
                  {move || i18n.t("approval-approve")}
                </button>
              </div>
            </form>
          }
          .into_any()
        },
        Step::Sending => view! { <p role="status">{move || i18n.t("approve-link-sending")}</p> }.into_any(),
        Step::Done => view! {
          <p role="status">{move || i18n.t("approval-approved")}</p>
          <div class="link-prompt-actions">
            <button class="btn btn-primary" on:click=move |_| close()>{move || i18n.t("close")}</button>
          </div>
        }
        .into_any(),
        Step::Failed(message) => view! {
          <p class="field-error" role="alert">{move || i18n.t("approve-link-failed")}</p>
          <p class="small mono" translate="no">{message}</p>
          <div class="link-prompt-actions">
            <button class="btn" on:click=move |_| close()>{move || i18n.t("close")}</button>
          </div>
        }
        .into_any(),
      }}
    </section>
  }
}

/// "Pixel 8 (Android)": what the user checks against the device in their hand.
fn describe(i18n: &I18n, request: &LinkRequestView) -> String {
  i18n.t_with(
    "link-request-device",
    &[
      ("device", &request.device_name),
      ("platform", &i18n.t(platform_key(&request.platform))),
    ],
  )
}

fn platform_key(platform: &DeviceLinkPlatform) -> &'static str {
  match platform {
    DeviceLinkPlatform::Android => "link-platform-android",
    DeviceLinkPlatform::Ios => "link-platform-ios",
    DeviceLinkPlatform::Macos => "link-platform-macos",
    DeviceLinkPlatform::Windows => "link-platform-windows",
    DeviceLinkPlatform::Linux => "link-platform-linux",
    DeviceLinkPlatform::Web => "link-platform-web",
    DeviceLinkPlatform::Other => "link-platform-other",
  }
}
