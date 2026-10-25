//! Runs the slash commands of the composer (see `slash.rs`) against the backend.

use leptos::prelude::*;
use teamcraft_client_api::actions::ChannelChanges;
use teamcraft_client_api::custom_status::is_allowed;
use teamcraft_proto::{HistoryPolicy, IdempotencyKey};

use crate::clock::{end_of_local_day, now_ms};
use crate::feedback::PendingAction;
use crate::i18n::I18n;
use crate::meetings::MeetingsHub;
use crate::model::{ConversationId, CustomStatus, StatusExpiry, User};
use crate::prefs::{ConversationNotify, MuteFor};
use crate::reminders::{Reminder, due_time, from_json, to_json};
use crate::settings::{load_string, store_string};
use crate::slash::{ReminderTime, SlashCommand, SlashError, StatusChange};
use crate::state::{AppState, Overlay};

// Markdown source: the backslash and both underscores are escaped so they show as typed.
const SHRUG: &str = "¯\\\\\\_(ツ)\\_/¯";
const REMINDERS_KEY: &str = "teamcraft.reminders";
const MS_PER_DAY: i64 = 86_400_000;

/// What the composer shows after a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notice {
  Text(String),
  Help,
}

/// What running a command asks of the composer.
pub enum Outcome {
  /// Nothing to send; show this.
  Notice(Notice),
  /// Send this text as a message.
  Send(String),
  Done,
}

pub fn explain_error(i18n: I18n, error: &SlashError) -> String {
  match error {
    SlashError::MissingArgument(hint) => i18n.t_with("slash-missing-argument", &[("usage", hint)]),
    SlashError::BadReminderTime => i18n.t("slash-bad-reminder"),
  }
}

pub fn run(state: AppState, i18n: I18n, conversation: ConversationId, command: SlashCommand) -> Outcome {
  let text = |key: &str| Outcome::Notice(Notice::Text(i18n.t(key)));
  match command {
    SlashCommand::Presence(presence) => {
      state.write(|backend| backend.set_presence(presence));
      Outcome::Done
    },
    SlashCommand::Status(StatusChange::Clear) => {
      state.write(|backend| backend.set_custom_status(None));
      text("slash-status-cleared")
    },
    SlashCommand::Status(StatusChange::Set { emoji, text: status }) => {
      let status = CustomStatus {
        emoji,
        text: status,
        expiry: StatusExpiry::Never,
        maybe_expires_at: None,
      };
      // The server would refuse it; say why instead of failing later.
      let policy = state.read(|backend| backend.presence_policy());
      if !is_allowed(&policy, &status) {
        return text(if policy.statuses_enabled {
          "slash-status-presets-only"
        } else {
          "slash-status-disabled"
        });
      }
      state.write(|backend| backend.set_custom_status(Some(status)));
      text("slash-status-set")
    },
    SlashCommand::Topic(topic) => change_channel(
      state,
      conversation,
      ChannelChanges {
        maybe_topic: Some(topic),
        ..ChannelChanges::default()
      },
    ),
    SlashCommand::Purpose(description) => change_channel(
      state,
      conversation,
      ChannelChanges {
        maybe_description: Some(description),
        ..ChannelChanges::default()
      },
    ),
    SlashCommand::Rename(name) => change_channel(
      state,
      conversation,
      ChannelChanges {
        maybe_name: Some(name),
        ..ChannelChanges::default()
      },
    ),
    SlashCommand::Join(name) => join_channel(state, i18n, conversation, &name),
    SlashCommand::Open(name) => match find_conversation(state, &name) {
      Some(found) => {
        state.open_conversation(found);
        Outcome::Done
      },
      None => Outcome::Notice(Notice::Text(i18n.t_with("slash-not-found", &[("name", &name)]))),
    },
    SlashCommand::Leave => {
      state.leave_with_undo(conversation, left_text(state, i18n, conversation));
      Outcome::Done
    },
    SlashCommand::Kick(name) => match find_person(state, &name) {
      Some(person) => {
        state.write(|backend| backend.remove_member(conversation, person.id));
        Outcome::Notice(Notice::Text(
          i18n.t_with("slash-kicked", &[("name", &person.display_name)]),
        ))
      },
      None => Outcome::Notice(Notice::Text(i18n.t_with("slash-not-found", &[("name", &name)]))),
    },
    SlashCommand::Invite(name) => match find_person(state, &name) {
      Some(person) => {
        state
          .write(|backend| backend.add_member_with_history(conversation, person.id, HistoryPolicy::FromJoin));
        Outcome::Notice(Notice::Text(
          i18n.t_with("slash-invited", &[("name", &person.display_name)]),
        ))
      },
      None => Outcome::Notice(Notice::Text(i18n.t_with("slash-not-found", &[("name", &name)]))),
    },
    SlashCommand::Msg { user, text: body } => match find_person(state, &user) {
      Some(person) => {
        state.open_direct(person.id);
        if body.is_empty() {
          Outcome::Done
        } else {
          // The conversation may not exist yet; the text goes out once the user is in it.
          Outcome::Notice(Notice::Text(i18n.t("slash-msg-send-there")))
        }
      },
      None => Outcome::Notice(Notice::Text(i18n.t_with("slash-not-found", &[("name", &user)]))),
    },
    SlashCommand::GroupMsg(_) => text("slash-groupmsg-unsupported"),
    SlashCommand::Mute => {
      let id = conversation.to_string();
      state.settings.prefs.update(|prefs| {
        let mut entry = prefs.notifications.conversation(&id);
        entry.mute = if entry.mute == MuteFor::Off {
          MuteFor::Forever
        } else {
          MuteFor::Off
        };
        prefs
          .notifications
          .set_conversation(&id, ConversationNotify { ..entry });
      });
      let is_muted = state
        .settings
        .prefs
        .with_untracked(|prefs| prefs.notifications.is_muted(&id));
      text(if is_muted { "slash-muted" } else { "slash-unmuted" })
    },
    SlashCommand::Me(action) => Outcome::Send(format!("_{action}_")),
    SlashCommand::Shrug(body) => Outcome::Send(if body.is_empty() {
      SHRUG.to_owned()
    } else {
      format!("{body} {SHRUG}")
    }),
    SlashCommand::Code(code) => Outcome::Send(format!("```\n{code}\n```")),
    SlashCommand::Search(query) => {
      state.open_search(Some(&query));
      Outcome::Done
    },
    SlashCommand::Settings => {
      state.open_settings();
      Outcome::Done
    },
    SlashCommand::Shortcuts => {
      state.overlay.set(Overlay::Shortcuts);
      Outcome::Done
    },
    SlashCommand::Help => Outcome::Notice(Notice::Help),
    SlashCommand::Remind { when, text: body } => remind(i18n, when, body),
    SlashCommand::Meet => start_meeting(state, i18n),
  }
}

fn change_channel(state: AppState, conversation: ConversationId, changes: ChannelChanges) -> Outcome {
  state.write(|backend| backend.update_channel(conversation, changes));
  Outcome::Done
}

fn join_channel(state: AppState, i18n: I18n, current: ConversationId, name: &str) -> Outcome {
  let space = state
    .read(|backend| backend.conversation(current))
    .and_then(|conversation| conversation.maybe_space)
    .unwrap_or_else(|| state.space.get_untracked());
  let found = state.read(|backend| {
    backend
      .browse_channels(space)
      .into_iter()
      .find(|channel| channel.name.eq_ignore_ascii_case(name) && !channel.is_archived)
  });
  match found {
    Some(channel) => {
      let conversation = channel.conversation;
      // Set first: a backend that refuses at once calls the cleanup inside `run_or_else`.
      state.pending_open.set(Some(conversation));
      if !channel.is_member {
        // A refused join shows its error and forgets the open (issues #322, #328).
        PendingAction::new().run_or_else(
          || state.write(|backend| backend.join_channel(conversation)),
          move |_| state.forget_pending_open(conversation),
        );
      }
      Outcome::Done
    },
    None => Outcome::Notice(Notice::Text(i18n.t_with("slash-not-found", &[("name", name)]))),
  }
}

fn find_conversation(state: AppState, name: &str) -> Option<ConversationId> {
  state.read(|backend| {
    let mut all = Vec::new();
    for space in backend.spaces() {
      all.extend(backend.space_conversations(space.id));
    }
    all.extend(backend.direct_conversations());
    all
      .into_iter()
      .find(|conversation| conversation.name.eq_ignore_ascii_case(name))
      .map(|conversation| conversation.id)
  })
}

/// A person by username or display name (case-insensitive).
pub fn find_person(state: AppState, name: &str) -> Option<User> {
  state.read(|backend| {
    backend
      .users()
      .into_iter()
      .find(|user| user.username.eq_ignore_ascii_case(name) || user.display_name.eq_ignore_ascii_case(name))
  })
}

fn remind(i18n: I18n, when: ReminderTime, body: String) -> Outcome {
  let now = now_ms();
  let start_of_today = end_of_local_day(now) - MS_PER_DAY;
  let due_at = due_time(when, now, start_of_today);
  let mut list = from_json(&load_string(REMINDERS_KEY).unwrap_or_default());
  list.push(Reminder { due_at, text: body });
  store_string(REMINDERS_KEY, &to_json(&list));
  let at = crate::clock::format_time(due_at, i18n.locale().tag());
  Outcome::Notice(Notice::Text(i18n.t_with("slash-reminder-set", &[("time", &at)])))
}

fn start_meeting(state: AppState, i18n: I18n) -> Outcome {
  let Some(hub) = use_context::<MeetingsHub>() else {
    return Outcome::Notice(Notice::Text(i18n.t("slash-meet-unavailable")));
  };
  let client = hub.client();
  leptos::task::spawn_local(async move {
    if let Ok(meeting) = client
      .start_instant(None, Vec::new(), Some(IdempotencyKey::random()))
      .await
    {
      // The server makes the chat without encryption; an instant meeting invites nobody.
      // Gone only with the app; then the chat stays without encryption (nobody else founds it).
      state.try_write(|backend| backend.set_up_meeting_chat(meeting.conversation_id, Vec::new()));
      state.open_meetings();
    }
  });
  Outcome::Notice(Notice::Text(i18n.t("slash-meet-starting")))
}

/// Reminders that came due: removes them from storage and returns them.
pub fn due_reminders(now: i64) -> Vec<Reminder> {
  let all = from_json(&load_string(REMINDERS_KEY).unwrap_or_default());
  if all.is_empty() {
    return Vec::new();
  }
  let (due, waiting) = crate::reminders::take_due(all, now);
  if !due.is_empty() {
    store_string(REMINDERS_KEY, &to_json(&waiting));
  }
  due
}

/// "Left #name" for the undo toast.
pub fn left_text(state: AppState, i18n: I18n, conversation: ConversationId) -> String {
  let name = state
    .read(|backend| backend.conversation(conversation))
    .map(|conv| conv.name)
    .unwrap_or_default();
  i18n.t_with("channel-left-toast", &[("name", &name)])
}
