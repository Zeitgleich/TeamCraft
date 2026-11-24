//! Message texts of other systems in the markup the import reads (`convert.rs`): mentions as
//! `@u:<id>`, links as `<url|label>`, emoji short codes as they are. Code (fenced or inline) is
//! never changed. Everything here takes hostile text: no recursion, and every search is bounded.

use std::collections::HashMap;

use crate::export::SourceId;

/// How far a Slack `*bold*` or `~strike~` may reach for its closing mark.
const MAX_SPAN_BYTES: usize = 1_000;
/// The longest `@name` looked up (Mattermost's user names have at most 22 characters).
const MAX_NAME_BYTES: usize = 64;

/// Who and what the text of a Slack export names by ID.
pub struct SlackNames<'a> {
  /// Slack user ID → the converted ID.
  pub users: &'a HashMap<String, SourceId>,
  /// Slack channel ID → its name.
  pub channels: &'a HashMap<String, String>,
}

/// Slack's mrkdwn: `<@U1>`, `<#C1|name>`, `<!here>`, `<url|label>`, `*bold*`, `~strike~` and the
/// escaped `&lt; &gt; &amp;`.
pub fn slack_text(text: &str, names: &SlackNames<'_>) -> String {
  outside_code(text, |plain, out| slack_plain(plain, names, out))
}

/// Mattermost's markdown: `@username` of the export's people becomes a mention.
pub fn mattermost_text(text: &str, users: &HashMap<String, SourceId>) -> String {
  outside_code(text, |plain, out| mattermost_plain(plain, users, out))
}

/// Copies code as it is and hands every other piece to `plain`.
fn outside_code(text: &str, mut plain: impl FnMut(&str, &mut String)) -> String {
  let mut out = String::with_capacity(text.len());
  let mut is_fenced = false;
  for (index, line) in text.split('\n').enumerate() {
    if index > 0 {
      out.push('\n');
    }
    if line.trim_start().starts_with("```") {
      is_fenced = !is_fenced;
      out.push_str(line);
      continue;
    }
    if is_fenced {
      out.push_str(line);
      continue;
    }
    let mut rest = line;
    while let Some(start) = rest.find('`') {
      plain(&rest[..start], &mut out);
      let code = &rest[start..];
      match code[1..].find('`') {
        Some(end) => {
          out.push_str(&code[..end + 2]);
          rest = &code[end + 2..];
        },
        None => {
          out.push_str(code);
          rest = "";
        },
      }
    }
    plain(rest, &mut out);
  }
  out
}

fn slack_plain(text: &str, names: &SlackNames<'_>, out: &mut String) {
  // Byte positions of closing marks already paired with an opening one.
  let mut closers: Vec<usize> = Vec::new();
  // The next `>` at or after the current position (`None`: there is none left), so a text of
  // many `<` is read once, not once per `<`.
  let mut maybe_next_close = text.find('>');
  let mut index = 0;
  while let Some(ch) = text[index..].chars().next() {
    let rest = &text[index..];
    if let Some(position) = closers.iter().position(|at| *at == index) {
      closers.swap_remove(position);
      out.push(ch);
      out.push(ch);
      index += 1;
      continue;
    }
    if maybe_next_close.is_some_and(|close| close < index) {
      maybe_next_close = text[index..].find('>').map(|offset| index + offset);
    }
    if ch == '<'
      && let Some(close) = maybe_next_close
    {
      slack_token(&text[index + 1..close], names, out);
      index = close + 1;
      continue;
    }
    if ch == '&'
      && let Some((unescaped, used)) = entity(rest)
    {
      out.push(unescaped);
      index += used;
      continue;
    }
    if (ch == '*' || ch == '~')
      && let Some(close) = closing_mark(text, index, ch)
    {
      closers.push(close);
      out.push(ch);
      out.push(ch);
      index += 1;
      continue;
    }
    out.push(ch);
    index += ch.len_utf8();
  }
}

/// The content of `<…>`.
fn slack_token(content: &str, names: &SlackNames<'_>, out: &mut String) {
  let (target, maybe_label) = match content.split_once('|') {
    Some((target, label)) => (target, Some(unescape(label)).filter(|label| !label.is_empty())),
    None => (content, None),
  };
  if let Some(user) = target.strip_prefix('@') {
    match (names.users.get(user), maybe_label) {
      (Some(id), _) => out.push_str(&format!("@u:{id}")),
      (None, Some(label)) => {
        out.push('@');
        out.push_str(label.trim_start_matches('@'));
      },
      (None, None) => {
        out.push('@');
        out.push_str(user);
      },
    }
    return;
  }
  if let Some(channel) = target.strip_prefix('#') {
    let name = maybe_label.or_else(|| names.channels.get(channel).cloned());
    out.push('#');
    out.push_str(name.as_deref().unwrap_or(channel));
    return;
  }
  if let Some(special) = target.strip_prefix('!') {
    match special {
      "here" | "channel" | "everyone" => {
        out.push('@');
        out.push_str(special);
      },
      // Subteams, dates and the like: the text Slack shows without the data behind it.
      _ => out.push_str(maybe_label.as_deref().unwrap_or("")),
    }
    return;
  }
  out.push('<');
  out.push_str(&unescape(target));
  if let Some(label) = maybe_label {
    out.push('|');
    out.push_str(&label);
  }
  out.push('>');
}

/// The `&lt;`, `&gt;` or `&amp;` at the start of `rest`.
fn entity(rest: &str) -> Option<(char, usize)> {
  [("&lt;", '<'), ("&gt;", '>'), ("&amp;", '&')]
    .into_iter()
    .find(|(entity, _)| rest.starts_with(entity))
    .map(|(entity, ch)| (ch, entity.len()))
}

fn unescape(text: &str) -> String {
  text
    .replace("&lt;", "<")
    .replace("&gt;", ">")
    .replace("&amp;", "&")
}

/// Where the mark at `open` closes, if it opens a span: after a non-word character, before a
/// non-space, closed on the same line after a non-space and before a non-word character.
fn closing_mark(text: &str, open: usize, mark: char) -> Option<usize> {
  let before = text[..open].chars().next_back();
  if before.is_some_and(char::is_alphanumeric) {
    return None;
  }
  let after = &text[open + 1..];
  let first = after.chars().next()?;
  if first.is_whitespace() || first == mark {
    return None;
  }
  let limit = after.len().min(MAX_SPAN_BYTES);
  let mut previous = first;
  for (offset, ch) in after.char_indices().skip(1) {
    if offset >= limit || ch == '\n' {
      return None;
    }
    if ch == mark && !previous.is_whitespace() {
      let next = after[offset + 1..].chars().next();
      if !next.is_some_and(char::is_alphanumeric) {
        return Some(open + 1 + offset);
      }
    }
    previous = ch;
  }
  None
}

fn mattermost_plain(text: &str, users: &HashMap<String, SourceId>, out: &mut String) {
  let mut previous: Option<char> = None;
  let mut index = 0;
  while let Some(ch) = text[index..].chars().next() {
    if ch == '@' && !previous.is_some_and(is_name_char) {
      let name_start = index + 1;
      let length = text[name_start..]
        .find(|ch: char| !is_name_char(ch))
        .unwrap_or(text.len() - name_start);
      let mut name = &text[name_start..name_start + length.min(MAX_NAME_BYTES)];
      // Mattermost drops trailing punctuation until a name matches ("@anna." ends a sentence).
      let maybe_id = loop {
        if let Some(id) = users.get(&name.to_lowercase()) {
          break Some(*id);
        }
        match name.strip_suffix(['.', '-', '_']) {
          Some(shorter) if !shorter.is_empty() => name = shorter,
          _ => break None,
        }
      };
      if let Some(id) = maybe_id {
        out.push_str(&format!("@u:{id}"));
        index = name_start + name.len();
        previous = name.chars().next_back();
        continue;
      }
    }
    out.push(ch);
    previous = Some(ch);
    index += ch.len_utf8();
  }
}

fn is_name_char(ch: char) -> bool {
  ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_')
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn slack_mentions_channels_and_specials() {
    let (users, channels) = names();
    let names = SlackNames {
      users: &users,
      channels: &channels,
    };
    assert_eq!(
      slack_text("hi <@U1> and <@U9|gone>, see <#C1> <#C2|ops> <!here>", &names),
      "hi @u:11 and @gone, see #general #ops @here"
    );
    assert_eq!(
      slack_text("<!subteam^S1|@devs> on <!date^1|Monday>", &names),
      "@devs on Monday"
    );
    assert_eq!(slack_text("<@U77>", &names), "@U77");
  }

  #[test]
  fn slack_links_entities_and_emphasis() {
    let (users, channels) = names();
    let names = SlackNames {
      users: &users,
      channels: &channels,
    };
    assert_eq!(
      slack_text(
        "<https://example.com/?a=1&amp;b=2|the docs> &lt;3 &amp; more",
        &names
      ),
      "<https://example.com/?a=1&b=2|the docs> <3 & more"
    );
    assert_eq!(
      slack_text("*bold* and ~gone~ but 2*3*4 and a * b", &names),
      "**bold** and ~~gone~~ but 2*3*4 and a * b"
    );
    assert_eq!(slack_text("*open only", &names), "*open only");
    assert_eq!(slack_text("a<b", &names), "a<b");
  }

  #[test]
  fn code_is_left_alone() {
    let (users, channels) = names();
    let names = SlackNames {
      users: &users,
      channels: &channels,
    };
    assert_eq!(
      slack_text("`*x* <@U1>` *y*\n```\n*z* &lt;\n```\n<@U1>", &names),
      "`*x* <@U1>` **y**\n```\n*z* &lt;\n```\n@u:11"
    );
    let people = HashMap::from([("anna".to_owned(), 7)]);
    assert_eq!(mattermost_text("`@anna` @anna", &people), "`@anna` @u:7");
  }

  #[test]
  fn mattermost_mentions_of_known_people() {
    let people = HashMap::from([("anna".to_owned(), 7), ("ben.b".to_owned(), 8)]);
    assert_eq!(
      mattermost_text(
        "@Anna, ask @ben.b. Mail anna@example.com or @nobody @all",
        &people
      ),
      "@u:7, ask @u:8. Mail anna@example.com or @nobody @all"
    );
    assert_eq!(mattermost_text("@anna_", &people), "@u:7_");
  }

  #[test]
  fn hostile_text_ends_quickly() {
    let (users, channels) = names();
    let names = SlackNames {
      users: &users,
      channels: &channels,
    };
    let many = "*a ~".repeat(50_000);
    let started = std::time::Instant::now();
    let _ = slack_text(&many, &names);
    let _ = slack_text(&"<".repeat(100_000), &names);
    let _ = mattermost_text(&"@a.".repeat(100_000), &HashMap::new());
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
  }

  fn names() -> (HashMap<String, SourceId>, HashMap<String, String>) {
    (
      HashMap::from([("U1".to_owned(), 11)]),
      HashMap::from([("C1".to_owned(), "general".to_owned())]),
    )
  }
}
