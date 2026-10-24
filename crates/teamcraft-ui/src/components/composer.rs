use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use teamcraft_client_api::actions::{FileSource, MessagePriority, OutgoingFile, OutgoingMessage};
use teamcraft_client_api::admin::format_bytes;
use wasm_bindgen_futures::JsFuture;

use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::channel_view::DroppedFiles;
use crate::components::composer_commands::{self, Notice, Outcome};
use crate::components::emoji_picker::EmojiPicker;
use crate::components::icons::{Icon, IconName};
use crate::components::rich_text::RichText;
use crate::groups::use_groups;
use crate::i18n::use_i18n;
use crate::keys::t_with_shortcuts;
use crate::mention::{self, TriggerKind};
use crate::model::{ConversationId, ThreadId};
use crate::model::{ConversationKind, MessageKind, User};
use crate::notifier::needs_channel_confirmation;
use crate::reactive::{SetIfAlive, UpdateIfAlive};
use crate::settings::SendMode;
use crate::slash::{self, Parsed};
use crate::state::use_app_state;

/// How long typing pauses before the draft is stored.
const DRAFT_SAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(800);

/// Tallest the input grows before it scrolls.
const MAX_INPUT_HEIGHT_PX: i32 = 220;
/// How many of the newest messages ↑ searches for the user's last one.
const EDIT_LAST_SCAN: usize = 50;
/// The newest messages that are certainly rendered while the feed follows the bottom.
const ON_SCREEN_ROWS: usize = 3;

/// A file chosen, dropped or pasted that is read into memory and waits for the send.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingFile {
  name: String,
  media_type: String,
  bytes: Vec<u8>,
  /// Object URL of an image for the thumbnail in the chip; revoked when the file leaves the list.
  maybe_preview_url: Option<String>,
}

/// One line in the picker under the input.
#[derive(Clone, Debug, PartialEq)]
struct Suggestion {
  /// What replaces the typed word (after the `@` or `/`).
  insert: String,
  label: String,
  detail: String,
  person: Option<User>,
  /// The typed word already is this suggestion (`/shrug`): Enter sends instead of completing.
  is_exact: bool,
}

const MAX_SUGGESTIONS: usize = 8;

/// People and groups whose name starts with, or contains, what was typed after `@`.
fn mention_suggestions(
  query: &str,
  people: &[User],
  groups: &[teamcraft_proto::UserGroupResponse],
  channel_label: &str,
) -> Vec<Suggestion> {
  let needle = query.to_lowercase();
  let rank = |names: &[&str]| -> Option<u8> {
    names
      .iter()
      .map(|name| name.to_lowercase())
      .filter_map(|name| {
        if name.starts_with(&needle) {
          Some(0)
        } else if name.contains(&needle) {
          Some(1)
        } else {
          None
        }
      })
      .min()
  };
  let mut ranked: Vec<(u8, Suggestion)> = Vec::new();
  if "channel".starts_with(&needle) {
    ranked.push((
      0,
      Suggestion {
        insert: "channel".to_owned(),
        label: "@channel".to_owned(),
        detail: channel_label.to_owned(),
        person: None,
        is_exact: false,
      },
    ));
  }
  for user in people.iter().filter(|user| !user.is_bot) {
    if let Some(score) = rank(&[&user.username, &user.display_name]) {
      ranked.push((
        score,
        Suggestion {
          insert: user.username.clone(),
          label: user.display_name.clone(),
          detail: match &user.maybe_organization {
            Some(organization) => format!("@{} · {organization}", user.username),
            None => format!("@{}", user.username),
          },
          person: Some(user.clone()),
          is_exact: false,
        },
      ));
    }
  }
  for group in groups {
    if let Some(score) = rank(&[&group.handle, &group.name]) {
      ranked.push((
        score,
        Suggestion {
          insert: group.handle.clone(),
          label: group.name.clone(),
          detail: format!("@{}", group.handle),
          person: None,
          is_exact: false,
        },
      ));
    }
  }
  ranked.sort_by_key(|(score, suggestion)| (*score, suggestion.label.to_lowercase()));
  ranked
    .into_iter()
    .map(|(_, suggestion)| suggestion)
    .take(MAX_SUGGESTIONS)
    .collect()
}

/// Message composer for a channel or, with `maybe_thread`, for a thread reply.
#[component]
pub fn Composer(
  conversation: ConversationId,
  #[prop(optional)] maybe_thread: Option<ThreadId>,
  /// i18n key of the placeholder text.
  #[prop(default = "composer-placeholder")]
  placeholder_key: &'static str,
  #[prop(default = true)] can_post: bool,
  #[prop(into, default = String::new())] conversation_name: String,
) -> impl IntoView {
  let conversation_name = StoredValue::new(conversation_name);
  let i18n = use_i18n();
  let state = use_app_state();
  // Thread replies can also be posted to the conversation itself.
  let also_send_to_channel = RwSignal::new(false);
  let channel_name = move || {
    state
      .read(|backend| backend.conversation(conversation))
      .map(|conv| match conv.kind {
        ConversationKind::Direct | ConversationKind::Group => conv.name,
        _ => format!("#{}", conv.name),
      })
      .unwrap_or_default()
  };
  let input = NodeRef::<html::Textarea>::new();
  let file_input = NodeRef::<html::Input>::new();
  let is_empty = RwSignal::new(true);
  let attachments = RwSignal::new(Vec::<PendingFile>::new());
  let upload_error = RwSignal::new(None::<String>);
  let is_drag_over = RwSignal::new(false);
  let notice = RwSignal::new(None::<Notice>);
  let suggestions = RwSignal::new(Vec::<Suggestion>::new());
  let selected_suggestion = RwSignal::new(0_usize);
  let indexed_suggestions = Memo::new(move |_| {
    suggestions
      .get()
      .into_iter()
      .enumerate()
      .collect::<Vec<(usize, Suggestion)>>()
  });
  let live_text = RwSignal::new(String::new());
  let show_formatting = Memo::new(move |_| state.settings.prefs.get().composer.show_formatting);

  let attachment_list = move || {
    attachments
      .get()
      .into_iter()
      .enumerate()
      .map(|(index, file)| (index, file.name, file.maybe_preview_url))
      .collect::<Vec<(usize, String, Option<String>)>>()
  };
  // Thumbnails of files that were removed or sent free their object URL.
  Effect::new(move |previous: Option<Vec<String>>| {
    let current: Vec<String> = attachments.with(|list| {
      list
        .iter()
        .filter_map(|file| file.maybe_preview_url.clone())
        .collect()
    });
    for url in previous
      .unwrap_or_default()
      .iter()
      .filter(|url| !current.contains(url))
    {
      drop(web_sys::Url::revoke_object_url(url));
    }
    current
  });

  // Drafts sync across devices: restore once the stored one arrives, store while typing.
  let is_draft_restored = RwSignal::new(false);
  let draft_generation = RwSignal::new(0_u32);
  Effect::new(move |_| {
    let stored = state.read(|backend| backend.draft(conversation, maybe_thread));
    if is_draft_restored.get_untracked() {
      return;
    }
    let Some(element) = input.get() else { return };
    if let Some(text) = stored.filter(|text| !text.is_empty()) {
      is_draft_restored.set(true);
      if element.value().is_empty() {
        element.set_value(&text);
        is_empty.set(false);
      }
    }
  });
  let save_draft = move || {
    draft_generation.update(|generation| *generation += 1);
    let generation = draft_generation.get_untracked();
    set_timeout(
      move || {
        // The composer may be gone by then (another channel opened); `try_*` then skips the save.
        if draft_generation.try_get_untracked() != Some(generation) {
          return;
        }
        let text = input
          .try_get_untracked()
          .flatten()
          .map(|element| element.value())
          .unwrap_or_default();
        state.try_write(|backend| backend.set_draft(conversation, maybe_thread, &text));
      },
      DRAFT_SAVE_DELAY,
    );
  };

  let resize = move || {
    if let Some(element) = input.get_untracked() {
      let style = web_sys::HtmlElement::style(&element);
      drop(style.set_property("height", "auto"));
      let height = element.scroll_height().min(MAX_INPUT_HEIGHT_PX);
      drop(style.set_property("height", &format!("{height}px")));
    }
  };

  // A message with @channel in a large conversation waits here until the user confirms.
  let pending_confirm = RwSignal::new(None::<usize>);
  let send_checked = move |is_confirmed: bool| {
    let Some(element) = input.get_untracked() else {
      return;
    };
    let mut text = element.value();
    if text.trim().is_empty() && attachments.with_untracked(Vec::is_empty) {
      return;
    }
    // Slash commands run on the client; a command that sends something falls through to the send.
    if attachments.with_untracked(Vec::is_empty) {
      match slash::parse(&text) {
        Parsed::Invalid(error) => {
          notice.set(Some(Notice::Text(composer_commands::explain_error(i18n, &error))));
          return;
        },
        Parsed::Command(command) => {
          let outcome = composer_commands::run(state, i18n, conversation, command);
          let sends = match outcome {
            Outcome::Send(body) => {
              text = body;
              notice.set(None);
              true
            },
            Outcome::Notice(shown) => {
              notice.set(Some(shown));
              false
            },
            Outcome::Done => {
              notice.set(None);
              false
            },
          };
          if !sends {
            element.set_value("");
            suggestions.set(Vec::new());
            live_text.set(String::new());
            is_empty.set(true);
            state.write(|backend| backend.set_draft(conversation, maybe_thread, ""));
            resize();
            return;
          }
        },
        Parsed::Message(message) => text = message,
      }
    }
    if state.settings.prefs.get_untracked().composer.convert_emoticons {
      text = crate::emoji::convert(&text);
    }
    if !is_confirmed {
      let members = state
        .read(|backend| backend.conversation(conversation))
        .map_or(0, |conv| conv.member_ids.len());
      let threshold = state
        .settings
        .prefs
        .get_untracked()
        .notifications
        .confirm_channel_over;
      if needs_channel_confirmation(&text, members, threshold) {
        pending_confirm.set(Some(members));
        return;
      }
    }
    pending_confirm.set(None);
    let files: Vec<OutgoingFile> = attachments
      .get_untracked()
      .into_iter()
      .map(|file| OutgoingFile {
        file_name: file.name,
        media_type: file.media_type,
        source: FileSource::Bytes { bytes: file.bytes },
      })
      .collect();
    let body = text;
    let maybe_reply_to = state
      .reply_target
      .get_untracked()
      .filter(|target| target.conversation == conversation)
      .map(|target| target.message);
    state.reply_target.set(None);
    state.write(|backend| {
      backend.send_message(OutgoingMessage {
        conversation,
        maybe_thread,
        body,
        maybe_reply_to,
        priority: MessagePriority::Normal,
        wants_acknowledgement: false,
        is_silent: false,
        also_send_to_channel: maybe_thread.is_some() && also_send_to_channel.get_untracked(),
        files,
      });
    });
    element.set_value("");
    also_send_to_channel.set(false);
    draft_generation.update(|generation| *generation += 1);
    state.write(|backend| backend.set_draft(conversation, maybe_thread, ""));
    suggestions.set(Vec::new());
    live_text.set(String::new());
    attachments.set(Vec::new());
    is_empty.set(true);
    resize();
    if maybe_thread.is_none() {
      state.jump_to_latest.update(|value| *value += 1);
    }
  };
  let send = move || send_checked(false);

  // The picker under the input: people and user groups after `@`, commands after a leading `/`.
  let groups = use_groups();
  let refresh_suggestions = move || {
    let Some(element) = input.get_untracked() else {
      return;
    };
    let cursor = element.selection_start().ok().flatten().unwrap_or(0) as usize;
    let Some(found) = mention::trigger(&element.value(), cursor) else {
      suggestions.set(Vec::new());
      return;
    };
    let list = match found.kind {
      TriggerKind::Command => slash::matching_commands(&found.query)
        .into_iter()
        .map(|(name, usage)| Suggestion {
          insert: name.to_owned(),
          label: format!("/{name} {usage}").trim_end().to_owned(),
          detail: String::new(),
          person: None,
          is_exact: name == found.query.to_lowercase(),
        })
        .collect(),
      TriggerKind::Mention => {
        let people = state.read(|backend| backend.users());
        let groups = groups.map(|hub| hub.groups()).unwrap_or_default();
        mention_suggestions(&found.query, &people, &groups, &i18n.t("mention-channel"))
      },
    };
    selected_suggestion.set(0);
    suggestions.set(list);
  };
  let accept_suggestion = move |index: usize| {
    let Some(chosen) = suggestions.with_untracked(|list| list.get(index).cloned()) else {
      return;
    };
    let Some(element) = input.get_untracked() else {
      return;
    };
    let value = element.value();
    let cursor = element.selection_start().ok().flatten().unwrap_or(0) as usize;
    if let Some(found) = mention::trigger(&value, cursor) {
      let (text, at) = mention::complete(&value, &found, cursor, &chosen.insert);
      element.set_value(&text);
      drop(element.set_selection_range(at as u32, at as u32));
      is_empty.set(text.trim().is_empty());
      save_draft();
    }
    suggestions.set(Vec::new());
    drop(element.focus());
  };
  // Formatting buttons wrap the selection in Markdown markers.
  let wrap = move |left: &'static str, right: &'static str| {
    let Some(element) = input.get_untracked() else {
      return;
    };
    let start = element.selection_start().ok().flatten().unwrap_or(0) as usize;
    let end = element.selection_end().ok().flatten().unwrap_or(0) as usize;
    let (text, from, to) = mention::wrap_selection(&element.value(), start, end, left, right);
    element.set_value(&text);
    drop(element.set_selection_range(from as u32, to as u32));
    drop(element.focus());
    is_empty.set(text.trim().is_empty());
    live_text.set(text);
    resize();
    save_draft();
  };

  // ↑ in the empty composer edits the user's newest message here (Slack, Teams, Mattermost).
  let edit_last_message = move || -> bool {
    let me = state.me().id;
    let found = state.read(|backend| match maybe_thread {
      Some(thread) => backend
        .thread(conversation, thread)
        .into_iter()
        .rev()
        .find(|message| {
          message.author == me && message.kind == MessageKind::User && message.maybe_thread.is_some()
        })
        .map(|message| (message.id, None)),
      None => {
        let count = backend.message_count(conversation);
        let start = count.saturating_sub(EDIT_LAST_SCAN);
        backend
          .messages(conversation, start, count - start)
          .into_iter()
          .enumerate()
          .rev()
          .find(|(_, message)| message.author == me && message.kind == MessageKind::User)
          .map(|(offset, message)| (message.id, Some(start + offset)))
      },
    });
    let Some((message, maybe_index)) = found else {
      return false;
    };
    // The newest rows are on screen while following; older ones are scrolled to so their row exists.
    if let Some(index) = maybe_index
      && state
        .read(|backend| backend.message_count(conversation))
        .saturating_sub(index)
        > ON_SCREEN_ROWS
    {
      state.jump_to_index.set(Some((conversation, index)));
    }
    state.edit_request.set(Some(message));
    true
  };

  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    if ev.is_composing() {
      return;
    }
    let listed = suggestions.with_untracked(Vec::len);
    if listed > 0 {
      match ev.key().as_str() {
        "ArrowDown" => {
          ev.prevent_default();
          selected_suggestion.update(|at| *at = (*at + 1) % listed);
          return;
        },
        "ArrowUp" => {
          ev.prevent_default();
          selected_suggestion.update(|at| *at = (*at + listed - 1) % listed);
          return;
        },
        "Enter" | "Tab" => {
          let is_exact = suggestions.with_untracked(|list| {
            list
              .get(selected_suggestion.get_untracked())
              .is_some_and(|item| item.is_exact)
          });
          // A fully typed command is sent, not completed.
          if !(is_exact && ev.key() == "Enter") {
            ev.prevent_default();
            accept_suggestion(selected_suggestion.get_untracked());
            return;
          }
        },
        "Escape" => {
          ev.prevent_default();
          suggestions.set(Vec::new());
          return;
        },
        _ => {},
      }
    }
    if ev.key() == "ArrowUp"
      && !ev.shift_key()
      && !ev.alt_key()
      && !ev.ctrl_key()
      && !ev.meta_key()
      && is_empty.get_untracked()
      && attachments.with_untracked(Vec::is_empty)
      && edit_last_message()
    {
      ev.prevent_default();
      return;
    }
    if ev.key() != "Enter" {
      return;
    }
    let modifier = ev.ctrl_key() || ev.meta_key();
    let should_send = match state.settings.send_mode.get_untracked() {
      SendMode::Enter => !ev.shift_key() && !modifier,
      SendMode::CtrlEnter => modifier,
    };
    if should_send {
      ev.prevent_default();
      send();
    } else if modifier {
      // Ctrl/Cmd+Enter in Enter-sends mode would otherwise do nothing; treat it as send as well.
      ev.prevent_default();
      send();
    }
  };

  let on_input = move |_| {
    is_empty.set(
      input
        .get_untracked()
        .is_none_or(|element| element.value().trim().is_empty()),
    );
    resize();
    save_draft();
    refresh_suggestions();
    if show_formatting.get_untracked() {
      live_text.set(
        input
          .get_untracked()
          .map(|element| element.value())
          .unwrap_or_default(),
      );
    }
  };

  // Reads the files into memory; the core encrypts and uploads them when the message is sent.
  let add_files = move |files: Vec<web_sys::File>| {
    let limit = state.read(|backend| backend.max_upload_bytes());
    for file in files {
      let size = file.size() as u64;
      if limit.is_some_and(|limit| size > limit) {
        upload_error.set(Some(i18n.t_with(
          "composer-file-too-large",
          &[
            ("name", &file.name()),
            ("limit", &format_bytes(limit.unwrap_or_default())),
          ],
        )));
        continue;
      }
      upload_error.set(None);
      leptos::task::spawn_local(async move {
        match JsFuture::from(file.array_buffer()).await {
          Ok(buffer) => {
            let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
            let media_type = if file.type_().is_empty() {
              "application/octet-stream".to_owned()
            } else {
              file.type_()
            };
            let maybe_preview_url = media_type
              .starts_with("image/")
              .then(|| web_sys::Url::create_object_url_with_blob(&file).ok())
              .flatten();
            let read = PendingFile {
              name: file.name(),
              media_type,
              bytes,
              maybe_preview_url,
            };
            if let Some(PendingFile {
              maybe_preview_url: Some(url),
              ..
            }) = add_read_file(attachments, read)
            {
              drop(web_sys::Url::revoke_object_url(&url));
            }
          },
          Err(_) => {
            let text = i18n.t_with("composer-file-unreadable", &[("name", &file.name())]);
            // The composer may be gone by now; see `add_read_file`.
            upload_error.set_if_alive(Some(text));
          },
        }
      });
    }
  };

  let on_files = move |_| {
    let Some(files) = file_input.get_untracked().and_then(|element| element.files()) else {
      return;
    };
    add_files((0..files.length()).filter_map(|i| files.item(i)).collect());
    if let Some(element) = file_input.get_untracked() {
      element.set_value("");
    }
  };
  // Drops anywhere on the conversation arrive through the channel view (main composer only).
  if maybe_thread.is_none()
    && let Some(DroppedFiles(dropped)) = use_context::<DroppedFiles>()
  {
    Effect::new(move |_| {
      let files = dropped.get();
      if !files.is_empty() {
        dropped.set(Vec::new());
        add_files(files);
      }
    });
  }
  let on_drop = move |ev: web_sys::DragEvent| {
    // Marks the drop as handled, so the conversation around doesn't attach the files a second time.
    ev.prevent_default();
    is_drag_over.set(false);
    if let Some(files) = ev.data_transfer().and_then(|transfer| transfer.files()) {
      add_files((0..files.length()).filter_map(|i| files.item(i)).collect());
    }
  };
  let on_paste = move |ev: web_sys::ClipboardEvent| {
    let Some(data) = ev.clipboard_data() else {
      return;
    };
    if let Some(files) = data.files()
      && files.length() > 0
    {
      ev.prevent_default();
      add_files((0..files.length()).filter_map(|i| files.item(i)).collect());
      return;
    }
    // A URL pasted over selected text links the text instead of replacing it.
    let (Ok(pasted), Some(element)) = (data.get_data("text/plain"), input.get_untracked()) else {
      return;
    };
    let start = element.selection_start().ok().flatten().unwrap_or(0) as usize;
    let end = element.selection_end().ok().flatten().unwrap_or(0) as usize;
    if let Some((text, cursor)) = mention::link_over_selection(&element.value(), start, end, &pasted) {
      ev.prevent_default();
      element.set_value(&text);
      drop(element.set_selection_range(cursor as u32, cursor as u32));
      is_empty.set(false);
      resize();
      save_draft();
    }
  };

  let maybe_emoji_anchor = RwSignal::new_local(None::<web_sys::Element>);
  // The picker takes the focus; the textarea keeps its selection, so the emoji lands at the cursor.
  let insert_emoji = move |emoji: String| {
    maybe_emoji_anchor.set(None);
    let Some(element) = input.get_untracked() else {
      return;
    };
    let value = element.value();
    let start = element.selection_start().ok().flatten().unwrap_or(0) as usize;
    let end = element.selection_end().ok().flatten().unwrap_or(0) as usize;
    let (text, cursor) = mention::replace_selection(&value, start, end, &emoji);
    element.set_value(&text);
    drop(element.focus());
    drop(element.set_selection_range(cursor as u32, cursor as u32));
    is_empty.set(false);
    resize();
    save_draft();
  };

  view! {
    <div
      class="composer"
      class:composer-disabled=!can_post
      class:composer-drop=move || is_drag_over.get()
      on:dragover=move |ev| {
        ev.prevent_default();
        is_drag_over.set(true);
      }
      on:dragleave=move |_| is_drag_over.set(false)
      on:drop=on_drop
    >
      <Show
        when=move || can_post
        fallback=move || {
          view! { <p class="composer-readonly">{move || i18n.t("composer-readonly")}</p> }
        }
      >
        <Show when=move || !attachments.with(Vec::is_empty) fallback=|| ()>
          <ul class="attachment-chips" aria-label=move || i18n.t("composer-attachments")>
            <For each=attachment_list key=|(i, name, _)| (*i, name.clone()) let:item>
              {
                let (index, name, maybe_preview_url) = item;
                let leading_is_image = maybe_preview_url.is_some();
                let leading = match maybe_preview_url {
                  Some(url) => view! { <img class="chip-thumb" src=url alt="" /> }.into_any(),
                  None => view! { <Icon name=IconName::File /> }.into_any(),
                };
                view! {
                  <li class="chip" class:chip-image=leading_is_image>
                    {leading}
                    <span class="chip-name">{name}</span>
                    <button
                      class="icon-btn icon-btn-small"
                      aria-label=move || i18n.t("composer-remove-attachment")
                      on:click=move |_| attachments.update(|list| { if index < list.len() { list.remove(index); } })
                    >
                      <Icon name=IconName::Close />
                    </button>
                  </li>
                }
              }
            </For>
          </ul>
        </Show>
        {move || {
          upload_error.get().map(|message| view! { <p class="composer-error" role="alert">{message}</p> })
        }}
        {move || {
          pending_confirm.get().map(|members| {
            view! {
              <div class="composer-confirm" role="alertdialog" aria-live="assertive">
                <span>{i18n.t_with("composer-confirm-channel", &[("count", &members.to_string())])}</span>
                <button class="btn btn-small btn-primary" on:click=move |_| send_checked(true)>
                  {move || i18n.t("composer-confirm-send")}
                </button>
                <button class="btn btn-small" on:click=move |_| pending_confirm.set(None)>
                  {move || i18n.t("cancel")}
                </button>
              </div>
            }
          })
        }}
        {move || {
          state
            .reply_target
            .get()
            .filter(|target| target.conversation == conversation)
            .map(|target| {
              view! {
                <div class="reply-banner">
                  <span class="grow" translate="no">
                    {i18n.t_with("quote-replying-to", &[("name", &target.author_name)])} " — " {target.excerpt}
                  </span>
                  <button class="icon-btn icon-btn-small" aria-label=move || i18n.t("quote-cancel") on:click=move |_| state.reply_target.set(None)>
                    <Icon name=IconName::Close />
                  </button>
                </div>
              }
            })
        }}
        {move || {
          notice.get().map(|shown| match shown {
            Notice::Text(message) => view! { <p class="composer-notice" role="status">{message}</p> }.into_any(),
            Notice::Help => view! {
              <div class="composer-notice composer-help" role="status">
                <p>{move || i18n.t("slash-help-title")}</p>
                <ul>
                  {slash::COMMANDS
                    .iter()
                    .map(|(name, usage)| view! { <li><code>{format!("/{name} {usage}").trim_end().to_owned()}</code></li> })
                    .collect_view()}
                </ul>
              </div>
            }
            .into_any(),
          })
        }}
        <Show when=move || !suggestions.with(Vec::is_empty) fallback=|| ()>
          <ul class="suggestions" role="listbox" aria-label=move || i18n.t("composer-suggestions")>
            <For each=move || indexed_suggestions.get() key=|(index, item)| (*index, item.label.clone()) let:entry>
              {
                let (index, item) = entry;
                view! {
                  <li role="option" aria-selected=move || (selected_suggestion.get() == index).to_string()>
                    <button
                      type="button"
                      class="suggestion"
                      class:current=move || selected_suggestion.get() == index
                      on:mousedown=move |ev| { ev.prevent_default(); accept_suggestion(index); }
                    >
                      {item.person.clone().map(|user| view! { <Avatar user=user size=AvatarSize::Small /> })}
                      <span translate="no">{item.label.clone()}</span>
                      <span class="suggestion-detail" translate="no">{item.detail.clone()}</span>
                    </button>
                  </li>
                }
              }
            </For>
          </ul>
        </Show>
        <Show when=move || show_formatting.get() fallback=|| ()>
          <div class="format-toolbar" role="toolbar" aria-label=move || i18n.t("composer-format")>
            <button type="button" class="icon-btn" title=move || i18n.t("format-bold") aria-label=move || i18n.t("format-bold") on:click=move |_| wrap("**", "**")><strong>"B"</strong></button>
            <button type="button" class="icon-btn" title=move || i18n.t("format-italic") aria-label=move || i18n.t("format-italic") on:click=move |_| wrap("_", "_")><em>"I"</em></button>
            <button type="button" class="icon-btn" title=move || i18n.t("format-strike") aria-label=move || i18n.t("format-strike") on:click=move |_| wrap("~~", "~~")><s>"S"</s></button>
            <button type="button" class="icon-btn" title=move || i18n.t("format-code") aria-label=move || i18n.t("format-code") on:click=move |_| wrap("`", "`")><code>"<>"</code></button>
            <button type="button" class="icon-btn" title=move || i18n.t("format-code-block") aria-label=move || i18n.t("format-code-block") on:click=move |_| wrap("```\n", "\n```")><code>"{}"</code></button>
          </div>
        </Show>
        <div class="composer-box">
          <label class="visually-hidden" for=format!("composer-{conversation}-{}", maybe_thread.map_or_else(String::new, |thread| thread.to_string()))>
            {move || i18n.t("composer-label")}
          </label>
          <textarea
            id=format!("composer-{conversation}-{}", maybe_thread.map_or_else(String::new, |thread| thread.to_string()))
            class="composer-input"
            rows="1"
            name="message"
            autocomplete="off"
            placeholder=move || {
              let name = conversation_name.get_value();
              if name.is_empty() { i18n.t(placeholder_key) } else { i18n.t_with(placeholder_key, &[("name", &name)]) }
            }
            node_ref=input
            on:keydown=on_keydown
            on:input=on_input
            on:click=move |_| refresh_suggestions()
            on:keyup=move |ev| {
              if matches!(ev.key().as_str(), "ArrowLeft" | "ArrowRight" | "Home" | "End") {
                refresh_suggestions();
              }
            }
            on:paste=on_paste
            data-composer=if maybe_thread.is_some() { "thread" } else { "channel" }
          ></textarea>
          <div class="composer-toolbar">
            <input type="file" multiple hidden node_ref=file_input on:change=on_files tabindex="-1" />
            <button
              class="icon-btn"
              aria-label=move || i18n.t("composer-attach")
              title=move || i18n.t("composer-attach")
              on:click=move |_| {
                if let Some(element) = file_input.get_untracked() {
                  let element: &web_sys::HtmlElement = element.unchecked_ref();
                  element.click();
                }
              }
            >
              <Icon name=IconName::Paperclip />
            </button>
            <button
              class="icon-btn"
              aria-label=move || i18n.t("composer-emoji")
              title=move || i18n.t("composer-emoji")
              aria-haspopup="dialog"
              aria-expanded=move || maybe_emoji_anchor.with(Option::is_some).to_string()
              on:click=move |ev| maybe_emoji_anchor.set(Some(event_target::<web_sys::Element>(&ev)))
            >
              <Icon name=IconName::Smile />
            </button>
            {move || {
              maybe_emoji_anchor
                .get()
                .map(|anchor| {
                  view! {
                    <EmojiPicker
                      anchor=anchor
                      returns_focus=false
                      on_pick=Callback::new(insert_emoji)
                      on_close=Callback::new(move |()| {
                        maybe_emoji_anchor.set(None);
                        if let Some(element) = input.get_untracked() {
                          drop(element.focus());
                        }
                      })
                    />
                  }
                })
            }}
            <button
              type="button"
              class="icon-btn"
              aria-pressed=move || show_formatting.get().to_string()
              aria-label=move || i18n.t("composer-format-toggle")
              title=move || i18n.t("composer-format-toggle")
              on:click=move |_| {
                state.settings.prefs.update(|prefs| prefs.composer.show_formatting = !prefs.composer.show_formatting);
                live_text.set(input.get_untracked().map(|element| element.value()).unwrap_or_default());
              }
            >
              <span aria-hidden="true">"Aa"</span>
            </button>
            {if maybe_thread.is_some() {
              // In a thread the hint makes room for "Also send to #channel" (Slack, Mattermost).
              view! {
                <label class="composer-hint also-send">
                  <input
                    type="checkbox"
                    prop:checked=move || also_send_to_channel.get()
                    on:change=move |ev| also_send_to_channel.set(event_target_checked(&ev))
                  />
                  <span>{move || i18n.t_with("composer-also-send", &[("name", &channel_name())])}</span>
                </label>
              }
                .into_any()
            } else {
              view! {
                <span class="composer-hint">
                  {move || match state.settings.send_mode.get() {
                    SendMode::Enter => t_with_shortcuts(i18n, "composer-hint-enter"),
                    SendMode::CtrlEnter => t_with_shortcuts(i18n, "composer-hint-ctrl-enter"),
                  }}
                </span>
              }
                .into_any()
            }}
            <button
              class="send-btn"
              disabled=move || is_empty.get() && attachments.with(Vec::is_empty)
              aria-label=move || i18n.t("composer-send")
              title=move || i18n.t("composer-send")
              on:click=move |_| send()
            >
              <Icon name=IconName::Send />
            </button>
          </div>
        </div>
        <Show when=move || show_formatting.get() && !live_text.with(|text| text.trim().is_empty()) fallback=|| ()>
          <div class="composer-preview" aria-label=move || i18n.t("composer-preview")>
            {move || view! { <RichText body=live_text.get() me=state.me().username /> }}
          </div>
        </Show>
        <p class="markdown-hint">{move || i18n.t("composer-markdown-hint")}</p>
      </Show>
    </div>
  }
}

/// Adds a file that was read in the background. The composer may be gone by then (another
/// conversation was opened while a big file was read): its signals are disposed, and writing them
/// would panic and abort the whole wasm app (issue #316). Returns the file when nobody took it.
fn add_read_file(attachments: RwSignal<Vec<PendingFile>>, file: PendingFile) -> Option<PendingFile> {
  let mut maybe_file = Some(file);
  attachments.try_update(|list| list.extend(maybe_file.take()));
  maybe_file
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn a_file_read_after_the_composer_closed_is_handed_back_without_a_panic() {
    let owner = Owner::new();
    let attachments = owner.with(|| RwSignal::new(Vec::<PendingFile>::new()));
    assert_eq!(add_read_file(attachments, file("open.txt")), None);
    assert_eq!(attachments.get_untracked().len(), 1);

    owner.cleanup();
    assert_eq!(
      add_read_file(attachments, file("late.txt")),
      Some(file("late.txt")),
      "a disposed list is not written"
    );
  }

  fn file(name: &str) -> PendingFile {
    PendingFile {
      name: name.into(),
      media_type: "text/plain".into(),
      bytes: b"x".to_vec(),
      maybe_preview_url: None,
    }
  }
}
