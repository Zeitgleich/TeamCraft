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

/// Where a command is typed and what the user may do there. Decides which commands are offered,
/// listed by `/help` and run: the server refuses the others (#648, #692), so the client neither
/// offers nor sends them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommandContext {
  /// A channel of a space: DMs, group DMs, notes to self and meeting chats are not.
  pub is_channel: bool,
  /// May change the channel's topic, purpose and name.
  pub can_change_channel: bool,
  /// May add people to the channel and remove them.
  pub can_manage_members: bool,
  /// Guests don't start meetings.
  pub is_guest: bool,
}

/// What a command needs to work where it is typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requirement {
  None,
  /// Only a channel (leaving): the server refuses it elsewhere.
  Channel,
  /// A channel and the right to change its properties.
  ChannelProperties,
  /// A channel and the right to manage its members.
  ChannelMembers,
  /// A full account, not a guest.
  MemberAccount,
}

/// Why a command doesn't work here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
  /// Only channels have it.
  NotAChannel,
  /// The user lacks the right.
  NotAllowed,
}

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
  /// The name it is typed with, without the slash.
  pub fn name(&self) -> &'static str {
    match self {
      Self::Presence(PresenceStatus::Away) => "away",
      Self::Presence(PresenceStatus::DoNotDisturb) => "dnd",
      Self::Presence(PresenceStatus::Offline) => "offline",
      Self::Presence(PresenceStatus::Online) => "online",
      Self::Status(_) => "status",
      Self::Topic(_) => "topic",
      Self::Purpose(_) => "purpose",
      Self::Rename(_) => "rename",
      Self::Join(_) => "join",
      Self::Open(_) => "open",
      Self::Leave => "leave",
      Self::Kick(_) => "kick",
      Self::Invite(_) => "invite",
      Self::Msg { .. } => "msg",
      Self::GroupMsg(_) => "groupmsg",
      Self::Mute => "mute",
      Self::Me(_) => "me",
      Self::Shrug(_) => "shrug",
      Self::Code(_) => "code",
      Self::Settings => "settings",
      Self::Shortcuts => "shortcuts",
      Self::Help => "help",
      Self::Remind { .. } => "remind",
      Self::Meet => "meet",
    }
  }

  /// Whether it works in `context`, and if not, why.
  pub fn check(&self, context: CommandContext) -> Result<(), Unavailable> {
    check(self.name(), context)
  }
}

impl Requirement {
  /// What the command typed as `name` needs: what the server checks before it acts.
  pub fn of(name: &str) -> Self {
    match name {
      "topic" | "purpose" | "rename" => Self::ChannelProperties,
      "kick" | "invite" => Self::ChannelMembers,
      "leave" => Self::Channel,
      "meet" => Self::MemberAccount,
      _ => Self::None,
    }
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

/// Commands whose name starts with `prefix` and that work in `context`, for the autocomplete list.
pub fn matching_commands(prefix: &str, context: CommandContext) -> Vec<(&'static str, &'static str)> {
  let prefix = prefix.to_lowercase();
  commands(context)
    .filter(|(name, _)| name.starts_with(&prefix))
    .collect()
}

/// The commands that work in `context`, for `/help`.
pub fn commands(context: CommandContext) -> impl Iterator<Item = (&'static str, &'static str)> {
  COMMANDS
    .iter()
    .copied()
    .filter(move |(name, _)| check(name, context).is_ok())
}

fn check(name: &str, context: CommandContext) -> Result<(), Unavailable> {
  let in_channel = |is_allowed: bool| match (context.is_channel, is_allowed) {
    (false, _) => Err(Unavailable::NotAChannel),
    (true, false) => Err(Unavailable::NotAllowed),
    (true, true) => Ok(()),
  };
  match Requirement::of(name) {
    Requirement::None => Ok(()),
    Requirement::Channel => in_channel(true),
    Requirement::ChannelProperties => in_channel(context.can_change_channel),
    Requirement::ChannelMembers => in_channel(context.can_manage_members),
    Requirement::MemberAccount if context.is_guest => Err(Unavailable::NotAllowed),
    Requirement::MemberAccount => Ok(()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  mod context {
    use super::*;

    const CHANNEL_MEMBER: CommandContext = CommandContext {
      is_channel: true,
      can_change_channel: false,
      can_manage_members: false,
      is_guest: false,
    };
    const CHANNEL_ADMIN: CommandContext = CommandContext {
      is_channel: true,
      can_change_channel: true,
      can_manage_members: true,
      is_guest: false,
    };
    /// A DM, group DM, the notes to self or a meeting chat: no channel, whatever the rights.
    const DIRECT: CommandContext = CommandContext {
      is_channel: false,
      can_change_channel: true,
      can_manage_members: true,
      is_guest: false,
    };
    const CHANNEL_CONTEXT_ONLY: [&str; 6] = ["topic", "purpose", "rename", "kick", "invite", "leave"];

    #[test]
    fn a_channel_admin_gets_every_command() {
      assert_eq!(names(CHANNEL_ADMIN).len(), COMMANDS.len());
    }

    #[test]
    fn outside_channels_only_channel_free_commands_are_offered() {
      let offered = names(DIRECT);
      for name in CHANNEL_CONTEXT_ONLY {
        assert!(!offered.contains(&name), "{name}");
      }
      for name in ["mute", "meet", "msg", "shrug", "remind", "join", "open"] {
        assert!(offered.contains(&name), "{name}");
      }
      assert_eq!(
        commands(DIRECT).count(),
        COMMANDS.len() - CHANNEL_CONTEXT_ONLY.len()
      );
    }

    #[test]
    fn a_plain_member_may_leave_but_not_manage() {
      let offered = names(CHANNEL_MEMBER);
      assert!(offered.contains(&"leave"));
      for name in ["topic", "purpose", "rename", "kick", "invite"] {
        assert!(!offered.contains(&name), "{name}");
      }
    }

    #[test]
    fn guests_do_not_start_meetings() {
      let guest = CommandContext {
        is_guest: true,
        ..CHANNEL_MEMBER
      };
      assert!(!names(guest).contains(&"meet"));
      assert_eq!(command("/meet").check(guest), Err(Unavailable::NotAllowed));
      assert_eq!(command("/meet").check(CHANNEL_MEMBER), Ok(()));
    }

    #[test]
    fn typed_commands_say_why_they_do_not_work() {
      for input in [
        "/topic Release week",
        "/purpose Plans",
        "/rename plans",
        "/kick @sam",
        "/leave",
      ] {
        assert_eq!(
          command(input).check(DIRECT),
          Err(Unavailable::NotAChannel),
          "{input}"
        );
      }
      for input in ["/topic Release week", "/invite @sam", "/kick @sam"] {
        assert_eq!(
          command(input).check(CHANNEL_MEMBER),
          Err(Unavailable::NotAllowed),
          "{input}"
        );
        assert_eq!(command(input).check(CHANNEL_ADMIN), Ok(()), "{input}");
      }
      assert_eq!(command("/mute").check(DIRECT), Ok(()));
    }

    #[test]
    fn every_listed_command_parses_to_its_own_name() {
      for (name, _) in COMMANDS {
        let args = if name == "remind" { "in 10m x" } else { "@a @b" };
        assert_eq!(command(&format!("/{name} {args}")).name(), name);
      }
    }

    fn names(context: CommandContext) -> Vec<&'static str> {
      matching_commands("", context)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
    }

    fn command(input: &str) -> SlashCommand {
      match parse(input) {
        Parsed::Command(command) => command,
        other => panic!("{input} is a command, not {other:?}"),
      }
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
      matching_commands("sh", CommandContext::default())
        .iter()
        .any(|(name, _)| *name == "shrug")
    );
  }
}
