use leptos::html;
use leptos::prelude::*;
use wasm_bindgen::JsCast;

use teamcraft_client_api::actions::{FileSource, MessagePriority, OutgoingFile, OutgoingMessage};
use teamcraft_client_api::admin::format_bytes;
use wasm_bindgen_futures::JsFuture;

use crate::components::avatar::{Avatar, AvatarSize};
use crate::components::channel_view::DroppedFiles;
use crate::components::composer_commands::{self, Notice, Outcome};
use crate::components::composer_editor;
use crate::components::composer_toolbar::{
  ActiveFormats, FormatAction, FormatToolbar, SendOptionsMenu, format_action_for, move_focus_vertical,
};
use crate::components::emoji_picker::EmojiPicker;
use crate::components::file_views::blob_url;
use crate::components::icons::{Icon, IconName};
use crate::editor_doc::{BlockFormat, InlineStyle};
use crate::groups::use_groups;
use crate::i18n::use_i18n;
use crate::keys::t_with_shortcuts;
use crate::media_formats::{displayable_type, essence, is_svg, media_type_for, needs_app_thumbnail};
use crate::media_layout::{APP_THUMBNAIL_SIDE, thumbnail_type};
use crate::markdown;
use crate::mention::{self, TriggerKind};
use crate::model::{ConversationId, ThreadId};
use crate::model::{ConversationKind, MessageKind, User};
use crate::notifier::needs_channel_confirmation;
use crate::raster;
use crate::reactive::SetIfAlive;
use crate::settings::SendMode;
use crate::slash::{self, Parsed};
use crate::state::use_app_state;
use crate::viewport::is_touch_primary;

/// How long typing pauses before the draft is stored.
const DRAFT_SAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(800);

/// How many of the newest messages ↑ searches for the user's last one.
const EDIT_LAST_SCAN: usize = 50;
/// The newest messages that are certainly rendered while the feed follows the bottom.
const ON_SCREEN_ROWS: usize = 3;

/// `keyCode` of a key press the input method handles (all browsers report it while composing).
const IME_KEY_CODE: u32 = 229;

/// Inline styles the toolbar shows a pressed state for.
const INLINE_STYLES: [InlineStyle; 5] = [
  InlineStyle::Bold,
  InlineStyle::Italic,
  InlineStyle::Underline,
  InlineStyle::Strike,
  InlineStyle::Code,
];

/// A file chosen, dropped or pasted that is read into memory and waits for the send.
#[derive(Clone, Debug, PartialEq, Eq)]
struct PendingFile {
  name: String,
  media_type: String,
  bytes: Vec<u8>,
  /// Object URL of an image for the thumbnail in the chip; revoked when the file leaves the list.
  maybe_preview_url: Option<String>,
  /// A thumbnail the browser drew for a format the core can't decode (`OutgoingFile`).
  maybe_thumbnail: Option<Vec<u8>>,
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
///
/// The input is a rich-text editor ([`composer_editor`]): formatting shows as it will in the message
/// and is stored as the Markdown subset. Toolbar, shortcuts and typed Markdown all format the same way.
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
  let editor = NodeRef::<html::Div>::new();
  let file_input = NodeRef::<html::Input>::new();
  let media_input = NodeRef::<html::Input>::new();
  let camera_input = NodeRef::<html::Input>::new();
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
  let has_just_composed = StoredValue::new(false);
  let on_composition_end = move |_| {
    has_just_composed.set_value(true);
    // Cleared once the task that ended the composition is over.
    set_timeout(
      move || {
        has_just_composed.try_set_value(false);
      },
      std::time::Duration::ZERO,
    );
  };
  let show_formatting = Memo::new(move |_| state.settings.prefs.get().composer.show_formatting);
  let active_formats = RwSignal::new(ActiveFormats::default());
  // The editor's selection while a dialog, menu or picker has the focus.
  let saved_selection = StoredValue::new_local(None::<web_sys::Range>);
  // The link dialog: what was selected when it opened, and its fields.
  let is_link_open = RwSignal::new(false);
  let link_original = StoredValue::new(String::new());
  let link_label = RwSignal::new(String::new());
  let link_url = RwSignal::new(String::new());
  let is_link_invalid = RwSignal::new(false);
  let link_url_input = NodeRef::<html::Input>::new();
  let attach_menu = NodeRef::<html::Div>::new();
  let is_attach_menu_open = RwSignal::new(false);
  let is_send_menu_open = RwSignal::new(false);
  let priority = RwSignal::new(MessagePriority::Normal);
  let is_silent = RwSignal::new(false);
  let wants_acknowledgement = RwSignal::new(false);
  let editor_id = format!(
    "composer-{conversation}-{}",
    maybe_thread.map_or_else(String::new, |thread| thread.to_string())
  );
  let suggestions_id = StoredValue::new(format!("{editor_id}-suggestions"));
  let editor_id = StoredValue::new(editor_id);

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
    let Some(element) = editor.get() else { return };
    composer_editor::prepare();
    if let Some(text) = stored.filter(|text| !text.is_empty()) {
      is_draft_restored.set(true);
      if composer_editor::is_blank(&element) {
        composer_editor::set_markdown(&element, &text);
        is_empty.set(composer_editor::is_blank(&element));
      }
    }
  });
  // Reads the whole draft, so only after typing pauses.
  let save_draft = move || {
    draft_generation.update(|generation| *generation += 1);
    let generation = draft_generation.get_untracked();
    set_timeout(
      move || {
        // The composer may be gone by then (another channel opened); `try_*` then skips the save.
        if draft_generation.try_get_untracked() != Some(generation) {
          return;
        }
        let text = editor
          .try_get_untracked()
          .flatten()
          .map(|element| composer_editor::markdown(&element))
          .unwrap_or_default();
        state.try_write(|backend| backend.set_draft(conversation, maybe_thread, &text));
      },
      DRAFT_SAVE_DELAY,
    );
  };
  let refresh_formats = move || {
    if !show_formatting.get_untracked() {
      return;
    }
    let Some(element) = editor.get_untracked() else {
      return;
    };
    let mut formats = ActiveFormats {
      block: composer_editor::block_format(&element),
      ..ActiveFormats::default()
    };
    for style in INLINE_STYLES {
      formats.set(style, composer_editor::is_inline_active(&element, style));
    }
    active_formats.set(formats);
  };

  let clear_editor = move || {
    if let Some(element) = editor.get_untracked() {
      composer_editor::clear(&element);
    }
    suggestions.set(Vec::new());
    is_empty.set(true);
    active_formats.set(ActiveFormats::default());
    draft_generation.update(|generation| *generation += 1);
    state.write(|backend| backend.set_draft(conversation, maybe_thread, ""));
  };

  // A message with @channel in a large conversation waits here until the user confirms.
  let pending_confirm = RwSignal::new(None::<usize>);
  let send_checked = move |is_confirmed: bool| {
    let Some(element) = editor.get_untracked() else {
      return;
    };
    let mut text = composer_editor::markdown(&element);
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
            clear_editor();
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
        maybe_thumbnail: file.maybe_thumbnail,
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
        priority: priority.get_untracked(),
        wants_acknowledgement: wants_acknowledgement.get_untracked(),
        is_silent: is_silent.get_untracked(),
        also_send_to_channel: maybe_thread.is_some() && also_send_to_channel.get_untracked(),
        files,
      });
    });
    clear_editor();
    also_send_to_channel.set(false);
    priority.set(MessagePriority::Normal);
    is_silent.set(false);
    wants_acknowledgement.set(false);
    attachments.set(Vec::new());
    if maybe_thread.is_none() {
      state.jump_to_latest.update(|value| *value += 1);
    }
  };
  let send = move || send_checked(false);

  // The picker under the input: people and user groups after `@`, commands after a leading `/`. It
  // looks at the text node at the caret only.
  let groups = use_groups();
  let refresh_suggestions = move || {
    let Some(element) = editor.get_untracked() else {
      return;
    };
    let Some(caret) = composer_editor::caret_text(&element) else {
      suggestions.set(Vec::new());
      return;
    };
    // A command only at the very start of the message.
    let found = mention::trigger(&caret.before, caret.before.encode_utf16().count()).filter(|found| {
      found.kind == TriggerKind::Mention
        || (caret.window_start == 0 && composer_editor::is_first_text(&element, &caret.node))
    });
    let Some(found) = found else {
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
  let after_edit = move || {
    if let Some(element) = editor.get_untracked() {
      is_empty.set(composer_editor::is_blank(&element));
    }
    save_draft();
    refresh_formats();
  };
  let accept_suggestion = move |index: usize| {
    let Some(chosen) = suggestions.with_untracked(|list| list.get(index).cloned()) else {
      return;
    };
    let Some(element) = editor.get_untracked() else {
      return;
    };
    drop(element.focus());
    if let Some(caret) = composer_editor::caret_text(&element)
      && let Some(found) = mention::trigger(&caret.before, caret.before.encode_utf16().count())
    {
      let marker = match found.kind {
        TriggerKind::Mention => '@',
        TriggerKind::Command => '/',
      };
      let start = caret.window_start + found.start;
      composer_editor::replace_in_text(
        &caret.node,
        start,
        caret.offset,
        &format!("{marker}{} ", chosen.insert),
      );
      after_edit();
    }
    suggestions.set(Vec::new());
  };

  // Toolbar buttons and shortcuts. A button that took the focus gives it back with the selection.
  let focus_editor = move || {
    let element = editor.get_untracked()?;
    let is_focused = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|document| document.active_element())
      .is_some_and(|active| active == **element);
    if !is_focused {
      saved_selection.with_value(|range| composer_editor::restore_selection(&element, range.as_ref()));
    }
    Some(element)
  };
  let open_link_dialog = move || {
    let Some(element) = focus_editor() else {
      return;
    };
    saved_selection.set_value(composer_editor::save_selection(&element));
    let selected = composer_editor::selected_text();
    let maybe_url = markdown::is_allowed_link(selected.trim()).then(|| selected.trim().to_owned());
    link_original.set_value(selected.clone());
    link_label.set(if maybe_url.is_some() {
      String::new()
    } else {
      selected
    });
    link_url.set(maybe_url.unwrap_or_default());
    is_link_invalid.set(false);
    is_link_open.set(true);
    request_animation_frame(move || {
      if let Some(input) = link_url_input.try_get_untracked().flatten() {
        drop(input.focus());
      }
    });
  };
  let run_format = move |action: FormatAction| {
    let Some(element) = focus_editor() else {
      return;
    };
    match action {
      FormatAction::Inline(style) => composer_editor::toggle_inline(&element, style),
      FormatAction::Block(format) => composer_editor::set_block_format(&element, format),
      FormatAction::Link => {
        open_link_dialog();
        return;
      },
    }
    after_edit();
  };
  let save_link = move || {
    let url = with_scheme(link_url.get_untracked().trim());
    if !markdown::is_allowed_link(&url) {
      is_link_invalid.set(true);
      return;
    }
    is_link_open.set(false);
    let Some(element) = editor.get_untracked() else {
      return;
    };
    saved_selection.with_value(|range| composer_editor::restore_selection(&element, range.as_ref()));
    // An unchanged label keeps the selection's own formatting.
    let label = link_label.get_untracked();
    let keeps_selection = link_original.with_value(|original| !original.is_empty() && label == *original);
    composer_editor::insert_link(&url, if keeps_selection { "" } else { &label });
    after_edit();
  };
  let close_link_dialog = move || {
    is_link_open.set(false);
    if let Some(element) = editor.get_untracked() {
      saved_selection.with_value(|range| composer_editor::restore_selection(&element, range.as_ref()));
    }
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
    // An input method owns the key while composing. WebKit (Safari, the macOS app, iOS) ends the
    // composition before the Enter that confirms it arrives, with `isComposing` already false: that
    // Enter has key code 229, or comes in the same task as `compositionend`. It must not send.
    if ev.is_composing() || ev.key_code() == IME_KEY_CODE {
      return;
    }
    if has_just_composed.get_value() {
      if ev.key() == "Enter" {
        ev.prevent_default();
      }
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
    if let Some(action) = format_action_for(&ev) {
      ev.prevent_default();
      run_format(action);
      return;
    }
    let has_modifier = ev.shift_key() || ev.alt_key() || ev.ctrl_key() || ev.meta_key();
    if ev.key() == "ArrowUp"
      && !has_modifier
      && is_empty.get_untracked()
      && attachments.with_untracked(Vec::is_empty)
      && edit_last_message()
    {
      ev.prevent_default();
      return;
    }
    if ev.key() == "ArrowDown"
      && !has_modifier
      && let Some(element) = editor.get_untracked()
      && composer_editor::leave_last_block(&element)
    {
      ev.prevent_default();
      after_edit();
      return;
    }
    if ev.key() != "Enter" {
      return;
    }
    ev.prevent_default();
    let modifier = ev.ctrl_key() || ev.meta_key();
    // On touch screens Enter is a newline and the send button sends, as in the phones' messengers.
    let should_send = match state.settings.send_mode.get_untracked() {
      _ if is_touch_primary() => modifier,
      SendMode::Enter => !ev.shift_key() && !modifier,
      SendMode::CtrlEnter => modifier,
    };
    // Ctrl/Cmd+Enter in Enter-sends mode would otherwise do nothing; treat it as send as well.
    if should_send || modifier {
      send();
    } else if let Some(element) = editor.get_untracked() {
      composer_editor::new_line(&element);
      after_edit();
    }
  };

  let on_input = move |ev: web_sys::Event| {
    let Some(element) = editor.get_untracked() else {
      return;
    };
    // Typed Markdown formats as it's typed; pastes and autocorrect insert text as it is.
    if let Some(input) = ev.dyn_ref::<web_sys::InputEvent>()
      && input.input_type() == "insertText"
      && let Some(typed) = input.data().and_then(|data| data.chars().last())
    {
      composer_editor::apply_typing_shortcut(&element, typed.encode_utf8(&mut [0; 4]));
    }
    is_empty.set(composer_editor::is_blank(&element));
    save_draft();
    refresh_suggestions();
    refresh_formats();
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
            let media_type = media_type_for(&file.name(), &file.type_());
            // SVG, AVIF, HEIC: the browser draws the thumbnail the core in the page can't make.
            let maybe_thumbnail = match needs_app_thumbnail(&media_type) {
              true => raster::app_thumbnail(&bytes, &media_type, APP_THUMBNAIL_SIDE).await,
              false => None,
            };
            let maybe_preview_url = chip_preview_url(&file, &media_type, maybe_thumbnail.as_deref());
            let read = PendingFile {
              name: file.name(),
              media_type,
              bytes,
              maybe_preview_url,
              maybe_thumbnail,
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

  let take_files = move |input: NodeRef<html::Input>| {
    let Some(element) = input.get_untracked() else {
      return;
    };
    if let Some(files) = element.files() {
      add_files((0..files.length()).filter_map(|i| files.item(i)).collect());
    }
    element.set_value("");
  };
  let pick_files = move |input: NodeRef<html::Input>| {
    is_attach_menu_open.set(false);
    if let Some(element) = input.get_untracked() {
      let element: &web_sys::HtmlElement = element.unchecked_ref();
      element.click();
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
  // Files are attached; dropped text goes in as plain text, never as the HTML that came with it.
  let on_drop = move |ev: web_sys::DragEvent| {
    // Marks the drop as handled, so the conversation around doesn't attach the files a second time.
    ev.prevent_default();
    is_drag_over.set(false);
    let Some(transfer) = ev.data_transfer() else {
      return;
    };
    match transfer.files().filter(|files| files.length() > 0) {
      Some(files) => add_files((0..files.length()).filter_map(|i| files.item(i)).collect()),
      None => {
        if let (Ok(text), Some(element)) = (transfer.get_data("text/plain"), editor.get_untracked())
          && !text.is_empty()
        {
          drop(element.focus());
          composer_editor::insert_text(&text);
          after_edit();
        }
      },
    }
  };
  // Pasted images are attached. Rich text is reduced to the subset; a URL pasted over selected text
  // links it; in a code block everything is plain text.
  let on_paste = move |ev: web_sys::ClipboardEvent| {
    let Some(data) = ev.clipboard_data() else {
      return;
    };
    ev.prevent_default();
    if let Some(files) = data.files()
      && files.length() > 0
    {
      add_files((0..files.length()).filter_map(|i| files.item(i)).collect());
      return;
    }
    let Some(element) = editor.get_untracked() else {
      return;
    };
    let plain = data.get_data("text/plain").unwrap_or_default();
    let url = plain.trim();
    let has_selection = !composer_editor::selected_text().is_empty();
    if composer_editor::block_format(&element) == BlockFormat::CodeBlock {
      composer_editor::insert_text(&plain);
    } else if has_selection && markdown::is_allowed_link(url) {
      composer_editor::insert_link(url, "");
    } else {
      let maybe_html = data.get_data("text/html").ok();
      composer_editor::insert_blocks(&composer_editor::pasted_blocks(maybe_html.as_deref(), &plain));
    }
    after_edit();
    refresh_suggestions();
  };

  let maybe_emoji_anchor = RwSignal::new_local(None::<web_sys::Element>);
  // The picker takes the focus; the saved selection puts the emoji where the caret was.
  let insert_emoji = move |emoji: String| {
    maybe_emoji_anchor.set(None);
    let Some(element) = editor.get_untracked() else {
      return;
    };
    saved_selection.with_value(|range| composer_editor::restore_selection(&element, range.as_ref()));
    composer_editor::insert_text(&emoji);
    after_edit();
  };
  // `@` and `/` buttons type the marker and open the picker, as typing it would.
  let start_mention = move || {
    let Some(element) = focus_editor() else {
      return;
    };
    composer_editor::collapse_to_end();
    let needs_space = composer_editor::caret_text(&element)
      .is_some_and(|caret| caret.before.chars().last().is_some_and(|ch| !ch.is_whitespace()));
    composer_editor::insert_text(if needs_space { " @" } else { "@" });
    after_edit();
    refresh_suggestions();
  };
  let start_command = move || {
    let Some(element) = focus_editor() else {
      return;
    };
    if composer_editor::is_blank(&element) {
      composer_editor::insert_text("/");
      after_edit();
      refresh_suggestions();
    } else {
      notice.set(Some(Notice::Help));
    }
  };
  let send_flags = move || {
    let mut flags = Vec::new();
    match priority.get() {
      MessagePriority::Important => flags.push(i18n.t("priority-important")),
      MessagePriority::Urgent => flags.push(i18n.t("priority-urgent")),
      _ => {},
    }
    if is_silent.get() {
      flags.push(i18n.t("composer-flag-silent"));
    }
    if wants_acknowledgement.get() {
      flags.push(i18n.t("composer-flag-ack"));
    }
    flags
  };
  let is_coarse_pointer = web_sys::window()
    .and_then(|window| window.match_media("(pointer: coarse)").ok().flatten())
    .is_some_and(|query| query.matches());

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
          <ul class="suggestions" role="listbox" id=suggestions_id.get_value() aria-label=move || i18n.t("composer-suggestions")>
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
        <div class="composer-box">
          <Show when=move || show_formatting.get() fallback=|| ()>
            <FormatToolbar active=active_formats on_action=Callback::new(run_format) />
          </Show>
          {move || {
            is_link_open.get().then(|| {
              view! {
                <form
                  class="link-dialog"
                  novalidate
                  role="dialog"
                  aria-label=move || i18n.t("format-link")
                  on:submit=move |ev| {
                    ev.prevent_default();
                    save_link();
                  }
                  on:keydown=move |ev: web_sys::KeyboardEvent| {
                    if ev.key() == "Escape" {
                      ev.prevent_default();
                      close_link_dialog();
                    }
                  }
                >
                  <label>
                    <span>{move || i18n.t("composer-link-text")}</span>
                    <input
                      type="text"
                      class="input"
                      prop:value=move || link_label.get()
                      on:input=move |ev| link_label.set(event_target_value(&ev))
                    />
                  </label>
                  <label>
                    <span>{move || i18n.t("composer-link-url")}</span>
                    <input
                      type="text"
                      class="input"
                      autocomplete="url"
                      inputmode="url"
                      placeholder="https://"
                      node_ref=link_url_input
                      prop:value=move || link_url.get()
                      aria-invalid=move || is_link_invalid.get().to_string()
                      on:input=move |ev| {
                        link_url.set(event_target_value(&ev));
                        is_link_invalid.set(false);
                      }
                    />
                  </label>
                  {move || is_link_invalid.get().then(|| view! { <p class="composer-error small" role="alert">{move || i18n.t("composer-link-invalid")}</p> })}
                  <div class="link-dialog-actions">
                    <button type="button" class="btn btn-small" on:click=move |_| close_link_dialog()>{move || i18n.t("cancel")}</button>
                    <button type="submit" class="btn btn-small btn-primary">{move || i18n.t("save")}</button>
                  </div>
                </form>
              }
            })
          }}
          <div
            id=editor_id.get_value()
            class="composer-input composer-editor"
            class:is-empty=move || is_empty.get()
            contenteditable="true"
            role="textbox"
            aria-multiline="true"
            aria-label=move || i18n.t("composer-label")
            aria-autocomplete="list"
            aria-controls=suggestions_id.get_value()
            aria-expanded=move || (!suggestions.with(Vec::is_empty)).to_string()
            spellcheck="true"
            translate="no"
            data-placeholder=move || {
              let name = conversation_name.get_value();
              if name.is_empty() { i18n.t(placeholder_key) } else { i18n.t_with(placeholder_key, &[("name", &name)]) }
            }
            node_ref=editor
            on:keydown=on_keydown
            on:compositionend=on_composition_end
            on:input=on_input
            on:click=move |_| {
              refresh_suggestions();
              refresh_formats();
            }
            on:keyup=move |ev| {
              if matches!(ev.key().as_str(), "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown" | "Home" | "End") {
                refresh_suggestions();
                refresh_formats();
              }
            }
            on:focus=move |_| refresh_formats()
            on:blur=move |_| {
              if let Some(element) = editor.get_untracked() {
                saved_selection.set_value(composer_editor::save_selection(&element));
              }
            }
            on:paste=on_paste
            data-composer=if maybe_thread.is_some() { "thread" } else { "channel" }
          ></div>
          <div class="composer-toolbar">
            <input type="file" multiple hidden node_ref=file_input on:change=move |_| take_files(file_input) tabindex="-1" />
            <input type="file" multiple hidden accept="image/*,video/*" node_ref=media_input on:change=move |_| take_files(media_input) tabindex="-1" />
            <input type="file" hidden accept="image/*" capture="environment" node_ref=camera_input on:change=move |_| take_files(camera_input) tabindex="-1" />
            <div class="composer-menu-wrap">
              <button
                type="button"
                class="icon-btn composer-add"
                aria-label=move || i18n.t("composer-attach")
                title=move || i18n.t("composer-attach")
                aria-haspopup="menu"
                aria-expanded=move || is_attach_menu_open.get().to_string()
                on:click=move |_| {
                  is_attach_menu_open.update(|open| *open = !*open);
                  request_animation_frame(move || focus_first_item(attach_menu));
                }
              >
                <Icon name=IconName::Plus />
              </button>
              <Show when=move || is_attach_menu_open.get() fallback=|| ()>
                <div class="popover-backdrop" on:mousedown=move |_| is_attach_menu_open.set(false)></div>
                <div
                  class="popover composer-attach-menu"
                  role="menu"
                  node_ref=attach_menu
                  aria-label=move || i18n.t("composer-attach")
                  on:keydown=move |ev: web_sys::KeyboardEvent| match ev.key().as_str() {
                    "Escape" => {
                      ev.prevent_default();
                      is_attach_menu_open.set(false);
                    },
                    "ArrowDown" | "ArrowUp" => {
                      ev.prevent_default();
                      move_focus_vertical(&ev, attach_menu);
                    },
                    _ => {},
                  }
                >
                  <button type="button" class="menu-item" role="menuitem" on:click=move |_| pick_files(file_input)>
                    <Icon name=IconName::File />
                    <span class="menu-label">{move || i18n.t("composer-attach-file")}</span>
                  </button>
                  <button type="button" class="menu-item" role="menuitem" on:click=move |_| pick_files(media_input)>
                    <Icon name=IconName::Image />
                    <span class="menu-label">{move || i18n.t("composer-attach-media")}</span>
                  </button>
                  {is_coarse_pointer.then(|| view! {
                    <button type="button" class="menu-item" role="menuitem" on:click=move |_| pick_files(camera_input)>
                      <Icon name=IconName::Camera />
                      <span class="menu-label">{move || i18n.t("composer-attach-camera")}</span>
                    </button>
                  })}
                </div>
              </Show>
            </div>
            <button
              type="button"
              class="icon-btn composer-format-toggle"
              aria-pressed=move || show_formatting.get().to_string()
              aria-label=move || i18n.t("composer-format-toggle")
              title=move || i18n.t("composer-format-toggle")
              on:mousedown=|ev| ev.prevent_default()
              on:click=move |_| {
                state.settings.prefs.update(|prefs| prefs.composer.show_formatting = !prefs.composer.show_formatting);
                refresh_formats();
              }
            >
              <span aria-hidden="true">"Aa"</span>
            </button>
            <button
              type="button"
              class="icon-btn"
              aria-label=move || i18n.t("composer-emoji")
              title=move || i18n.t("composer-emoji")
              aria-haspopup="dialog"
              aria-expanded=move || maybe_emoji_anchor.with(Option::is_some).to_string()
              on:mousedown=|ev| ev.prevent_default()
              on:click=move |ev| {
                if let Some(element) = editor.get_untracked() {
                  saved_selection.set_value(composer_editor::save_selection(&element));
                }
                maybe_emoji_anchor.set(Some(event_target::<web_sys::Element>(&ev)));
              }
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
                        if let Some(element) = editor.get_untracked() {
                          saved_selection.with_value(|range| composer_editor::restore_selection(&element, range.as_ref()));
                        }
                      })
                    />
                  }
                })
            }}
            <button
              type="button"
              class="icon-btn"
              aria-label=move || i18n.t("composer-mention")
              title=move || i18n.t("composer-mention")
              on:mousedown=|ev| ev.prevent_default()
              on:click=move |_| start_mention()
            >
              <Icon name=IconName::AtSign />
            </button>
            <button
              type="button"
              class="icon-btn"
              aria-label=move || i18n.t("composer-commands")
              title=move || i18n.t("composer-commands")
              on:mousedown=|ev| ev.prevent_default()
              on:click=move |_| start_command()
            >
              <Icon name=IconName::Slash />
            </button>
            // A thread has no room for the hint; "Also send to" sits below the box instead.
            {maybe_thread.is_none().then(|| view! {
              <span class="composer-hint">
                {move || match state.settings.send_mode.get() {
                  SendMode::Enter => t_with_shortcuts(i18n, "composer-hint-enter"),
                  SendMode::CtrlEnter => t_with_shortcuts(i18n, "composer-hint-ctrl-enter"),
                }}
              </span>
            })}
            {maybe_thread.is_some().then(|| view! { <span class="grow"></span> })}
            <span class="send-flags" aria-live="polite">
              {move || send_flags().into_iter().map(|flag| view! { <span class="send-flag">{flag}</span> }).collect_view()}
            </span>
            <div class="send-group composer-menu-wrap">
              <button
                class="send-btn"
                disabled=move || is_empty.get() && attachments.with(Vec::is_empty)
                aria-label=move || i18n.t("composer-send")
                title=move || i18n.t("composer-send")
                on:click=move |_| send()
              >
                <Icon name=IconName::Send />
              </button>
              <button
                type="button"
                class="send-menu-btn"
                aria-label=move || i18n.t("composer-send-options")
                title=move || i18n.t("composer-send-options")
                aria-haspopup="menu"
                aria-expanded=move || is_send_menu_open.get().to_string()
                on:click=move |_| is_send_menu_open.update(|open| *open = !*open)
              >
                <Icon name=IconName::ChevronDown />
              </button>
              <Show when=move || is_send_menu_open.get() fallback=|| ()>
                <SendOptionsMenu
                  priority=priority
                  is_silent=is_silent
                  wants_acknowledgement=wants_acknowledgement
                  on_close=Callback::new(move |()| {
                    is_send_menu_open.set(false);
                    if let Some(element) = editor.get_untracked() {
                      saved_selection.with_value(|range| composer_editor::restore_selection(&element, range.as_ref()));
                    }
                  })
                />
              </Show>
            </div>
          </div>
        </div>
        // Slack and Mattermost: a thread reply can also go to the conversation.
        {maybe_thread.is_some().then(|| view! {
          <label class="also-send">
            <input
              type="checkbox"
              prop:checked=move || also_send_to_channel.get()
              on:change=move |ev| also_send_to_channel.set(event_target_checked(&ev))
            />
            <span>{move || i18n.t_with("composer-also-send", &[("name", &channel_name())])}</span>
          </label>
        })}
      </Show>
    </div>
  }
}

/// The chip's preview of a picked picture. An SVG shows its drawn thumbnail, never itself: a `blob:`
/// URL of an SVG would run its scripts if opened in a tab (see `crate::raster`).
fn chip_preview_url(
  file: &web_sys::File,
  media_type: &str,
  maybe_thumbnail: Option<&[u8]>,
) -> Option<String> {
  if let Some(thumbnail) = maybe_thumbnail {
    return blob_url(thumbnail, thumbnail_type(thumbnail));
  }
  // The blob URL of the picked file carries the file's own type: that one must be allowlisted.
  let is_shown_as_is = essence(media_type).starts_with("image/")
    && !is_svg(media_type)
    && displayable_type(&file.type_()).is_some_and(|shown| shown.starts_with("image/"));
  is_shown_as_is
    .then(|| web_sys::Url::create_object_url_with_blob(file).ok())
    .flatten()
}

fn focus_first_item(menu: NodeRef<html::Div>) {
  if let Some(item) = menu
    .try_get_untracked()
    .flatten()
    .and_then(|menu| menu.query_selector("[role=menuitem]").ok().flatten())
    .and_then(|item| item.dyn_into::<web_sys::HtmlElement>().ok())
  {
    drop(item.focus());
  }
}

/// `example.com` means `https://example.com`.
fn with_scheme(url: &str) -> String {
  if url.contains("://") || url.starts_with(markdown::PERMALINK_PREFIX) {
    url.to_owned()
  } else {
    format!("https://{url}")
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
      maybe_thumbnail: None,
    }
  }
}
