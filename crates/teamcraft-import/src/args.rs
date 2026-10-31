//! The command line (docs/import-synology.md §7–§9).

use std::path::PathBuf;

pub const USAGE: &str = "usage: teamcraft-import --state <dir> <command> [options]

commands:
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
  --language <de|en>    how mentions of people without an account read (default: en)";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
  pub state: PathBuf,
  pub command: Command,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
  Login { server: String },
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
  let state = maybe_state.ok_or("--state is missing")?;
  let take = |name: &str| {
    options
      .iter()
      .find(|(key, _)| key == name)
      .map(|(_, value)| value.clone())
  };
  let known: &[&str] = match maybe_command.as_deref() {
    Some("login") => &["server"],
    _ => &["export", "uploads", "channels", "users", "language"],
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
    })
  };
  let command = match maybe_command.as_deref() {
    Some("login") => Command::Login {
      server: take("server").ok_or("--server is missing")?,
    },
    Some("plan") => Command::Plan(inputs()?),
    Some("run") => Command::Run(inputs()?),
    Some("serve") => Command::Serve(inputs()?),
    Some("report") => Command::Report(inputs()?),
    Some(other) => return Err(format!("unknown command {other:?}")),
    None => return Err("no command".into()),
  };
  Ok(Args { state, command })
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
