//! The options and the list of the public download links of one file (e2ee.md §4.5.1): what a new
//! link allows (how long, how many downloads, an optional password) and the file's live links with
//! Copy and Revoke. The password is turned into a key wrap on this device; the server never sees it.

use leptos::prelude::*;
use teamcraft_client_api::actions::{MIN_SHARE_PASSWORD_CHARS, ShareLink, ShareLinkOptions};
use teamcraft_proto::AttachmentId;

use crate::clock::format_date_time;
use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::copy_to_clipboard;
use crate::components::modal::Modal;
use crate::components::pending_button::PendingButton;
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, MessageId};
use crate::state::use_app_state;

const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;
/// How long a new link lasts when nothing else is chosen.
const DEFAULT_EXPIRY_SECONDS: u64 = 7 * SECONDS_PER_DAY;
/// The expiry choices: i18n key and length. The server counts from its own clock and shortens a
/// choice above the instance maximum; the list shows when the link really ends.
const EXPIRIES: [(&str, u64); 4] = [
  ("share-expiry-hour", SECONDS_PER_HOUR),
  ("share-expiry-day", SECONDS_PER_DAY),
  ("share-expiry-week", 7 * SECONDS_PER_DAY),
  ("share-expiry-month", 30 * SECONDS_PER_DAY),
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
  let expiry = RwSignal::new(DEFAULT_EXPIRY_SECONDS);
  let creating = PendingAction::new();
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

  let is_password_short =
    move || password.with(|text| !text.is_empty() && text.chars().count() < MIN_SHARE_PASSWORD_CHARS);
  let create = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    if is_password_short() {
      return;
    }
    let options = ShareLinkOptions {
      maybe_expires_in_seconds: Some(expiry.get_untracked()),
      maybe_max_downloads: Some(limit.get_untracked()).filter(|limit| *limit > 0),
      maybe_password: Some(password.get_untracked()).filter(|password| !password.is_empty()),
    };
    creating.run_and_then(
      move || {
        file_id.with_value(|file_id| {
          state.write(|backend| backend.create_share_link(conversation, message, file_id, options));
        });
      },
      move || {
        // Shown once the core created the link; a failure was reported by the backend. The text of
        // the password is replaced (the core wipes its copy; a signal's string cannot be wiped).
        password.try_set(String::new());
        state.show_brief_toast(i18n.t("share-created"));
      },
    );
  };
  let copy = move |link: &ShareLink| {
    copy_to_clipboard(&link.url);
    state.show_brief_toast(i18n.t("lightbox-share-copied"));
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
                let revoking = PendingAction::new();
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
                    <PendingButton
                      pending=revoking
                      class="btn btn-small btn-danger"
                      on_click=move |_| {
                        let share_id = share_id.clone();
                        revoking.run_and_then(
                          move || {
                            file_id.with_value(|file_id| {
                              state.write(|backend| backend.revoke_share_link(conversation, message, file_id, &share_id));
                            });
                          },
                          move || state.show_brief_toast(i18n.t("share-revoked")),
                        );
                      }
                    >
                      {move || i18n.t("share-revoke")}
                    </PendingButton>
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
            on:change=move |ev| expiry.set(event_target_value(&ev).parse().unwrap_or(DEFAULT_EXPIRY_SECONDS))
          >
            {EXPIRIES
              .into_iter()
              .map(|(key, seconds)| view! {
                <option value=seconds.to_string() selected=seconds == DEFAULT_EXPIRY_SECONDS>{move || i18n.t(key)}</option>
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
            aria-describedby="share-password-hint"
            aria-invalid=move || is_password_short().to_string()
            prop:value=move || password.get()
            on:input=move |ev| password.set(event_target_value(&ev))
          />
          <p id="share-password-hint" class="muted-text small" class:warn-text=is_password_short>
            {move || i18n.t_with("share-password-hint", &[("count", &MIN_SHARE_PASSWORD_CHARS.to_string())])}
          </p>
          <div class="share-actions">
            <button
              type="submit"
              class="btn btn-primary"
              prop:disabled=move || creating.is_pending() || is_password_short()
              aria-busy=move || creating.is_pending().then_some("true")
            >
              {move || i18n.t("share-create")}
            </button>
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
