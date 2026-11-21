//! Call recording in the call screens (issue #216, e2ee.md §8.3).
//!
//! - **Indicator:** while anybody records, the call header says who, and for how long when it is
//!   this device. It comes from the signed notices the media layer verified, and nothing closes it.
//! - **Banner:** once per recording that starts, and on joining a call that is being recorded: who
//!   records, with "Leave call" next to "Continue" (objecting means leaving). When the verified
//!   policy doesn't allow the recorder, it says so.
//! - **Button:** "Record" in the control bar, only where the app records (the desktop app, and the
//!   browser where it has WebCodecs, #2114) and the verified policy allows the user (admin.md
//!   §1.1).
//! - **Take:** the finished recording waits in a card, also after the call ended, until the user
//!   posts it into the call's conversation (an encrypted attachment) or deletes it. The desktop
//!   shell stages its decrypted copy for the core; in the browser the call module hands over the
//!   decrypted file, which goes to the core like a picked file.

use std::collections::BTreeSet;

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;
use teamcraft_client_api::actions::{FileSource, MessagePriority, OutgoingFile, OutgoingMessage};
use teamcraft_client_api::admin::format_bytes;
use teamcraft_client_api::backend::Backend;
use teamcraft_client_api::calls::CallRecordingTake;
use teamcraft_client_api::failure::ActionFailure;
use teamcraft_proto::{CallRecordingPolicy, RecorderRole, Role, UserId};

use crate::auth::tauri;
use crate::components::icons::{Icon, IconName};
use crate::i18n::{I18n, use_i18n};
use crate::media_layout::format_duration;
use crate::state::{AppState, use_app_state};

/// A call that started this recently counts as just joined: a recording found then was running
/// before the user came in.
const JUST_JOINED_MS: u64 = 10_000;

/// The recording state the call header shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct RecordingStatus {
  /// When this device started recording (wall clock).
  maybe_own_since: Option<i64>,
  /// The SFU doesn't confirm the own notice: the others may not see the recording.
  is_own_unconfirmed: bool,
  /// The others who record, by name, in a stable order.
  others: Vec<Recorder>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Recorder {
  user: UserId,
  name: String,
  /// The verified policy allows them to record.
  is_allowed: bool,
  /// The server says they left, but their device may still hear the call.
  has_left: bool,
}

/// What the control bar needs for the record button.
#[derive(Clone, Copy)]
pub struct RecordingControl {
  /// The button shows: the app records, and the policy allows the user.
  pub is_offered: Memo<bool>,
  pub is_recording: Memo<bool>,
  policy: Memo<CallRecordingPolicy>,
  state: AppState,
}

/// The record button's state for the active call.
pub fn use_recording_control() -> RecordingControl {
  let state = use_app_state();
  let policy = Memo::new(move |_| state.read(|backend| backend.call_recording_policy()));
  let is_offered = Memo::new(move |_| {
    state.read(|backend| backend.can_record_calls() && may_record(&policy.get(), &backend.my_role()))
  });
  let is_recording = Memo::new(move |_| {
    state.read(|backend| {
      backend
        .active_call()
        .is_some_and(|active| active.maybe_own_recording.is_some())
    })
  });
  RecordingControl {
    is_offered,
    is_recording,
    policy,
    state,
  }
}

impl RecordingControl {
  /// Starts recording, or stops the running recording.
  pub fn toggle(&self) {
    let state = self.state;
    if self.is_recording.get_untracked() {
      state.write(|backend| backend.stop_call_recording());
    } else {
      let max_secs = self.policy.get_untracked().max_seconds();
      state.write(|backend| backend.start_call_recording(max_secs));
    }
  }
}

/// The header's recording indicator.
#[component]
pub fn RecordingIndicator() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let status = Memo::new(move |_| state.read(recording_status));
  move || {
    let status = status.get();
    let names = names_of(i18n, &status.others);
    let text = match (status.maybe_own_since, names.is_empty()) {
      (None, true) => return None,
      (Some(since), true) => {
        // The call clock ticks every second while the call shows.
        call.elapsed_ms.track();
        let elapsed = u64::try_from(crate::clock::now_ms() - since).unwrap_or(0);
        i18n.t_with("call-recording-own", &[("time", &format_duration(elapsed))])
      },
      (None, false) => i18n.t_with("call-recording-by", &[("names", &names)]),
      (Some(_), false) => i18n.t_with("call-recording-own-and-by", &[("names", &names)]),
    };
    let is_unconfirmed = status.is_own_unconfirmed;
    Some(view! {
      <span class="recording-pill" role="status" title=i18n.t("call-recording-detail")>
        <span class="recording-dot" aria-hidden="true"></span>
        <span>{text}</span>
      </span>
      {is_unconfirmed.then(|| view! {
        <span class="recording-pill is-warning" role="alert">
          <Icon name=IconName::Alert />
          <span>{i18n.t("call-recording-unconfirmed")}</span>
        </span>
      })}
    })
  }
}

/// The banner of a recording that started, or that was running when the user joined.
#[component]
pub fn RecordingBanner() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let call = state.call;
  let others = Memo::new(move |_| state.read(recording_status).others);
  // Who was recording at the last look, and the banner shown: (was running at the join, who).
  let seen = StoredValue::new(BTreeSet::<UserId>::new());
  let shown = RwSignal::new(None::<(bool, Vec<UserId>)>);
  Effect::new(move |_| {
    let now: BTreeSet<UserId> = others.get().iter().map(|recorder| recorder.user).collect();
    let before = seen.get_value();
    let started: Vec<UserId> = now.difference(&before).copied().collect();
    seen.set_value(now.clone());
    if !started.is_empty() {
      let is_at_join = call
        .elapsed_ms
        .get_untracked()
        .is_none_or(|elapsed| elapsed < JUST_JOINED_MS);
      shown.set(Some((is_at_join, started)));
    } else if shown
      .get_untracked()
      .is_some_and(|(_, users)| users.iter().all(|user| !now.contains(user)))
    {
      // Everybody it named stopped: nothing to decide any more.
      shown.set(None);
    }
  });
  move || {
    let (is_at_join, users) = shown.get()?;
    let recorders: Vec<Recorder> = others
      .get()
      .into_iter()
      .filter(|recorder| users.contains(&recorder.user))
      .collect();
    let names = names_of(i18n, &recorders);
    let key = if is_at_join {
      "call-recording-banner-running"
    } else {
      "call-recording-banner-started"
    };
    let is_unallowed = recorders.iter().any(|recorder| !recorder.is_allowed);
    Some(view! {
      <div class="call-recording-banner" role="alert">
        <Icon name=IconName::Record />
        <p>
          {i18n.t_with(key, &[("names", &names)])}
          " "
          {i18n.t("call-recording-banner-choice")}
          {is_unallowed.then(|| view! { <strong>" "{i18n.t("call-recording-not-allowed")}</strong> })}
        </p>
        <div class="call-recording-actions">
          <button type="button" class="btn btn-small" on:click=move |_| shown.set(None)>
            {i18n.t("call-recording-continue")}
          </button>
          <button type="button" class="btn btn-small btn-danger" on:click=move |_| call.leave()>
            {i18n.t("call-leave")}
          </button>
        </div>
      </div>
    })
  }
}

/// The finished recording, until the user posts or deletes it. Outside the call screen: it
/// outlives the call.
#[component]
pub fn CallRecordingTakeCard() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let take = Memo::new(move |_| state.read(|backend| backend.call_recording_take()));
  let is_posting = RwSignal::new(false);
  let is_confirming_delete = RwSignal::new(false);
  let post = move |take: CallRecordingTake| {
    if is_posting.get_untracked() {
      return;
    }
    is_posting.set(true);
    spawn_local(async move {
      match recording_source().await {
        Ok(source) => {
          // The card may be gone by now (the take left the snapshot): the app state outlives it.
          if state
            .try_write(|backend| backend.send_message(recording_message(take, source)))
            .is_some()
          {
            state.show_brief_toast(i18n.t("call-recording-posted"));
          }
        },
        Err(failure) => crate::feedback::report_failure(failure),
      }
      is_posting.try_set(false);
    });
  };
  // In the browser the take lives in the page: closing or reloading it loses the take.
  let hint_key = if tauri::is_available() {
    "call-recording-take-hint"
  } else {
    "call-recording-take-hint-browser"
  };
  move || {
    let take = take.get()?;
    let meta = i18n.t_with(
      "call-recording-take-meta",
      &[
        ("time", &format_duration(take.duration_ms)),
        ("size", &format_bytes(take.size_bytes)),
      ],
    );
    Some(view! {
      <section class="call-recording-take" role="region" aria-labelledby="call-recording-take-title">
        <Icon name=IconName::Record />
        <div class="call-recording-take-text">
          <h2 id="call-recording-take-title">{i18n.t("call-recording-take-title")}</h2>
          <p class="muted-text">{meta}</p>
          {take.is_at_limit.then(|| view! { <p class="muted-text">{i18n.t("call-recording-take-limit")}</p> })}
          {take.is_at_memory_limit.then(|| view! { <p class="muted-text">{i18n.t("call-recording-take-memory-limit")}</p> })}
          <p class="muted-text">{i18n.t(hint_key)}</p>
        </div>
        <Show
          when=move || is_confirming_delete.get()
          fallback=move || view! {
            <div class="call-recording-actions">
              <button
                type="button"
                class="btn btn-small btn-primary"
                disabled=move || is_posting.get()
                on:click=move |_| post(take)
              >
                {move || i18n.t(if is_posting.get() { "call-recording-posting" } else { "call-recording-post" })}
              </button>
              <button type="button" class="btn btn-small" on:click=move |_| is_confirming_delete.set(true)>
                {i18n.t("call-recording-delete")}
              </button>
            </div>
          }
        >
          <div class="call-recording-actions" role="group" aria-label=move || i18n.t("call-recording-delete-confirm")>
            <p>{i18n.t("call-recording-delete-confirm")}</p>
            <button type="button" class="btn btn-small" on:click=move |_| is_confirming_delete.set(false)>
              {i18n.t("cancel")}
            </button>
            <button
              type="button"
              class="btn btn-small btn-danger"
              on:click=move |_| {
                is_confirming_delete.set(false);
                state.write(|backend| backend.discard_call_recording());
              }
            >
              {i18n.t("call-recording-delete")}
            </button>
          </div>
        </Show>
      </section>
    })
  }
}

/// Whether the user, of `role`, may record under `policy`.
fn may_record(policy: &CallRecordingPolicy, role: &Role) -> bool {
  policy.allows(RecorderRole::of(role))
}

/// The recording state of the active call, by name. Whether a recorder is allowed uses the
/// verified policy and the server's guest flag and admin list: a lie there only changes the "not
/// allowed" note, never whether the recording shows.
fn recording_status(backend: &dyn Backend) -> RecordingStatus {
  let Some(active) = backend.active_call() else {
    return RecordingStatus::default();
  };
  let policy = backend.call_recording_policy();
  let (owners, admins) = backend.admin_role_holders();
  let mut others: Vec<Recorder> = active
    .recorders
    .iter()
    .map(|recorder| {
      let maybe_user = backend.user(recorder.user);
      let is_guest = maybe_user.as_ref().is_some_and(|user| user.is_guest);
      let role = if is_guest {
        RecorderRole::Guest
      } else if owners.contains(&recorder.user) || admins.contains(&recorder.user) {
        RecorderRole::Admin
      } else {
        RecorderRole::Member
      };
      Recorder {
        user: recorder.user,
        name: maybe_user
          .map(|user| user.display_name)
          .filter(|name| !name.is_empty())
          .unwrap_or_default(),
        is_allowed: policy.allows(role),
        has_left: recorder.has_left,
      }
    })
    .collect();
  others.sort_by(|a, b| a.name.cmp(&b.name).then(a.user.cmp(&b.user)));
  RecordingStatus {
    maybe_own_since: active.maybe_own_recording.map(|own| own.started_at),
    is_own_unconfirmed: active.maybe_own_recording.is_some_and(|own| own.is_unconfirmed),
    others,
  }
}

/// The recorders' names for a sentence; somebody without a known name counts as "someone".
fn names_of(i18n: I18n, recorders: &[Recorder]) -> String {
  recorders
    .iter()
    .map(|recorder| {
      let name = if recorder.name.is_empty() {
        i18n.t("call-recording-someone")
      } else {
        recorder.name.clone()
      };
      if recorder.has_left {
        i18n.t_with("call-recording-left", &[("name", &name)])
      } else {
        name
      }
    })
    .collect::<Vec<_>>()
    .join(", ")
}

/// The decrypted recording as the core uploads it: the desktop shell's staged copy, or the file
/// the browser's call module hands over (registered with the core like a picked file).
async fn recording_source() -> Result<FileSource, ActionFailure> {
  if tauri::is_available() {
    let path = tauri::call::<String>("call_recording_stage", &json!({}))
      .await
      .map_err(ActionFailure::other)?;
    return Ok(FileSource::Path { path });
  }
  browser_recording_source().await
}

#[cfg(target_arch = "wasm32")]
async fn browser_recording_source() -> Result<FileSource, ActionFailure> {
  let blob = crate::web_calls::recording_file().await?;
  let bag = web_sys::FilePropertyBag::new();
  bag.set_type("audio/ogg");
  let file =
    web_sys::File::new_with_blob_sequence_and_options(&js_sys::Array::of1(&blob), &file_name_now(), &bag)
      .map_err(|_| ActionFailure::other("the recording could not be made a file"))?;
  crate::large_files::source_of(&crate::large_files::LargeFile::Picked(file))
    .await
    .map_err(ActionFailure::other)
}

#[cfg(not(target_arch = "wasm32"))]
async fn browser_recording_source() -> Result<FileSource, ActionFailure> {
  Err(ActionFailure::other("calls run in the browser only on the web"))
}

/// The message that posts a recording into its call's conversation.
fn recording_message(take: CallRecordingTake, source: FileSource) -> OutgoingMessage {
  OutgoingMessage {
    maybe_client_message_id: None,
    conversation: take.conversation,
    maybe_thread: None,
    body: String::new(),
    maybe_reply_to: None,
    maybe_reply_attachment: None,
    priority: MessagePriority::Normal,
    wants_acknowledgement: false,
    is_silent: false,
    also_send_to_channel: false,
    files: vec![OutgoingFile {
      file_name: file_name_now(),
      media_type: "audio/ogg".to_owned(),
      source,
      maybe_thumbnail: None,
      maybe_duration_ms: Some(take.duration_ms),
      maybe_waveform: None,
      maybe_draft_id: None,
      maybe_motion: None,
    }],
    link_preview: Default::default(),
  }
}

/// `call-recording-20261117-1405.ogg`, in local time.
fn file_name_now() -> String {
  let date = js_sys::Date::new_0();
  format!(
    "call-recording-{:04}{:02}{:02}-{:02}{:02}.ogg",
    date.get_full_year(),
    date.get_month() + 1,
    date.get_date(),
    date.get_hours(),
    date.get_minutes(),
  )
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::CallRecorders;

  use super::*;

  #[test]
  fn the_button_follows_the_policy_and_the_role() {
    let policy = |recorders| CallRecordingPolicy {
      recorders,
      ..CallRecordingPolicy::default()
    };
    assert!(
      !may_record(&CallRecordingPolicy::default(), &Role::Owner),
      "off by default"
    );
    assert!(may_record(&policy(CallRecorders::Admins), &Role::Owner));
    assert!(!may_record(&policy(CallRecorders::Admins), &Role::Member));
    assert!(may_record(&policy(CallRecorders::Members), &Role::Member));
    assert!(!may_record(&policy(CallRecorders::Members), &Role::Guest));
  }
}
