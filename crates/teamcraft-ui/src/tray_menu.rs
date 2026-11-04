//! The desktop tray icon's menu (macOS menu bar, Windows notification area, Linux StatusNotifier):
//! status, the controls of the user's call, a ringing call, and the general entries. The menu is
//! built here from the app's state and language ([`menu_model`], pure) and sent to the shell
//! whenever it changes; a chosen item comes back as a [`TrayAction`] and does what the matching
//! control in the window does ([`run`]), so nothing about calls or presence is duplicated.

use leptos::prelude::*;
use teamcraft_client_api::tray_menu::{
  TRAY_ACTION_EVENT, TrayAction, TrayCallState, TrayEntry, TrayMenuModel,
};

use crate::auth::tauri;
use crate::calls::{CallKind, CallPhase};
use crate::clock::{now_ms, tomorrow_at};
use crate::components::call_prejoin::call_heading;
use crate::components::status_menu::{MORNING_MINUTE, MS_PER_HOUR};
use crate::i18n::I18n;
use crate::meetings::MeetingsHub;
use crate::model::PresenceStatus;
use crate::state::AppState;

/// The user's own call, as far as the menu shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayCall {
  /// "Call with Bob".
  pub title: String,
  pub is_huddle: bool,
  pub is_muted: bool,
  pub is_camera_on: bool,
  pub is_sharing_screen: bool,
}

/// What decides the menu.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrayInput {
  pub presence: PresenceStatus,
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
    entries.push(TrayEntry::header(call.title.clone()));
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
    // The length of a running do-not-disturb isn't kept as a choice: only the status is checked.
    TrayEntry::action(TrayAction::DoNotDisturbForHour, t("tray-dnd-hour")),
    TrayEntry::action(TrayAction::DoNotDisturbUntilTomorrow, t("tray-dnd-tomorrow")),
    TrayEntry::choice(
      TrayAction::StatusOffline,
      t("tray-status-offline"),
      is(PresenceStatus::Offline),
    ),
    TrayEntry::Separator,
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

/// Keeps the shell's tray menu in step with the state (desktop app): rebuilt when the presence,
/// the call, a ringing call or the language changes, never on a timer. Chosen items come back
/// through [`run`].
pub fn start(state: AppState, i18n: I18n) {
  let maybe_hub = use_context::<MeetingsHub>();
  let call = state.call;
  Effect::new(move |previous: Option<TrayMenuModel>| {
    let presence = state.me().presence;
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
          is_huddle: request.kind == CallKind::Huddle,
          is_muted: !call.is_mic_on.get(),
          is_camera_on: call.is_camera_on.get(),
          is_sharing_screen: call.is_sharing_screen.get(),
        }
      });
    let input = TrayInput {
      presence,
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
  tauri::listen_text(TRAY_ACTION_EVENT, move |id| {
    if let Some(action) = TrayAction::from_id(&id) {
      run(state, action);
    }
  });
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
    // The shell does these itself.
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
      maybe_ringing: None,
      maybe_call: None,
    }
  }

  fn call() -> TrayCall {
    TrayCall {
      title: "Call with Bob".to_owned(),
      is_huddle: false,
      is_muted: false,
      is_camera_on: false,
      is_sharing_screen: false,
    }
  }

  const TAIL: [&str; 5] = ["-", "open_app", "check_for_updates", "-", "quit"];

  #[test]
  fn idle_has_status_and_the_general_entries_only() {
    let model = menu_model(&input(PresenceStatus::Away), &|key| key.to_owned());
    assert_eq!(model.call_state, TrayCallState::Idle);
    let list = keys(&model);
    assert_eq!(
      &list[..6],
      [
        "[ ] status_online",
        "[x] status_away",
        "[ ] status_do_not_disturb",
        "do_not_disturb_for_hour",
        "do_not_disturb_until_tomorrow",
        "[ ] status_offline",
      ]
    );
    assert_eq!(&list[6..], &TAIL);
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
