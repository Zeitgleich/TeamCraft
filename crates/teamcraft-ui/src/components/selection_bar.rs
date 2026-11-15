//! The bar of selection mode (issue #2070): how many messages are selected, and what to do with
//! them: forward, copy and delete, or cancel. It sits above the feed of the conversation the
//! selection belongs to.

use leptos::prelude::*;
use wasm_bindgen::JsCast;

use crate::channel_access::ChannelAccess;
use crate::clock::format_date_time;
use crate::components::icons::{Icon, IconName};
use crate::components::message_actions::copy_and_confirm;
use crate::i18n::use_i18n;
use crate::model::{ConversationId, Message};
use crate::selection::{DeleteAccess, copy_text, delete_access};
use crate::state::{Overlay, SidePanel, use_app_state};

/// The selection bar of `conversation`; nothing while selection mode is off or elsewhere.
#[component]
pub fn SelectionBar(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let locale = i18n.locale().tag();
  let access = ChannelAccess::new(state, conversation);
  let is_active = Memo::new(move |_| {
    state
      .selection
      .with(|selection| selection.is_active_in(conversation))
  });
  let count = Memo::new(move |_| state.selection.with(|selection| selection.len()));
  let is_confirming = RwSignal::new(false);
  let cancel_confirm = NodeRef::<leptos::html::Button>::new();

  // The selected messages, oldest first. Read when something is done with them, and for the delete
  // check once per change of the selection, not per revision.
  let selected = move || -> Vec<Message> {
    let feed = state
      .try_read(|backend| backend.messages(conversation, 0, usize::MAX))
      .unwrap_or_default();
    state
      .selection
      .with_untracked(|selection| selection.in_order(&feed).into_iter().cloned().collect())
  };
  let delete_state = Memo::new(move |_| {
    state.selection.with(|selection| selection.len());
    let messages = selected();
    let refs: Vec<&Message> = messages.iter().collect();
    delete_access(&refs, state.me().id, access.message_permissions().can_post)
  });

  // Whether to say why Delete is off: only with something selected.
  let is_delete_refused = Memo::new(move |_| delete_state.get() == DeleteAccess::Refused && count.get() > 0);

  // A message deleted meanwhile (by its author, or by an edit that removed it) drops out.
  Effect::new(move |_| {
    if !is_active.get() {
      return;
    }
    let ids: Vec<_> = state
      .selection
      .with(|selection| selection.ids().iter().copied().collect());
    let gone: Vec<_> = state
      .read(|backend| backend.message_extras(conversation, &ids))
      .into_iter()
      .filter(|extras| extras.is_deleted)
      .map(|extras| extras.message)
      .collect();
    if !gone.is_empty() {
      state
        .selection
        .update(|selection| selection.forget(|id| gone.contains(&id)));
    }
  });
  // Leaving selection mode closes a pending confirmation with it.
  Effect::new(move |_| {
    if !is_active.get() {
      is_confirming.set(false);
    }
  });
  // The confirmation takes the focus, on the safe choice.
  Effect::new(move |_| {
    if is_confirming.get()
      && let Some(button) = cancel_confirm.get()
    {
      drop(button.focus());
    }
  });

  // Esc leaves selection mode, unless it has another job (a dialog, a text field, a panel); the
  // confirmation goes first.
  let escape = window_event_listener(leptos::ev::keydown, move |ev| {
    if ev.key() != "Escape" || ev.default_prevented() || !is_active.get_untracked() {
      return;
    }
    let is_from_text_field = ev
      .target()
      .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
      .is_some_and(|element| {
        element
          .closest("input, textarea, select, [contenteditable]")
          .ok()
          .flatten()
          .is_some()
      });
    if is_from_text_field
      || state.overlay.get_untracked() != Overlay::None
      || state.panel.get_untracked() != SidePanel::Closed
    {
      return;
    }
    ev.prevent_default();
    if is_confirming.get_untracked() {
      is_confirming.set(false);
    } else {
      state.selection.update(|selection| selection.clear());
    }
  });
  on_cleanup(move || escape.remove());

  let forward = move |_| state.overlay.set(Overlay::ForwardSelection(conversation));
  let copy = move |_| {
    let messages = selected();
    let refs: Vec<&Message> = messages.iter().collect();
    let text = copy_text(
      &refs,
      |user| {
        state
          .read(|backend| backend.user(user))
          .map(|found| found.display_name)
          .unwrap_or_default()
      },
      |time| format_date_time(time, locale),
    );
    copy_and_confirm(text);
    state.selection.update(|selection| selection.clear());
  };
  let delete = move |_| {
    let ids: Vec<_> = selected().iter().map(|message| message.id).collect();
    let toast = i18n.t_count("selection-deleted-toast", ids.len());
    state.delete_many_with_undo(conversation, ids, toast);
    state.selection.update(|selection| selection.clear());
  };

  move || {
    is_active.get().then(|| {
      view! {
        <div class="selection-bar" role="toolbar" aria-label=move || i18n.t("selection-toolbar")>
          <Show
            when=move || is_confirming.get()
            fallback=move || {
              view! {
                <button type="button" class="btn btn-small" on:click=move |_| state.selection.update(|selection| selection.clear())>
                  {move || i18n.t("cancel")}
                </button>
                <span class="selection-count" role="status" aria-live="polite">
                  {move || i18n.t_count("selection-count", count.get())}
                </span>
                <Show when=move || is_delete_refused.get() fallback=|| ()>
                  <span class="selection-hint" id="selection-delete-hint">{move || i18n.t("selection-delete-refused")}</span>
                </Show>
                <span class="grow"></span>
                <button type="button" class="btn btn-small" prop:disabled=move || count.get() == 0 on:click=forward>
                  <Icon name=IconName::Forward />
                  {move || i18n.t("action-forward")}
                </button>
                <button type="button" class="btn btn-small" prop:disabled=move || count.get() == 0 on:click=copy>
                  <Icon name=IconName::Copy />
                  {move || i18n.t("selection-copy")}
                </button>
                <button
                  type="button"
                  class="btn btn-small btn-danger"
                  prop:disabled=move || delete_state.get() == DeleteAccess::Refused
                  aria-describedby=move || is_delete_refused.get().then_some("selection-delete-hint")
                  on:click=move |_| is_confirming.set(true)
                >
                  <Icon name=IconName::Trash />
                  {move || i18n.t("action-delete")}
                </button>
              }
            }
          >
            <span class="selection-count" role="alert">
              {move || i18n.t_count("selection-delete-confirm", count.get())}
            </span>
            <span class="grow"></span>
            <button type="button" class="btn btn-small" node_ref=cancel_confirm on:click=move |_| is_confirming.set(false)>
              {move || i18n.t("cancel")}
            </button>
            <button type="button" class="btn btn-small btn-danger" on:click=delete>
              {move || i18n.t("action-delete")}
            </button>
          </Show>
        </div>
      }
    })
  }
}
