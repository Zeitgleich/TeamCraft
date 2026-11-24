//! The command line (docs/import-synology.md §7–§9, docs/import-slack.md).

use std::path::PathBuf;
use std::time::Duration;

use crate::sources::SourceKind;

pub const USAGE: &str = "usage: teamcraft-import --state <dir> <command> [options]
       teamcraft-import convert --from <slack|mattermost> --source <path> --out <dir>

commands:
  convert                               turn a Slack export (unpacked folder) or a Mattermost
                                        bulk export (its .jsonl file) into an export the other
                                        commands read, with mapping templates (no --state)
  login  --server <url>                 sign in as the import account in the browser
  plan   <inputs>                       dry run: what the import would do (report-plan.md)
  run    <inputs>                       import; stop and start again at any time
  serve  <inputs>                       keep running: add people when they set up the app, then leave
  report <inputs>                       what was done (report.md)

inputs:
  --export <dir>        the export (channels.ndjson, posts.ndjson, ...)
  --uploads <dir>       the attachments (default: <export>/raw/uploads)
  --channels <csv>      the channel mapping
  --users <csv>         the user mapping
  --language <de|en>    how mentions of people without an account read (default: en)
  --max-file-mib <n>    leave files larger than this out, named on their message (default: none)
  --join-wait <secs>    how long to wait for a member's device to let the import account into an
                        existing conversation before going on (default: 120; a later run tries again)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
  /// Required by every command but `convert`.
  pub maybe_state: Option<PathBuf>,
  pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
  Login { server: String },
  Convert(ConvertInputs),
  Plan(Inputs),
  Run(Inputs),
  Serve(Inputs),
  Report(Inputs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inputs {
  pub export: PathBuf,
  pub maybe_uploads: Option<PathBuf>,
  pub channels: PathBuf,
  pub users: PathBuf,
  pub language: Language,
  /// Files above this size are left out (named on their message); `None`: every file.
  pub maybe_max_file_bytes: Option<u64>,
  /// How long to wait for a member's device; `None` is the run's default.
  pub maybe_join_wait: Option<Duration>,
}

/// What `convert` reads and where it writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertInputs {
  pub from: SourceKind,
  pub source: PathBuf,
  pub out: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
  En,
  De,
}

impl Language {
  /// How a mention of somebody without an account reads (the apps' `user-deleted-name`).
  pub fn deleted_name(self) -> &'static str {
    match self {
      Self::En => "Deleted user",
      Self::De => "Gelöschter Nutzer",
    }
  }
}

pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Args, String> {
  let mut maybe_state = None;
  let mut maybe_command = None;
  let mut options: Vec<(String, String)> = Vec::new();
  let mut arguments = arguments.into_iter();
  while let Some(argument) = arguments.next() {
    match argument.strip_prefix("--") {
      Some(name) => {
        let value = arguments
          .next()
          .ok_or_else(|| format!("--{name} needs a value"))?;
        if name == "state" {
          maybe_state = Some(PathBuf::from(value));
        } else {
          options.push((name.to_owned(), value));
        }
      },
      None if maybe_command.is_none() => maybe_command = Some(argument),
      None => return Err(format!("unexpected argument {argument:?}")),
    }
  }
  if maybe_state.is_none() && maybe_command.as_deref() != Some("convert") {
    return Err("--state is missing".into());
  }
  let take = |name: &str| {
    options
      .iter()
      .find(|(key, _)| key == name)
      .map(|(_, value)| value.clone())
  };
  let known: &[&str] = match maybe_command.as_deref() {
    Some("login") => &["server"],
    Some("convert") => &["from", "source", "out"],
    _ => &[
      "export",
      "uploads",
      "channels",
      "users",
      "language",
      "max-file-mib",
      "join-wait",
    ],
  };
  if let Some((unknown, _)) = options.iter().find(|(key, _)| !known.contains(&key.as_str())) {
    return Err(format!("unknown option --{unknown}"));
  }
  let inputs = || -> Result<Inputs, String> {
    Ok(Inputs {
      export: take("export").ok_or("--export is missing")?.into(),
      maybe_uploads: take("uploads").map(PathBuf::from),
      channels: take("channels").ok_or("--channels is missing")?.into(),
      users: take("users").ok_or("--users is missing")?.into(),
      language: match take("language").as_deref() {
        None | Some("en") => Language::En,
        Some("de") => Language::De,
        Some(other) => return Err(format!("unknown language {other:?}")),
      },
      maybe_max_file_bytes: match take("max-file-mib") {
        None => None,
        Some(value) => Some(
          value
            .parse::<u64>()
            .map_err(|_| format!("--max-file-mib takes a number of MiB, not {value:?}"))?
            .saturating_mul(1024 * 1024),
        ),
      },
      maybe_join_wait: match take("join-wait") {
        None => None,
        Some(value) => {
          Some(Duration::from_secs(value.parse::<u64>().map_err(|_| {
            format!("--join-wait takes a number of seconds, not {value:?}")
          })?))
        },
      },
    })
  };
  let command = match maybe_command.as_deref() {
    Some("login") => Command::Login {
      server: take("server").ok_or("--server is missing")?,
    },
    Some("convert") => Command::Convert(ConvertInputs {
      from: take("from").ok_or("--from is missing")?.parse()?,
      source: take("source").ok_or("--source is missing")?.into(),
      out: take("out").ok_or("--out is missing")?.into(),
    }),
    Some("plan") => Command::Plan(inputs()?),
    Some("run") => Command::Run(inputs()?),
    Some("serve") => Command::Serve(inputs()?),
    Some("report") => Command::Report(inputs()?),
    Some(other) => return Err(format!("unknown command {other:?}")),
    None => return Err("no command".into()),
  };
  Ok(Args { maybe_state, command })
}

#[cfg(test)]
mod tests {
  use super::*;

  fn args(text: &str) -> Result<Args, String> {
    parse(text.split_whitespace().map(str::to_owned))
  }

  #[test]
  fn commands_and_their_options() {
    let login = args("--state s login --server https://chat.example.com").unwrap();
    assert_eq!(
      login.command,
      Command::Login {
        server: "https://chat.example.com".into()
      }
    );
    let run = args("run --export e --channels c.csv --users u.csv --language de --state s").unwrap();
    let Command::Run(inputs) = run.command else {
      panic!()
    };
    assert_eq!(inputs.language, Language::De);
    assert_eq!(inputs.maybe_uploads, None);
    assert_eq!(inputs.maybe_max_file_bytes, None);
    let capped = args("--state s run --export e --channels c --users u --max-file-mib 2").unwrap();
    let Command::Run(inputs) = capped.command else {
      panic!()
    };
    assert_eq!(inputs.maybe_max_file_bytes, Some(2 * 1024 * 1024));
    let quick = args("--state s run --export e --channels c --users u --join-wait 5").unwrap();
    let Command::Run(inputs) = quick.command else {
      panic!()
    };
    assert_eq!(inputs.maybe_join_wait, Some(Duration::from_secs(5)));
    assert!(args("--state s run --export e --channels c --users u --join-wait soon").is_err());
    assert!(args("--state s run --export e --channels c --users u --max-file-mib x").is_err());
  }

  #[test]
  fn convert_needs_no_state() {
    let convert = args("convert --from slack --source export --out converted").unwrap();
    assert_eq!(convert.maybe_state, None);
    assert_eq!(
      convert.command,
      Command::Convert(ConvertInputs {
        from: SourceKind::Slack,
        source: "export".into(),
        out: "converted".into(),
      })
    );
    assert!(
      args("convert --from teams --source e --out o")
        .unwrap_err()
        .contains("teams")
    );
    assert!(
      args("convert --from slack --out o")
        .unwrap_err()
        .contains("--source")
    );
    assert!(
      args("convert --from slack --source e --out o --users u")
        .unwrap_err()
        .contains("--users")
    );
  }

  #[test]
  fn mistakes_are_named() {
    assert!(args("run --export e").unwrap_err().contains("--state"));
    assert!(
      args("--state s run --export e --users u")
        .unwrap_err()
        .contains("--channels")
    );
    assert!(
      args("--state s login --server x --export e")
        .unwrap_err()
        .contains("--export")
    );
    assert!(args("--state s fly").unwrap_err().contains("fly"));
    assert!(
      args("--state s run --export")
        .unwrap_err()
        .contains("needs a value")
    );
  }
}
