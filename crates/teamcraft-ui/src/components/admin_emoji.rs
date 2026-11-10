//! The workspace's custom emoji (issue #207, admin.md §3.11): admins add pictures under a
//! `:shortcode:`, rename and delete them. Each change is signed on this device; the members' apps
//! show only emoji an admin of the verified roster signed. The pictures are stored unencrypted on
//! the server, like the organization's call backgrounds, which the page says.

use leptos::html;
use leptos::prelude::*;
use teamcraft_client_api::custom_emoji::{CustomEmoji, NewShortcodeError, check_new_shortcode, picture_error_key};
use teamcraft_proto::inspect_custom_emoji_picture;
use wasm_bindgen_futures::JsFuture;

use crate::components::admin_console::SectionHeader;
use crate::custom_emoji::CustomEmojiGlyph;
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::use_i18n;
use crate::reactive::SetIfAlive;
use crate::state::use_app_state;

const FORM_ID: &str = "admin-emoji-add";
const NAME_FIELD: &str = "admin-emoji-name";
const FILE_FIELD: &str = "admin-emoji-file";

#[component]
pub fn EmojiSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let set = Memo::new(move |_| state.read(|backend| backend.custom_emoji()));
  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-emoji-title"))
      subtitle=Signal::derive(move || i18n.t("admin-emoji-subtitle"))
    />
    <p class="muted-text small">{move || i18n.t("admin-emoji-storage-note")}</p>
    <AddForm />
    <section class="policy-group" aria-label=move || i18n.t("admin-emoji-list")>
      <h3>
        {move || {
          let latest = set.get();
          i18n.t_with(
            "admin-emoji-count",
            &[
              ("count", &latest.emoji.len().to_string()),
              ("max", &latest.limits.max_count.to_string()),
            ],
          )
        }}
      </h3>
      <Show
        when=move || !set.with(|set| set.emoji.is_empty())
        fallback=move || view! { <p class="muted-text">{move || i18n.t("admin-emoji-none")}</p> }
      >
        <ul class="admin-list admin-emoji-list">
          <For each=move || set.get().emoji key=|emoji| (emoji.id, emoji.shortcode.clone()) let:emoji>
            <EmojiRow emoji=emoji />
          </For>
        </ul>
      </Show>
    </section>
  }
}

/// One emoji: its picture and name, who signed it, rename and delete (asked once more).
#[component]
fn EmojiRow(emoji: CustomEmoji) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = emoji.id;
  let reference = emoji.reference();
  let text = emoji.text();
  let signer = state
    .read(|backend| backend.user(emoji.signed_by))
    .map_or_else(String::new, |user| user.display_name);
  let is_renaming = RwSignal::new(false);
  let is_confirming = RwSignal::new(false);
  let draft = RwSignal::new(emoji.shortcode.clone());
  let maybe_error = RwSignal::new(None::<String>);
  let field_id = format!("admin-emoji-rename-{id}");
  let error_dom_id = format!("{field_id}-error");
  let rename = move || {
    let shortcode = draft.get_untracked().trim().to_owned();
    let checked = state.read(|backend| check_new_shortcode(&backend.custom_emoji(), &shortcode, Some(id)));
    match checked {
      Ok(()) => {
        state.write(|backend| backend.rename_custom_emoji(id, &shortcode));
        maybe_error.set(None);
        is_renaming.set(false);
      },
      Err(error) => maybe_error.set(Some(i18n.t(shortcode_error_key(error)))),
    }
  };
  let delete_label = text.clone();
  view! {
    <li class="admin-row admin-emoji-row">
      <span class="admin-emoji-picture"><CustomEmojiGlyph reference=reference /></span>
      <Show
        when=move || is_renaming.get()
        fallback={
          let text = text.clone();
          move || view! { <span class="admin-row-name" translate="no">{text.clone()}</span> }
        }
      >
        <form
          class="admin-emoji-rename"
          on:submit=move |ev: web_sys::SubmitEvent| {
            ev.prevent_default();
            rename();
          }
        >
          <input
            id=field_id.clone()
            class="input"
            type="text"
            maxlength="32"
            autocomplete="off"
            spellcheck="false"
            aria-label=move || i18n.t("admin-emoji-name")
            aria-invalid=move || maybe_error.with(Option::is_some).then_some("true")
            aria-describedby=move || maybe_error.with(Option::is_some).then(|| error_dom_id.clone())
            prop:value=move || draft.get()
            on:input=move |ev| draft.set(event_target_value(&ev))
          />
          <button class="btn btn-small btn-primary" type="submit">{move || i18n.t("save")}</button>
          <button class="btn btn-small" type="button" on:click=move |_| is_renaming.set(false)>
            {move || i18n.t("cancel")}
          </button>
          {
            let error_dom_id = format!("admin-emoji-rename-{id}-error");
            move || {
              maybe_error
                .get()
                .map(|error| view! { <p class="field-error" id=error_dom_id.clone() role="alert">{error}</p> })
            }
          }
        </form>
      </Show>
      <span class="muted-text small">{move || i18n.t_with("admin-emoji-signed-by", &[("name", &signer)])}</span>
      <Show when=move || !is_renaming.get() && !is_confirming.get() fallback=|| ()>
        <button class="btn btn-small" on:click=move |_| is_renaming.set(true)>
          {move || i18n.t("admin-emoji-rename")}
        </button>
        <button class="btn btn-danger btn-small" on:click=move |_| is_confirming.set(true)>
          {move || i18n.t("admin-emoji-delete")}
        </button>
      </Show>
      <Show when=move || is_confirming.get() fallback=|| ()>
        <span class="small" role="status">
          {
            let label = delete_label.clone();
            move || i18n.t_with("admin-emoji-delete-confirm", &[("name", &label)])
          }
        </span>
        <button
          class="btn btn-danger btn-small"
          on:click=move |_| {
            is_confirming.set(false);
            state.write(|backend| backend.delete_custom_emoji(id));
          }
        >
          {move || i18n.t("admin-emoji-delete")}
        </button>
        <button class="btn btn-small" on:click=move |_| is_confirming.set(false)>{move || i18n.t("cancel")}</button>
      </Show>
    </li>
  }
}

/// Adds an emoji: a name and a still PNG or WebP picture within the limits, checked here as the
/// server checks them.
#[component]
fn AddForm() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let name = RwSignal::new(String::new());
  let file_input = NodeRef::<html::Input>::new();
  let errors = FormErrors::new();
  let limits = Memo::new(move |_| state.read(|backend| backend.custom_emoji().limits));
  let submit = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    let shortcode = name.get_untracked().trim().trim_matches(':').to_owned();
    let maybe_file = file_input
      .get_untracked()
      .and_then(|element| element.files())
      .and_then(|files| files.item(0));
    let mut found = FieldErrors::new();
    let set = state.read(|backend| backend.custom_emoji());
    if let Err(error) = check_new_shortcode(&set, &shortcode, None) {
      found.push(NAME_FIELD, i18n.t(shortcode_error_key(error)));
    }
    if maybe_file.is_none() {
      found.push(FILE_FIELD, i18n.t("admin-emoji-choose-file"));
    }
    let Some(file) = maybe_file.filter(|_| found.is_empty()) else {
      errors.refuse(FORM_ID, found);
      return;
    };
    errors.clear();
    let limits = set.limits;
    leptos::task::spawn_local(async move {
      let read = JsFuture::from(file.array_buffer()).await;
      // The form may be gone by now; its errors with it.
      if name.try_get_untracked().is_none() {
        return;
      }
      let maybe_bytes = read.ok().map(|buffer| js_sys::Uint8Array::new(&buffer).to_vec());
      let checked = maybe_bytes.ok_or("admin-emoji-error-format").and_then(|bytes| {
        inspect_custom_emoji_picture(&bytes, limits.max_bytes, limits.max_side_px)
          .map(|_| bytes)
          .map_err(picture_error_key)
      });
      let bytes = match checked {
        Ok(bytes) => bytes,
        Err(key) => {
          let mut found = FieldErrors::new();
          found.push(FILE_FIELD, i18n.t(key));
          errors.refuse(FORM_ID, found);
          return;
        },
      };
      state.try_write(|backend| backend.add_custom_emoji(&shortcode, bytes));
      name.set_if_alive(String::new());
      if let Some(element) = file_input.try_get_untracked().flatten() {
        // async-ok: a DOM input element, not a signal.
        element.set_value("");
      }
    });
  };
  // Picking a file names the emoji after it when no name was typed.
  let on_file = move |_| {
    let maybe_file_name = file_input
      .get_untracked()
      .and_then(|element| element.files())
      .and_then(|files| files.item(0))
      .map(|file| file.name());
    if let Some(file_name) = maybe_file_name
      && name.get_untracked().trim().is_empty()
    {
      name.set(suggested_shortcode(&file_name));
    }
    errors.clear_field(FILE_FIELD);
  };
  view! {
    <form id=FORM_ID class="admin-upload admin-emoji-add" novalidate=true on:submit=submit>
      <div class="field">
        <label for=NAME_FIELD>{move || i18n.t("admin-emoji-name")}</label>
        <input
          id=NAME_FIELD
          class="input"
          type="text"
          maxlength="34"
          autocomplete="off"
          spellcheck="false"
          placeholder="ship_it"
          aria-invalid=errors.invalid(NAME_FIELD)
          aria-describedby=errors.described(NAME_FIELD)
          prop:value=move || name.get()
          on:input=move |ev| {
            name.set(event_target_value(&ev));
            errors.clear_field(NAME_FIELD);
          }
        />
        <FieldError errors=errors field=NAME_FIELD />
      </div>
      <div class="field">
        <label for=FILE_FIELD>{move || i18n.t("admin-emoji-file")}</label>
        <input
          id=FILE_FIELD
          type="file"
          accept="image/png,image/webp"
          aria-invalid=errors.invalid(FILE_FIELD)
          aria-describedby=errors.described(FILE_FIELD)
          node_ref=file_input
          on:change=on_file
        />
        <small class="muted-text">
          {move || {
            let limits = limits.get();
            i18n.t_with(
              "admin-emoji-file-hint",
              &[
                ("kb", &(limits.max_bytes / 1024).to_string()),
                ("px", &limits.max_side_px.to_string()),
              ],
            )
          }}
        </small>
        <FieldError errors=errors field=FILE_FIELD />
      </div>
      <button class="btn btn-primary" type="submit">{move || i18n.t("admin-emoji-add")}</button>
    </form>
  }
}

fn shortcode_error_key(error: NewShortcodeError) -> &'static str {
  match error {
    NewShortcodeError::Invalid => "admin-emoji-error-name",
    NewShortcodeError::BuiltIn => "admin-emoji-error-built-in",
    NewShortcodeError::Taken => "admin-emoji-error-taken",
  }
}

/// A shortcode from a file name: `Ship It!.png` becomes `ship_it`.
fn suggested_shortcode(file_name: &str) -> String {
  let stem = file_name.rsplit_once('.').map_or(file_name, |(stem, _)| stem);
  let mut shortcode = String::new();
  for character in stem.chars().flat_map(char::to_lowercase) {
    match character {
      'a'..='z' | '0'..='9' | '-' => shortcode.push(character),
      _ if !shortcode.ends_with('_') && !shortcode.is_empty() => shortcode.push('_'),
      _ => {},
    }
  }
  shortcode.trim_end_matches('_').chars().take(32).collect()
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn file_names_become_shortcodes() {
    assert_eq!(suggested_shortcode("Ship It!.png"), "ship_it");
    assert_eq!(suggested_shortcode("team-logo.webp"), "team-logo");
    assert_eq!(suggested_shortcode("__x__.png"), "x");
    assert_eq!(suggested_shortcode(&format!("{}.png", "a".repeat(40))).len(), 32);
  }
}
