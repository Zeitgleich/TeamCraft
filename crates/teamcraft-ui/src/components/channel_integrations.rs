use std::collections::HashSet;

use leptos::prelude::*;
use teamcraft_client_api::actions::ChannelChanges;
use teamcraft_proto::{HistoryPolicy, Permission};

use crate::channel_access::ChannelAccess;
use crate::components::admin_integrations::CopyButton;
use crate::components::icons::{Icon, IconName};
use crate::components::workspace_dialogs::FailureNote;
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::integrations::{ingest_url, new_source_token, source_block, webhook_url};
use crate::model::{ConversationId, ConversationKind, User};
use crate::state::use_app_state;

/// How many bots the search lists at most.
const MAX_BOT_RESULTS: usize = 8;

/// The "Integrations" section of the channel settings (docs/bridge.md): normal or log mode, the
/// bots in the channel with adding and removing, and what to point a source at. A bot is a normal
/// member (a guest of the space), so any channel can have one; adding it commits its device into
/// the channel's encryption group like adding a person.
#[component]
pub fn ChannelIntegrations(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let access = ChannelAccess::new(state, conversation);
  let can_manage = move || access.can(&Permission::ManageChannelProperties);
  let info = Memo::new(move |_| state.read(|backend| backend.conversation(conversation)));
  let kind = Memo::new(move |_| info.get().map(|info| info.kind));
  let is_log = move || kind.get() == Some(ConversationKind::Integration);
  let is_public = move || kind.get() == Some(ConversationKind::Public);
  let mode_pending = PendingAction::new();
  let set_mode = move |is_integration: bool| {
    if is_log() == is_integration {
      return;
    }
    mode_pending.run(|| {
      state.write(|backend| {
        backend.update_channel(
          conversation,
          ChannelChanges {
            maybe_is_integration: Some(is_integration),
            ..ChannelChanges::default()
          },
        )
      })
    });
  };

  view! {
    <h3 class="detail-heading">{move || i18n.t("channel-mode")}</h3>
    <div class="segmented" role="radiogroup" aria-label=move || i18n.t("channel-mode")>
      <button
        type="button"
        role="radio"
        aria-checked=move || (!is_log()).to_string()
        aria-pressed=move || (!is_log()).to_string()
        prop:disabled=move || !can_manage() || mode_pending.is_pending()
        on:click=move |_| set_mode(false)
      >
        {move || i18n.t("channel-mode-normal")}
      </button>
      <button
        type="button"
        role="radio"
        aria-checked=move || is_log().to_string()
        aria-pressed=move || is_log().to_string()
        prop:disabled=move || !can_manage() || is_public() || mode_pending.is_pending()
        on:click=move |_| set_mode(true)
      >
        {move || i18n.t("channel-mode-log")}
      </button>
    </div>
    <p class="hint">
      {move || {
        if is_public() {
          i18n.t("channel-mode-public-hint")
        } else if is_log() {
          i18n.t("channel-mode-log-hint")
        } else {
          i18n.t("channel-mode-normal-hint")
        }
      }}
    </p>
    <FailureNote pending=mode_pending />
    <BotMembers conversation=conversation can_manage=Signal::derive(can_manage) />
    <SourceHints conversation=conversation />
  }
}

/// The bots in the channel and a search for more.
#[component]
fn BotMembers(conversation: ConversationId, can_manage: Signal<bool>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let query = RwSignal::new(String::new());
  let member_ids = Memo::new(move |_| {
    state
      .read(|backend| backend.conversation(conversation))
      .map(|info| info.member_ids.into_iter().collect::<HashSet<_>>())
      .unwrap_or_default()
  });
  let bots_in = Memo::new(move |_| {
    let ids = member_ids.get();
    let mut bots: Vec<User> = state.read(|backend| {
      ids
        .iter()
        .filter_map(|id| backend.user(*id))
        .filter(|user| user.is_bot)
        .collect()
    });
    bots.sort_by_key(|bot| bot.display_name.to_lowercase());
    bots
  });
  let candidates = Memo::new(move |_| {
    let ids = member_ids.get();
    let text = query.get();
    state.read(|backend| {
      backend
        .search_people(text.trim())
        .into_iter()
        .filter(|user| user.is_bot && !ids.contains(&user.id))
        .take(MAX_BOT_RESULTS)
        .collect::<Vec<_>>()
    })
  });
  // The space the channel is in decides whether the bot may be put in it first (as a guest).
  let joins_space = state
    .read(|backend| backend.conversation(conversation))
    .and_then(|info| info.maybe_space)
    .and_then(|id| {
      state
        .read(|backend| backend.spaces())
        .into_iter()
        .find(|space| space.id == id)
    })
    .is_some_and(|space| space.can_add_members || space.can_invite_guests);

  view! {
    <h3 class="detail-heading">{move || i18n.t("channel-bots")}</h3>
    <ul class="settings-member-list">
      <For each=move || bots_in.get() key=|bot| bot.id let:bot>
        <BotMemberRow conversation=conversation bot=bot can_manage=can_manage />
      </For>
    </ul>
    <Show when=move || bots_in.with(Vec::is_empty) fallback=|| ()>
      <p class="muted-text">{move || i18n.t("channel-bots-none")}</p>
    </Show>
    <Show when=move || can_manage.get() fallback=|| ()>
      <label for="channel-bot-search">{move || i18n.t("channel-bots-add")}</label>
      <input
        id="channel-bot-search"
        class="input"
        type="search"
        placeholder=move || i18n.t("channel-bots-search")
        prop:value=move || query.get()
        on:input=move |ev| query.set(event_target_value(&ev))
      />
      <ul class="settings-member-list">
        <For each=move || candidates.get() key=|bot| bot.id let:bot>
          <BotCandidateRow conversation=conversation bot=bot joins_space=joins_space />
        </For>
      </ul>
      <p class="hint">{move || i18n.t("channel-bots-hint")}</p>
    </Show>
  }
}

#[component]
fn BotMemberRow(conversation: ConversationId, bot: User, can_manage: Signal<bool>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = bot.id;
  let is_confirming = RwSignal::new(false);
  let pending = PendingAction::new();
  view! {
    <li class="settings-member">
      <span class="grow" translate="no">{bot.display_name.clone()}</span>
      <span class="badge">{move || i18n.t("badge-bot")}</span>
      <Show when=move || can_manage.get() fallback=|| ()>
        <Show
          when=move || is_confirming.get()
          fallback=move || {
            view! {
              <button type="button" class="btn btn-small btn-quiet" on:click=move |_| is_confirming.set(true)>
                {move || i18n.t("channel-bots-remove")}
              </button>
            }
          }
        >
          <button
            type="button"
            class="btn btn-small btn-danger"
            prop:disabled=move || pending.is_pending()
            on:click=move |_| pending.run(|| state.write(|backend| backend.remove_member(conversation, id)))
          >
            {move || i18n.t("channel-bots-remove-confirm")}
          </button>
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(false)>
            {move || i18n.t("cancel")}
          </button>
        </Show>
      </Show>
    </li>
  }
}

#[component]
fn BotCandidateRow(conversation: ConversationId, bot: User, joins_space: bool) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let id = bot.id;
  let pending = PendingAction::new();
  view! {
    <li class="settings-member">
      <span class="grow" translate="no">{bot.display_name.clone()}</span>
      <button
        type="button"
        class="btn btn-small btn-primary"
        prop:disabled=move || pending.is_pending()
        aria-busy=move || pending.is_pending().then_some("true")
        on:click=move |_| {
          // A bot never reads what was written before it was added.
          pending.run(|| {
            state.write(|backend| {
              backend.add_people_to_channel(conversation, vec![(id, true)], HistoryPolicy::FromJoin, joins_space)
            })
          })
        }
      >
        <Icon name=IconName::Plus />
        {move || i18n.t("channel-bots-add-button")}
      </button>
    </li>
  }
}

/// What to point Grafana, CI or `journalctl` at: a source block for the bridge's configuration
/// with this channel, and the two URLs.
#[component]
fn SourceHints(conversation: ConversationId) -> impl IntoView {
  let i18n = use_i18n();
  let token = new_source_token();
  let block = source_block(conversation, "my-source", &token);
  let webhook = webhook_url(&token);
  let ingest = ingest_url(&token);
  let logs_command = format!("journalctl -f | curl -T - -H 'Content-Type: text/plain' {ingest}");
  let hook_command = format!("curl -d '{{\"text\":\"Deploy finished\"}}' {webhook}");
  view! {
    <h3 class="detail-heading">{move || i18n.t("channel-sources")}</h3>
    <p class="hint">{move || i18n.t("channel-sources-hint")}</p>
    <label for="channel-source-block">{move || i18n.t("channel-sources-block")}</label>
    <textarea id="channel-source-block" class="input mono" rows="5" readonly prop:value=block.clone()></textarea>
    <div class="integration-copy-row"><CopyButton text=block.clone() /></div>
    <label for="channel-source-hook">{move || i18n.t("channel-sources-webhook")}</label>
    <input id="channel-source-hook" class="input mono" readonly prop:value=hook_command.clone() />
    <div class="integration-copy-row"><CopyButton text=hook_command.clone() /></div>
    <label for="channel-source-logs">{move || i18n.t("channel-sources-logs")}</label>
    <input id="channel-source-logs" class="input mono" readonly prop:value=logs_command.clone() />
    <div class="integration-copy-row"><CopyButton text=logs_command.clone() /></div>
  }
}
