//! Built-in slash commands (PLAN.md §14): typed into the composer, run on the client. This module
//! parses them; `components::composer_commands` runs them against the backend.

use crate::model::PresenceStatus;

/// Every command with its arguments, for `/help` and the autocomplete list.
pub const COMMANDS: [(&str, &str); 23] = [
  ("away", ""),
  ("online", ""),
  ("dnd", ""),
  ("offline", ""),
  ("status", "[emoji] text | clear"),
  ("topic", "text"),
  ("purpose", "text"),
  ("rename", "name"),
  ("join", "channel"),
  ("open", "channel"),
  ("leave", ""),
  ("kick", "@person"),
  ("invite", "@person"),
  ("msg", "@person [text]"),
  ("groupmsg", "@person @person …"),
  ("mute", ""),
  ("me", "text"),
  ("shrug", "[text]"),
  ("code", "text"),
  ("settings", ""),
  ("shortcuts", ""),
  ("remind", "in 10m|at 14:30 text"),
  ("meet", ""),
];

/// Commands that change channel properties: DMs and group DMs have none, and the server refuses
/// them there (#648), so they are neither offered nor sent.
const CHANNEL_ONLY: [&str; 3] = ["topic", "purpose", "rename"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlashCommand {
  Presence(PresenceStatus),
  Status(StatusChange),
  Topic(String),
  Purpose(String),
  Rename(String),
  Join(String),
  Open(String),
  Leave,
  Kick(String),
  Invite(String),
  Msg {
    user: String,
    text: String,
  },
  GroupMsg(Vec<String>),
  Mute,
  /// An action message: `/me waves` becomes "_waves_".
  Me(String),
  Shrug(String),
  Code(String),
  Settings,
  Shortcuts,
  Help,
  Remind {
    when: ReminderTime,
    text: String,
  },
  Meet,
}

impl SlashCommand {
  /// Whether it only makes sense in a channel (see [`CHANNEL_ONLY`]).
  pub fn is_channel_only(&self) -> bool {
    matches!(self, Self::Topic(_) | Self::Purpose(_) | Self::Rename(_))
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusChange {
  Clear,
  Set { emoji: String, text: String },
}

/// When a `/remind` fires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderTime {
  /// After this many minutes.
  In { minutes: u32 },
  /// At this time of day (minutes since midnight): today if still ahead, else tomorrow.
  At { minute_of_day: u16 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlashError {
  /// The command needs something after its name; the hint says what.
  MissingArgument(&'static str),
  /// The reminder time was not understood.
  BadReminderTime,
}

/// What the composer text means: a message to send as typed, or a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parsed {
  Message(String),
  Command(SlashCommand),
  Invalid(SlashError),
}

/// Reads composer text. Text that does not start with a known command is a plain message; `//text`
/// sends `/text`.
pub fn parse(input: &str) -> Parsed {
  let trimmed = input.trim();
  if let Some(escaped) = trimmed.strip_prefix("//") {
    return Parsed::Message(format!("/{escaped}"));
  }
  let Some(rest) = trimmed.strip_prefix('/') else {
    return Parsed::Message(input.to_owned());
  };
  let (name, args) = match rest.split_once(char::is_whitespace) {
    Some((name, args)) => (name, args.trim()),
    None => (rest, ""),
  };
  let name = name.to_lowercase();
  let need = |hint: &'static str| -> Result<String, Parsed> {
    if args.is_empty() {
      Err(Parsed::Invalid(SlashError::MissingArgument(hint)))
    } else {
      Ok(args.to_owned())
    }
  };
  let command = match name.as_str() {
    "away" => SlashCommand::Presence(PresenceStatus::Away),
    "online" => SlashCommand::Presence(PresenceStatus::Online),
    "dnd" => SlashCommand::Presence(PresenceStatus::DoNotDisturb),
    "offline" => SlashCommand::Presence(PresenceStatus::Offline),
    "status" => match status_change(args) {
      Some(change) => SlashCommand::Status(change),
      None => return Parsed::Invalid(SlashError::MissingArgument("[emoji] text | clear")),
    },
    "topic" => return wrap(need("text"), SlashCommand::Topic),
    "purpose" => return wrap(need("text"), SlashCommand::Purpose),
    "rename" => return wrap(need("name"), SlashCommand::Rename),
    "join" => {
      return wrap(need("channel"), |name| {
        SlashCommand::Join(name.trim_start_matches('~').to_owned())
      });
    },
    "open" => {
      return wrap(need("channel"), |name| {
        SlashCommand::Open(name.trim_start_matches('~').to_owned())
      });
    },
    "leave" => SlashCommand::Leave,
    "kick" => return wrap(need("@person"), |user| SlashCommand::Kick(strip_at(&user))),
    "invite" => return wrap(need("@person"), |user| SlashCommand::Invite(strip_at(&user))),
    "msg" => {
      let (user, text) = match args.split_once(char::is_whitespace) {
        Some((user, text)) => (user, text.trim()),
        None => (args, ""),
      };
      if user.is_empty() {
        return Parsed::Invalid(SlashError::MissingArgument("@person [text]"));
      }
      SlashCommand::Msg {
        user: strip_at(user),
        text: text.to_owned(),
      }
    },
    "groupmsg" => {
      let users: Vec<String> = args.split_whitespace().map(strip_at).collect();
      if users.len() < 2 {
        return Parsed::Invalid(SlashError::MissingArgument("@person @person …"));
      }
      SlashCommand::GroupMsg(users)
    },
    "mute" => SlashCommand::Mute,
    "me" => return wrap(need("text"), SlashCommand::Me),
    "shrug" => SlashCommand::Shrug(args.to_owned()),
    "code" => return wrap(need("text"), SlashCommand::Code),
    "settings" => SlashCommand::Settings,
    "shortcuts" => SlashCommand::Shortcuts,
    "help" => SlashCommand::Help,
    "remind" => match reminder(args) {
      Ok((when, text)) => SlashCommand::Remind { when, text },
      Err(error) => return Parsed::Invalid(error),
    },
    "meet" => SlashCommand::Meet,
    _ => return Parsed::Message(input.to_owned()),
  };
  Parsed::Command(command)
}

fn wrap(argument: Result<String, Parsed>, build: impl FnOnce(String) -> SlashCommand) -> Parsed {
  match argument {
    Ok(value) => Parsed::Command(build(value)),
    Err(invalid) => invalid,
  }
}

fn strip_at(name: impl AsRef<str>) -> String {
  name.as_ref().trim_start_matches('@').to_owned()
}

/// `clear`, `🏖 on holiday` or just `on holiday` (then without emoji).
fn status_change(args: &str) -> Option<StatusChange> {
  if args.is_empty() {
    return None;
  }
  if args.eq_ignore_ascii_case("clear") {
    return Some(StatusChange::Clear);
  }
  let (first, rest) = match args.split_once(char::is_whitespace) {
    Some((first, rest)) => (first, rest.trim()),
    None => (args, ""),
  };
  // A first word without letters or digits is the emoji.
  if !first.chars().any(char::is_alphanumeric) {
    return Some(StatusChange::Set {
      emoji: first.to_owned(),
      text: rest.to_owned(),
    });
  }
  Some(StatusChange::Set {
    emoji: String::new(),
    text: args.to_owned(),
  })
}

/// `in 10m text`, `in 2h text`, `in 1d text`, `at 14:30 text`.
fn reminder(args: &str) -> Result<(ReminderTime, String), SlashError> {
  let mut words = args.splitn(3, char::is_whitespace);
  let (Some(kind), Some(time)) = (words.next(), words.next()) else {
    return Err(SlashError::MissingArgument("in 10m|at 14:30 text"));
  };
  let text = words.next().unwrap_or("").trim().to_owned();
  if text.is_empty() {
    return Err(SlashError::MissingArgument("in 10m|at 14:30 text"));
  }
  let when = match kind.to_lowercase().as_str() {
    "in" => {
      let (digits, unit) = time.split_at(time.find(|c: char| !c.is_ascii_digit()).unwrap_or(time.len()));
      let amount: u32 = digits.parse().map_err(|_| SlashError::BadReminderTime)?;
      let minutes = match unit.to_lowercase().as_str() {
        "m" | "min" | "mins" => amount,
        "h" | "hr" | "hrs" => amount.saturating_mul(60),
        "d" => amount.saturating_mul(24 * 60),
        _ => return Err(SlashError::BadReminderTime),
      };
      if minutes == 0 {
        return Err(SlashError::BadReminderTime);
      }
      ReminderTime::In { minutes }
    },
    "at" => {
      let (hours, minutes) = time.split_once(':').ok_or(SlashError::BadReminderTime)?;
      let (hours, minutes): (u16, u16) = (
        hours.parse().map_err(|_| SlashError::BadReminderTime)?,
        minutes.parse().map_err(|_| SlashError::BadReminderTime)?,
      );
      if hours >= 24 || minutes >= 60 {
        return Err(SlashError::BadReminderTime);
      }
      ReminderTime::At {
        minute_of_day: hours * 60 + minutes,
      }
    },
    _ => return Err(SlashError::BadReminderTime),
  };
  Ok((when, text))
}

/// Commands whose name starts with `prefix`, for the autocomplete list.
pub fn matching_commands(prefix: &str, is_channel: bool) -> Vec<(&'static str, &'static str)> {
  let prefix = prefix.to_lowercase();
  commands(is_channel)
    .filter(|(name, _)| name.starts_with(&prefix))
    .collect()
}

/// The commands of a channel, or of a DM or group DM (without [`CHANNEL_ONLY`]), for `/help`.
pub fn commands(is_channel: bool) -> impl Iterator<Item = (&'static str, &'static str)> {
  COMMANDS
    .iter()
    .copied()
    .filter(move |(name, _)| is_channel || !CHANNEL_ONLY.contains(name))
}

#[cfg(test)]
mod tests {
  use super::*;

  mod channel_only {
    use super::*;

    #[test]
    fn dms_are_not_offered_channel_commands() {
      let names = |is_channel| {
        matching_commands("", is_channel)
          .into_iter()
          .map(|(name, _)| name)
          .collect::<Vec<_>>()
      };
      for name in CHANNEL_ONLY {
        assert!(names(true).contains(&name), "{name}");
        assert!(!names(false).contains(&name), "{name}");
      }
      assert!(names(false).contains(&"mute"));
      assert_eq!(names(true).len(), COMMANDS.len());
      assert_eq!(commands(false).count(), COMMANDS.len() - CHANNEL_ONLY.len());
    }

    #[test]
    fn typed_channel_commands_are_recognized() {
      for input in ["/topic Release week", "/purpose Plans", "/rename plans"] {
        let Parsed::Command(command) = parse(input) else {
          panic!("{input} is a command");
        };
        assert!(command.is_channel_only(), "{input}");
      }
      let Parsed::Command(command) = parse("/mute") else {
        panic!("/mute is a command");
      };
      assert!(!command.is_channel_only());
    }
  }

  #[test]
  fn plain_text_and_escaped_slashes_are_messages() {
    assert_eq!(parse("hello"), Parsed::Message("hello".into()));
    assert_eq!(parse("//code is fun"), Parsed::Message("/code is fun".into()));
    assert_eq!(parse("/usr/bin/env"), Parsed::Message("/usr/bin/env".into()));
  }

  #[test]
  fn presence_and_arguments() {
    assert_eq!(
      parse("/away"),
      Parsed::Command(SlashCommand::Presence(PresenceStatus::Away))
    );
    assert_eq!(
      parse("/dnd"),
      Parsed::Command(SlashCommand::Presence(PresenceStatus::DoNotDisturb))
    );
    assert_eq!(
      parse("/topic Release week"),
      Parsed::Command(SlashCommand::Topic("Release week".into()))
    );
    assert_eq!(
      parse("/topic"),
      Parsed::Invalid(SlashError::MissingArgument("text"))
    );
    assert_eq!(
      parse("/kick @sam"),
      Parsed::Command(SlashCommand::Kick("sam".into()))
    );
    assert_eq!(
      parse("/msg @sam hi there"),
      Parsed::Command(SlashCommand::Msg {
        user: "sam".into(),
        text: "hi there".into()
      })
    );
  }

  #[test]
  fn status_with_and_without_emoji() {
    assert_eq!(
      parse("/status clear"),
      Parsed::Command(SlashCommand::Status(StatusChange::Clear))
    );
    assert_eq!(
      parse("/status 🏖 on holiday"),
      Parsed::Command(SlashCommand::Status(StatusChange::Set {
        emoji: "🏖".into(),
        text: "on holiday".into()
      }))
    );
    assert_eq!(
      parse("/status in a meeting"),
      Parsed::Command(SlashCommand::Status(StatusChange::Set {
        emoji: String::new(),
        text: "in a meeting".into()
      }))
    );
  }

  #[test]
  fn reminders_parse_relative_and_absolute_times() {
    assert_eq!(
      parse("/remind in 2h call Sam"),
      Parsed::Command(SlashCommand::Remind {
        when: ReminderTime::In { minutes: 120 },
        text: "call Sam".into()
      })
    );
    assert_eq!(
      parse("/remind at 14:30 stand-up"),
      Parsed::Command(SlashCommand::Remind {
        when: ReminderTime::At { minute_of_day: 870 },
        text: "stand-up".into()
      })
    );
    assert_eq!(
      parse("/remind in 0m x"),
      Parsed::Invalid(SlashError::BadReminderTime)
    );
    assert_eq!(
      parse("/remind at 25:00 x"),
      Parsed::Invalid(SlashError::BadReminderTime)
    );
    assert_eq!(
      parse("/remind in 5m"),
      Parsed::Invalid(SlashError::MissingArgument("in 10m|at 14:30 text"))
    );
  }

  #[test]
  fn groupmsg_needs_two_people_and_prefixes_complete() {
    assert_eq!(
      parse("/groupmsg @a"),
      Parsed::Invalid(SlashError::MissingArgument("@person @person …"))
    );
    assert_eq!(
      parse("/groupmsg @a @b"),
      Parsed::Command(SlashCommand::GroupMsg(vec!["a".into(), "b".into()]))
    );
    assert!(
      matching_commands("sh", true)
        .iter()
        .any(|(name, _)| *name == "shrug")
    );
  }
}
