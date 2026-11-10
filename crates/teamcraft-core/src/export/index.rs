//! `index.html`: the archive's page for people. One self-contained file without scripts that
//! links to the parts; everything that isn't the page's own markup is escaped. The words come
//! translated from the UI ([`DataExportLabels`]).

use std::fmt::Write as _;

use teamcraft_client_api::data_export::DataExportLabels;

use super::ExportCheckpoint;
use super::format::ROOT;

/// The page for the export as `state` describes it, made at `now_ms`.
pub(super) fn page(labels: &DataExportLabels, state: &ExportCheckpoint, now_ms: i64) -> String {
  let (year, month, day) = super::zip::civil_from_days(now_ms.div_euclid(86_400_000));
  let made_on = labels
    .made_on
    .replace("{date}", &format!("{year:04}-{month:02}-{day:02}"));
  let mut rows = String::new();
  for summary in &state.summaries {
    let name = if summary.name.is_empty() {
      summary.id.to_string()
    } else {
      summary.name.clone()
    };
    let link = match summary.parts.first() {
      Some(path) => format!(r#"<a href="{}">{}</a>"#, escape(relative(path)), escape(&name)),
      None => escape(&name),
    };
    let limited = if summary.is_history_limited {
      format!(
        r#" <span class="note">{}</span>"#,
        escape(&labels.history_limited)
      )
    } else {
      String::new()
    };
    // Writing into a String can't fail.
    let _ = write!(
      rows,
      "<tr><td>{link}{limited}</td><td class=\"kind\">{}</td><td class=\"count\">{}</td><td class=\"count\">{}</td></tr>",
      escape(&summary.kind),
      summary.messages,
      summary.files,
    );
  }
  format!(
    r#"<!doctype html>
<html lang="{language}">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>{title}</title>
<style>
:root {{ color-scheme: light dark; }}
body {{ margin: 0; font: 16px/1.5 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }}
main {{ max-width: 52rem; margin: 3rem auto; padding: 0 1.5rem; }}
h1 {{ margin: 0 0 0.25rem; font-size: 1.75rem; line-height: 1.15; }}
h2 {{ margin: 2rem 0 0.5rem; font-size: 1.1rem; }}
.made {{ margin: 0 0 1.25rem; opacity: 0.7; }}
table {{ width: 100%; border-collapse: collapse; }}
th, td {{ padding: 0.4rem 0.5rem; border-bottom: 1px solid color-mix(in srgb, currentColor 15%, transparent); text-align: left; }}
.count {{ text-align: right; font-variant-numeric: tabular-nums; }}
.kind, .note {{ opacity: 0.7; font-size: 0.9em; }}
</style>
</head>
<body>
<main>
<h1>{title}</h1>
<p class="made">{made_on}</p>
<p>{intro}</p>
<h2>{account_heading}</h2>
<p>{account_text}</p>
<ul>
<li><a href="account/personal-data.json">account/personal-data.json</a></li>
<li><a href="account/account-data.json">account/account-data.json</a></li>
<li><a href="account/security-log.ndjson">account/security-log.ndjson</a></li>
</ul>
<h2>{conversations_heading}</h2>
<table>
<thead><tr><th>{conversation_column}</th><th></th><th class="count">{messages_column}</th><th class="count">{files_column}</th></tr></thead>
<tbody>
{rows}
</tbody>
</table>
<p>{not_included}</p>
<p>{manifest_text}</p>
</main>
</body>
</html>
"#,
    language = escape(&labels.language),
    title = escape(&labels.title),
    made_on = escape(&made_on),
    intro = escape(&labels.intro),
    account_heading = escape(&labels.account_heading),
    account_text = escape(&labels.account_text),
    conversations_heading = escape(&labels.conversations_heading),
    conversation_column = escape(&labels.conversation_column),
    messages_column = escape(&labels.messages_column),
    files_column = escape(&labels.files_column),
    not_included = escape(&labels.not_included),
    manifest_text = escape(&labels.manifest_text),
  )
}

/// A path of the archive as the index page (at the root folder) links to it.
fn relative(path: &str) -> &str {
  path
    .strip_prefix(ROOT)
    .and_then(|rest| rest.strip_prefix('/'))
    .unwrap_or(path)
}

fn escape(text: &str) -> String {
  let mut escaped = String::with_capacity(text.len());
  for character in text.chars() {
    match character {
      '&' => escaped.push_str("&amp;"),
      '<' => escaped.push_str("&lt;"),
      '>' => escaped.push_str("&gt;"),
      '"' => escaped.push_str("&quot;"),
      '\'' => escaped.push_str("&#39;"),
      character => escaped.push(character),
    }
  }
  escaped
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn names_from_other_people_cannot_inject_markup() {
    assert_eq!(
      escape(r#"<script>alert("x")</script>&'"#),
      "&lt;script&gt;alert(&quot;x&quot;)&lt;/script&gt;&amp;&#39;"
    );
  }

  #[test]
  fn links_are_relative_to_the_root_folder() {
    assert_eq!(
      relative("teamcraft-export/conversations/1/messages-0001.ndjson"),
      "conversations/1/messages-0001.ndjson"
    );
  }
}
