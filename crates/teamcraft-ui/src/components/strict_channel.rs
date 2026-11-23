//! Strict channels (e2ee.md §4.6.2, issue #1897): the setting in a private channel's privacy
//! section, and the notice in the channel when this device stopped at a refused change or holds a
//! welcome back.
//!
//! Nothing shows while the backend has no [`StrictChannel`] for the channel: an app that doesn't
//! implement the rule claims no protection. Every action is offered only where both the signed
//! state and the server's permissions allow it ([`StrictGates`]); the core decides again and a
//! refusal reaches the user as a failure.

use leptos::prelude::*;
use teamcraft_client_api::strict_channel::{
  StrictActor, StrictChannel, StrictGates, StrictLoss, StrictLossKind, StrictRole, strict_candidates,
  strict_losses_in,
};
use teamcraft_proto::{ChannelRole, ConversationId, Permission, UserId};

use crate::channel_access::ChannelAccess;
use crate::components::icons::{Icon, IconName};
use crate::components::workspace_dialogs::FailureNote;
use crate::feedback::PendingAction;
use crate::i18n::{I18n, use_i18n};
use crate::state::{AppState, use_app_state};

/// "Member changes" in a private channel's privacy section: whether only signed admins change its
/// members, and the way there (D3, D2, D6).
#[component]
pub fn ChannelStrictSection(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  // The core makes the view afresh from the group on its own after every change; opening the
  // section also catches a roster that moved.
  Effect::new(move |_| state.write(|backend| backend.refresh_strict_channel(conversation)));
  let strict = Memo::new(move |_| state.read(|backend| backend.strict_channel(conversation)));
  let gates = strict_gates(state, conversation, strict);
  move || {
    strict.get().map(|channel| {
      let is_strict = channel.is_strict;
      view! {
        <h3>{move || i18n.t("channel-strict-title")}</h3>
        <p class="settings-row" role="status">
          <Icon name=if is_strict { IconName::Shield } else { IconName::Lock } />
          <span>{move || if is_strict { i18n.t("channel-strict-on") } else { i18n.t("channel-strict-off") }}</span>
        </p>
        <p class="hint">{move || i18n.t("channel-strict-hint")}</p>
        {if is_strict {
          view! { <StrictOn conversation=conversation channel=channel gates=gates /> }.into_any()
        } else {
          view! { <StrictOff conversation=conversation channel=channel gates=gates /> }.into_any()
        }}
      }
    })
  }
}

/// The notices in a channel's view: this device stopped at a refused change (D4), or a welcome
/// back into the channel waits for the user's word.
#[component]
pub fn StrictChannelNotice(conversation: ConversationId) -> impl IntoView {
  let state = use_app_state();
  let strict = Memo::new(move |_| state.read(|backend| backend.strict_channel(conversation)));
  let gates = strict_gates(state, conversation, strict);
  move || {
    let channel = strict.get()?;
    let stop = channel
      .maybe_stop
      .map(|stop| view! { <StrictStopNotice conversation=conversation maybe_signer=stop.maybe_signer gates=gates /> });
    let rejoin = channel
      .maybe_rejoin
      .map(|_| view! { <StrictRejoinNotice conversation=conversation strict=strict /> });
    Some(view! { {stop} {rejoin} })
  }
}

/// A strict channel: its roles, and turning it off.
#[component]
fn StrictOn(conversation: ConversationId, channel: StrictChannel, gates: Memo<StrictGates>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let pending = PendingAction::new();
  let is_confirming = RwSignal::new(false);
  let roles = channel.roles.clone();
  view! {
    <ul class="settings-member-list strict-list">
      {roles
        .into_iter()
        .map(|holder| {
          let name = name_of(state, i18n, holder.user);
          let key = match holder.role {
            StrictRole::Admin => "channel-strict-role-admin",
            StrictRole::Moderator => "channel-strict-role-moderator",
          };
          view! {
            <li class="settings-row">
              <span class="grow" translate="no">{name}</span>
              <span class="muted-text">{move || i18n.t(key)}</span>
            </li>
          }
        })
        .collect_view()}
    </ul>
    <Show when=move || gates.get().can_turn_off fallback=|| ()>
      <Show
        when=move || is_confirming.get()
        fallback=move || view! { <button type="button" class="btn" on:click=move |_| is_confirming.set(true)>{move || i18n.t("channel-strict-turn-off")}</button> }
      >
        <p role="alert">{move || i18n.t("channel-strict-turn-off-confirm")}</p>
        <div class="inline-actions">
          <button type="button" class="btn btn-primary" prop:disabled=move || pending.is_pending()
            on:click=move |_| pending.run_and_then(|| state.write(|backend| backend.turn_strict_off(conversation)), move || is_confirming.set(false))>
            {move || i18n.t("channel-strict-turn-off")}
          </button>
          <button type="button" class="btn" on:click=move |_| is_confirming.set(false)>{move || i18n.t("cancel")}</button>
        </div>
        <FailureNote pending=pending />
      </Show>
    </Show>
  }
}

/// A channel that isn't strict yet: who may turn it on, who loses what, and the devices' steps.
#[component]
fn StrictOff(
  conversation: ConversationId,
  channel: StrictChannel,
  gates: Memo<StrictGates>,
) -> impl IntoView {
  let i18n = use_i18n();
  view! {
    <Show
      when=move || gates.get().can_turn_on
      fallback=move || view! { <p class="hint">{move || i18n.t("channel-strict-ask-workspace-admin")}</p> }
    >
      <StrictLosses conversation=conversation channel=channel.clone() />
      <StrictSteps conversation=conversation channel=channel.clone() gates=gates />
    </Show>
  }
}

/// Who loses what when the channel turns strict (D1, D2), with giving them a signed channel role
/// first on offer where the user may.
#[component]
fn StrictLosses(conversation: ConversationId, channel: StrictChannel) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let access = ChannelAccess::new(state, conversation);
  let candidates = Memo::new(move |_| state.read(|backend| strict_candidates(backend, conversation)));
  let scheme = access.scheme;
  let losses = Memo::new(move |_| {
    scheme.with(|scheme| state.read(|backend| strict_losses_in(backend, conversation, &channel, scheme)))
  });
  let can_give_roles = Memo::new(move |_| access.can(&Permission::ManageChannelRoles));
  move || {
    let list = losses.get();
    if list.is_empty() {
      return view! { <p class="hint">{move || i18n.t("channel-strict-nobody-loses")}</p> }.into_any();
    }
    view! {
      <p class="hint">{move || i18n.t("channel-strict-losses")}</p>
      <ul class="settings-member-list strict-list">
        {list
          .into_iter()
          .map(|loss| {
            // Only a member of the channel can be given a role here (the members section's rule).
            let is_member = candidates.with(|found| {
              found
                .iter()
                .any(|candidate| candidate.user == loss.user && candidate.maybe_channel_role.is_some())
            });
            view! { <StrictLossRow conversation=conversation loss=loss can_give_role=Signal::derive(move || can_give_roles.get() && is_member) /> }
          })
          .collect_view()}
      </ul>
    }
    .into_any()
  }
}

#[component]
fn StrictLossRow(
  conversation: ConversationId,
  loss: StrictLoss,
  can_give_role: Signal<bool>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let pending = PendingAction::new();
  let name = name_of(state, i18n, loss.user);
  let key = match loss.loss {
    StrictLossKind::ChangingMembers => "channel-strict-loses-changing",
    StrictLossKind::AddingMembers => "channel-strict-loses-adding",
  };
  let user = loss.user;
  view! {
    <li class="settings-row">
      <span class="grow">
        <span translate="no">{name}</span>
        <span class="muted-text">{move || i18n.t(key)}</span>
      </span>
      <Show when=move || can_give_role.get() fallback=|| ()>
        <button type="button" class="btn btn-small" prop:disabled=move || pending.is_pending()
          on:click=move |_| pending.run(|| state.write(|backend| backend.set_channel_role(conversation, user, ChannelRole::Admin)))>
          {move || i18n.t("channel-strict-make-admin")}
        </button>
      </Show>
    </li>
  }
}

/// Steps 2 to 4: ask the devices, the devices that block (D6), turning it on.
#[component]
fn StrictSteps(
  conversation: ConversationId,
  channel: StrictChannel,
  gates: Memo<StrictGates>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let ask = PendingAction::new();
  let turn_on = PendingAction::new();
  let is_confirming = RwSignal::new(false);
  let readiness = channel.maybe_readiness.clone().unwrap_or_default();
  let has_admin = channel
    .roles
    .iter()
    .any(|holder| holder.role == StrictRole::Admin);
  let has_asked = readiness.maybe_asked_by.is_some();
  if !has_admin {
    // At least one admin with a leaf, or nobody could add anyone (§4.6.2, step 4).
    return view! { <p class="hint" role="note">{move || i18n.t("channel-strict-needs-admin")}</p> }
      .into_any();
  }
  if readiness.is_ready {
    return view! {
      <Show
        when=move || is_confirming.get()
        fallback=move || view! { <button type="button" class="btn btn-primary" on:click=move |_| is_confirming.set(true)>{move || i18n.t("channel-strict-turn-on")}</button> }
      >
        <p role="alert">{move || i18n.t("channel-strict-turn-on-confirm")}</p>
        <div class="inline-actions">
          <button type="button" class="btn btn-primary" prop:disabled=move || turn_on.is_pending()
            on:click=move |_| turn_on.run_and_then(|| state.write(|backend| backend.turn_strict_on(conversation)), move || is_confirming.set(false))>
            {move || i18n.t("channel-strict-turn-on")}
          </button>
          <button type="button" class="btn" on:click=move |_| is_confirming.set(false)>{move || i18n.t("cancel")}</button>
        </div>
        <FailureNote pending=turn_on />
      </Show>
    }
    .into_any();
  }
  if !has_asked {
    return view! {
      <p class="hint">{move || i18n.t("channel-strict-ask-hint")}</p>
      <div class="inline-actions">
        <button type="button" class="btn btn-primary" prop:disabled=move || ask.is_pending()
          on:click=move |_| ask.run(|| state.write(|backend| backend.ask_strict_ready(conversation)))>
          {move || i18n.t("channel-strict-ask")}
        </button>
      </div>
      <FailureNote pending=ask />
    }
    .into_any();
  }
  let waiting = readiness.waiting.clone();
  let waiting_count = waiting.len();
  let refreshing_count = readiness.refreshing.len();
  view! {
    <p role="status">{move || i18n.t_count("channel-strict-waiting", waiting_count)}</p>
    <Show when=move || refreshing_count != 0 fallback=|| ()>
      <p class="hint" role="status">{move || i18n.t_count("channel-strict-refreshing", refreshing_count)}</p>
    </Show>
    <ul class="settings-member-list strict-list">
      {waiting
        .into_iter()
        .map(|device| view! { <StrictBlockerRow conversation=conversation user=device.user device=device.device gates=gates /> })
        .collect_view()}
    </ul>
  }
  .into_any()
}

/// A device that blocks turning strict on, with removing it on offer (confirmed first).
#[component]
fn StrictBlockerRow(
  conversation: ConversationId,
  user: UserId,
  device: teamcraft_proto::DeviceId,
  gates: Memo<StrictGates>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let pending = PendingAction::new();
  let is_confirming = RwSignal::new(false);
  let name = name_of(state, i18n, user);
  let confirm_name = name.clone();
  view! {
    <li class="strict-blocker">
      <div class="settings-row">
        <span class="grow">
          <span translate="no">{name}</span>
          <span class="muted-text">{move || i18n.t("channel-strict-blocker-device")}</span>
        </span>
        <Show when=move || gates.get().can_remove_blocker && !is_confirming.get() fallback=|| ()>
          <button type="button" class="btn btn-small" on:click=move |_| is_confirming.set(true)>{move || i18n.t("channel-strict-remove-device")}</button>
        </Show>
      </div>
      <Show when=move || is_confirming.get() fallback=|| ()>
        <p role="alert">{
          let confirm_name = confirm_name.clone();
          move || i18n.t_with("channel-strict-remove-device-confirm", &[("name", &confirm_name)])
        }</p>
        <div class="inline-actions">
          <button type="button" class="btn btn-primary" prop:disabled=move || pending.is_pending()
            on:click=move |_| pending.run_and_then(|| state.write(|backend| backend.remove_strict_blocker(conversation, user, device)), move || is_confirming.set(false))>
            {move || i18n.t("channel-strict-remove-device")}
          </button>
          <button type="button" class="btn" on:click=move |_| is_confirming.set(false)>{move || i18n.t("cancel")}</button>
        </div>
        <FailureNote pending=pending />
      </Show>
    </li>
  }
}

/// "A change to this channel's members was refused …" with the admin's way on (D4).
#[component]
fn StrictStopNotice(
  conversation: ConversationId,
  maybe_signer: Option<UserId>,
  gates: Memo<StrictGates>,
) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let pending = PendingAction::new();
  let is_confirming_removal = RwSignal::new(false);
  let me = state.me().id;
  // Never "Remove yourself": the stop then names somebody else's change, or nobody.
  let named = StoredValue::new(
    maybe_signer
      .filter(|signer| *signer != me)
      .map(|signer| (signer, name_of(state, i18n, signer))),
  );
  let remove_label = move || {
    named.with_value(|maybe| {
      maybe
        .as_ref()
        .map(|(_, name)| i18n.t_with("channel-strict-remove-and-continue", &[("name", name)]))
        .unwrap_or_default()
    })
  };
  let remove_confirm = move || {
    named.with_value(|maybe| {
      maybe
        .as_ref()
        .map(|(_, name)| i18n.t_with("channel-strict-remove-and-continue-confirm", &[("name", name)]))
        .unwrap_or_default()
    })
  };
  let has_named = move || named.with_value(Option::is_some);
  let remove = move |_| {
    if let Some((signer, _)) = named.get_value() {
      pending.run_and_then(
        || state.write(|backend| backend.continue_strict_channel(conversation, Some(signer))),
        move || is_confirming_removal.set(false),
      );
    }
  };
  let carry_on =
    move |_| pending.run(|| state.write(|backend| backend.continue_strict_channel(conversation, None)));
  view! {
    <div class="key-banner strict-notice" role="status">
      <Icon name=IconName::Alert />
      <div class="key-banner-text">
        <span>{move || i18n.t("channel-strict-stopped")}</span>
        <Show when=move || gates.get().can_continue fallback=|| ()>
          <Show when=move || is_confirming_removal.get() fallback=|| ()>
            <p role="alert">{remove_confirm}</p>
          </Show>
          <div class="inline-actions">
            <Show when=move || has_named() && !is_confirming_removal.get() fallback=|| ()>
              <button type="button" class="btn btn-small" prop:disabled=move || pending.is_pending()
                on:click=move |_| is_confirming_removal.set(true)>{remove_label}</button>
            </Show>
            <Show when=move || is_confirming_removal.get() fallback=|| ()>
              <button type="button" class="btn btn-small" prop:disabled=move || pending.is_pending() on:click=remove>{remove_label}</button>
              <button type="button" class="btn btn-small" on:click=move |_| is_confirming_removal.set(false)>{move || i18n.t("cancel")}</button>
            </Show>
            <Show when=move || !is_confirming_removal.get() fallback=|| ()>
              <button type="button" class="btn btn-small" prop:disabled=move || pending.is_pending() on:click=carry_on>
                {move || i18n.t("channel-strict-continue")}
              </button>
            </Show>
          </div>
          <FailureNote pending=pending />
        </Show>
      </div>
    </div>
  }
}

/// A welcome back that wasn't opened: who is new or gone in it, and joining on the user's word once
/// the wait for an admin is over.
#[component]
fn StrictRejoinNotice(conversation: ConversationId, strict: Memo<Option<StrictChannel>>) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let pending = PendingAction::new();
  move || {
    let rejoin = strict.get()?.maybe_rejoin?;
    let names = |users: &[UserId]| {
      users
        .iter()
        .map(|user| name_of(state, i18n, *user))
        .collect::<Vec<_>>()
        .join(", ")
    };
    let sender = name_of(state, i18n, rejoin.sender);
    let joined = names(&rejoin.joined);
    let gone = names(&rejoin.gone);
    let body = if rejoin.is_accepted {
      view! { <span>{move || i18n.t("channel-strict-rejoin-accepted")}</span> }.into_any()
    } else if rejoin.is_due {
      view! {
        <span>{move || i18n.t_with("channel-strict-rejoin-due", &[("name", &sender)])}</span>
        <Show when={let joined = joined.clone(); move || !joined.is_empty()} fallback=|| ()>
          <span>{let joined = joined.clone(); move || i18n.t_with("channel-strict-rejoin-joined", &[("names", &joined)])}</span>
        </Show>
        <Show when={let gone = gone.clone(); move || !gone.is_empty()} fallback=|| ()>
          <span>{let gone = gone.clone(); move || i18n.t_with("channel-strict-rejoin-gone", &[("names", &gone)])}</span>
        </Show>
        <div class="inline-actions">
          <button type="button" class="btn btn-small" prop:disabled=move || pending.is_pending()
            on:click=move |_| pending.run(|| state.write(|backend| backend.accept_strict_rejoin(conversation)))>
            {move || i18n.t("channel-strict-rejoin")}
          </button>
        </div>
        <FailureNote pending=pending />
      }
      .into_any()
    } else {
      view! { <span>{move || i18n.t("channel-strict-rejoin-waiting")}</span> }.into_any()
    };
    Some(view! {
      <div class="key-banner strict-notice" role="status">
        <Icon name=IconName::Shield />
        <div class="key-banner-text">{body}</div>
      </div>
    })
  }
}

/// The user's gates in the channel, reactive.
fn strict_gates(
  state: AppState,
  conversation: ConversationId,
  strict: Memo<Option<StrictChannel>>,
) -> Memo<StrictGates> {
  let access = ChannelAccess::new(state, conversation);
  let me = state.me().id;
  Memo::new(move |_| {
    strict.with(|maybe_channel| {
      maybe_channel
        .as_ref()
        .map_or_else(StrictGates::default, |channel| {
          StrictGates::new(
            channel,
            StrictActor {
              user: me,
              can_manage_members: access.can(&Permission::ManageMembers),
              can_manage_channel_roles: access.can(&Permission::ManageChannelRoles),
            },
          )
        })
    })
  })
}

fn name_of(state: AppState, i18n: I18n, user: UserId) -> String {
  state
    .read(|backend| backend.user(user))
    .map(|person| person.display_name)
    .unwrap_or_else(|| i18n.t("channel-strict-someone"))
}
