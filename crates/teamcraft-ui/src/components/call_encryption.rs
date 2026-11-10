//! The banner of a call this device couldn't be added to the end-to-end encryption of (#1555):
//! no media flows, so the user hears why, and can leave, or join again once the SFU takes this
//! device again.

use leptos::prelude::*;
use teamcraft_client_api::calls::{CallRequest, EncryptionFailure};

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::TimestampMs;
use crate::state::use_app_state;

const MS_PER_MINUTE: TimestampMs = 60_000;

/// Shows while the active call reports an encryption failure.
#[component]
pub fn EncryptionFailureBanner(request: CallRequest) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let failure = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .active_call()
        .and_then(|active| active.maybe_encryption_failure)
    })
  });
  // The retry waits from when this page saw the failure; the SFU counts from its refusal, a moment
  // earlier, so a retry is never refused for being early.
  let seen_at = RwSignal::new(None::<TimestampMs>);
  Effect::new(move |_| match failure.get() {
    Some(_) if seen_at.get_untracked().is_none() => seen_at.set(Some(crate::clock::now_ms())),
    None => seen_at.set(None),
    Some(_) => {},
  });
  let minutes_left = Memo::new(move |_| {
    // The call clock ticks every second while the call shows.
    call.elapsed_ms.track();
    minutes_left(failure.get()?, seen_at.get()?, crate::clock::now_ms())
  });
  let retry = move |_| {
    call.leave();
    call.join(request);
  };
  move || {
    failure.get()?;
    Some(view! {
      <div class="call-encryption-failed" role="alert">
        <Icon name=IconName::Alert />
        <p>
          {i18n.t("call-encryption-failed")}
          {move || minutes_left.get().map(|minutes| format!(" {}", i18n.t_count("call-encryption-retry-in", minutes)))}
        </p>
        <div class="call-encryption-actions">
          <button type="button" class="btn btn-small" disabled=move || minutes_left.get().is_some() on:click=retry>
            {i18n.t("call-encryption-retry")}
          </button>
          <button type="button" class="btn btn-small btn-danger" on:click=move |_| call.leave()>
            {i18n.t("call-leave")}
          </button>
        </div>
      </div>
    })
  }
}

/// Whole minutes until a retry may be taken, rounded up; `None` once it may.
fn minutes_left(failure: EncryptionFailure, seen_at: TimestampMs, now: TimestampMs) -> Option<usize> {
  let retry_at = seen_at + TimestampMs::from(failure.retry_after_secs) * 1000;
  let left = retry_at - now;
  (left > 0).then(|| usize::try_from((left + MS_PER_MINUTE - 1) / MS_PER_MINUTE).unwrap_or(usize::MAX))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn retry_waits_whole_minutes_rounded_up() {
    let failure = EncryptionFailure {
      retry_after_secs: 300,
    };
    assert_eq!(minutes_left(failure, 0, 0), Some(5));
    assert_eq!(minutes_left(failure, 0, 1), Some(5));
    assert_eq!(minutes_left(failure, 0, 240_001), Some(1));
    assert_eq!(minutes_left(failure, 0, 300_000), None);
    let at_once = EncryptionFailure { retry_after_secs: 0 };
    assert_eq!(minutes_left(at_once, 0, 0), None);
  }
}
