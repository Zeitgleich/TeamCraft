//! The workspace's sticker packs (issue #2132, admin.md §3.11): admins create packs, add pictures
//! with a name and an alt text, and rename, reorder and remove them. Each change is the pack's next
//! version, signed on this device; the members' apps use only packs an admin of the verified
//! roster signed. The pictures are stored unencrypted on the server, which the page says.

use leptos::html;
use leptos::prelude::*;
use teamcraft_client_api::custom_emoji::picture_error_key;
use teamcraft_proto::{
  StickerEntry, StickerPackChange, StickerPackKey, VerifiedStickerPack, check_custom_emoji_shortcode,
  check_sticker_alt, check_sticker_pack_name, inspect_custom_emoji_picture,
};
use wasm_bindgen_futures::JsFuture;

use crate::components::admin_console::SectionHeader;
use crate::components::icons::{Icon, IconName};
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::use_i18n;
use crate::reactive::SetIfAlive;
use crate::state::use_app_state;
use crate::stickers::StickerPicture;

const NEW_PACK_FORM: &str = "admin-sticker-pack-new";
const NEW_PACK_FIELD: &str = "admin-sticker-pack-name";

#[component]
pub fn StickersSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  crate::stickers::want_packs(state);
  let set = Memo::new(move |_| state.read(|backend| backend.sticker_packs()));
  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-stickers-title"))
      subtitle=Signal::derive(move || i18n.t("admin-stickers-subtitle"))
    />
    <p class="muted-text small">{move || i18n.t("admin-stickers-storage-note")}</p>
    <NewPackForm />
    <p class="muted-text small">
      {move || {
        let latest = set.get();
        i18n.t_with(
          "admin-stickers-pack-count",
          &[
            ("count", &latest.packs.len().to_string()),
            ("max", &latest.limits.max_packs.to_string()),
          ],
        )
      }}
    </p>
    <Show
      when=move || !set.with(|set| set.packs.is_empty())
      fallback=move || view! { <p class="muted-text">{move || i18n.t("admin-stickers-none")}</p> }
    >
      <For each=move || set.get().packs key=|pack| (pack.pack, pack.version) let:pack>
        <PackCard pack=pack />
      </For>
    </Show>
  }
}

/// Creates an empty pack under a name.
#[component]
fn NewPackForm() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let name = RwSignal::new(String::new());
  let errors = FormErrors::new();
  let submit = move |ev: web_sys::SubmitEvent| {
    ev.prevent_default();
    let typed = name.get_untracked().trim().to_owned();
    if check_sticker_pack_name(&typed).is_err() {
      let mut found = FieldErrors::new();
      found.push(NEW_PACK_FIELD, i18n.t("admin-stickers-error-pack-name"));
      errors.refuse(NEW_PACK_FORM, found);
      return;
    }
    errors.clear();
    state.write(|backend| backend.create_sticker_pack(&typed));
    name.set(String::new());
  };
  view! {
    <form id=NEW_PACK_FORM class="admin-upload admin-sticker-new" novalidate=true on:submit=submit>
      <div class="field">
        <label for=NEW_PACK_FIELD>{move || i18n.t("admin-stickers-pack-name")}</label>
        <input
          id=NEW_PACK_FIELD
          class="input"
          type="text"
          maxlength="64"
          autocomplete="off"
          aria-invalid=errors.invalid(NEW_PACK_FIELD)
          aria-describedby=errors.described(NEW_PACK_FIELD)
          prop:value=move || name.get()
          on:input=move |ev| {
            name.set(event_target_value(&ev));
            errors.clear_field(NEW_PACK_FIELD);
          }
        />
        <FieldError errors=errors field=NEW_PACK_FIELD />
      </div>
      <button class="btn btn-primary" type="submit">{move || i18n.t("admin-stickers-create")}</button>
    </form>
  }
}

/// One pack: its name (renamed in place), its stickers in order and the form to add one.
#[component]
fn PackCard(pack: VerifiedStickerPack) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let key = pack.pack;
  let name = pack.name.clone();
  let count = pack.stickers.len();
  let is_renaming = RwSignal::new(false);
  let is_confirming = RwSignal::new(false);
  let draft = RwSignal::new(pack.name.clone());
  let maybe_error = RwSignal::new(None::<String>);
  let rename_id = format!("admin-sticker-pack-{key}-rename");
  let rename = move || {
    let typed = draft.get_untracked().trim().to_owned();
    if check_sticker_pack_name(&typed).is_err() {
      maybe_error.set(Some(i18n.t("admin-stickers-error-pack-name")));
      return;
    }
    maybe_error.set(None);
    is_renaming.set(false);
    state.write(|backend| backend.change_sticker_pack(key, StickerPackChange::Rename { name: typed }));
  };
  let signer = state
    .read(|backend| backend.user(pack.signed_by))
    .map_or_else(String::new, |user| user.display_name);
  let last = count.saturating_sub(1);
  let confirm_name = name.clone();
  view! {
    <section class="policy-group admin-sticker-pack" aria-label=name.clone()>
      <div class="admin-sticker-pack-head">
        <Show
          when=move || is_renaming.get()
          fallback={
            let name = name.clone();
            move || view! { <h3>{name.clone()}</h3> }
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
              id=rename_id.clone()
              class="input"
              type="text"
              maxlength="64"
              autocomplete="off"
              aria-label=move || i18n.t("admin-stickers-pack-name")
              aria-invalid=move || maybe_error.with(Option::is_some).then_some("true")
              aria-describedby={
                let error_id = format!("{rename_id}-error");
                move || maybe_error.with(Option::is_some).then(|| error_id.clone())
              }
              prop:value=move || draft.get()
              on:input=move |ev| draft.set(event_target_value(&ev))
            />
            <button class="btn btn-small btn-primary" type="submit">{move || i18n.t("save")}</button>
            <button class="btn btn-small" type="button" on:click=move |_| is_renaming.set(false)>
              {move || i18n.t("cancel")}
            </button>
            {
              let error_id = format!("{rename_id}-error");
              move || {
                maybe_error
                  .get()
                  .map(|error| view! { <p class="field-error" id=error_id.clone() role="alert">{error}</p> })
              }
            }
          </form>
        </Show>
        <span class="muted-text small">
          {move || i18n.t_with("admin-emoji-signed-by", &[("name", &signer)])}
        </span>
        <Show when=move || !is_renaming.get() && !is_confirming.get() fallback=|| ()>
          <button class="btn btn-small" on:click=move |_| is_renaming.set(true)>
            {move || i18n.t("admin-emoji-rename")}
          </button>
          <button class="btn btn-danger btn-small" on:click=move |_| is_confirming.set(true)>
            {move || i18n.t("admin-stickers-delete-pack")}
          </button>
        </Show>
        <Show when=move || is_confirming.get() fallback=|| ()>
          <span class="small" role="status">
            {
              let label = confirm_name.clone();
              move || i18n.t_with("admin-stickers-delete-pack-confirm", &[("name", &label)])
            }
          </span>
          <button
            class="btn btn-danger btn-small"
            on:click=move |_| {
              is_confirming.set(false);
              state.write(|backend| backend.delete_sticker_pack(key));
            }
          >
            {move || i18n.t("admin-stickers-delete-pack")}
          </button>
          <button class="btn btn-small" on:click=move |_| is_confirming.set(false)>{move || i18n.t("cancel")}</button>
        </Show>
      </div>
      {(count == 0).then(|| view! { <p class="muted-text">{move || i18n.t("sticker-pack-empty")}</p> })}
      <ul class="admin-list admin-sticker-list">
        {pack
          .stickers
          .iter()
          .cloned()
          .enumerate()
          .map(|(index, sticker)| view! { <StickerRow pack=key sticker=sticker index=index last=last /> })
          .collect_view()}
      </ul>
      <AddStickerForm pack=pack />
    </section>
  }
}

/// One sticker: its picture, name and alt text, edited in place, moved up or down, removed.
#[component]
fn StickerRow(pack: StickerPackKey, sticker: StickerEntry, index: usize, last: usize) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_editing = RwSignal::new(false);
  let is_confirming = RwSignal::new(false);
  let name_draft = RwSignal::new(sticker.name.clone());
  let alt_draft = RwSignal::new(sticker.alt.clone());
  let errors = FormErrors::new();
  let base = format!("admin-sticker-{pack}-{index}");
  let (form_id, name_field, alt_field) = (
    format!("{base}-edit"),
    format!("{base}-name"),
    format!("{base}-alt"),
  );
  let name = sticker.name.clone();
  let change =
    move |change: StickerPackChange| state.write(|backend| backend.change_sticker_pack(pack, change));
  let save = {
    let (form_id, name_field, alt_field, name) = (
      form_id.clone(),
      name_field.clone(),
      alt_field.clone(),
      name.clone(),
    );
    move |ev: web_sys::SubmitEvent| {
      ev.prevent_default();
      let (new_name, alt) = (
        name_draft.get_untracked().trim().to_owned(),
        alt_draft.get_untracked().trim().to_owned(),
      );
      let mut found = FieldErrors::new();
      if check_custom_emoji_shortcode(&new_name).is_err() {
        found.push(&name_field, i18n.t("admin-emoji-error-name"));
      }
      if check_sticker_alt(&alt).is_err() {
        found.push(&alt_field, i18n.t("admin-stickers-error-alt"));
      }
      if !found.is_empty() {
        errors.refuse(&form_id, found);
        return;
      }
      errors.clear();
      is_editing.set(false);
      change(StickerPackChange::Edit {
        name: name.clone(),
        new_name,
        alt,
      });
    }
  };
  let (up_name, down_name, remove_name) = (name.clone(), name.clone(), name.clone());
  let label = sticker.alt.clone();
  view! {
    <li class="admin-row admin-sticker-row">
      <StickerPicture sha256=sticker.sha256 alt=sticker.alt.clone() class="admin-sticker-picture" />
      <Show
        when=move || is_editing.get()
        fallback={
          let (name, alt) = (sticker.name.clone(), sticker.alt.clone());
          move || view! {
            <span class="admin-sticker-text">
              <span class="admin-row-name" translate="no">{name.clone()}</span>
              <span class="muted-text small">{alt.clone()}</span>
            </span>
          }
        }
      >
        <form id=form_id.clone() class="admin-sticker-edit" novalidate=true on:submit=save.clone()>
          <div class="field">
            <label for=name_field.clone()>{move || i18n.t("admin-stickers-name")}</label>
            <input
              id=name_field.clone()
              class="input"
              type="text"
              maxlength="32"
              autocomplete="off"
              spellcheck="false"
              aria-invalid=errors.invalid(name_field.clone())
              aria-describedby=errors.described(name_field.clone())
              prop:value=move || name_draft.get()
              on:input=move |ev| name_draft.set(event_target_value(&ev))
            />
            <FieldError errors=errors field=name_field.clone() />
          </div>
          <div class="field">
            <label for=alt_field.clone()>{move || i18n.t("admin-stickers-alt")}</label>
            <input
              id=alt_field.clone()
              class="input"
              type="text"
              maxlength="200"
              autocomplete="off"
              aria-invalid=errors.invalid(alt_field.clone())
              aria-describedby=errors.described(alt_field.clone())
              prop:value=move || alt_draft.get()
              on:input=move |ev| alt_draft.set(event_target_value(&ev))
            />
            <FieldError errors=errors field=alt_field.clone() />
          </div>
          <button class="btn btn-small btn-primary" type="submit">{move || i18n.t("save")}</button>
          <button class="btn btn-small" type="button" on:click=move |_| is_editing.set(false)>
            {move || i18n.t("cancel")}
          </button>
        </form>
      </Show>
      <Show when=move || !is_editing.get() && !is_confirming.get() fallback=|| ()>
        <button
          class="icon-btn"
          disabled={index == 0}
          aria-label=move || i18n.t("admin-stickers-move-up")
          title=move || i18n.t("admin-stickers-move-up")
          on:click={
            let name = up_name.clone();
            move |_| change(StickerPackChange::Move { name: name.clone(), to: index.saturating_sub(1) })
          }
        >
          <Icon name=IconName::ArrowUp />
        </button>
        <button
          class="icon-btn"
          disabled={index >= last}
          aria-label=move || i18n.t("admin-stickers-move-down")
          title=move || i18n.t("admin-stickers-move-down")
          on:click={
            let name = down_name.clone();
            move |_| change(StickerPackChange::Move { name: name.clone(), to: index + 1 })
          }
        >
          <Icon name=IconName::ArrowDown />
        </button>
        <button class="btn btn-small" on:click=move |_| is_editing.set(true)>{move || i18n.t("admin-stickers-edit")}</button>
        <button class="btn btn-danger btn-small" on:click=move |_| is_confirming.set(true)>
          {move || i18n.t("admin-stickers-remove")}
        </button>
      </Show>
      <Show when=move || is_confirming.get() fallback=|| ()>
        <span class="small" role="status">
          {
            let label = label.clone();
            move || i18n.t_with("admin-stickers-remove-confirm", &[("name", &label)])
          }
        </span>
        <button
          class="btn btn-danger btn-small"
          on:click={
            let name = remove_name.clone();
            move |_| {
              is_confirming.set(false);
              change(StickerPackChange::Remove { name: name.clone() });
            }
          }
        >
          {move || i18n.t("admin-stickers-remove")}
        </button>
        <button class="btn btn-small" on:click=move |_| is_confirming.set(false)>{move || i18n.t("cancel")}</button>
      </Show>
    </li>
  }
}

/// Adds a sticker to a pack: a still PNG or WebP within the limits, a name and an alt text,
/// checked here as the server checks them.
#[component]
fn AddStickerForm(pack: VerifiedStickerPack) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let key = pack.pack;
  let base = format!("admin-sticker-{key}-add");
  let (form_id, name_field, alt_field, file_field) = (
    base.clone(),
    format!("{base}-name"),
    format!("{base}-alt"),
    format!("{base}-file"),
  );
  let name = RwSignal::new(String::new());
  let alt = RwSignal::new(String::new());
  let file_input = NodeRef::<html::Input>::new();
  let errors = FormErrors::new();
  let limits = Memo::new(move |_| state.read(|backend| backend.sticker_packs().limits));
  let taken: Vec<String> = pack.stickers.iter().map(|sticker| sticker.name.clone()).collect();
  let is_full = Memo::new(move |_| {
    state.read(|backend| {
      let set = backend.sticker_packs();
      set
        .pack(key)
        .is_some_and(|pack| pack.stickers.len() >= set.limits.max_stickers_per_pack as usize)
    })
  });
  let submit = {
    let (form_id, name_field, alt_field, file_field) = (
      form_id.clone(),
      name_field.clone(),
      alt_field.clone(),
      file_field.clone(),
    );
    move |ev: web_sys::SubmitEvent| {
      ev.prevent_default();
      let typed_name = name.get_untracked().trim().trim_matches(':').to_owned();
      let typed_alt = alt.get_untracked().trim().to_owned();
      let maybe_file = file_input
        .get_untracked()
        .and_then(|element| element.files())
        .and_then(|files| files.item(0));
      let mut found = FieldErrors::new();
      if check_custom_emoji_shortcode(&typed_name).is_err() {
        found.push(&name_field, i18n.t("admin-emoji-error-name"));
      } else if taken.contains(&typed_name) {
        found.push(&name_field, i18n.t("admin-stickers-error-taken"));
      }
      if check_sticker_alt(&typed_alt).is_err() {
        found.push(&alt_field, i18n.t("admin-stickers-error-alt"));
      }
      if maybe_file.is_none() {
        found.push(&file_field, i18n.t("admin-emoji-choose-file"));
      }
      let Some(file) = maybe_file.filter(|_| found.is_empty()) else {
        errors.refuse(&form_id, found);
        return;
      };
      errors.clear();
      let limits = limits.get_untracked();
      let (form_id, file_field) = (form_id.clone(), file_field.clone());
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
            found.push(&file_field, i18n.t(key));
            errors.refuse(&form_id, found);
            return;
          },
        };
        state.try_write(|backend| backend.add_sticker(key, &typed_name, &typed_alt, bytes));
        name.set_if_alive(String::new());
        alt.set_if_alive(String::new());
        if let Some(element) = file_input.try_get_untracked().flatten() {
          // async-ok: a DOM input element, not a signal.
          element.set_value("");
        }
      });
    }
  };
  view! {
    <form id=form_id.clone() class="admin-upload admin-sticker-add" novalidate=true on:submit=submit>
      <div class="field">
        <label for=file_field.clone()>{move || i18n.t("admin-emoji-file")}</label>
        <input
          id=file_field.clone()
          type="file"
          accept="image/png,image/webp"
          disabled=move || is_full.get()
          aria-invalid=errors.invalid(file_field.clone())
          aria-describedby=errors.described(file_field.clone())
          node_ref=file_input
          on:change={
            let file_field = file_field.clone();
            move |_| errors.clear_field(&file_field)
          }
        />
        <small class="muted-text">
          {move || {
            let limits = limits.get();
            i18n.t_with(
              "admin-stickers-file-hint",
              &[
                ("kb", &(limits.max_bytes / 1024).to_string()),
                ("px", &limits.max_side_px.to_string()),
              ],
            )
          }}
        </small>
        <FieldError errors=errors field=file_field.clone() />
      </div>
      <div class="field">
        <label for=name_field.clone()>{move || i18n.t("admin-stickers-name")}</label>
        <input
          id=name_field.clone()
          class="input"
          type="text"
          maxlength="34"
          autocomplete="off"
          spellcheck="false"
          disabled=move || is_full.get()
          aria-invalid=errors.invalid(name_field.clone())
          aria-describedby=errors.described(name_field.clone())
          prop:value=move || name.get()
          on:input={
            let name_field = name_field.clone();
            move |ev| {
              name.set(event_target_value(&ev));
              errors.clear_field(&name_field);
            }
          }
        />
        <FieldError errors=errors field=name_field.clone() />
      </div>
      <div class="field">
        <label for=alt_field.clone()>{move || i18n.t("admin-stickers-alt")}</label>
        <input
          id=alt_field.clone()
          class="input"
          type="text"
          maxlength="200"
          autocomplete="off"
          disabled=move || is_full.get()
          aria-invalid=errors.invalid(alt_field.clone())
          aria-describedby=errors.described(alt_field.clone())
          prop:value=move || alt.get()
          on:input={
            let alt_field = alt_field.clone();
            move |ev| {
              alt.set(event_target_value(&ev));
              errors.clear_field(&alt_field);
            }
          }
        />
        <small class="muted-text">{move || i18n.t("admin-stickers-alt-hint")}</small>
        <FieldError errors=errors field=alt_field.clone() />
      </div>
      <button class="btn btn-primary" type="submit" disabled=move || is_full.get()>
        {move || i18n.t("admin-stickers-add")}
      </button>
      {move || is_full.get().then(|| view! { <p class="muted-text small">{move || i18n.t("admin-stickers-pack-full")}</p> })}
    </form>
  }
}
