//! Device approval prompt, QR linking and the key-change banner (e2ee.md §3 and §5).

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::failure::{ActionFailure, FailureKind};
use teamcraft_client_api::remote::PhoneLinkStatus;

use crate::clock::now_ms;
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::feedback::failure_message;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, UserId};
use crate::reactive::{SetIfAlive, use_interval_while};
use crate::remote_backend;
use crate::security::group_fingerprint;
use crate::state::{Overlay, use_app_state};

/// Demo payload of the QR code: a rendezvous ID and a throw-away authentication key, no real secrets.
#[cfg(feature = "demo")]
const DEMO_LINK_URI: &str =
  "teamcraft://link?v=1&rv=7Qm2xD9aVt4cLw8e&ak=ZGVtby1rZXktbm90LWEtc2VjcmV0LTMyLWJ5dGVz";
#[cfg(feature = "demo")]
const DEMO_LINK_CODE: &str = "4 7 1 9";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
  Pending,
  Approved,
  Denied,
}

/// "New login on Firefox on Linux – code 4 7 1 9. Approve?" shown on the user's other devices.
#[component]
pub fn DeviceApprovalDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let decision = RwSignal::new(Decision::Pending);

  move || {
    let approval = state.device_prompt.get()?;
    let device_name = StoredValue::new(approval.device_name.clone());
    let spaced_code = StoredValue::new(approval.spaced_code());
    let location = StoredValue::new(approval.location.clone());
    let code = StoredValue::new(approval.code.clone());
    Some(view! {
      <Modal labelled_by="approval-title" class="modal-small">
        <header class="modal-head">
          <h2 id="approval-title">{move || i18n.t("approval-title")}</h2>
        </header>
        <div class="modal-body approval">
          <Show
            when=move || decision.get() == Decision::Pending
            fallback=move || {
              view! {
                <p class="approval-result" role="status">
                  {move || match decision.get() {
                    Decision::Approved => i18n.t("approval-approved"),
                    _ => i18n.t("approval-denied"),
                  }}
                </p>
              }
            }
          >
            <p>
              {move || i18n.t_with("approval-question", &[("device", &device_name.get_value())])}
            </p>
            <p class="approval-code" aria-label=move || i18n.t_with("approval-code-label", &[("code", &spaced_code.get_value())])>
              <span aria-hidden="true">{move || code.get_value()}</span>
            </p>
            <p class="muted-text small">{move || i18n.t("approval-compare")}</p>
            <dl class="meta-list">
              <dt>{move || i18n.t("approval-location")}</dt>
              <dd translate="no">{move || location.get_value()}</dd>
            </dl>
            <p class="no-content-note">
              <Icon name=IconName::Alert />
              <span>{move || i18n.t("approval-warning")}</span>
            </p>
          </Show>
        </div>
        <footer class="modal-foot">
          <span class="grow"></span>
          <Show
            when=move || decision.get() == Decision::Pending
            fallback=move || {
              view! {
                <button
                  class="btn btn-primary"
                  data-autofocus
                  on:click=move |_| {
                    state.device_prompt.set(None);
                    state.close_overlay();
                  }
                >
                  {move || i18n.t("close")}
                </button>
              }
            }
          >
            <button class="btn" on:click=move |_| decision.set(Decision::Denied)>
              {move || i18n.t("approval-deny")}
            </button>
            <button class="btn btn-primary" data-autofocus on:click=move |_| decision.set(Decision::Approved)>
              {move || i18n.t("approval-approve")}
            </button>
          </Show>
        </footer>
      </Modal>
    })
  }
}

/// On the existing device: link a new device. With the desktop core this is the real flow (enter the
/// new device's code, compare the short code, approve); without it linking is not possible here.
#[component]
pub fn LinkDeviceDialog() -> impl IntoView {
  if crate::auth::tauri::has_core() {
    view! { <ApproveLinkDialog /> }.into_any()
  } else {
    view! { <LinkUnavailableDialog /> }.into_any()
  }
}

/// Without the client core (it did not start) there are no keys to hand over.
#[cfg(not(feature = "demo"))]
#[component]
fn LinkUnavailableDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  view! {
    <Modal labelled_by="link-title" class="modal-link">
      <header class="modal-head">
        <h2 id="link-title">{move || i18n.t("link-title")}</h2>
        <button class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <p class="muted-text">{move || i18n.t("link-unavailable")}</p>
      <footer class="modal-foot">
        <span class="grow"></span>
        <button class="btn btn-primary" data-autofocus on:click=move |_| state.close_overlay()>
          {move || i18n.t("close")}
        </button>
      </footer>
    </Modal>
  }
}

/// The demo build shows its pretend linking screen instead.
#[cfg(feature = "demo")]
#[component]
fn LinkUnavailableDialog() -> impl IntoView {
  view! { <DemoLinkDialog /> }
}

/// How often the dialog asks whether the phone scanned the code, and ticks the time left.
const PHONE_LINK_POLL_MS: u64 = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
enum ApprovePhase {
  /// A code for a new phone to scan (issue #998): the phone has the camera, this screen the code.
  /// `None` while it is being made.
  ShowCode(Option<ShownCode>),
  /// The new device is a computer that shows a code: type or paste it here.
  EnterCode,
  Connecting,
  /// The new device shows a short code; the user types it here. The core checks it, so only
  /// someone looking at the new device can approve.
  ConfirmCode {
    error: Option<String>,
  },
  Sending,
  Done,
  Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ShownCode {
  code: String,
  maybe_expires_at_ms: Option<i64>,
}

#[component]
fn ApproveLinkDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let code = RwSignal::new(String::new());
  let phase = RwSignal::new(ApprovePhase::ShowCode(None));
  let now = RwSignal::new(now_ms());
  let close = move || {
    spawn_local(async { drop(remote_backend::approval_cancel().await) });
    state.close_overlay();
  };

  // The common case first: the new device is a phone, which scans this screen (issue #998). A new
  // computer shows its own code instead ("Enter a code instead").
  let show_code = move || {
    phase.set(ApprovePhase::ShowCode(None));
    spawn_local(async move {
      if let Err(message) = remote_backend::phone_link_start().await {
        phase.set_if_alive(ApprovePhase::Failed(message));
      }
    });
  };
  show_code();
  let is_showing_code = move || matches!(phase.get(), ApprovePhase::ShowCode(_));
  use_interval_while(PHONE_LINK_POLL_MS, is_showing_code, move || {
    now.set(now_ms());
    spawn_local(async move {
      let Ok(status) = remote_backend::phone_link_status().await else {
        return;
      };
      // Only while the code is still on screen: the user may have switched to typing a code.
      if !matches!(phase.try_get_untracked(), Some(ApprovePhase::ShowCode(_))) {
        return;
      }
      phase.set_if_alive(match status {
        PhoneLinkStatus::Idle => return,
        PhoneLinkStatus::WaitingForScan {
          code,
          maybe_expires_at_ms,
        } => ApprovePhase::ShowCode(Some(ShownCode {
          code,
          maybe_expires_at_ms,
        })),
        PhoneLinkStatus::Scanned => ApprovePhase::ConfirmCode { error: None },
        PhoneLinkStatus::Failed { message } => ApprovePhase::Failed(message),
      });
    });
  });
  let enter_code_instead = move |_| {
    // The shown code is closed: only one way at a time.
    spawn_local(async { drop(remote_backend::approval_cancel().await) });
    phase.set(ApprovePhase::EnterCode);
  };

  let connect = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let entered = code.get_untracked().trim().to_owned();
    if entered.is_empty() {
      return;
    }
    phase.set(ApprovePhase::Connecting);
    spawn_local(async move {
      phase.set_if_alive(match remote_backend::approval_start(&entered).await {
        Ok(()) => ApprovePhase::ConfirmCode { error: None },
        Err(message) => ApprovePhase::Failed(message),
      });
    });
  };
  let typed = RwSignal::new(String::new());
  let approve = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let short_code = typed.get_untracked();
    phase.set(ApprovePhase::Sending);
    spawn_local(async move {
      phase.set_if_alive(match remote_backend::approval_confirm(&short_code).await {
        Ok(()) => ApprovePhase::Done,
        // A wrong code keeps the attempt open for a retry; after a few misses the core drops it and
        // the next call fails with "no device waiting", which ends here as well.
        Err(message) if message.contains("did not match the one") => {
          typed.set_if_alive(String::new());
          ApprovePhase::ConfirmCode { error: Some(message) }
        },
        Err(message) => ApprovePhase::Failed(message),
      });
    });
  };

  view! {
    <Modal labelled_by="approve-link-title" class="modal-small">
      <header class="modal-head">
        <h2 id="approve-link-title">{move || i18n.t("approve-link-title")}</h2>
        <button class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| close()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body approval">
        {move || match phase.get() {
          ApprovePhase::ShowCode(maybe_shown) => {
            view! {
              <p>{move || i18n.t("approve-link-phone-intro")}</p>
              {match maybe_shown {
                Some(shown) => {
                  let maybe_expires_at_ms = shown.maybe_expires_at_ms;
                  let seconds_left = move || {
                    maybe_expires_at_ms.map(|expires_at_ms| ((expires_at_ms - now.get()).max(0) + 999) / 1000)
                  };
                  view! {
                    <Show
                      when=move || seconds_left() != Some(0)
                      fallback=move || {
                        view! {
                          <p class="field-error" role="alert">{move || i18n.t("approve-link-phone-expired")}</p>
                          <button type="button" class="btn btn-primary" data-autofocus on:click=move |_| show_code()>
                            {move || i18n.t("approve-link-phone-new-code")}
                          </button>
                        }
                      }
                    >
                      <div class="link-qr"><QrImage data=shown.code.clone() /></div>
                      <p class="muted-text small" role="status">
                        {move || {
                          seconds_left()
                            .map(|seconds| {
                              i18n.t_with("link-code-valid", &[("time", &format_time_left(seconds))])
                            })
                            .unwrap_or_default()
                        }}
                      </p>
                    </Show>
                  }
                  .into_any()
                },
                None => view! { <p class="auth-status" role="status"><span class="spinner" aria-hidden="true"></span></p> }.into_any(),
              }}
              <p class="muted-text small">{move || i18n.t("approve-link-phone-other-ways")}</p>
              <button type="button" class="btn" on:click=enter_code_instead>
                {move || i18n.t("approve-link-enter-instead")}
              </button>
            }
            .into_any()
          },
          ApprovePhase::EnterCode | ApprovePhase::Connecting => {
            let is_connecting = phase.get() == ApprovePhase::Connecting;
            view! {
              <form on:submit=connect>
                <p>{move || i18n.t("approve-link-intro")}</p>
                <label class="visually-hidden" for="approve-link-code">{move || i18n.t("approve-link-code")}</label>
                <input
                  id="approve-link-code"
                  class="input mono"
                  data-autofocus
                  autocomplete="off"
                  spellcheck="false"
                  placeholder="tclink2:…"
                  prop:value=move || code.get()
                  on:input=move |ev| code.set(event_target_value(&ev))
                />
                <p class="muted-text small">{move || i18n.t("approve-link-hint")}</p>
                <button type="submit" class="btn btn-primary" prop:disabled=is_connecting>
                  {move || i18n.t("approve-link-connect")}
                </button>
                <button type="button" class="btn" prop:disabled=is_connecting on:click=move |_| show_code()>
                  {move || i18n.t("approve-link-show-instead")}
                </button>
              </form>
            }
            .into_any()
          },
          ApprovePhase::ConfirmCode { error } => {
            view! {
              <form on:submit=approve>
                <p>{move || i18n.t("approve-link-type-code")}</p>
                <label class="visually-hidden" for="approve-link-short">{move || i18n.t("approve-link-short")}</label>
                <input
                  id="approve-link-short"
                  class="input mono"
                  inputmode="numeric"
                  data-autofocus
                  autocomplete="off"
                  aria-invalid=error.is_some().then_some("true")
                  aria-describedby=error.is_some().then_some("approve-link-short-error")
                  prop:value=move || typed.get()
                  on:input=move |ev| typed.set(event_target_value(&ev))
                />
                {error.map(|_| view! { <p id="approve-link-short-error" class="field-error" role="alert">{move || i18n.t("approve-link-wrong")}</p> })}
                <p class="no-content-note">
                  <Icon name=IconName::Alert />
                  <span>{move || i18n.t("approval-warning")}</span>
                </p>
                <button type="submit" class="btn btn-primary">{move || i18n.t("approval-approve")}</button>
              </form>
            }
            .into_any()
          },
          ApprovePhase::Sending => view! { <p role="status">{move || i18n.t("approve-link-sending")}</p> }.into_any(),
          ApprovePhase::Done => view! { <p role="status">{move || i18n.t("approval-approved")}</p> }.into_any(),
          ApprovePhase::Failed(message) => {
            // Say what went wrong and what to do (issue #645); the raw text only when unknown.
            let failure = ActionFailure::from_wire(&message);
            let is_known = !matches!(failure.kind, FailureKind::Other);
            view! {
              <p class="field-error" role="alert">
                {move || if is_known { failure_message(i18n, &failure) } else { i18n.t("approve-link-failed") }}
              </p>
              {(!is_known).then(|| view! { <p class="small mono" translate="no">{message.clone()}</p> })}
              <button type="button" class="btn" on:click=move |_| show_code()>
                {move || i18n.t("approve-link-retry")}
              </button>
            }
            .into_any()
          },
        }}
      </div>
    </Modal>
  }
}

/// "4:05" for the time a code stays valid.
fn format_time_left(seconds: i64) -> String {
  format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Demo screen of the browser-only build: a QR code that is not a real login.
#[cfg(feature = "demo")]
#[component]
fn DemoLinkDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_linked = RwSignal::new(false);

  view! {
    <Modal labelled_by="link-title" class="modal-link">
      <header class="modal-head">
        <h2 id="link-title">{move || i18n.t("link-title")}</h2>
        <button class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="link-body">
        <div class="link-qr">
          <QrImage data=DEMO_LINK_URI.to_owned() />
          <p class="muted-text small">{move || i18n.t("link-qr-note")}</p>
        </div>
        <div class="link-steps">
          <ol>
            <li>{move || i18n.t("link-step-open")}</li>
            <li>{move || i18n.t("link-step-scan")}</li>
            <li>{move || i18n.t("link-step-code")}</li>
          </ol>
          <p class="approval-code" aria-label=move || i18n.t_with("approval-code-label", &[("code", DEMO_LINK_CODE)])>
            <span aria-hidden="true">{DEMO_LINK_CODE.replace(' ', "")}</span>
          </p>
          <p class="status-line" role="status">
            {move || if is_linked.get() { i18n.t("link-done") } else { i18n.t("link-waiting") }}
          </p>
          <p class="muted-text small">{move || i18n.t("link-alternative")}</p>
        </div>
      </div>
      <footer class="modal-foot">
        <button class="btn" on:click=move |_| is_linked.set(true) disabled=move || is_linked.get()>
          {move || i18n.t("link-simulate")}
        </button>
        <span class="grow"></span>
        <button class="btn btn-primary" data-autofocus on:click=move |_| state.close_overlay()>
          {move || if is_linked.get() { i18n.t("close") } else { i18n.t("cancel") }}
        </button>
      </footer>
    </Modal>
  }
}

/// A QR code drawn as one SVG path. Light modules on a white quiet zone keep it scannable in dark mode.
#[component]
pub(crate) fn QrImage(data: String) -> impl IntoView {
  let i18n = use_i18n();
  let Some(crate::qr::QrPath { size, path }) = crate::qr::svg_path(&data) else {
    tracing::warn!("could not encode the link QR code");
    return ().into_any();
  };
  view! {
    <svg
      class="qr"
      viewBox=format!("0 0 {size} {size}")
      role="img"
      aria-label=move || i18n.t("link-qr-label")
      shape-rendering="crispEdges"
    >
      <rect width=size height=size fill="#ffffff" />
      <path d=path fill="#111111" />
    </svg>
  }
  .into_any()
}

/// Warning above a conversation when someone's identity key changed.
#[component]
pub fn KeyChangeBanner(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let change = Memo::new(move |_| state.read(|backend| backend.key_change(conversation)));

  move || {
    let change = change.get()?;
    let name = state
      .read(|backend| backend.user(change.user))
      .map(|user| user.display_name)
      .unwrap_or_default();
    let name_for_text = name.clone();
    Some(view! {
      <div class="key-banner" role="alert">
        <Icon name=IconName::Alert />
        <div class="key-banner-text">
          <strong translate="no">{move || i18n.t_with("key-changed-title", &[("name", &name)])}</strong>
          <span>{move || i18n.t_with("key-changed-body", &[("name", &name_for_text)])}</span>
        </div>
        <button class="btn btn-small" on:click=move |_| state.overlay.set(Overlay::VerifyKey(conversation))>
          {move || i18n.t("key-verify")}
        </button>
        <button class="btn btn-small" on:click=move |_| state.write(|backend| backend.acknowledge_key_change(conversation))>
          {move || i18n.t("key-accept")}
        </button>
      </div>
    })
  }
}

/// Compare the previous and the new fingerprint out of band before trusting the new key.
#[component]
pub fn VerifyKeyDialog(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let change = state.read(|backend| backend.key_change(conversation));

  let Some(change) = change else {
    state.close_overlay();
    return ().into_any();
  };
  let name = state
    .read(|backend| backend.user(change.user))
    .map(|user| user.display_name)
    .unwrap_or_default();
  let old = group_fingerprint(&change.old_fingerprint);
  let new = group_fingerprint(&change.new_fingerprint);

  view! {
    <Modal labelled_by="verify-title" class="modal-small">
      <header class="modal-head">
        <h2 id="verify-title" translate="no">{move || i18n.t_with("key-verify-title", &[("name", &name)])}</h2>
        <button class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body">
        <p>{move || i18n.t("key-verify-body")}</p>
        <h3 class="detail-heading">{move || i18n.t("key-new")}</h3>
        <p class="fingerprint" translate="no">{new}</p>
        <h3 class="detail-heading">{move || i18n.t("key-old")}</h3>
        <p class="fingerprint muted" translate="no">{old}</p>
      </div>
      <footer class="modal-foot">
        <span class="grow"></span>
        <button class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("close")}</button>
        <button
          class="btn btn-primary"
          data-autofocus
          on:click=move |_| {
            state.write(|backend| backend.acknowledge_key_change(conversation));
            state.close_overlay();
          }
        >
          {move || i18n.t("key-mark-verified")}
        </button>
      </footer>
    </Modal>
  }
  .into_any()
}

/// Compare the safety number with a contact over another channel and record the result.
#[component]
pub fn VerifyContactDialog(user: UserId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let name = state
    .read(|backend| backend.user(user))
    .map(|found| found.display_name)
    .unwrap_or_default();
  let verification = Memo::new(move |_| state.read(|backend| backend.contact_verification(user)));
  let title_name = name.clone();

  view! {
    <Modal labelled_by="verify-contact-title" class="modal-small">
      <header class="modal-head">
        <h2 id="verify-contact-title" translate="no">{move || i18n.t_with("contact-verify-title", &[("name", &title_name)])}</h2>
        <button class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body">
        <p>{move || i18n.t_with("contact-verify-body", &[("name", &name)])}</p>
        <p class="fingerprint" translate="no">{move || verification.get().safety_number}</p>
        <p class="status-line" role="status">
          {move || if verification.get().is_verified { i18n.t("contact-verified") } else { i18n.t("contact-not-verified") }}
        </p>
      </div>
      <footer class="modal-foot">
        <span class="grow"></span>
        <button class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("close")}</button>
        <button
          class="btn btn-primary"
          data-autofocus
          on:click=move |_| {
            let is_verified = verification.get_untracked().is_verified;
            state.write(|backend| backend.set_contact_verified(user, !is_verified));
          }
        >
          {move || if verification.get().is_verified { i18n.t("contact-unmark") } else { i18n.t("key-mark-verified") }}
        </button>
      </footer>
    </Modal>
  }
}
