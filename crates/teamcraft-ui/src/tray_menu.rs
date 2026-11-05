//! The desktop tray icon's menu (macOS menu bar, Windows notification area, Linux StatusNotifier):
//! status, the controls of the user's call, a ringing call, and the general entries. The menu is
//! built here from the app's state and language ([`menu_model`], pure) and sent to the shell
//! whenever it changes; a chosen item comes back as a [`TrayAction`] and does what the matching
//! control in the window does ([`run`]), so nothing about calls or presence is duplicated.
//!
//! The call header carries the call's duration in whole minutes, so the menu is sent again once a
//! minute during a call; "Check for updates" is done by the shell, and the window then says what
//! came of it on a toast.

use leptos::prelude::*;
use teamcraft_client_api::tray_menu::{
  TRAY_ACTION_EVENT, TrayAction, TrayCallState, TrayEntry, TrayMenuModel,
};
use teamcraft_client_api::updates::{UpdateBlocker, UpdateState};

use crate::auth::tauri;
use crate::calls::{CallKind, CallPhase};
use crate::clock::{now_ms, tomorrow_at};
use crate::components::call_prejoin::call_heading;
use crate::components::status_menu::{MORNING_MINUTE, MS_PER_HOUR};
use crate::i18n::I18n;
use crate::meetings::MeetingsHub;
use crate::model::PresenceStatus;
use crate::model::TimestampMs;
use crate::state::{AppState, Overlay};

/// "Pause notifications → For 30 minutes" (the focus button's shortest choice).
const PAUSE_SHORT_MS: TimestampMs = 30 * 60_000;
const MS_PER_MINUTE: u64 = 60_000;
/// The shell's event when the update status changed (`updates::UPDATE_CHANGED_EVENT`).
const UPDATE_CHANGED_EVENT: &str = "update-changed";
/// How long the window waits for the check the tray asked for before it gives up on saying so.
const UPDATE_CHECK_PATIENCE_MS: TimestampMs = 120_000;

/// The user's own call, as far as the menu shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayCall {
  /// "Call with Bob".
  pub title: String,
  /// "12 min": how long this device has been in the call ([`duration_text`]); `None` in its first
  /// minute.
  pub maybe_duration: Option<String>,
  pub is_huddle: bool,
  /// The user hosts the meeting of this call, so it can end for everyone (the call screen's host
  /// control, confirmed there).
  pub can_end_for_everyone: bool,
  pub is_muted: bool,
  pub is_camera_on: bool,
  pub is_sharing_screen: bool,
}

/// What decides the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayInput {
  pub presence: PresenceStatus,
  /// The workspace allows custom statuses: "Set a custom status…" is offered.
  pub are_statuses_enabled: bool,
  /// "Bob is calling you" while a call rings on this device.
  pub maybe_ringing: Option<String>,
  pub maybe_call: Option<TrayCall>,
}

/// The menu for `input`; `t` words an i18n key in the user's language.
pub fn menu_model(input: &TrayInput, t: &dyn Fn(&str) -> String) -> TrayMenuModel {
  let mut entries = Vec::new();
  if let Some(text) = &input.maybe_ringing {
    entries.push(TrayEntry::header(text.clone()));
    entries.push(TrayEntry::action(TrayAction::AcceptCall, t("tray-call-accept")));
    entries.push(TrayEntry::action(TrayAction::DeclineCall, t("incoming-decline")));
    entries.push(TrayEntry::Separator);
  }
  if let Some(call) = &input.maybe_call {
    entries.push(TrayEntry::header(call_header(call)));
    entries.push(match call.is_muted {
      true => TrayEntry::action(TrayAction::Unmute, t("control-mic-unmute")),
      false => TrayEntry::action(TrayAction::Mute, t("control-mic-mute")),
    });
    entries.push(match call.is_camera_on {
      true => TrayEntry::action(TrayAction::CameraOff, t("control-camera-off")),
      false => TrayEntry::action(TrayAction::CameraOn, t("control-camera-on")),
    });
    if call.is_sharing_screen {
      entries.push(TrayEntry::action(TrayAction::StopSharing, t("share-stop")));
    }
    let leave = if call.is_huddle {
      "huddle-leave"
    } else {
      "call-bar-leave"
    };
    entries.push(TrayEntry::action(TrayAction::LeaveCall, t(leave)));
    if call.can_end_for_everyone {
      entries.push(TrayEntry::action(
        TrayAction::EndCallForEveryone,
        t("tray-end-for-everyone"),
      ));
    }
    entries.push(TrayEntry::action(TrayAction::OpenCall, t("call-open")));
    entries.push(TrayEntry::Separator);
  }
  let is = |presence| input.presence == presence;
  entries.extend([
    TrayEntry::choice(
      TrayAction::StatusOnline,
      t("tray-status-available"),
      is(PresenceStatus::Online),
    ),
    TrayEntry::choice(
      TrayAction::StatusAway,
      t("presence-away"),
      is(PresenceStatus::Away),
    ),
    TrayEntry::choice(
      TrayAction::StatusDoNotDisturb,
      t("presence-dnd"),
      is(PresenceStatus::DoNotDisturb),
    ),
    TrayEntry::choice(
      TrayAction::StatusOffline,
      t("tray-status-offline"),
      is(PresenceStatus::Offline),
    ),
  ]);
  if input.are_statuses_enabled {
    entries.push(TrayEntry::action(
      TrayAction::SetCustomStatus,
      t("tray-custom-status"),
    ));
  }
  entries.push(TrayEntry::Separator);
  // As the focus button in the window: pausing is do not disturb for a while, and while it is on,
  // one entry ends it. The length of a running pause isn't kept as a choice.
  entries.push(match input.presence {
    PresenceStatus::DoNotDisturb => {
      TrayEntry::action(TrayAction::ResumeNotifications, t("tray-resume-notifications"))
    },
    _ => TrayEntry::Submenu {
      label: t("tray-pause-notifications"),
      entries: vec![
        TrayEntry::action(TrayAction::PauseNotificationsBriefly, t("focus-30-minutes")),
        TrayEntry::action(TrayAction::DoNotDisturbForHour, t("presence-dnd-for-hour")),
        TrayEntry::action(
          TrayAction::DoNotDisturbUntilTomorrow,
          t("presence-dnd-until-morning"),
        ),
      ],
    },
  });
  entries.extend([
    TrayEntry::Separator,
    TrayEntry::action(TrayAction::NewMessage, t("tray-new-message")),
    TrayEntry::action(TrayAction::OpenApp, t("tray-show")),
    TrayEntry::action(TrayAction::CheckForUpdates, t("tray-check-updates")),
    TrayEntry::Separator,
    TrayEntry::action(TrayAction::Quit, t("tray-quit")),
  ]);
  let call_state = match &input.maybe_call {
    None => TrayCallState::Idle,
    Some(call) if call.is_muted => TrayCallState::Muted,
    Some(_) => TrayCallState::InCall,
  };
  TrayMenuModel { entries, call_state }
}

/// "Call with Bob · 12 min".
fn call_header(call: &TrayCall) -> String {
  match &call.maybe_duration {
    Some(duration) => format!("{} · {duration}", call.title),
    None => call.title.clone(),
  }
}

/// Words an i18n key with its arguments (`I18n::t_with`).
pub type TranslateWith<'a> = dyn Fn(&str, &[(&str, &str)]) -> String + 'a;

/// "12 min" or "1 h 05 min" for `minutes` in a call; `None` before the first whole minute.
pub fn duration_text(minutes: u64, t_with: &TranslateWith<'_>) -> Option<String> {
  match (minutes / 60, minutes % 60) {
    (0, 0) => None,
    (0, minutes) => Some(t_with("tray-call-minutes", &[("minutes", &minutes.to_string())])),
    (hours, minutes) => Some(t_with(
      "tray-call-hours",
      &[
        ("hours", &hours.to_string()),
        ("minutes", &format!("{minutes:02}")),
      ],
    )),
  }
}

/// Keeps the shell's tray menu in step with the state (desktop app): rebuilt when the presence,
/// the call (its duration once a minute), a ringing call or the language changes. Chosen items
/// come back through [`run`].
pub fn start(state: AppState, i18n: I18n) {
  let maybe_hub = use_context::<MeetingsHub>();
  let call = state.call;
  let update_check = UpdateCheckWatch::new();
  // The clock ticks every second; the menu only changes with the whole minutes.
  let call_minutes = Memo::new(move |_| call.elapsed_ms.get().map(|ms| ms / MS_PER_MINUTE));
  Effect::new(move |previous: Option<TrayMenuModel>| {
    let presence = state.me().presence;
    let are_statuses_enabled = state.read(|backend| backend.presence_policy()).statuses_enabled;
    let maybe_ringing = call.incoming.get().and_then(|incoming| {
      let caller = state.read(|backend| backend.user(incoming.caller))?;
      let key = if incoming.with_video {
        "incoming-video"
      } else {
        "incoming-audio"
      };
      Some(i18n.t_with(key, &[("name", &caller.display_name)]))
    });
    let maybe_call = (call.phase.get() == CallPhase::Active)
      .then(|| call.maybe_request.get())
      .flatten()
      .map(|request| {
        let maybe_meeting_title = maybe_hub.and_then(|hub| hub.title_for(request.conversation));
        let name = state
          .read(|backend| backend.conversation(request.conversation))
          .map(|conversation| conversation.name)
          .unwrap_or_default();
        TrayCall {
          title: call_heading(i18n, request.kind, maybe_meeting_title.as_deref(), &name),
          maybe_duration: duration_text(call_minutes.get().unwrap_or(0), &|key, args| {
            i18n.t_with(key, args)
          }),
          is_huddle: request.kind == CallKind::Huddle,
          can_end_for_everyone: maybe_hub
            .and_then(|hub| hub.hosted_meeting_for(request.conversation, state.me().id))
            .is_some(),
          is_muted: !call.is_mic_on.get(),
          is_camera_on: call.is_camera_on.get(),
          is_sharing_screen: call.is_sharing_screen.get(),
        }
      });
    let input = TrayInput {
      presence,
      are_statuses_enabled,
      maybe_ringing,
      maybe_call,
    };
    let model = menu_model(&input, &|key| i18n.t(key));
    if previous.as_ref() != Some(&model) {
      let sent = model.clone();
      leptos::task::spawn_local(async move { tauri::tray_menu_set(&sent).await });
    }
    model
  });
  // Signed out (the signed-in app is gone): back to the shell's plain menu.
  on_cleanup(|| leptos::task::spawn_local(async { tauri::tray_menu_set(&TrayMenuModel::default()).await }));
  tauri::listen_text(TRAY_ACTION_EVENT, move |id| match TrayAction::from_id(&id) {
    Some(TrayAction::CheckForUpdates) => update_check.begin(state, i18n),
    Some(action) => run(state, action),
    None => {},
  });
  tauri::listen(UPDATE_CHANGED_EVENT, move || update_check.follow(state, i18n));
}

/// The update check the tray asked for, until the window has said what came of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PendingCheck {
  asked_at: TimestampMs,
  /// The shell was seen checking: a failure from now on is this check's.
  has_seen_checking: bool,
}

/// What came of a check the tray asked for, from the shell's update state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckFeedback {
  /// Not done yet.
  Wait,
  /// An update was found or is on its way: the update banner says so.
  Quiet,
  /// "TeamCraft is up to date."
  UpToDate,
  /// Updates are off (by the organization, the machine, the user's choice, the build).
  Off(UpdateBlocker),
  Failed(String),
}

/// What `state` says about a check asked for at `asked_at`. A failure only counts once the check
/// was seen running (an older one may still be shown), and so does "up to date" unless its time
/// says it is newer.
pub fn check_feedback(state: &UpdateState, asked_at: TimestampMs, has_seen_checking: bool) -> CheckFeedback {
  match state {
    UpdateState::Checking => CheckFeedback::Wait,
    UpdateState::Idle {
      maybe_checked_at: Some(checked_at),
    } if *checked_at >= asked_at || has_seen_checking => CheckFeedback::UpToDate,
    UpdateState::Idle { .. } => CheckFeedback::Wait,
    UpdateState::Failed { message } if has_seen_checking => CheckFeedback::Failed(message.clone()),
    UpdateState::Failed { .. } => CheckFeedback::Wait,
    UpdateState::Off { reason } => CheckFeedback::Off(*reason),
    UpdateState::Available { .. }
    | UpdateState::Downloading { .. }
    | UpdateState::Installing { .. }
    | UpdateState::Ready { .. } => CheckFeedback::Quiet,
  }
}

/// Follows a check the tray asked for and says on a toast what came of it ("TeamCraft is up to
/// date."); a found update is left to the update banner.
#[derive(Clone, Copy)]
struct UpdateCheckWatch(StoredValue<Option<PendingCheck>>);

impl UpdateCheckWatch {
  fn new() -> Self {
    Self(StoredValue::new(None))
  }

  /// The tray's "Check for updates" was chosen (the shell checks already).
  fn begin(self, state: AppState, i18n: I18n) {
    let asked_at = now_ms();
    self.0.set_value(Some(PendingCheck {
      asked_at,
      has_seen_checking: false,
    }));
    set_timeout(
      move || {
        if self
          .0
          .try_get_value()
          .flatten()
          .is_some_and(|pending| pending.asked_at == asked_at)
        {
          self.0.try_set_value(None);
        }
      },
      std::time::Duration::from_millis(UPDATE_CHECK_PATIENCE_MS.unsigned_abs()),
    );
    self.follow(state, i18n);
  }

  /// Reads the shell's state again while a check is pending.
  fn follow(self, state: AppState, i18n: I18n) {
    if self.0.try_get_value().flatten().is_none() {
      return;
    }
    leptos::task::spawn_local(async move {
      let Some(status) = tauri::update_status().await else {
        return;
      };
      let Some(mut pending) = self.0.try_get_value().flatten() else {
        return;
      };
      if status.state == UpdateState::Checking {
        pending.has_seen_checking = true;
        self.0.try_set_value(Some(pending));
      }
      let text = match check_feedback(&status.state, pending.asked_at, pending.has_seen_checking) {
        CheckFeedback::Wait => return,
        CheckFeedback::Quiet => None,
        CheckFeedback::UpToDate => Some(i18n.t("update-state-idle")),
        CheckFeedback::Off(reason) => Some(i18n.t(&format!(
          "update-off-{}",
          crate::components::update_notice::blocker_slug(reason)
        ))),
        CheckFeedback::Failed(message) => Some(i18n.t_with("update-state-failed", &[("message", &message)])),
      };
      self.0.try_set_value(None);
      if let Some(text) = text {
        state.show_toast(text);
      }
    });
  }
}

/// Does what the control with the same meaning in the window does.
pub fn run(state: AppState, action: TrayAction) {
  let call = state.call;
  match action {
    TrayAction::StatusOnline => state.set_presence_until(PresenceStatus::Online, None),
    TrayAction::StatusAway => state.set_presence_until(PresenceStatus::Away, None),
    TrayAction::StatusOffline => state.set_presence_until(PresenceStatus::Offline, None),
    TrayAction::StatusDoNotDisturb => state.set_presence_until(PresenceStatus::DoNotDisturb, None),
    TrayAction::DoNotDisturbForHour => {
      state.set_presence_until(PresenceStatus::DoNotDisturb, Some(now_ms() + MS_PER_HOUR));
    },
    TrayAction::DoNotDisturbUntilTomorrow => state.set_presence_until(
      PresenceStatus::DoNotDisturb,
      Some(tomorrow_at(now_ms(), MORNING_MINUTE)),
    ),
    TrayAction::PauseNotificationsBriefly => {
      state.set_presence_until(PresenceStatus::DoNotDisturb, Some(now_ms() + PAUSE_SHORT_MS));
    },
    // As pressing the focus button while it is on.
    TrayAction::ResumeNotifications => state.set_presence_until(PresenceStatus::Online, None),
    TrayAction::AcceptCall => {
      if let Some(incoming) = call.incoming.get_untracked() {
        // The desktop card accepts with audio; the camera is one click away in the call.
        state.accept_call(incoming, false);
      }
    },
    TrayAction::DeclineCall => call.decline_incoming(),
    TrayAction::Mute => call.set_muted(true),
    TrayAction::Unmute => call.set_muted(false),
    TrayAction::CameraOn => call.set_camera(true),
    TrayAction::CameraOff => call.set_camera(false),
    TrayAction::StopSharing => call.set_screen_share(false),
    TrayAction::LeaveCall => call.leave(),
    TrayAction::OpenCall => call.is_minimized.set(false),
    TrayAction::EndCallForEveryone => {
      call.is_minimized.set(false);
      call.is_end_for_everyone_asked.set(true);
    },
    TrayAction::SetCustomStatus => state.overlay.set(Overlay::CustomStatus),
    TrayAction::NewMessage => state.overlay.set(Overlay::NewMessage),
    // The global shortcut: only a running call has a microphone to switch.
    TrayAction::ToggleMute => {
      if call.phase.get_untracked() == CallPhase::Active {
        call.set_muted(call.is_mic_on.get_untracked());
      }
    },
    // The shell does these itself ("Check for updates" is followed by `UpdateCheckWatch`).
    TrayAction::OpenApp | TrayAction::CheckForUpdates | TrayAction::Quit => {},
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn keys(model: &TrayMenuModel) -> Vec<String> {
    model
      .entries
      .iter()
      .map(|entry| match entry {
        TrayEntry::Item {
          maybe_action: Some(action),
          maybe_checked,
          ..
        } => match maybe_checked {
          Some(true) => format!("[x] {}", action.id()),
          Some(false) => format!("[ ] {}", action.id()),
          None => action.id().to_owned(),
        },
        TrayEntry::Item { label, .. } => format!("# {label}"),
        TrayEntry::Separator => "-".to_owned(),
        TrayEntry::Submenu { label, .. } => format!("> {label}"),
      })
      .collect()
  }

  fn input(presence: PresenceStatus) -> TrayInput {
    TrayInput {
      presence,
      are_statuses_enabled: true,
      maybe_ringing: None,
      maybe_call: None,
    }
  }

  fn call() -> TrayCall {
    TrayCall {
      title: "Call with Bob".to_owned(),
      maybe_duration: None,
      is_huddle: false,
      can_end_for_everyone: false,
      is_muted: false,
      is_camera_on: false,
      is_sharing_screen: false,
    }
  }

  const TAIL: [&str; 8] = [
    "-",
    "> tray-pause-notifications",
    "-",
    "new_message",
    "open_app",
    "check_for_updates",
    "-",
    "quit",
  ];

  #[test]
  fn idle_has_status_and_the_general_entries_only() {
    let model = menu_model(&input(PresenceStatus::Away), &|key| key.to_owned());
    assert_eq!(model.call_state, TrayCallState::Idle);
    let list = keys(&model);
    assert_eq!(
      &list[..4],
      [
        "[ ] status_online",
        "[x] status_away",
        "[ ] status_do_not_disturb",
        "[ ] status_offline",
      ]
    );
    assert_eq!(list[4], "set_custom_status");
    assert_eq!(&list[5..], &TAIL);
  }

  #[test]
  fn no_custom_status_entry_where_the_workspace_has_no_statuses() {
    let mut input = input(PresenceStatus::Online);
    input.are_statuses_enabled = false;
    let list = keys(&menu_model(&input, &|key| key.to_owned()));
    assert!(!list.iter().any(|line| line == "set_custom_status"));
    assert_eq!(&list[4..], &TAIL);
  }

  #[test]
  fn notifications_pause_from_a_submenu_and_resume_with_one_entry() {
    let model = menu_model(&input(PresenceStatus::Online), &|key| key.to_owned());
    let pause = model.entries.iter().find_map(|entry| match entry {
      TrayEntry::Submenu { label, entries } if label == "tray-pause-notifications" => Some(entries),
      _ => None,
    });
    let choices: Vec<_> = pause
      .expect("a pause submenu")
      .iter()
      .filter_map(|entry| match entry {
        TrayEntry::Item {
          maybe_action: Some(action),
          label,
          ..
        } => Some((*action, label.as_str())),
        _ => None,
      })
      .collect();
    assert_eq!(
      choices,
      [
        (TrayAction::PauseNotificationsBriefly, "focus-30-minutes"),
        (TrayAction::DoNotDisturbForHour, "presence-dnd-for-hour"),
        (
          TrayAction::DoNotDisturbUntilTomorrow,
          "presence-dnd-until-morning"
        ),
      ]
    );
    let paused = keys(&menu_model(&input(PresenceStatus::DoNotDisturb), &|key| {
      key.to_owned()
    }));
    assert_eq!(&paused[5..8], ["-", "resume_notifications", "-"]);
    assert!(!paused.iter().any(|line| line.starts_with('>')));
  }

  #[test]
  fn the_host_of_the_meeting_can_end_it_for_everyone() {
    let mut input = input(PresenceStatus::Online);
    input.maybe_call = Some(TrayCall {
      can_end_for_everyone: true,
      ..call()
    });
    let list = keys(&menu_model(&input, &|key| key.to_owned()));
    assert_eq!(
      &list[..6],
      [
        "# Call with Bob",
        "mute",
        "camera_on",
        "leave_call",
        "end_call_for_everyone",
        "open_call"
      ]
    );
  }

  #[test]
  fn the_call_header_carries_its_duration() {
    let mut input = input(PresenceStatus::Online);
    input.maybe_call = Some(TrayCall {
      maybe_duration: Some("12 min".to_owned()),
      ..call()
    });
    let list = keys(&menu_model(&input, &|key| key.to_owned()));
    assert_eq!(list[0], "# Call with Bob · 12 min");
  }

  #[test]
  fn durations_count_whole_minutes_and_hours() {
    let t_with = |key: &str, args: &[(&str, &str)]| {
      let args: Vec<String> = args
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
      format!("{key}({})", args.join(","))
    };
    assert_eq!(duration_text(0, &t_with), None, "nothing in the first minute");
    assert_eq!(
      duration_text(12, &t_with).as_deref(),
      Some("tray-call-minutes(minutes=12)")
    );
    assert_eq!(
      duration_text(65, &t_with).as_deref(),
      Some("tray-call-hours(hours=1,minutes=05)")
    );
    assert_eq!(
      duration_text(120, &t_with).as_deref(),
      Some("tray-call-hours(hours=2,minutes=00)")
    );
  }

  #[test]
  fn a_check_from_the_tray_says_up_to_date_once_it_is_done() {
    let asked_at = 1_000;
    let idle = |at| UpdateState::Idle {
      maybe_checked_at: Some(at),
    };
    assert_eq!(
      check_feedback(&idle(500), asked_at, false),
      CheckFeedback::Wait,
      "an older check"
    );
    assert_eq!(
      check_feedback(&idle(1_200), asked_at, false),
      CheckFeedback::UpToDate
    );
    assert_eq!(
      check_feedback(&UpdateState::Checking, asked_at, false),
      CheckFeedback::Wait
    );
    assert_eq!(
      check_feedback(
        &UpdateState::Idle {
          maybe_checked_at: None
        },
        asked_at,
        false
      ),
      CheckFeedback::Wait
    );
  }

  #[test]
  fn a_failure_counts_only_after_the_check_was_seen_running() {
    let failed = UpdateState::Failed {
      message: "offline".to_owned(),
    };
    assert_eq!(check_feedback(&failed, 1_000, false), CheckFeedback::Wait);
    assert_eq!(
      check_feedback(&failed, 1_000, true),
      CheckFeedback::Failed("offline".to_owned())
    );
  }

  #[test]
  fn a_found_update_is_left_to_the_banner_and_off_says_why() {
    let available = UpdateState::Available {
      version: "2.0.0".to_owned(),
      maybe_notes_url: None,
    };
    assert_eq!(check_feedback(&available, 1_000, false), CheckFeedback::Quiet);
    assert_eq!(
      check_feedback(
        &UpdateState::Off {
          reason: UpdateBlocker::Policy
        },
        1_000,
        false
      ),
      CheckFeedback::Off(UpdateBlocker::Policy)
    );
  }

  #[test]
  fn new_message_and_custom_status_use_their_own_labels() {
    let model = menu_model(&input(PresenceStatus::Online), &|key| format!("<{key}>"));
    let label_of = |wanted: TrayAction| {
      model.entries.iter().find_map(|entry| match entry {
        TrayEntry::Item {
          maybe_action: Some(action),
          label,
          ..
        } if *action == wanted => Some(label.clone()),
        _ => None,
      })
    };
    assert_eq!(
      label_of(TrayAction::NewMessage).as_deref(),
      Some("<tray-new-message>")
    );
    assert_eq!(
      label_of(TrayAction::SetCustomStatus).as_deref(),
      Some("<tray-custom-status>")
    );
  }

  #[test]
  fn exactly_the_current_status_is_checked() {
    for presence in PresenceStatus::ALL {
      let model = menu_model(&input(presence), &|key| key.to_owned());
      let checked = keys(&model).iter().filter(|line| line.starts_with("[x]")).count();
      assert_eq!(checked, 1, "{presence:?}");
    }
  }

  #[test]
  fn a_call_adds_its_controls_above_the_status() {
    let mut input = input(PresenceStatus::Online);
    input.maybe_call = Some(call());
    let model = menu_model(&input, &|key| key.to_owned());
    assert_eq!(model.call_state, TrayCallState::InCall);
    let list = keys(&model);
    assert_eq!(
      &list[..7],
      [
        "# Call with Bob",
        "mute",
        "camera_on",
        "leave_call",
        "open_call",
        "-",
        "[x] status_online"
      ]
    );
  }

  #[test]
  fn the_call_entries_follow_the_state_of_the_call() {
    let mut input = input(PresenceStatus::Online);
    input.maybe_call = Some(TrayCall {
      is_muted: true,
      is_camera_on: true,
      is_sharing_screen: true,
      is_huddle: true,
      ..call()
    });
    let model = menu_model(&input, &|key| key.to_owned());
    assert_eq!(model.call_state, TrayCallState::Muted);
    assert_eq!(
      &keys(&model)[..6],
      [
        "# Call with Bob",
        "unmute",
        "camera_off",
        "stop_sharing",
        "leave_call",
        "open_call"
      ]
    );
    let leave = model.entries.iter().find_map(|entry| match entry {
      TrayEntry::Item {
        maybe_action: Some(TrayAction::LeaveCall),
        label,
        ..
      } => Some(label.as_str()),
      _ => None,
    });
    assert_eq!(leave, Some("huddle-leave"));
  }

  #[test]
  fn a_ringing_call_offers_accept_and_decline_first() {
    let mut input = input(PresenceStatus::Online);
    input.maybe_ringing = Some("Bob is calling you".to_owned());
    let model = menu_model(&input, &|key| key.to_owned());
    assert_eq!(
      &keys(&model)[..5],
      [
        "# Bob is calling you",
        "accept_call",
        "decline_call",
        "-",
        "[x] status_online"
      ]
    );
  }

  #[test]
  fn every_label_comes_from_the_translator() {
    let mut input = input(PresenceStatus::Online);
    input.maybe_ringing = Some("ring".to_owned());
    input.maybe_call = Some(TrayCall {
      is_sharing_screen: true,
      ..call()
    });
    let model = menu_model(&input, &|key| format!("<{key}>"));
    for entry in &model.entries {
      if let TrayEntry::Item {
        maybe_action: Some(_),
        label,
        ..
      } = entry
      {
        assert!(label.starts_with('<') && label.ends_with('>'), "{label}");
      }
    }
  }
}
