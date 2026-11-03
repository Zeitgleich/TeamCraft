//! The import's report (docs/import-synology.md §9): per conversation what came over and what
//! waits, every skipped conversation with its reason, and the webhooks to recreate. Written as
//! Markdown into the state directory; it names conversations and accounts, so it stays there.

use std::fmt::Write;

use crate::export::Export;
use crate::plan::{Plan, Target};
use crate::progress::{Progress, ProgressError, Step};

pub fn render(
  plan: &Plan,
  export: &Export,
  progress: &Progress,
  is_dry_run: bool,
) -> Result<String, ProgressError> {
  let mut out = String::new();
  let title = if is_dry_run {
    "Import plan (dry run)"
  } else {
    "Import report"
  };
  let _ = writeln!(out, "# {title}\n");
  let total_posts: u64 = plan
    .items
    .iter()
    .filter(|item| item.has_history)
    .map(|item| item.post_count)
    .sum();
  let mut stored_posts = 0;
  for item in &plan.items {
    stored_posts += progress.stored_posts(item.source)?;
  }
  let _ = writeln!(
    out,
    "{} conversations, {} skipped; {stored_posts} of {total_posts} posts imported.\n",
    plan.items.len(),
    plan.skips.len()
  );
  let users = progress.notes("users")?;
  if !users.is_empty() {
    let _ = writeln!(out, "## Mapped user names without an account\n");
    for note in users {
      let _ = writeln!(out, "- {note}");
    }
    let _ = writeln!(out);
  }
  let _ = writeln!(out, "## Conversations\n");
  let _ = writeln!(out, "| Synology | Target | Posts | Missing files | State |");
  let _ = writeln!(out, "|---|---|---|---|---|");
  for item in &plan.items {
    let key = item.key();
    let target = match &item.target {
      Target::NewChannel { space, name, .. } => format!("new channel {space}/{name}"),
      Target::ExistingChannel { space, name } => format!("into {space}/{name}"),
      Target::Dm { .. } => "direct message".into(),
      Target::Notes { .. } => "notes".into(),
    };
    let posts = if item.has_history {
      format!("{} of {}", progress.stored_posts(item.source)?, item.post_count)
    } else {
      "no history".into()
    };
    let mut state = if progress.is_done(&key, Step::Left)? {
      "done".to_owned()
    } else if progress.is_done(&key, Step::MemberState)? {
      "imported; the import account is still in it".to_owned()
    } else if progress.conversation(&key)?.is_some() {
      "started".to_owned()
    } else {
      "not started".to_owned()
    };
    for note in progress.notes(&key)? {
      state.push_str("; ");
      state.push_str(&note);
    }
    let _ = writeln!(
      out,
      "| {} ({}) | {target} | {posts} | {} | {state} |",
      cell(&item.title),
      item.source,
      progress.missing_files(item.source)?
    );
  }
  if !plan.skips.is_empty() {
    let _ = writeln!(out, "\n## Skipped\n");
    let _ = writeln!(out, "| Synology | Reason |");
    let _ = writeln!(out, "|---|---|");
    for skip in &plan.skips {
      let _ = writeln!(
        out,
        "| {} ({}) | {} |",
        cell(&skip.title),
        skip.source,
        skip.reason.describe()
      );
    }
  }
  if !export.webhooks.is_empty() {
    let _ = writeln!(out, "\n## Webhooks to recreate as bridge sources\n");
    let _ = writeln!(out, "| Name | Channel | Kind | Target |");
    let _ = writeln!(out, "|---|---|---|---|");
    for webhook in &export.webhooks {
      let channel = match webhook.channel_id {
        Some(id) => export
          .channels
          .get(&id)
          .map_or_else(|| id.to_string(), crate::plan::title_of),
        None => "-".to_owned(),
      };
      let kind = if webhook.is_outgoing {
        "outgoing"
      } else {
        "incoming"
      };
      // Only the host of an outgoing webhook: its path or query may hold a secret.
      let target = webhook
        .url
        .as_deref()
        .and_then(|url| url::Url::parse(url).ok())
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default();
      let disabled = if webhook.disabled { " (disabled)" } else { "" };
      let _ = writeln!(
        out,
        "| {}{disabled} | {} | {kind} | {target} |",
        cell(webhook.name.as_deref().unwrap_or("")),
        cell(&channel)
      );
    }
  }
  Ok(out)
}

fn cell(text: &str) -> String {
  text.replace('|', "\\|").replace('\n', " ")
}
