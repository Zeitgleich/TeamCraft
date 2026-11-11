//! *Ask my IT department* on the web and desktop setup (e2ee.md §6.3.1, issue #1705): the person
//! lost every device and the recovery kit. An explanation first, not a self-service: the IT
//! department decides. If the organization set up recovery, the steps are the phones' ones:
//! read the code to an admin → save a new recovery kit → check it → finish once the admins
//! approved. Without it, the explanation says that only a reset is left.
//!
//! The request's one-time key lives in the core's memory only. The page keeps the request's ID
//! (not secret) in `localStorage`. Mounted again (a reload; on desktop the core outlives the
//! webview), it asks the core first: a request the core still holds is resumed at its code. Only
//! when the core lost it does the page say that the old code no longer works; the stored request
//! is withdrawn when the person asks again. The kit's key is made afresh either way: nothing used
//! it yet unless a finish got far enough, and then that kit is the way back (e2ee.md §6.3.1).
//!
//! The recovering device has no device keys yet, so no gateway tells it about approvals: it asks
//! the server every [`STATUS_POLL_MS`] while the code or the finish step is on screen and the page
//! is visible, and not at all otherwise.

use leptos::prelude::*;
use leptos::task::spawn_local;
use teamcraft_client_api::failure::{ActionFailure, FailureKind};
use teamcraft_client_api::remote::{EnrollSecret, OrgRecoveryStage, OrgRecoveryStatus};
use teamcraft_proto::RecoveryRequestId;

use crate::components::icons::{Icon, IconName};
use crate::components::onboarding::{
  DeviceNameField, EnrollError, StepPoint, StepProgress, StepTitle, focus_title_on_change,
};
use crate::components::recovery_kit_setup::{KitCheckStep, KitSaveStep, SecondaryAction, new_kit_key};
use crate::feedback::failure_message;
use crate::i18n::use_i18n;
use crate::model::UserId;
use crate::reactive::{SetIfAlive, use_interval_while, use_page_visible};
use crate::remote_backend;
use crate::session::SessionControl;
use crate::settings::{load_string, org_recovery_request_key, store_string};

/// How often the open request's state is asked while the person looks at it. Approvals take
/// minutes (a phone call with an admin); five seconds is quick enough to feel live.
const STATUS_POLL_MS: u64 = 5_000;
/// The step before this flow (the choice of ways) counts as step 1.
const STEPS_BEFORE: u8 = 1;
const STEP_COUNT: u8 = 5;

/// The steps of a recovery through the organization, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItStep {
  /// What the IT department can do; the request starts here.
  Explain,
  /// The code to read to an admin.
  Code,
  Save,
  Check,
  /// Wait for the approvals, name the device, finish.
  Finish,
}

/// Where a mounted page starts, from what the core still holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mount {
  /// The core still holds a request: show its code again.
  Resume,
  /// A request was open but the core lost its key: say so, ask again.
  Restarted,
  /// Nothing was asked yet.
  Fresh,
}

/// What a request's state means on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StageView {
  /// Still on its way: the status line, finishing not yet possible.
  Pending,
  /// The admins approved and the key is there.
  Ready,
  /// Denied, expired or withdrawn: the person can ask again.
  Closed,
}

/// The person has no device and no kit: what the IT department can do, and the recovery itself
/// when the organization set it up.
#[component]
pub(crate) fn AskTheItDepartment(
  account: String,
  maybe_user: Option<UserId>,
  device_name: RwSignal<String>,
  control: SessionControl,
  on_back: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  let maybe_marker = StoredValue::new(maybe_user.map(org_recovery_request_key));
  // A request from before a reload or a restart of the page.
  let maybe_previous = StoredValue::new(
    maybe_marker
      .with_value(|marker| marker.as_deref().and_then(load_string))
      .and_then(|id| id.parse::<RecoveryRequestId>().ok()),
  );
  let was_restarted = RwSignal::new(false);
  let step = RwSignal::new(ItStep::Explain);
  let status = RwSignal::new(OrgRecoveryStatus::NotStarted);
  // Asking again before the core answered could withdraw a request it still holds.
  let is_starting = RwSignal::new(true);
  let maybe_start_error = RwSignal::new(None::<String>);
  let is_offline = RwSignal::new(false);
  let key = new_kit_key();
  let account = StoredValue::new(account);
  focus_title_on_change(step);

  let remember = move |maybe_id: Option<RecoveryRequestId>| {
    let value = maybe_id.map(|id| id.to_string()).unwrap_or_default();
    maybe_marker.try_with_value(|marker| {
      if let Some(marker) = marker.as_deref() {
        store_string(marker, &value);
      }
    });
  };
  // Asks the organization; the request this page made before (or before a reload) is withdrawn.
  let ask = move || {
    if is_starting.get_untracked() {
      return;
    }
    is_starting.set(true);
    maybe_start_error.set(None);
    let previous = maybe_previous.get_value();
    spawn_local(async move {
      match remote_backend::org_recovery_start(previous).await {
        Ok(started) => {
          let maybe_id = match &started {
            OrgRecoveryStatus::Requested { request_id, .. } => Some(*request_id),
            _ => None,
          };
          remember(maybe_id);
          maybe_previous.try_set_value(maybe_id);
          status.set_if_alive(started);
          step.set_if_alive(ItStep::Code);
        },
        Err(message) => maybe_start_error.set_if_alive(Some(message)),
      }
      is_starting.set_if_alive(false);
    });
  };
  // What the core still holds decides where the page starts.
  spawn_local(async move {
    let maybe_held = remote_backend::org_recovery_status().await.ok();
    let has_marker = maybe_previous.try_with_value(Option::is_some).unwrap_or(false);
    match mount_from(maybe_held.as_ref(), has_marker) {
      Mount::Resume => {
        if let Some(held) = maybe_held {
          if let OrgRecoveryStatus::Requested { request_id, .. } = &held {
            remember(Some(*request_id));
            maybe_previous.try_set_value(Some(*request_id));
          }
          status.set_if_alive(held);
          step.set_if_alive(ItStep::Code);
        }
      },
      Mount::Restarted => was_restarted.set_if_alive(true),
      Mount::Fresh => {},
    }
    is_starting.set_if_alive(false);
  });
  // Set up: the request is done, the marker goes (signing out clears it too, session.rs).
  Effect::new(move |was_enrolling: Option<bool>| {
    let is_enrolling = control.is_enrolling.get();
    if was_enrolling == Some(true) && !is_enrolling {
      if control.enroll_error.with_untracked(Option::is_none) {
        remember(None);
      } else {
        // A failed finish: the core may have lost the request (restarted); the status says so.
        spawn_local(async move {
          if let Ok(latest) = remote_backend::org_recovery_status().await {
            status.set_if_alive(latest);
          }
        });
      }
    }
    is_enrolling
  });
  let back = move || {
    if is_starting.get_untracked() {
      return;
    }
    if matches!(status.get_untracked(), OrgRecoveryStatus::Requested { .. }) {
      spawn_local(async { drop(remote_backend::org_recovery_cancel().await) });
      remember(None);
      maybe_previous.set_value(None);
    }
    on_back.run(());
  };

  // Only while the state is on screen, the page is visible and a request is open.
  let is_visible = use_page_visible();
  let is_watching = move || {
    is_visible.get()
      && matches!(step.get(), ItStep::Code | ItStep::Finish)
      && status.with(|current| match current {
        OrgRecoveryStatus::Requested { stage, .. } => stage_view(stage) == StageView::Pending,
        _ => false,
      })
  };
  use_interval_while(STATUS_POLL_MS, is_watching, move || {
    spawn_local(async move {
      match remote_backend::org_recovery_status().await {
        Ok(latest) => {
          is_offline.set_if_alive(false);
          status.set_if_alive(latest);
        },
        // Offline or the server briefly away: keep what was shown and try again next time.
        Err(_) => is_offline.set_if_alive(true),
      }
    });
  });

  let finish = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    if control.is_enrolling.get_untracked() || !is_ready(&status.get_untracked()) {
      return;
    }
    if let Some(recovery_key) = key.try_with_value(|key| key.to_display().to_string()) {
      control.enroll(
        EnrollSecret::OrgRecovery { recovery_key },
        device_name.get_untracked().trim().to_owned(),
      );
    }
  };
  let go = move |to: ItStep| Callback::new(move |()| step.set(to));

  view! {
    {move || {
      let current = step.get();
      let body = match current {
        ItStep::Explain => view! {
          <StepTitle text=Signal::derive(move || i18n.t("onboarding-it-title")) />
          <Show when=move || was_restarted.get() fallback=|| ()>
            <div class="auth-alert" role="status">
              <Icon name=IconName::Info />
              <p>{move || i18n.t("org-recovery-restarted")}</p>
            </div>
          </Show>
          <p class="step-lead">{move || i18n.t("onboarding-it-lead")}</p>
          <ul class="step-points">
            <StepPoint icon=IconName::Users title="org-recovery-point-decide" text="org-recovery-point-decide-text" />
            <StepPoint icon=IconName::Key title="org-recovery-point-code" text="org-recovery-point-code-text" />
            <StepPoint icon=IconName::Shield title="org-recovery-point-kit" text="org-recovery-point-kit-text" />
          </ul>
          {move || maybe_start_error.get().map(|message| view! {
            <p id="org-recovery-start-error" class="field-error" role="alert">{failure_text(i18n, &message)}</p>
          })}
          <div class="step-actions">
            <button
              type="button"
              class="btn btn-primary"
              data-autofocus
              aria-busy=move || is_starting.get().then_some("true")
              aria-describedby=move || maybe_start_error.with(Option::is_some).then_some("org-recovery-start-error")
              prop:disabled=move || is_starting.get()
              on:click=move |_| ask()
            >
              <Show when=move || is_starting.get() fallback=|| ()>
                <span class="spinner" aria-hidden="true"></span>
              </Show>
              {move || i18n.t("org-recovery-show-code")}
            </button>
            <button type="button" class="btn" prop:disabled=move || is_starting.get() on:click=move |_| back()>
              {move || i18n.t("onboarding-back")}
            </button>
          </div>
        }
        .into_any(),
        ItStep::Code => view! { <CodeStep status=status is_offline=is_offline is_starting=is_starting on_ask=Callback::new(move |()| ask()) on_continue=go(ItStep::Save) on_back=Callback::new(move |()| back()) /> }.into_any(),
        ItStep::Save => view! {
          <StepTitle text=Signal::derive(move || i18n.t("onboarding-save-title")) />
          <p class="step-lead">{move || i18n.t("org-recovery-save-lead")}</p>
          <KitSaveStep
            key=key
            account=account.get_value()
            on_continue=go(ItStep::Check)
            maybe_secondary=Some(SecondaryAction { label: Signal::derive(move || i18n.t("onboarding-back")), on_click: go(ItStep::Code) })
          />
        }
        .into_any(),
        ItStep::Check => view! {
          <StepTitle text=Signal::derive(move || i18n.t("onboarding-check-title")) />
          <p class="step-lead">{move || i18n.t("onboarding-check-lead")}</p>
          <KitCheckStep
            key=key
            finish_label=Signal::derive(move || i18n.t("onboarding-continue"))
            is_busy=Signal::derive(|| false)
            on_confirmed=Callback::new(move |_key: String| step.set(ItStep::Finish))
            secondary=SecondaryAction { label: Signal::derive(move || i18n.t("kit-confirm-back")), on_click: go(ItStep::Save) }
          />
        }
        .into_any(),
        ItStep::Finish => view! {
          <StepTitle text=Signal::derive(move || i18n.t("org-recovery-finish-title")) />
          <p class="step-lead">{move || i18n.t("org-recovery-finish-lead")}</p>
          <RequestState status=status is_offline=is_offline is_starting=is_starting on_ask=Callback::new(move |()| ask()) />
          <form class="auth-form" novalidate on:submit=finish>
            <DeviceNameField device_name=device_name />
            // A lost request is said by the status above, with the way to ask again.
            <Show when=move || matches!(status.get(), OrgRecoveryStatus::Requested { .. }) fallback=|| ()>
              <EnrollError message_key="org-recovery-finish-failed" control=control />
            </Show>
            <div class="step-actions">
              <button
                type="submit"
                class="btn btn-primary"
                aria-busy=move || control.is_enrolling.get().then_some("true")
                prop:disabled=move || !is_ready(&status.get()) || control.is_enrolling.get()
              >
                <Show when=move || control.is_enrolling.get() fallback=|| ()>
                  <span class="spinner" aria-hidden="true"></span>
                </Show>
                {move || if control.is_enrolling.get() { i18n.t("enroll-working") } else { i18n.t("org-recovery-finish") }}
              </button>
              <button
                type="button"
                class="btn"
                prop:disabled=move || control.is_enrolling.get()
                on:click=move |_| step.set(ItStep::Check)
              >
                {move || i18n.t("onboarding-back")}
              </button>
            </div>
          </form>
        }
        .into_any(),
      };
      view! {
        <StepProgress current=STEPS_BEFORE + step_number(current) total=STEPS_BEFORE + STEP_COUNT />
        <div class="step-body">{body}</div>
      }
    }}
  }
}

/// The code to read to an admin, or why there is none.
#[component]
fn CodeStep(
  status: RwSignal<OrgRecoveryStatus>,
  is_offline: RwSignal<bool>,
  is_starting: RwSignal<bool>,
  on_ask: Callback<()>,
  on_continue: Callback<()>,
  on_back: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  let is_unavailable = move || !matches!(status.get(), OrgRecoveryStatus::Requested { .. });
  view! {
    <StepTitle text=Signal::derive(move || {
      if is_unavailable() { i18n.t("org-recovery-unavailable-title") } else { i18n.t("org-recovery-code-title") }
    }) />
    {move || match status.get() {
      OrgRecoveryStatus::Requested { code, .. } => view! {
        <p class="step-lead">{move || i18n.t("org-recovery-code-lead")}</p>
        <p class="approval-code org-recovery-code" translate="no" aria-label=move || i18n.t("org-recovery-code-label")>{code}</p>
        <RequestState status=status is_offline=is_offline is_starting=is_starting on_ask=on_ask />
        <p class="muted-text small">{move || i18n.t("org-recovery-code-next")}</p>
        <div class="step-actions">
          <button type="button" class="btn btn-primary" data-autofocus on:click=move |_| on_continue.run(())>
            {move || i18n.t("onboarding-continue")}
          </button>
          <button type="button" class="btn" on:click=move |_| on_back.run(())>{move || i18n.t("org-recovery-withdraw")}</button>
        </div>
      }
      .into_any(),
      _ => view! {
        <p class="step-lead">{move || i18n.t("org-recovery-unavailable")}</p>
        <div class="step-actions">
          <button type="button" class="btn" data-autofocus on:click=move |_| on_back.run(())>{move || i18n.t("onboarding-back")}</button>
        </div>
      }
      .into_any(),
    }}
  }
}

/// Where the request stands, as a status line; a closed request offers to ask again.
#[component]
fn RequestState(
  status: RwSignal<OrgRecoveryStatus>,
  is_offline: RwSignal<bool>,
  is_starting: RwSignal<bool>,
  on_ask: Callback<()>,
) -> impl IntoView {
  let i18n = use_i18n();
  move || {
    let OrgRecoveryStatus::Requested { stage, .. } = status.get() else {
      // The core lost the request (it restarted): the code is gone, ask again.
      return view! { <ClosedRequest text_key="org-recovery-lost" is_starting=is_starting on_ask=on_ask /> }
        .into_any();
    };
    match stage_view(&stage) {
      StageView::Closed => {
        let text_key = match stage {
          OrgRecoveryStage::Denied => "org-recovery-denied",
          OrgRecoveryStage::Expired => "org-recovery-expired",
          _ => "org-recovery-cancelled",
        };
        view! { <ClosedRequest text_key=text_key is_starting=is_starting on_ask=on_ask /> }.into_any()
      },
      StageView::Ready => view! {
        <p class="status-line ok-text" role="status">
          <Icon name=IconName::Check />
          {move || i18n.t("org-recovery-ready")}
        </p>
      }
      .into_any(),
      StageView::Pending => {
        let text = match stage {
          OrgRecoveryStage::Waiting {
            approvals,
            required_approvals,
          } => i18n.t_count_with(
            "org-recovery-waiting",
            usize::try_from(required_approvals).unwrap_or(usize::MAX),
            &[("approvals", &approvals.to_string())],
          ),
          OrgRecoveryStage::Approved => i18n.t("org-recovery-approved"),
          _ => i18n.t("org-recovery-sent"),
        };
        view! {
          <p class="status-line" role="status">
            <span class="spinner" aria-hidden="true"></span>
            {text}
          </p>
          <Show when=move || is_offline.get() fallback=|| ()>
            <p class="muted-text small" role="status">{move || i18n.t("org-recovery-offline")}</p>
          </Show>
        }
        .into_any()
      },
    }
  }
}

/// A request that can't go on any more, and the way to a new one.
#[component]
fn ClosedRequest(text_key: &'static str, is_starting: RwSignal<bool>, on_ask: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <div class="auth-alert" role="alert">
      <Icon name=IconName::Alert />
      <p>{move || i18n.t(text_key)}</p>
    </div>
    <div class="step-aside">
      <button
        type="button"
        class="btn btn-small"
        aria-busy=move || is_starting.get().then_some("true")
        prop:disabled=move || is_starting.get()
        on:click=move |_| on_ask.run(())
      >
        {move || i18n.t("org-recovery-ask-again")}
      </button>
    </div>
  }
}

/// Where the page starts: `maybe_held` is what the core answered (`None`: no answer), with a
/// request ID kept from before (`has_marker`).
fn mount_from(maybe_held: Option<&OrgRecoveryStatus>, has_marker: bool) -> Mount {
  match maybe_held {
    Some(OrgRecoveryStatus::Requested { .. }) => Mount::Resume,
    _ if has_marker => Mount::Restarted,
    _ => Mount::Fresh,
  }
}

fn step_number(step: ItStep) -> u8 {
  match step {
    ItStep::Explain => 1,
    ItStep::Code => 2,
    ItStep::Save => 3,
    ItStep::Check => 4,
    ItStep::Finish => 5,
  }
}

fn stage_view(stage: &OrgRecoveryStage) -> StageView {
  match stage {
    OrgRecoveryStage::Ready => StageView::Ready,
    OrgRecoveryStage::Denied | OrgRecoveryStage::Expired | OrgRecoveryStage::Cancelled => StageView::Closed,
    // A stage this version doesn't know yet: keep waiting and asking.
    OrgRecoveryStage::Sent
    | OrgRecoveryStage::Waiting { .. }
    | OrgRecoveryStage::Approved
    | OrgRecoveryStage::Unknown => StageView::Pending,
  }
}

/// Whether the admins' answer is there, so the device can finish.
fn is_ready(status: &OrgRecoveryStatus) -> bool {
  matches!(status, OrgRecoveryStatus::Requested { stage, .. } if stage_view(stage) == StageView::Ready)
}

/// A start the core refused, in words: offline and the like by kind, anything else generically.
fn failure_text(i18n: crate::i18n::I18n, message: &str) -> String {
  let failure = ActionFailure::from_wire(message);
  match failure.kind {
    FailureKind::Other => i18n.t("org-recovery-start-failed"),
    _ => failure_message(i18n, &failure),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn only_a_delivered_key_lets_the_device_finish() {
    let requested = |stage| OrgRecoveryStatus::Requested {
      request_id: "7".parse().unwrap(),
      code: "ab12".into(),
      stage,
    };
    assert!(is_ready(&requested(OrgRecoveryStage::Ready)));
    for stage in [
      OrgRecoveryStage::Sent,
      OrgRecoveryStage::Waiting {
        approvals: 2,
        required_approvals: 2,
      },
      OrgRecoveryStage::Approved,
      OrgRecoveryStage::Denied,
      OrgRecoveryStage::Unknown,
    ] {
      assert!(!is_ready(&requested(stage)));
    }
    assert!(!is_ready(&OrgRecoveryStatus::NotStarted));
    assert!(!is_ready(&OrgRecoveryStatus::Unavailable));
  }

  #[test]
  fn closed_requests_stop_the_polling_and_offer_a_new_one() {
    for stage in [
      OrgRecoveryStage::Denied,
      OrgRecoveryStage::Expired,
      OrgRecoveryStage::Cancelled,
    ] {
      assert_eq!(stage_view(&stage), StageView::Closed);
    }
    assert_eq!(stage_view(&OrgRecoveryStage::Unknown), StageView::Pending);
    assert_eq!(stage_view(&OrgRecoveryStage::Approved), StageView::Pending);
  }

  #[test]
  fn a_request_the_core_still_holds_is_resumed() {
    let held = OrgRecoveryStatus::Requested {
      request_id: "7".parse().unwrap(),
      code: "ab12".into(),
      stage: OrgRecoveryStage::Ready,
    };
    assert_eq!(mount_from(Some(&held), true), Mount::Resume);
    assert_eq!(mount_from(Some(&held), false), Mount::Resume);
  }

  #[test]
  fn a_lost_request_says_so_only_when_one_was_open() {
    assert_eq!(
      mount_from(Some(&OrgRecoveryStatus::NotStarted), true),
      Mount::Restarted
    );
    assert_eq!(mount_from(None, true), Mount::Restarted);
    assert_eq!(
      mount_from(Some(&OrgRecoveryStatus::NotStarted), false),
      Mount::Fresh
    );
    assert_eq!(mount_from(None, false), Mount::Fresh);
  }

  #[test]
  fn the_marker_is_per_account() {
    assert_eq!(org_recovery_request_key(5), "teamcraft.onboarding.org-recovery.5");
    assert_ne!(org_recovery_request_key(5), org_recovery_request_key(6));
  }

  #[test]
  fn the_steps_count_after_the_choice_of_ways() {
    assert_eq!(STEPS_BEFORE + step_number(ItStep::Explain), 2);
    assert_eq!(
      STEPS_BEFORE + step_number(ItStep::Finish),
      STEPS_BEFORE + STEP_COUNT
    );
  }
}
