use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use teamcraft_client_api::actions::{FileSource, MessagePriority, OutgoingFile, OutgoingMessage};
use teamcraft_client_api::admin::format_bytes;
use wasm_bindgen_futures::JsFuture;

use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::{ConversationId, ThreadId};
use crate::notifier::needs_channel_confirmation;
use crate::settings::SendMode;
use crate::state::use_app_state;

/// How long typing pauses before the draft is stored.
const DRAFT_SAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(800);

/// Tallest the input grows before it scrolls.
const MAX_INPUT_HEIGHT_PX: i32 = 220;
const PLACEHOLDER_EMOJI: &str = "🙂";

/// A file chosen, dropped or pasted that is read into memory and waits for the send.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingFile {
  name: String,
  media_type: String,
  bytes: Vec<u8>,
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
  let input = NodeRef::<html::Textarea>::new();
  let file_input = NodeRef::<html::Input>::new();
  let is_empty = RwSignal::new(true);
  let attachments = RwSignal::new(Vec::<PendingFile>::new());
  let upload_error = RwSignal::new(None::<String>);
  let is_drag_over = RwSignal::new(false);

  let attachment_list = move || {
    attachments
      .get()
      .into_iter()
      .enumerate()
      .map(|(index, file)| (index, file.name))
      .collect::<Vec<(usize, String)>>()
  };

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
        if draft_generation.get_untracked() != generation {
          return;
        }
        let text = input
          .get_untracked()
          .map(|element| element.value())
          .unwrap_or_default();
        state.write(|backend| backend.set_draft(conversation, maybe_thread, &text));
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
    let text = element.value();
    if text.trim().is_empty() && attachments.with_untracked(Vec::is_empty) {
      return;
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
        also_send_to_channel: false,
        files,
      });
    });
    element.set_value("");
    draft_generation.update(|generation| *generation += 1);
    state.write(|backend| backend.set_draft(conversation, maybe_thread, ""));
    attachments.set(Vec::new());
    is_empty.set(true);
    resize();
    if maybe_thread.is_none() {
      state.jump_to_latest.update(|value| *value += 1);
    }
  };
  let send = move || send_checked(false);

  let on_keydown = move |ev: web_sys::KeyboardEvent| {
    if ev.key() != "Enter" || ev.is_composing() {
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
            attachments.update(|list| {
              list.push(PendingFile {
                name: file.name(),
                media_type,
                bytes,
              })
            });
          },
          Err(_) => upload_error.set(Some(
            i18n.t_with("composer-file-unreadable", &[("name", &file.name())]),
          )),
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
  let on_drop = move |ev: web_sys::DragEvent| {
    ev.prevent_default();
    is_drag_over.set(false);
    if let Some(files) = ev.data_transfer().and_then(|transfer| transfer.files()) {
      add_files((0..files.length()).filter_map(|i| files.item(i)).collect());
    }
  };
  let on_paste = move |ev: web_sys::ClipboardEvent| {
    let Some(files) = ev.clipboard_data().and_then(|data| data.files()) else {
      return;
    };
    if files.length() > 0 {
      ev.prevent_default();
      add_files((0..files.length()).filter_map(|i| files.item(i)).collect());
    }
  };

  let insert_emoji = move |_| {
    if let Some(element) = input.get_untracked() {
      element.set_value(&format!("{}{PLACEHOLDER_EMOJI}", element.value()));
      is_empty.set(false);
      drop(element.focus());
    }
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
            <For each=attachment_list key=|(i, name)| (*i, name.clone()) let:item>
              {
                let (index, name) = item;
                view! {
                  <li class="chip">
                    <Icon name=IconName::File />
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
              on:click=insert_emoji
            >
              <Icon name=IconName::Smile />
            </button>
            <span class="composer-hint">
              {move || match state.settings.send_mode.get() {
                SendMode::Enter => i18n.t("composer-hint-enter"),
                SendMode::CtrlEnter => i18n.t("composer-hint-ctrl-enter"),
              }}
            </span>
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
        <p class="markdown-hint">{move || i18n.t("composer-markdown-hint")}</p>
      </Show>
    </div>
  }
}
