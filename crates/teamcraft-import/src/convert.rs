//! A Synology post as TeamCraft content (e2ee.md §4.10): its text with mentions, links and emoji
//! in TeamCraft's markup, its author, reactions and files.
//!
//! Synology writes mentions as `@u:<id>`, links as `<url|label>` or `<url>`, and emoji as gemoji
//! short codes (`:thumbsup:`). Code (fenced or inline) is left as it is.

use std::collections::{BTreeMap, HashMap};

use teamcraft_core::imported::{ImportedAuthor, ImportedReaction};
use teamcraft_proto::UserId;

use crate::export::{Export, POST_FILE, POST_STICKER, Post, SourceId};

/// Who is who: mapped accounts, bots and the text for everybody else.
pub struct People {
  /// Synology user → TeamCraft account and its user name.
  pub accounts: HashMap<SourceId, (UserId, String)>,
  pub bots: BTreeMap<SourceId, String>,
  /// How a mention of somebody without an account reads ("@Deleted user").
  pub deleted_name: String,
}

impl People {
  pub fn author(&self, user: SourceId) -> ImportedAuthor {
    if let Some((user_id, _)) = self.accounts.get(&user) {
      return ImportedAuthor::User { user_id: *user_id };
    }
    match self.bots.get(&user) {
      Some(name) => ImportedAuthor::Named { name: name.clone() },
      None => ImportedAuthor::Deleted,
    }
  }
}

/// A post ready to be encrypted, once its files are up.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedPost {
  pub source_id: SourceId,
  pub sent_at_ms: i64,
  pub maybe_edited_at_ms: Option<i64>,
  pub author: ImportedAuthor,
  pub body: String,
  pub mentions: Vec<UserId>,
  pub reactions: Vec<ImportedReaction>,
  pub files: Vec<FileToUpload>,
  /// The thread root it answers, in Synology's IDs.
  pub maybe_root: Option<SourceId>,
}

/// A file of the export to upload with its post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileToUpload {
  pub path: std::path::PathBuf,
  pub file_name: String,
  pub media_type: String,
  pub is_image: bool,
}

pub fn prepare(post: &Post, export: &Export, people: &People) -> PreparedPost {
  let text = post.message.clone().unwrap_or_default();
  let mut files = Vec::new();
  let body = match post.kind {
    POST_STICKER => sticker(&text, export, &mut files),
    POST_FILE => {
      let props = post.file_props.clone().unwrap_or_default();
      let file_name = props
        .name
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| format!("file-{}", post.id));
      let media_type = media_type_of(&file_name, props.extension.as_deref());
      files.push(FileToUpload {
        path: export.upload_path(post),
        is_image: props.is_image || media_type.starts_with("image/"),
        file_name,
        media_type,
      });
      // Synology puts the file's name in the text of a file post; the file says it already.
      let caption = text.trim();
      if props.name.as_deref().is_some_and(|name| name.trim() == caption) {
        String::new()
      } else {
        caption.to_owned()
      }
    },
    _ => text,
  };
  let (body, mentions) = convert_text(&body, people);
  PreparedPost {
    source_id: post.id,
    sent_at_ms: post.create_at,
    maybe_edited_at_ms: post.maybe_edited_at(),
    author: people.author(post.user_id),
    body,
    mentions,
    reactions: reactions_of(post.id, export, people),
    files,
    maybe_root: post.maybe_root(),
  }
}

/// A sticker post (`:name:`): Synology's own sticker pictures become a picture, emoji become text.
fn sticker(text: &str, export: &Export, files: &mut Vec<FileToUpload>) -> String {
  let name = text.trim().trim_matches(':');
  let path = export.sticker_path(name);
  if !name.is_empty() && path.is_file() {
    files.push(FileToUpload {
      path,
      file_name: format!("{name}.png"),
      media_type: "image/png".into(),
      is_image: true,
    });
    return String::new();
  }
  emoji_of(name).map_or_else(|| text.to_owned(), str::to_owned)
}

fn reactions_of(post: SourceId, export: &Export, people: &People) -> Vec<ImportedReaction> {
  let Some(reactions) = export.reactions.get(&post) else {
    return Vec::new();
  };
  let mut by_emoji: BTreeMap<String, Vec<ImportedAuthor>> = BTreeMap::new();
  for reaction in reactions {
    let emoji = emoji_of(&reaction.name).map_or_else(|| format!(":{}:", reaction.name), str::to_owned);
    let author = people.author(reaction.user_id);
    let authors = by_emoji.entry(emoji).or_default();
    if !authors.contains(&author) || author == ImportedAuthor::Deleted {
      authors.push(author);
    }
  }
  by_emoji
    .into_iter()
    .map(|(emoji, authors)| ImportedReaction { emoji, authors })
    .collect()
}

/// The text in TeamCraft's markup and the accounts it mentions.
pub fn convert_text(text: &str, people: &People) -> (String, Vec<UserId>) {
  let mut out = String::with_capacity(text.len());
  let mut mentions = Vec::new();
  let mut is_fenced = false;
  for (index, line) in text.split('\n').enumerate() {
    if index > 0 {
      out.push('\n');
    }
    if line.trim_start().starts_with("```") {
      is_fenced = !is_fenced;
      out.push_str(line);
    } else if is_fenced {
      out.push_str(line);
    } else {
      convert_line(line, people, &mut out, &mut mentions);
    }
  }
  (out, mentions)
}

fn convert_line(line: &str, people: &People, out: &mut String, mentions: &mut Vec<UserId>) {
  let mut rest = line;
  while let Some(ch) = rest.chars().next() {
    if ch == '`' {
      // Inline code up to the closing backtick stays as it is.
      match rest[1..].find('`') {
        Some(end) => {
          out.push_str(&rest[..end + 2]);
          rest = &rest[end + 2..];
        },
        None => {
          out.push_str(rest);
          rest = "";
        },
      }
      continue;
    }
    if let Some((text, used)) = mention(rest, people, mentions) {
      out.push_str(&text);
      rest = &rest[used..];
      continue;
    }
    if let Some((text, used)) = link(rest) {
      out.push_str(&text);
      rest = &rest[used..];
      continue;
    }
    if let Some((code, used)) = short_code(rest, out.chars().last()) {
      match code {
        ShortCode::Emoji(emoji) => out.push_str(emoji),
        // Kept whole, so its closing colon doesn't open the next code (`:unknown::grin:`).
        ShortCode::Unknown => out.push_str(&rest[..used]),
      }
      rest = &rest[used..];
      continue;
    }
    out.push(ch);
    rest = &rest[ch.len_utf8()..];
  }
}

/// `@u:<id>` at the start of `rest`.
fn mention(rest: &str, people: &People, mentions: &mut Vec<UserId>) -> Option<(String, usize)> {
  let digits = rest.strip_prefix("@u:")?;
  let length = digits.chars().take_while(char::is_ascii_digit).count();
  if length == 0 {
    return None;
  }
  let id: SourceId = digits[..length].parse().ok()?;
  let used = 3 + length;
  Some(match people.accounts.get(&id) {
    Some((user_id, username)) => {
      if !mentions.contains(user_id) {
        mentions.push(*user_id);
      }
      (format!("@{username}"), used)
    },
    None => match people.bots.get(&id) {
      Some(name) => (format!("@{name}"), used),
      None => (format!("@{}", people.deleted_name), used),
    },
  })
}

/// `<url|label>` or `<url>` at the start of `rest`.
fn link(rest: &str) -> Option<(String, usize)> {
  let inner = rest.strip_prefix('<')?;
  let end = inner.find('>')?;
  let content = &inner[..end];
  let (url, maybe_label) = match content.split_once('|') {
    Some((url, label)) => (url, Some(label)),
    None => (content, None),
  };
  let is_link = ["http://", "https://", "mailto:"]
    .iter()
    .any(|prefix| url.starts_with(prefix))
    && !url.contains(char::is_whitespace);
  if !is_link {
    return None;
  }
  let text = match maybe_label
    .map(str::trim)
    .filter(|label| !label.is_empty() && *label != url)
  {
    Some(label) => format!("[{}]({url})", label.replace(['[', ']'], "")),
    None => url.to_owned(),
  };
  Some((text, end + 2))
}

/// A `:name:` short code in the text.
enum ShortCode {
  Emoji(&'static str),
  /// Shaped like one, but no emoji has the name: it stays text.
  Unknown,
}

/// `:name:` at the start of `rest`, standing on its own (not inside a word or a time like
/// `12:30:45`), and how many bytes it takes. Codes may follow each other (`:grin::thumbsup:`).
fn short_code(rest: &str, maybe_before: Option<char>) -> Option<(ShortCode, usize)> {
  if maybe_before.is_some_and(char::is_alphanumeric) {
    return None;
  }
  let inner = rest.strip_prefix(':')?;
  let end = inner.find(':')?;
  let name = &inner[..end];
  if name.is_empty()
    || !name
      .chars()
      .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '+' | '-'))
    || name.chars().all(|ch| ch.is_ascii_digit())
  {
    return None;
  }
  let after = inner[end + 1..].chars().next();
  if after.is_some_and(char::is_alphanumeric) {
    return None;
  }
  let code = emoji_of(name).map_or(ShortCode::Unknown, ShortCode::Emoji);
  Some((code, end + 2))
}

/// The emoji of a gemoji short code, also of the EmojiOne names Synology uses for some.
pub fn emoji_of(name: &str) -> Option<&'static str> {
  let name = ALIASES
    .iter()
    .find(|(alias, _)| *alias == name)
    .map_or(name, |(_, gemoji)| gemoji);
  emojis::get_by_shortcode(name).map(emojis::Emoji::as_str)
}

/// Synology (EmojiOne) names that gemoji spells otherwise.
const ALIASES: [(&str, &str); 12] = [
  ("nerd", "nerd_face"),
  ("slight_frown", "slightly_frowning_face"),
  ("slight_smile", "slightly_smiling_face"),
  ("hand_splayed", "raised_hand_with_fingers_splayed"),
  ("thumbsup_tone1", "+1"),
  ("thumbsup_tone2", "+1"),
  ("thumbsup_tone3", "+1"),
  ("thumbsup_tone4", "+1"),
  ("thumbsup_tone5", "+1"),
  ("nauseated_face", "nauseated_face"),
  ("rolling_eyes", "roll_eyes"),
  ("upside_down", "upside_down_face"),
];

/// The media type of a file by its extension; generic when unknown.
pub fn media_type_of(file_name: &str, maybe_extension: Option<&str>) -> String {
  let extension = maybe_extension
    .filter(|extension| !extension.is_empty())
    .map(str::to_ascii_lowercase)
    .or_else(|| {
      file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
    })
    .unwrap_or_default();
  match extension.as_str() {
    "png" => "image/png",
    "jpg" | "jpeg" => "image/jpeg",
    "gif" => "image/gif",
    "webp" => "image/webp",
    "bmp" => "image/bmp",
    "heic" => "image/heic",
    "svg" => "image/svg+xml",
    "pdf" => "application/pdf",
    "txt" | "log" => "text/plain",
    "csv" => "text/csv",
    "md" => "text/markdown",
    "html" | "htm" => "text/html",
    "json" => "application/json",
    "xml" => "application/xml",
    "zip" => "application/zip",
    "doc" => "application/msword",
    "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "xls" => "application/vnd.ms-excel",
    "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "ppt" => "application/vnd.ms-powerpoint",
    "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "mp4" => "video/mp4",
    "mov" => "video/quicktime",
    "webm" => "video/webm",
    "mp3" => "audio/mpeg",
    "m4a" => "audio/mp4",
    "ogg" => "audio/ogg",
    "wav" => "audio/wav",
    _ => "application/octet-stream",
  }
  .to_owned()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn mentions_become_user_names_and_mentions_of_the_unmapped_read_as_deleted() {
    let people = people();
    let (text, mentions) = convert_text("hi @u:7 and @u:8, ask @u:99 or @here", &people);
    assert_eq!(text, "hi @anna and @Deleted user, ask @Monitor or @here");
    assert_eq!(mentions, vec![user(70)]);
    let (_, twice) = convert_text("@u:7 @u:7", &people);
    assert_eq!(twice.len(), 1);
  }

  #[test]
  fn links_take_teamcraft_markup() {
    let people = people();
    assert_eq!(
      convert_text(
        "see <https://example.com/a|the docs> or <https://example.com/b>",
        &people
      )
      .0,
      "see [the docs](https://example.com/a) or https://example.com/b"
    );
    assert_eq!(convert_text("a <b> c", &people).0, "a <b> c");
    assert_eq!(convert_text("<javascript:x|y>", &people).0, "<javascript:x|y>");
  }

  #[test]
  fn short_codes_become_emoji_but_not_times_or_code() {
    let people = people();
    assert_eq!(
      convert_text("ok :thumbsup: :nerd:", &people).0,
      "ok \u{1f44d} \u{1f913}"
    );
    assert_eq!(convert_text("at 12:30:45 sharp", &people).0, "at 12:30:45 sharp");
    assert_eq!(convert_text("`:thumbsup:` x", &people).0, "`:thumbsup:` x");
    assert_eq!(
      convert_text("```\n:thumbsup:\n```", &people).0,
      "```\n:thumbsup:\n```"
    );
    assert_eq!(convert_text("a:thumbsup:", &people).0, "a:thumbsup:");
    assert_eq!(
      convert_text(":no_such_emoji_here:", &people).0,
      ":no_such_emoji_here:"
    );
    assert_eq!(convert_text("@u:", &people).0, "@u:");
  }

  #[test]
  fn adjacent_short_codes_each_become_emoji() {
    let people = people();
    assert_eq!(convert_text(":grin::thumbsup:", &people).0, "\u{1f601}\u{1f44d}");
    assert_eq!(
      convert_text("great :grin::no_such_emoji_here::thumbsup:", &people).0,
      "great \u{1f601}:no_such_emoji_here:\u{1f44d}"
    );
    assert_eq!(
      convert_text("`:grin::thumbsup:` and 12:30:00", &people).0,
      "`:grin::thumbsup:` and 12:30:00"
    );
  }

  #[test]
  fn files_get_a_media_type_from_their_extension() {
    assert_eq!(media_type_of("Report.PDF", None), "application/pdf");
    assert_eq!(media_type_of("x", Some("png")), "image/png");
    assert_eq!(media_type_of("archive.unknown", None), "application/octet-stream");
  }

  #[test]
  fn authors_are_accounts_bots_or_deleted() {
    let people = people();
    assert_eq!(people.author(7), ImportedAuthor::User { user_id: user(70) });
    assert_eq!(
      people.author(99),
      ImportedAuthor::Named {
        name: "Monitor".into()
      }
    );
    assert_eq!(people.author(8), ImportedAuthor::Deleted);
  }

  fn people() -> People {
    People {
      accounts: HashMap::from([(7, (user(70), "anna".to_owned()))]),
      bots: BTreeMap::from([(99, "Monitor".to_owned())]),
      deleted_name: "Deleted user".into(),
    }
  }

  fn user(raw: i64) -> UserId {
    raw.to_string().parse().unwrap()
  }
}
