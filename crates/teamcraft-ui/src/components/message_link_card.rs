//! Embedded previews of linked messages (#319): under a message that links to another message of
//! this server, a quote card with the author, time, source, text and attachments of the linked one.
//!
//! The viewer's own client resolves each card from its decrypted store
//! ([`ChatBackend::linked_message`](teamcraft_client_api::backend::ChatBackend::linked_message)),
//! and only for conversations the viewer is a member of. The sender embeds nothing but the link,
//! so neither the server nor people outside the source conversation learn anything from it. The
//! card re-reads with every store change, so edits and deletes show at once.

use leptos::prelude::*;
use teamcraft_client_api::actions::{FileInfo, LinkedMessage, PermalinkTarget};

use crate::clock::format_date_time;
use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::icons::{Icon, IconName};
use crate::components::rich_text::RichText;
use crate::i18n::use_i18n;
use crate::message_links::{message_links_in, own_hosts};
use crate::model::{ConversationKind, Message};
use crate::object_url::ObjectUrl;
use crate::state::use_app_state;

/// How much of the linked text the card renders; the rest is cut off with an ellipsis.
const PREVIEW_CHARS: usize = 400;
/// Attachment chips a card shows; the rest are counted ("+2").
const MAX_FILE_CHIPS: usize = 4;

/// The previews of all message links in `body` (at most a few).
#[component]
pub fn MessageLinkPreviews(#[prop(into)] body: String) -> impl IntoView {
  message_links_in(&body, &own_hosts())
    .into_iter()
    .map(|link| view! { <MessageLinkCard link=link /> })
    .collect_view()
}

/// One linked message: its card, "deleted", or the notice that the viewer can't see it.
#[component]
fn MessageLinkCard(link: String) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let linked = Memo::new(move |_| state.read(|backend| backend.linked_message(&link)));
  move || match linked.get() {
    // Still looking (or a backend without messages): nothing yet rather than a wrong notice.
    None => ().into_any(),
    Some(LinkedMessage::Unavailable) => view! {
      <div class="linked-message linked-unavailable" role="note">
        <Icon name=IconName::Lock />
        <span>{move || i18n.t("linked-message-unavailable")}</span>
      </div>
    }
    .into_any(),
    Some(LinkedMessage::Deleted { target }) => view! {
      <div class="linked-message linked-deleted" role="note">
        <SourceLine target=target is_reply=target.maybe_thread.is_some() />
        <em class="msg-deleted">{move || i18n.t("linked-message-deleted")}</em>
      </div>
    }
    .into_any(),
    // A member whose device doesn't have the message yet: no lock, it isn't about access.
    Some(LinkedMessage::NotLoaded { target }) => {
      let (conversation, id, maybe_thread) = (target.conversation, target.message, target.maybe_thread);
      view! {
        <div class="linked-message linked-not-loaded" role="note">
          <SourceLine target=target is_reply=target.maybe_thread.is_some() />
          <span>{move || i18n.t("linked-message-not-loaded")}</span>
          <button class="link-btn linked-jump" on:click=move |_| state.open_message(conversation, id, maybe_thread)>
            {move || i18n.t("linked-message-open")}
          </button>
        </div>
      }
      .into_any()
    },
    Some(LinkedMessage::Visible {
      target,
      message,
      files,
    }) => view! { <VisibleCard target=target message=message files=files /> }.into_any(),
  }
}

#[component]
fn VisibleCard(target: PermalinkTarget, message: Message, files: Vec<FileInfo>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let author = state.read(|backend| backend.user(message.author));
  let author_name = author
    .as_ref()
    .map(|user| user.display_name.clone())
    .unwrap_or_default();
  let is_reply = message.maybe_thread.is_some();
  let maybe_thread = target.maybe_thread.or(message.maybe_thread);
  let (conversation, id) = (target.conversation, target.message);
  let me = state.me().username;
  let body = truncated(&message.body, PREVIEW_CHARS);
  // A message of files only names them in its body; the chips show them already.
  let has_text = !body.trim().is_empty() && !(body.starts_with("📎") && !files.is_empty());
  view! {
    <div class="linked-message">
      <div class="linked-head">
        {author.map(|user| view! { <Avatar user=user size=AvatarSize::Small /> })}
        <span class="linked-author" translate="no">{author_name}</span>
        <time class="linked-time" datetime=message.sent_at.to_string()>{format_date_time(message.sent_at, locale)}</time>
      </div>
      <SourceLine target=target is_reply=is_reply />
      {has_text.then(|| view! { <div class="linked-body"><RichText body=body me=me /></div> })}
      {(!files.is_empty()).then(|| view! { <FileChips files=files /> })}
      <button
        class="link-btn linked-jump"
        on:click=move |_| state.open_message(conversation, id, maybe_thread)
      >
        {move || i18n.t("linked-message-open")}
      </button>
    </div>
  }
}

/// "From a thread in #channel" or "In #channel".
#[component]
fn SourceLine(target: PermalinkTarget, is_reply: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let name = state
    .read(|backend| backend.conversation(target.conversation))
    .map(|conversation| match conversation.kind {
      ConversationKind::Public | ConversationKind::Private => format!("#{}", conversation.name),
      _ => conversation.name,
    })
    .unwrap_or_default();
  let key = if is_reply {
    "linked-message-from-thread"
  } else {
    "linked-message-from"
  };
  view! {
    <span class="linked-source" translate="no">{move || i18n.t_with(key, &[("conversation", &name)])}</span>
  }
}

/// Small chips for the attachments: a thumbnail for pictures that have one, an icon otherwise.
#[component]
fn FileChips(files: Vec<FileInfo>) -> impl IntoView {
  let hidden = files.len().saturating_sub(MAX_FILE_CHIPS);
  let chips: Vec<_> = files
    .into_iter()
    .take(MAX_FILE_CHIPS)
    .map(|file| {
      let thumbnail = file
        .maybe_thumbnail_png
        .as_deref()
        .and_then(|bytes| ObjectUrl::from_bytes(bytes, "image/png"));
      let maybe_src = thumbnail.as_ref().map(|url| url.as_str().to_owned());
      // Revoked when the chip leaves the screen.
      StoredValue::new_local(thumbnail);
      let title = file.file_name.clone();
      view! {
        <span class="linked-file" title=title>
          {match maybe_src {
            Some(src) => view! { <img src=src alt="" loading="lazy" /> }.into_any(),
            None => view! { <Icon name=IconName::Paperclip /> }.into_any(),
          }}
          <span class="linked-file-name" translate="no">{file.file_name}</span>
        </span>
      }
    })
    .collect();
  view! {
    <div class="linked-files">
      {chips}
      {(hidden > 0).then(|| view! { <span class="linked-file linked-file-more">{format!("+{hidden}")}</span> })}
    </div>
  }
}

/// At most `max_chars` characters of `body`, with an ellipsis where it was cut.
fn truncated(body: &str, max_chars: usize) -> String {
  match body.char_indices().nth(max_chars) {
    Some((cut, _)) => format!("{}…", body[..cut].trim_end()),
    None => body.to_owned(),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn long_text_is_cut_on_a_character_boundary() {
    assert_eq!(truncated("short", 10), "short");
    assert_eq!(truncated("äöü und mehr", 4), "äöü…");
    assert_eq!(
      truncated(&"x".repeat(500), PREVIEW_CHARS).chars().count(),
      PREVIEW_CHARS + 1
    );
  }
}
