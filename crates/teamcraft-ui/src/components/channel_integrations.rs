use std::collections::HashSet;
use std::sync::Arc;

use leptos::prelude::*;
use teamcraft_client_api::actions::ChannelChanges;
use teamcraft_client_api::model::{IntegrationSource, NewIntegrationSource};
use teamcraft_crypto::source_token::SourceToken;
use teamcraft_proto::{
  HistoryPolicy, MAX_SOURCE_NAME_CHARS, Permission, SourceId, SourceKind, SourceOptions,
};

use crate::channel_access::ChannelAccess;
use crate::components::admin_integrations::CopyButton;
use crate::components::icons::{Icon, IconName};
use crate::components::roster_banner::unsigned_reason;
use crate::components::space_invites::web_origin;
use crate::components::workspace_dialogs::FailureNote;
use crate::feedback::PendingAction;
use crate::i18n::use_i18n;
use crate::integrations::{
  BridgeLocation, NewSource, bridge_base_url, ingest_url, is_valid_source_name, webhook_url,
};
use crate::model::{ConversationId, ConversationKind, User};
use crate::state::use_app_state;

/// How many bots the search lists at most.
const MAX_BOT_RESULTS: usize = 8;

/// The "Integrations" section of the channel settings (docs/bridge.md): normal or log mode, the
/// bots in the channel with adding and removing, and the channel's sources. A bot is a normal
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
  // Sources are announced in the channel; a bot added later can't read the announcement.
  let has_bot = Memo::new(move |_| {
    let ids = info.get().map(|info| info.member_ids).unwrap_or_default();
    state.read(|backend| {
      ids
        .iter()
        .filter_map(|id| backend.user(*id))
        .any(|user| user.is_bot)
    })
  });
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
    <div class="channel-integrations">
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
    <Sources conversation=conversation has_bot=Signal::from(has_bot) />
    </div>
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
      <div class="field">
        <label for="channel-bot-search">{move || i18n.t("channel-bots-add")}</label>
        <input
          id="channel-bot-search"
          class="input"
          type="search"
          placeholder=move || i18n.t("channel-bots-search")
          prop:value=move || query.get()
          on:input=move |ev| query.set(event_target_value(&ev))
        />
      </div>
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

/// The channel's bridge sources (docs/bridge.md, "Sources"): the ones in force with revoke, and
/// adding one. Adding generates the token here, announces only its hash in the channel (an
/// encrypted control message the bridge reads) and shows the URL once; nothing goes into the
/// bridge's configuration. Only admins of the verified workspace roster manage sources (the bridge
/// doesn't read the signed channel roles yet, e2ee.md §13.1); a creator may revoke their
/// own.
#[component]
fn Sources(conversation: ConversationId, has_bot: Signal<bool>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let sources = Memo::new(move |_| state.read(|backend| backend.integration_sources(conversation)));
  let can_manage = Signal::derive(move || state.read(|backend| backend.can_manage_integration_sources()));
  let maybe_reveal: RwSignal<Option<Revealed>> = RwSignal::new(None);
  view! {
    <h3 class="detail-heading">{move || i18n.t("channel-sources")}</h3>
    <ul class="settings-member-list">
      <For each=move || sources.get() key=|source| source.id let:source>
        <SourceRow conversation=conversation source=source can_manage=can_manage />
      </For>
    </ul>
    <Show when=move || sources.with(Vec::is_empty) fallback=|| ()>
      <p class="muted-text">{move || i18n.t("channel-sources-none")}</p>
    </Show>
    <Show
      when=move || maybe_reveal.with(Option::is_some)
      fallback=move || {
        view! {
          <Show
            when=move || can_manage.get()
            fallback=move || view! { <p class="hint">{move || who_adds_sources(i18n, state)}</p> }
          >
            <AddSource conversation=conversation has_bot=has_bot on_added=move |revealed| maybe_reveal.set(Some(revealed)) />
          </Show>
        }
      }
    >
      {move || maybe_reveal.get().map(|revealed| view! { <RevealSource revealed=revealed on_done=move || maybe_reveal.set(None) /> })}
    </Show>
  }
}

/// Why the user can't add sources and whom to ask (issue #764): an admin role nobody signed yet
/// says so; everybody else learns which workspace admins can.
fn who_adds_sources(i18n: crate::i18n::I18n, state: crate::state::AppState) -> String {
  if let Some(reason) = unsigned_reason(i18n, state) {
    return reason;
  }
  let admins = state
    .read(|backend| backend.owner_roster())
    .map(|status| signed_admin_ids(&status))
    .unwrap_or_default();
  if admins.is_empty() {
    return i18n.t("channel-sources-admins-only");
  }
  let names = admins
    .iter()
    .map(|user| {
      state
        .read(|backend| backend.user(*user))
        .map_or_else(|| user.to_string(), |found| found.display_name)
    })
    .collect::<Vec<_>>()
    .join(", ");
  i18n.t_with("channel-sources-ask-admins", &[("admins", &names)])
}

/// Everybody with admin rights in the signed roster: owners, then admins.
fn signed_admin_ids(status: &teamcraft_client_api::admin::OwnerRosterStatus) -> Vec<crate::model::UserId> {
  status
    .owners
    .iter()
    .map(|owner| owner.user)
    .chain(status.admins.iter().copied())
    .collect()
}

/// A source just added, with its token: shown once, kept only while this panel is open. The token
/// is zeroized when the panel closes (the last reference goes).
#[derive(Clone)]
struct Revealed {
  name: String,
  kind: SourceKind,
  token: Arc<SourceToken>,
}

impl PartialEq for Revealed {
  fn eq(&self, other: &Self) -> bool {
    Arc::ptr_eq(&self.token, &other.token)
  }
}

#[component]
fn SourceRow(
  conversation: ConversationId,
  source: IntegrationSource,
  can_manage: Signal<bool>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let is_confirming = RwSignal::new(false);
  let id = source.id;
  let is_own = source.created_by == state.me().id;
  let can_revoke = move || can_manage.get() || is_own;
  let creator = state
    .read(|backend| backend.user(source.created_by))
    .map(|user| user.display_name)
    .unwrap_or_default();
  let added = crate::clock::format_date(source.created_at_ms, i18n.locale().tag());
  let kind_key = kind_label(&source.kind);
  view! {
    <li class="settings-member">
      <span class="grow source-meta">
        <span translate="no">{source.name.clone()}</span>
        <span class="muted-text">
          {move || i18n.t_with("channel-sources-added-by", &[("name", &creator), ("date", &added)])}
        </span>
      </span>
      <span class="badge">{move || i18n.t(kind_key)}</span>
      <Show when=can_revoke fallback=|| ()>
        <Show
          when=move || is_confirming.get()
          fallback=move || {
            view! {
              <button type="button" class="btn btn-small btn-quiet" on:click=move |_| is_confirming.set(true)>
                {move || i18n.t("channel-sources-revoke")}
              </button>
            }
          }
        >
          <button
            type="button"
            class="btn btn-small btn-danger"
            on:click=move |_| state.write(|backend| backend.revoke_integration_source(conversation, id))
          >
            {move || i18n.t("channel-sources-revoke-confirm")}
          </button>
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(false)>
            {move || i18n.t("cancel")}
          </button>
        </Show>
      </Show>
    </li>
  }
}

/// Name, kind and (for webhooks) whether danger attachments alert, then "Add source".
#[component]
fn AddSource(
  conversation: ConversationId,
  has_bot: Signal<bool>,
  #[prop(into)] on_added: Callback<Revealed>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let name = RwSignal::new(String::new());
  let kind = RwSignal::new(SourceKind::Webhook);
  let is_danger_alert = RwSignal::new(true);
  let maybe_error: RwSignal<Option<&'static str>> = RwSignal::new(None);
  let trimmed = move || name.with(|name| name.trim().to_owned());
  let can_add = move || has_bot.get() && is_valid_source_name(&trimmed());
  let add = move || {
    let Some(new_source) = NewSource::generate() else {
      maybe_error.set(Some("channel-sources-no-random"));
      return;
    };
    maybe_error.set(None);
    let source_name = trimmed();
    let source_kind = kind.get_untracked();
    let options = SourceOptions {
      danger_is_alert: source_kind == SourceKind::Webhook && is_danger_alert.get_untracked(),
      ..SourceOptions::default()
    };
    let announcement = NewIntegrationSource {
      id: SourceId::random(),
      name: source_name.clone(),
      kind: source_kind.clone(),
      token_sha256: new_source.token_sha256,
      options,
    };
    state.write(|backend| backend.add_integration_source(conversation, announcement));
    name.set(String::new());
    on_added.run(Revealed {
      name: source_name,
      kind: source_kind,
      token: Arc::new(new_source.token),
    });
  };
  let kind_button = move |value: SourceKind, key: &'static str| {
    let is_selected = Signal::derive({
      let selected = value.clone();
      move || kind.with(|kind| *kind == selected)
    });
    view! {
      <button
        type="button"
        role="radio"
        aria-checked=move || is_selected.get().to_string()
        aria-pressed=move || is_selected.get().to_string()
        on:click=move |_| kind.set(value.clone())
      >
        {move || i18n.t(key)}
      </button>
    }
  };
  view! {
    <div class="field">
      <label for="channel-source-name">{move || i18n.t("channel-sources-name")}</label>
      <input
        id="channel-source-name"
        class="input"
        type="text"
        maxlength=MAX_SOURCE_NAME_CHARS.to_string()
        placeholder=move || i18n.t("channel-sources-name-placeholder")
        prop:value=move || name.get()
        on:input=move |ev| name.set(event_target_value(&ev))
        on:keydown=move |ev| {
          if ev.key() == "Enter" && can_add() {
            add();
          }
        }
      />
    </div>
    <div class="segmented" role="radiogroup" aria-label=move || i18n.t("channel-sources-kind")>
      {kind_button(SourceKind::Webhook, "channel-sources-kind-webhook")}
      {kind_button(SourceKind::Log, "channel-sources-kind-log")}
    </div>
    <Show when=move || kind.with(|kind| *kind == SourceKind::Webhook) fallback=|| ()>
      <label class="checkbox">
        <input
          type="checkbox"
          prop:checked=move || is_danger_alert.get()
          on:change=move |ev| is_danger_alert.set(event_target_checked(&ev))
        />
        <span>{move || i18n.t("channel-sources-danger-alert")}</span>
      </label>
    </Show>
    <div class="integration-copy-row">
      <button type="button" class="btn btn-small btn-primary" prop:disabled=move || !can_add() on:click=move |_| add()>
        <Icon name=IconName::Plus />
        {move || i18n.t("channel-sources-add")}
      </button>
    </div>
    <p class="hint">
      {move || if has_bot.get() { i18n.t("channel-sources-hint") } else { i18n.t("channel-sources-needs-bot") }}
    </p>
    {move || maybe_error.get().map(|key| view! { <p class="field-error" role="alert">{i18n.t(key)}</p> })}
  }
}

/// The new source's URL, shown once with a copy button and an example, on the instance's host
/// name (teamcraft-deploy) or on the bridge's loopback default.
#[component]
fn RevealSource(revealed: Revealed, #[prop(into)] on_done: Callback<()>) -> impl IntoView {
  let i18n = use_i18n();
  let server = web_origin().unwrap_or_default();
  let location = RwSignal::new(BridgeLocation::Instance);
  let token = StoredValue::new(Arc::clone(&revealed.token));
  let kind = revealed.kind.clone();
  let is_webhook = kind == SourceKind::Webhook;
  let url = Memo::new(move |_| {
    let base = bridge_base_url(location.get(), &server);
    token.with_value(|token| {
      if is_webhook {
        webhook_url(&base, token.expose_secret())
      } else {
        ingest_url(&base, token.expose_secret())
      }
    })
  });
  let example = Signal::derive(move || {
    let url = url.get();
    if is_webhook {
      format!("curl -d '{{\"text\":\"Deploy finished\"}}' {url}")
    } else {
      format!("journalctl -f | curl -T - -H 'Content-Type: text/plain' {url}")
    }
  });
  let url_text = Signal::derive(move || url.get());
  let location_button = move |value: BridgeLocation, key: &'static str| {
    let is_selected = move || location.get() == value;
    view! {
      <button
        type="button"
        role="radio"
        aria-checked=move || is_selected().to_string()
        aria-pressed=move || is_selected().to_string()
        on:click=move |_| location.set(value)
      >
        {move || i18n.t(key)}
      </button>
    }
  };
  let name = revealed.name.clone();
  view! {
    <p class="hint" role="status">{move || i18n.t_with("channel-sources-created", &[("name", &name)])}</p>
    <div class="segmented" role="radiogroup" aria-label=move || i18n.t("channel-sources-bridge")>
      {location_button(BridgeLocation::Instance, "channel-sources-bridge-instance")}
      {location_button(BridgeLocation::Loopback, "channel-sources-bridge-loopback")}
    </div>
    <p class="hint">
      {move || match location.get() {
        BridgeLocation::Instance => i18n.t("channel-sources-bridge-instance-hint"),
        BridgeLocation::Loopback => i18n.t("channel-sources-bridge-loopback-hint"),
      }}
    </p>
    <label for="channel-source-url">{move || i18n.t("channel-sources-url")}</label>
    <input id="channel-source-url" class="input mono" readonly prop:value=url_text />
    <div class="integration-copy-row"><CopyButton text=url_text /></div>
    <label for="channel-source-example">{move || i18n.t("channel-sources-example")}</label>
    <input id="channel-source-example" class="input mono" readonly prop:value=example />
    <div class="integration-copy-row"><CopyButton text=example /></div>
    <p class="hint">{move || i18n.t("channel-sources-shown-once")}</p>
    <div class="integration-copy-row">
      <button type="button" class="btn btn-small" on:click=move |_| on_done.run(())>
        {move || i18n.t("channel-sources-done")}
      </button>
    </div>
  }
}

fn kind_label(kind: &SourceKind) -> &'static str {
  match kind {
    SourceKind::Webhook => "channel-sources-kind-webhook",
    SourceKind::Log => "channel-sources-kind-log",
    SourceKind::Unknown(_) => "channel-sources-kind-other",
  }
}
