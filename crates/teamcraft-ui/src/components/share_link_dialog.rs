//! The options and the list of the public download links of one file (e2ee.md §4.5.1): what a new
//! link allows (how long, how many downloads, an optional password) and the file's live links with
//! Copy and Revoke. The password is turned into a key wrap on this device; the server never sees it.

use leptos::prelude::*;
use teamcraft_client_api::actions::{ShareLink, ShareLinkOptions};
use teamcraft_proto::AttachmentId;

use crate::clock::{format_date_time, now_ms};
use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::copy_to_clipboard;
use crate::components::modal::Modal;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, MessageId};
use crate::state::use_app_state;

const MS_PER_HOUR: i64 = 3_600_000;
const MS_PER_DAY: i64 = 24 * MS_PER_HOUR;
/// How long a new link lasts when nothing else is chosen.
const DEFAULT_EXPIRY_MS: i64 = 7 * MS_PER_DAY;
/// The expiry choices: i18n key and length. The server caps them (`shares.max_expiry_days`) and
/// says so when a choice is above the cap.
const EXPIRIES: [(&str, i64); 4] = [
  ("share-expiry-hour", MS_PER_HOUR),
  ("share-expiry-day", MS_PER_DAY),
  ("share-expiry-week", 7 * MS_PER_DAY),
  ("share-expiry-month", 30 * MS_PER_DAY),
];
/// The download limits: `0` is "no limit".
const LIMITS: [u32; 4] = [0, 1, 5, 10];

#[component]
pub fn ShareLinkDialog(
  conversation: ConversationId,
  message: MessageId,
  file: AttachmentId,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let file_id = StoredValue::new(file.to_string());
  let expiry = RwSignal::new(DEFAULT_EXPIRY_MS);
  let limit = RwSignal::new(0_u32);
  let password = RwSignal::new(String::new());
  let links = Memo::new(move |_| {
    file_id.with_value(|file_id| state.read(|backend| backend.share_links(conversation, message, file_id)))
  });
  // The counts shown are the server's: ask once when the dialog opens.
  Effect::new(move |_| {
    file_id.with_value(|file_id| {
      state.write(|backend| backend.refresh_share_links(conversation, message, file_id))
    });
  });

  let create = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    let password_text = password.get_untracked();
    let options = ShareLinkOptions {
      maybe_expires_at: Some(now_ms().saturating_add(expiry.get_untracked())),
      maybe_max_downloads: Some(limit.get_untracked()).filter(|limit| *limit > 0),
      maybe_password: Some(password_text).filter(|password| !password.is_empty()),
    };
    file_id.with_value(|file_id| {
      state.write(|backend| backend.create_share_link(conversation, message, file_id, options));
    });
    password.set(String::new());
    state.show_brief_toast(i18n.t("share-created"));
  };
  let copy = move |link: &ShareLink| {
    copy_to_clipboard(&link.url);
    state.show_brief_toast(i18n.t("lightbox-share-copied"));
  };
  let revoke = move |share_id: String| {
    file_id.with_value(|file_id| {
      state.write(|backend| backend.revoke_share_link(conversation, message, file_id, &share_id));
    });
    state.show_brief_toast(i18n.t("share-revoked"));
  };

  view! {
    <Modal labelled_by="share-title" class="modal-small">
      <header class="modal-head">
        <h2 id="share-title">{move || i18n.t("share-title")}</h2>
        <button type="button" class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body share-dialog">
        <Show when=move || !links.with(Vec::is_empty) fallback=|| ()>
          <h3 class="share-heading">{move || i18n.t("share-active")}</h3>
          <ul class="share-links">
            <For each=move || links.get() key=|link| link.share_id.clone() let:link>
              {
                let for_copy = link.clone();
                let share_id = link.share_id.clone();
                let summary = {
                  let link = link.clone();
                  move || link_summary(&link, i18n)
                };
                view! {
                  <li class="share-link">
                    <span class="share-link-info">{summary}</span>
                    <button type="button" class="btn btn-small" on:click=move |_| copy(&for_copy)>
                      {move || i18n.t("share-copy")}
                    </button>
                    <button type="button" class="btn btn-small btn-danger" on:click=move |_| revoke(share_id.clone())>
                      {move || i18n.t("share-revoke")}
                    </button>
                  </li>
                }
              }
            </For>
          </ul>
        </Show>
        <form class="share-form" on:submit=create>
          <h3 class="share-heading">{move || i18n.t("share-new")}</h3>
          <label for="share-expiry">{move || i18n.t("share-expiry")}</label>
          <select
            id="share-expiry"
            class="input"
            data-autofocus
            on:change=move |ev| expiry.set(event_target_value(&ev).parse().unwrap_or(DEFAULT_EXPIRY_MS))
          >
            {EXPIRIES
              .into_iter()
              .map(|(key, ms)| view! {
                <option value=ms.to_string() selected=ms == DEFAULT_EXPIRY_MS>{move || i18n.t(key)}</option>
              })
              .collect_view()}
          </select>
          <label for="share-limit">{move || i18n.t("share-limit")}</label>
          <select
            id="share-limit"
            class="input"
            on:change=move |ev| limit.set(event_target_value(&ev).parse().unwrap_or(0))
          >
            {LIMITS
              .into_iter()
              .map(|count| view! {
                <option value=count.to_string()>
                  {move || if count == 0 { i18n.t("share-limit-none") } else { count.to_string() }}
                </option>
              })
              .collect_view()}
          </select>
          <label for="share-password">{move || i18n.t("share-password")}</label>
          <input
            id="share-password"
            class="input"
            type="password"
            autocomplete="new-password"
            prop:value=move || password.get()
            on:input=move |ev| password.set(event_target_value(&ev))
          />
          <p class="muted-text small">{move || i18n.t("share-password-hint")}</p>
          <div class="share-actions">
            <button type="submit" class="btn btn-primary">{move || i18n.t("share-create")}</button>
          </div>
        </form>
      </div>
      <footer class="modal-foot">
        <span class="grow"></span>
        <button type="button" class="btn" on:click=move |_| state.close_overlay()>{move || i18n.t("close")}</button>
      </footer>
    </Modal>
  }
}

/// "Expires … · 2 of 5 downloads · password": what the link allows and how much is used.
fn link_summary(link: &ShareLink, i18n: crate::i18n::I18n) -> String {
  let locale = i18n.locale().tag();
  let mut parts = Vec::new();
  if let Some(expires_at) = link.maybe_expires_at {
    parts.push(i18n.t_with(
      "share-expires",
      &[("when", &format_date_time(expires_at, locale))],
    ));
  }
  parts.push(match link.maybe_max_downloads {
    Some(max) => i18n.t_with(
      "share-downloads-limited",
      &[
        ("used", &link.download_count.to_string()),
        ("max", &max.to_string()),
      ],
    ),
    None => i18n.t_with("share-downloads", &[("used", &link.download_count.to_string())]),
  });
  if link.has_password {
    parts.push(i18n.t("share-has-password"));
  }
  parts.join(" · ")
}
