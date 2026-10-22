//! Device approval prompt, QR linking and the key-change banner (e2ee.md §3 and §5).

use leptos::prelude::*;
use qrcode::{Color, QrCode};

use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, UserId};
use crate::security::group_fingerprint;
use crate::state::{Overlay, use_app_state};

/// Demo payload of the QR code: a rendezvous ID and a throw-away authentication key, no real secrets.
const DEMO_LINK_URI: &str =
  "teamcraft://link?v=1&rv=7Qm2xD9aVt4cLw8e&ak=ZGVtby1rZXktbm90LWEtc2VjcmV0LTMyLWJ5dGVz";
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

/// Shown on the new device: a QR code for a phone to scan, or the code to approve on another device.
#[component]
pub fn LinkDeviceDialog() -> impl IntoView {
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
          <QrImage data=DEMO_LINK_URI />
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
fn QrImage(data: &'static str) -> impl IntoView {
  const QUIET_ZONE: usize = 3;
  let i18n = use_i18n();
  let Ok(code) = QrCode::new(data.as_bytes()) else {
    tracing::warn!("could not encode the link QR code");
    return ().into_any();
  };
  let width = code.width();
  let size = width + 2 * QUIET_ZONE;
  let path: String = code
    .to_colors()
    .into_iter()
    .enumerate()
    .filter(|(_, color)| *color == Color::Dark)
    .map(|(index, _)| {
      format!(
        "M{} {}h1v1h-1z",
        index % width + QUIET_ZONE,
        index / width + QUIET_ZONE
      )
    })
    .collect();
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
