//! Retention per channel (issue #2360): the instance default at the top, each channel's own
//! setting marked where it has one, several channels changed or reset at once, and every change
//! collected until the page's save bar saves it.

use std::collections::{HashMap, HashSet};

use leptos::prelude::*;
use teamcraft_proto::{Permission, RetentionPolicy};

use crate::admin::{AdminChannel, AdminChannelFilter, AdminChannelQuery, RetentionOverride};
use crate::channel_access::can_in_any_channel_now;
use crate::components::admin_console::{ListMore, SectionHeader};
use crate::components::admin_legal_hold::LegalHoldPanel;
use crate::components::admin_save::{SaveBar, use_save_tracker};
use crate::form_errors::{FieldError, FieldErrors, FormErrors};
use crate::i18n::{I18n, use_i18n};
use crate::model::{ConversationId, ConversationKind};
use crate::reactive::use_debounced;
use crate::state::use_app_state;

/// Instance maximum an owner may not exceed when the signed policy sets no limit of its own.
const MAX_DAYS: u32 = 730;
const MAX_MESSAGES: u32 = 1_000_000;
/// The wait after the last key before the search asks the server.
const SEARCH_DEBOUNCE_MS: u64 = 250;
const FORM_ID: &str = "retention-form";
const BULK_ID: &str = "retention-bulk";
const FOLLOW: &str = "follow";
const OWN: &str = "own";

/// Retention per channel: keep the last N days and/or N messages, whichever is stricter. A channel
/// without a setting of its own follows its space and the instance default.
#[component]
pub fn RetentionSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let channels = Memo::new(move |_| {
    state
      .read(|backend| backend.admin_channels())
      .into_iter()
      .filter(|c| c.kind != ConversationKind::Direct)
      .collect::<Vec<_>>()
  });
  // Channels page by page, searched by the server (issue #1241). The Channels page's quick filters
  // don't apply here: this list is of every channel.
  let query = RwSignal::new(state.read(|backend| backend.admin_channel_query()).text);
  let debounced = use_debounced(query, SEARCH_DEBOUNCE_MS);
  Effect::new(move |_| {
    let asked = AdminChannelQuery {
      text: debounced.get(),
      filter: AdminChannelFilter::All,
    };
    state.write(|backend| backend.admin_set_channel_query(asked));
  });
  let policy = Memo::new(move |_| state.read(|backend| backend.workspace_policy()).1.retention);
  let limits = Signal::derive(move || Limits::of_policy(policy.get()));
  // Changing a channel's retention is a channel property: admins hold it by their instance role,
  // also in channels they aren't in.
  let can_edit = Memo::new(move |_| can_in_any_channel_now(state, &Permission::ManageChannelProperties));
  let page = RetentionPage::new();
  let tracker = use_save_tracker();
  let errors = FormErrors::new();

  // A change counts as done once the list shows it, also when another admin made the same one.
  Effect::new(move |_| {
    let stored: HashMap<ConversationId, RetentionOverride> = channels.with(|list| {
      list
        .iter()
        .map(|channel| (channel.id, channel.own_retention))
        .collect()
    });
    let had_drafts = page.drafts.with_untracked(|drafts| !drafts.is_empty());
    page.drop_arrived(&stored, limits.get_untracked());
    if had_drafts && page.drafts.with_untracked(HashMap::is_empty) {
      tracker.settled();
    }
  });
  let is_dirty = Signal::derive(move || page.has_changes(limits.get()));
  // After a refused save, the errors follow the input (forms.md).
  Effect::new(move |_| {
    let found = page.check(i18n, limits.get()).err().unwrap_or_default();
    errors.follow(found);
  });
  let save = move |ev: leptos::ev::SubmitEvent| {
    ev.prevent_default();
    match page.check(i18n, limits.get_untracked()) {
      Ok(changes) => {
        errors.clear();
        if changes.is_empty() {
          return;
        }
        state.write(|backend| {
          for (channel, own) in &changes {
            backend.set_channel_retention(*channel, *own);
          }
        });
        tracker.begin();
      },
      Err(found) => errors.refuse(FORM_ID, found),
    }
  };
  let change_count = move || page.change_count(limits.get());

  view! {
    <SectionHeader
      title=Signal::derive(move || i18n.t("admin-retention-title"))
      subtitle=Signal::derive(move || i18n.t("admin-retention-subtitle"))
    />
    <InstanceDefault policy=policy />
    <form id=FORM_ID class="retention-form" novalidate on:submit=save>
      <div class="admin-toolbar">
        <label class="visually-hidden" for="admin-retention-search">{move || i18n.t("admin-search")}</label>
        <input
          id="admin-retention-search"
          class="input"
          type="search"
          name="q"
          autocomplete="off"
          spellcheck="false"
          placeholder=move || i18n.t("admin-search-placeholder")
          prop:value=move || query.get()
          on:input=move |ev| query.set(event_target_value(&ev))
        />
      </div>
      <Show when=move || can_edit.get() fallback=move || view! { <p class="muted-text small admin-list-hint">{move || i18n.t("admin-retention-read-only")}</p> }>
        <BulkBar page=page channels=channels limits=limits />
      </Show>
      <div class="table-wrap">
        <table class="data-table retention-table">
          <caption class="visually-hidden">{move || i18n.t("admin-retention-title")}</caption>
          <thead>
            <tr>
              <th scope="col" class="select-col">
                <Show when=move || can_edit.get() fallback=|| ()>
                  <label class="checkbox">
                    <input
                      type="checkbox"
                      prop:checked=move || page.are_all_selected(&channels.get())
                      prop:indeterminate=move || page.is_some_selected(&channels.get())
                      on:change=move |ev| page.select_all(&channels.get_untracked(), event_target_checked(&ev))
                    />
                    <span class="visually-hidden">{move || i18n.t("admin-retention-select-all")}</span>
                  </label>
                </Show>
              </th>
              <th scope="col">{move || i18n.t("admin-col-channel")}</th>
              <th scope="col">{move || i18n.t("retention-days")}</th>
              <th scope="col">{move || i18n.t("retention-messages")}</th>
              <th scope="col"><span class="visually-hidden">{move || i18n.t("retention-actions")}</span></th>
            </tr>
          </thead>
          <tbody>
            <For each=move || channels.get() key=|channel| (channel.id, channel.own_retention, channel.space_retention) let:channel>
              <RetentionRow channel=channel page=page policy=policy limits=limits errors=errors can_edit=can_edit />
            </For>
          </tbody>
        </table>
      </div>
      <Show when=move || channels.with(Vec::is_empty) fallback=|| ()>
        <p class="empty-state">{move || i18n.t("admin-empty")}</p>
      </Show>
      <ListMore
        has_more=Signal::derive(move || state.read(|backend| backend.admin_channels_have_more()))
        on_more=Callback::new(move |()| state.write(|backend| backend.admin_load_more_channels()))
      />
      <SaveBar
        tracker=tracker
        is_dirty=is_dirty
        on_discard=Callback::new(move |()| {
          page.drafts.set(HashMap::new());
          errors.clear();
        })
        save_label=Signal::derive(move || i18n.t_count("admin-retention-save", change_count()))
      />
    </form>
    <LegalHoldPanel />
  }
}

/// What applies where a channel sets nothing of its own, and how far owners may go.
#[component]
fn InstanceDefault(policy: Memo<RetentionPolicy>) -> impl IntoView {
  let i18n = use_i18n();
  let default = move || summary(i18n, RetentionOverride::from(policy.get().default));
  let limit = move || {
    let limit = RetentionOverride::from(policy.get().limit);
    if limit.is_empty() {
      i18n.t("admin-retention-no-limit")
    } else {
      summary(i18n, limit)
    }
  };
  let pinned = move || {
    i18n.t(if policy.get().keep_pinned {
      "admin-retention-pinned-kept"
    } else {
      "admin-retention-pinned-deleted"
    })
  };
  view! {
    <section class="retention-default" aria-labelledby="retention-default-title">
      <h3 id="retention-default-title" class="detail-heading">{move || i18n.t("admin-retention-default-title")}</h3>
      <dl class="meta-list">
        <dt>{move || i18n.t("admin-retention-default")}</dt>
        <dd>{default}</dd>
        <dt>{move || i18n.t("admin-retention-limit")}</dt>
        <dd>{limit}</dd>
        <dt>{move || i18n.t("admin-retention-pinned")}</dt>
        <dd>{pinned}</dd>
      </dl>
      <p class="muted-text small">{move || i18n.t("admin-retention-default-hint")}</p>
    </section>
  }
}

/// The selected channels' change: the same setting for all, or back to what they follow.
#[component]
fn BulkBar(page: RetentionPage, channels: Memo<Vec<AdminChannel>>, limits: Signal<Limits>) -> impl IntoView {
  let i18n = use_i18n();
  let bulk = RwSignal::new(OwnDraft::of(RetentionOverride::default()));
  let draft: Signal<OwnDraft> = bulk.into();
  let set_draft = Callback::new(move |changed: OwnDraft| bulk.set(changed));
  let errors = FormErrors::new();
  let days_id = format!("{BULK_ID}-days");
  let messages_id = format!("{BULK_ID}-messages");
  let selected_count = move || page.selected.with(HashSet::len);
  let check = {
    let (days_id, messages_id) = (days_id.clone(), messages_id.clone());
    move || draft.get().check(i18n, limits.get(), &days_id, &messages_id)
  };
  Effect::new({
    let check = check.clone();
    move |_| errors.follow(check().err().unwrap_or_default())
  });
  let apply = Callback::new({
    let check = check.clone();
    move |()| match check() {
      Ok(own) => {
        errors.clear();
        let targets = page.selected_channels(&channels.get_untracked());
        page.stage_all(&targets, own);
      },
      Err(found) => errors.refuse(BULK_ID, found),
    }
  });
  let reset = Callback::new(move |()| {
    let targets = page.selected_channels(&channels.get_untracked());
    page.stage_all(&targets, RetentionOverride::default());
  });

  view! {
    <Show when=move || { selected_count() > 0 } fallback=|| ()>
      <div id=BULK_ID class="retention-bulk" role="group" aria-label=move || i18n.t("admin-retention-bulk-label")>
        <p class="retention-bulk-count" role="status">{move || i18n.t_count("admin-retention-selected", selected_count())}</p>
        <div class="retention-bulk-fields">
          <div class="retention-bulk-field" data-label=move || i18n.t("retention-days")>
          <LimitInput
            draft=draft
            set_draft=set_draft
            limit=Limit::Days
            input_id=days_id.clone()
            errors=errors
            follow_label=Signal::derive(move || i18n.t("admin-retention-follow-plain"))
            choice_label=Signal::derive(move || i18n.t("admin-retention-bulk-days"))
            max=Signal::derive(move || limits.get().max_days)
          />
          </div>
          <div class="retention-bulk-field" data-label=move || i18n.t("retention-messages")>
          <LimitInput
            draft=draft
            set_draft=set_draft
            limit=Limit::Messages
            input_id=messages_id.clone()
            errors=errors
            follow_label=Signal::derive(move || i18n.t("admin-retention-follow-plain"))
            choice_label=Signal::derive(move || i18n.t("admin-retention-bulk-messages"))
            max=Signal::derive(move || limits.get().max_messages)
          />
          </div>
        </div>
        <div class="retention-bulk-actions">
          <button type="button" class="btn btn-small" on:click=move |_| apply.run(())>{move || i18n.t("admin-retention-bulk-apply")}</button>
          <button type="button" class="btn btn-small" on:click=move |_| reset.run(())>{move || i18n.t("admin-retention-bulk-reset")}</button>
          <button type="button" class="btn btn-small" on:click=move |_| page.selected.set(HashSet::new())>
            {move || i18n.t("admin-retention-bulk-clear")}
          </button>
        </div>
      </div>
    </Show>
  }
}

#[component]
fn RetentionRow(
  channel: AdminChannel,
  page: RetentionPage,
  policy: Memo<RetentionPolicy>,
  limits: Signal<Limits>,
  errors: FormErrors,
  can_edit: Memo<bool>,
) -> impl IntoView {
  let i18n = use_i18n();
  let id = channel.id;
  let stored = channel.own_retention;
  let space = channel.space_retention;
  let name = channel.name.clone();
  let days_id = days_field_id(id);
  let messages_id = messages_field_id(id);
  // The row's fields: the collected change, else what is stored.
  let draft = Signal::derive(move || page.draft_of(id).unwrap_or_else(|| OwnDraft::of(stored)));
  let set_draft = Callback::new(move |changed: OwnDraft| page.stage(id, changed));
  let above = move || space.or(RetentionOverride::from(policy.get().default));
  let follow_label = move |limit: Limit| {
    let source = if limit.of(space).is_some() {
      "admin-retention-follow-space"
    } else {
      "admin-retention-follow-default"
    };
    let value = limit_text(i18n, limit, limit.of(above()));
    i18n.t_with(source, &[("value", &value)])
  };
  let is_own = move || draft.get().parsed(limits.get()).is_none_or(|own| !own.is_empty());
  let is_changed = move || page.is_changed(id, limits.get());
  let is_selected = move || page.selected.with(|selected| selected.contains(&id));

  view! {
    <tr class:is-changed=is_changed class:selected=is_selected>
      <td class="select-col">
        <Show when=move || can_edit.get() fallback=|| ()>
          <label class="checkbox">
            <input
              type="checkbox"
              prop:checked=is_selected
              on:change=move |ev| page.select(id, event_target_checked(&ev))
            />
            <span class="visually-hidden">{
              let name = name.clone();
              move || i18n.t_with("admin-retention-select", &[("channel", &name)])
            }</span>
          </label>
        </Show>
      </td>
      <th scope="row">
        <span class="row-name" translate="no">{format!("# {}", channel.name)}</span>
        <span class="row-sub muted-text small" translate="no">{channel.space_name.clone()}</span>
        <span class="retention-marks">
          <Show when=is_own fallback=|| ()>
            <span class="badge badge-active">{move || i18n.t("admin-retention-own")}</span>
          </Show>
          <Show when=is_changed fallback=|| ()>
            <span class="badge">{move || i18n.t("admin-retention-changed")}</span>
          </Show>
        </span>
      </th>
      <td class="retention-cell" data-label=move || i18n.t("retention-days")>
        <LimitInput
          draft=draft
          set_draft=set_draft
          limit=Limit::Days
          input_id=days_id
          errors=errors
          follow_label=Signal::derive(move || follow_label(Limit::Days))
          choice_label=Signal::derive({
            let name = channel.name.clone();
            move || i18n.t_with("admin-retention-choice-days", &[("channel", &name)])
          })
          max=Signal::derive(move || limits.get().max_days)
          is_disabled=Signal::derive(move || !can_edit.get())
        />
      </td>
      <td class="retention-cell" data-label=move || i18n.t("retention-messages")>
        <LimitInput
          draft=draft
          set_draft=set_draft
          limit=Limit::Messages
          input_id=messages_id
          errors=errors
          follow_label=Signal::derive(move || follow_label(Limit::Messages))
          choice_label=Signal::derive({
            let name = channel.name.clone();
            move || i18n.t_with("admin-retention-choice-messages", &[("channel", &name)])
          })
          max=Signal::derive(move || limits.get().max_messages)
          is_disabled=Signal::derive(move || !can_edit.get())
        />
      </td>
      <td>
        <Show when=move || can_edit.get() && is_own() fallback=|| ()>
          <button
            type="button"
            class="btn btn-small"
            on:click=move |_| page.stage(id, OwnDraft::with(stored, RetentionOverride::default()))
          >
            {move || i18n.t("admin-retention-reset")}
          </button>
        </Show>
      </td>
    </tr>
  }
}

/// One limit as a choice and a number: follow the space or instance default, or keep the last N,
/// with its unit next to the number. The number stays visible (and disabled) while the choice is
/// "follow", so the layout doesn't move.
#[component]
fn LimitInput(
  draft: Signal<OwnDraft>,
  set_draft: Callback<OwnDraft>,
  limit: Limit,
  input_id: String,
  errors: FormErrors,
  /// What the "follow" choice says, e.g. "Default (365 days)".
  #[prop(into)]
  follow_label: Signal<String>,
  #[prop(into)] choice_label: Signal<String>,
  #[prop(into)] max: Signal<u32>,
  #[prop(into, optional)] is_disabled: Signal<bool>,
) -> impl IntoView {
  let i18n = use_i18n();
  let choice_id = format!("{input_id}-choice");
  let field = move || limit.field(&draft.get());
  let unit = move || {
    let count = field().text.trim().parse::<usize>().unwrap_or(0);
    i18n.t_count(limit.unit_key(), count)
  };
  let update = move |change: &dyn Fn(&mut LimitDraft)| {
    let mut changed = draft.get_untracked();
    change(limit.field_mut(&mut changed));
    set_draft.run(changed);
  };

  view! {
    <div class="retention-limit">
      <select
        id=choice_id
        class="input"
        aria-label=move || choice_label.get()
        disabled=move || is_disabled.get()
        on:change=move |ev| {
          let is_own = event_target_value(&ev) == OWN;
          update(&|field| field.is_own = is_own);
        }
      >
        <option value=FOLLOW selected=move || !field().is_own>{move || follow_label.get()}</option>
        <option value=OWN selected=move || field().is_own>{move || i18n.t("admin-retention-limited")}</option>
      </select>
      <span class="retention-number">
      <input
        id=input_id.clone()
        class="input narrow"
        inputmode="numeric"
        autocomplete="off"
        min="1"
        max=move || max.get().to_string()
        aria-label=move || choice_label.get()
        disabled=move || is_disabled.get() || !field().is_own
        aria-invalid=errors.invalid(input_id.clone())
        aria-describedby=errors.described(input_id.clone())
        prop:value=move || field().text
        on:input=move |ev| {
          let text = event_target_value(&ev);
          update(&|field| field.text = text.clone());
        }
      />
      <span class="muted-text small">{unit}</span>
      </span>
      <FieldError errors=errors field=input_id />
    </div>
  }
}

/// The page's collected changes and selection; `Copy`, shared by its parts.
#[derive(Clone, Copy)]
struct RetentionPage {
  /// Changed rows by channel, kept while the search or paging hides them.
  drafts: RwSignal<HashMap<ConversationId, OwnDraft>>,
  selected: RwSignal<HashSet<ConversationId>>,
}

impl RetentionPage {
  fn new() -> Self {
    Self {
      drafts: RwSignal::new(HashMap::new()),
      selected: RwSignal::new(HashSet::new()),
    }
  }

  fn draft_of(&self, channel: ConversationId) -> Option<OwnDraft> {
    self.drafts.with(|drafts| drafts.get(&channel).cloned())
  }

  fn stage(&self, channel: ConversationId, draft: OwnDraft) {
    self.drafts.update(|drafts| {
      drafts.insert(channel, draft);
    });
  }

  /// The same own setting for every channel in `targets`.
  fn stage_all(&self, targets: &[(ConversationId, RetentionOverride)], own: RetentionOverride) {
    self.drafts.update(|drafts| {
      for (channel, stored) in targets {
        drafts.insert(*channel, OwnDraft::with(*stored, own));
      }
    });
  }

  fn is_changed(&self, channel: ConversationId, limits: Limits) -> bool {
    self
      .drafts
      .with(|drafts| drafts.get(&channel).is_some_and(|draft| draft.is_changed(limits)))
  }

  fn has_changes(&self, limits: Limits) -> bool {
    self.change_count(limits) > 0
  }

  fn change_count(&self, limits: Limits) -> usize {
    self
      .drafts
      .with(|drafts| drafts.values().filter(|draft| draft.is_changed(limits)).count())
  }

  /// Every changed row's own setting, or the errors of every wrong field in table order.
  fn check(
    &self,
    i18n: I18n,
    limits: Limits,
  ) -> Result<Vec<(ConversationId, RetentionOverride)>, FieldErrors> {
    let mut sorted: Vec<(ConversationId, OwnDraft)> = self
      .drafts
      .with(|drafts| drafts.iter().map(|(id, draft)| (*id, draft.clone())).collect());
    sorted.sort_by_key(|(id, _)| *id);
    let mut changes = Vec::new();
    let mut found = FieldErrors::new();
    for (id, draft) in sorted {
      match draft.check(i18n, limits, &days_field_id(id), &messages_field_id(id)) {
        Ok(own) if own != draft.stored => changes.push((id, own)),
        Ok(_) => {},
        Err(errors) => found.extend(errors),
      }
    }
    if found.is_empty() { Ok(changes) } else { Err(found) }
  }

  /// Drops the changes the list shows as stored now.
  fn drop_arrived(&self, stored: &HashMap<ConversationId, RetentionOverride>, limits: Limits) {
    let is_arrived = |id: &ConversationId, draft: &OwnDraft| {
      stored
        .get(id)
        .is_some_and(|own| *own != draft.stored && draft.parsed(limits) == Some(*own))
    };
    let has_arrived = self
      .drafts
      .with_untracked(|drafts| drafts.iter().any(|(id, draft)| is_arrived(id, draft)));
    if has_arrived {
      self
        .drafts
        .update(|drafts| drafts.retain(|id, draft| !is_arrived(id, draft)));
    }
  }

  fn select(&self, channel: ConversationId, is_selected: bool) {
    self.selected.update(|selected| {
      if is_selected {
        selected.insert(channel);
      } else {
        selected.remove(&channel);
      }
    });
  }

  fn select_all(&self, channels: &[AdminChannel], is_selected: bool) {
    self.selected.update(|selected| {
      for channel in channels {
        if is_selected {
          selected.insert(channel.id);
        } else {
          selected.remove(&channel.id);
        }
      }
    });
  }

  fn are_all_selected(&self, channels: &[AdminChannel]) -> bool {
    !channels.is_empty()
      && self
        .selected
        .with(|selected| channels.iter().all(|channel| selected.contains(&channel.id)))
  }

  fn is_some_selected(&self, channels: &[AdminChannel]) -> bool {
    !self.are_all_selected(channels)
      && self
        .selected
        .with(|selected| channels.iter().any(|channel| selected.contains(&channel.id)))
  }

  /// The selected channels the list holds, with what is stored for each.
  fn selected_channels(&self, channels: &[AdminChannel]) -> Vec<(ConversationId, RetentionOverride)> {
    self.selected.with_untracked(|selected| {
      channels
        .iter()
        .filter(|channel| selected.contains(&channel.id))
        .map(|channel| {
          let stored = self
            .drafts
            .with_untracked(|drafts| drafts.get(&channel.id).map(|draft| draft.stored));
          (channel.id, stored.unwrap_or(channel.own_retention))
        })
        .collect()
    })
  }
}

/// One channel's fields: its own setting as entered, next to what is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnDraft {
  stored: RetentionOverride,
  days: LimitDraft,
  messages: LimitDraft,
}

impl OwnDraft {
  fn of(stored: RetentionOverride) -> Self {
    Self::with(stored, stored)
  }

  fn with(stored: RetentionOverride, own: RetentionOverride) -> Self {
    Self {
      stored,
      days: LimitDraft::of(own.maybe_days),
      messages: LimitDraft::of(own.maybe_max_messages),
    }
  }

  fn parsed(&self, limits: Limits) -> Option<RetentionOverride> {
    Some(RetentionOverride {
      maybe_days: self.days.parse(limits.max_days).ok()?,
      maybe_max_messages: self.messages.parse(limits.max_messages).ok()?,
    })
  }

  /// A wrong field is a change too: it must not be dropped silently.
  fn is_changed(&self, limits: Limits) -> bool {
    self.parsed(limits) != Some(self.stored)
  }

  fn check(
    &self,
    i18n: I18n,
    limits: Limits,
    days_id: &str,
    messages_id: &str,
  ) -> Result<RetentionOverride, FieldErrors> {
    let days = self.days.parse(limits.max_days);
    let messages = self.messages.parse(limits.max_messages);
    let mut found = FieldErrors::new();
    if days.is_err() {
      found.push(
        days_id,
        i18n.t_with(
          "admin-retention-error-days",
          &[("max", &limits.max_days.to_string())],
        ),
      );
    }
    if messages.is_err() {
      found.push(
        messages_id,
        i18n.t_with(
          "admin-retention-error-messages",
          &[("max", &limits.max_messages.to_string())],
        ),
      );
    }
    match (days, messages) {
      (Ok(maybe_days), Ok(maybe_max_messages)) => Ok(RetentionOverride {
        maybe_days,
        maybe_max_messages,
      }),
      _ => Err(found),
    }
  }
}

/// What one limit's fields hold.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LimitDraft {
  /// "Keep the last N" (otherwise: follow the space or the instance default).
  is_own: bool,
  text: String,
}

impl LimitDraft {
  fn of(maybe_value: Option<u32>) -> Self {
    Self {
      is_own: maybe_value.is_some(),
      text: maybe_value.map(|value| value.to_string()).unwrap_or_default(),
    }
  }

  fn parse(&self, max: u32) -> Result<Option<u32>, &'static str> {
    parse_limit(self.is_own, &self.text, max)
  }
}

/// Which of the two limits a field is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Limit {
  Days,
  Messages,
}

impl Limit {
  fn field(self, draft: &OwnDraft) -> LimitDraft {
    match self {
      Limit::Days => draft.days.clone(),
      Limit::Messages => draft.messages.clone(),
    }
  }

  fn field_mut(self, draft: &mut OwnDraft) -> &mut LimitDraft {
    match self {
      Limit::Days => &mut draft.days,
      Limit::Messages => &mut draft.messages,
    }
  }

  fn of(self, setting: RetentionOverride) -> Option<u32> {
    match self {
      Limit::Days => setting.maybe_days,
      Limit::Messages => setting.maybe_max_messages,
    }
  }

  fn unit_key(self) -> &'static str {
    match self {
      Limit::Days => "admin-retention-unit-days",
      Limit::Messages => "admin-retention-unit-messages",
    }
  }
}

/// How far the limits may go: the signed policy's limit where it sets one, else the console's own
/// upper bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Limits {
  max_days: u32,
  max_messages: u32,
}

impl Limits {
  fn of_policy(policy: RetentionPolicy) -> Self {
    let limit = policy.limit;
    Self {
      max_days: limit.maybe_max_age_days.unwrap_or(MAX_DAYS),
      max_messages: limit
        .maybe_max_messages
        .and_then(|count| u32::try_from(count).ok())
        .unwrap_or(MAX_MESSAGES),
    }
  }
}

fn days_field_id(channel: ConversationId) -> String {
  format!("retention-days-{channel}")
}

fn messages_field_id(channel: ConversationId) -> String {
  format!("retention-messages-{channel}")
}

/// One limit in words: "365 days", "the last 500 messages", or "no limit".
fn limit_text(i18n: I18n, limit: Limit, maybe_value: Option<u32>) -> String {
  match (limit, maybe_value) {
    (_, None) => i18n.t("admin-retention-no-limit-short"),
    (Limit::Days, Some(days)) => i18n.t_count("admin-retention-days", days as usize),
    (Limit::Messages, Some(count)) => i18n.t_count("admin-retention-messages", count as usize),
  }
}

/// A setting in words: both limits, or "Keep everything".
fn summary(i18n: I18n, setting: RetentionOverride) -> String {
  let mut parts = Vec::new();
  if let Some(days) = setting.maybe_days {
    parts.push(i18n.t_count("admin-retention-days", days as usize));
  }
  if let Some(messages) = setting.maybe_max_messages {
    parts.push(i18n.t_count("admin-retention-messages", messages as usize));
  }
  if parts.is_empty() {
    i18n.t("admin-retention-forever")
  } else {
    parts.join(" / ")
  }
}

/// One limit as chosen: following the level above is its own choice, not an empty field; "keep
/// the last N" needs a whole number from 1 to `max`.
fn parse_limit(is_own: bool, text: &str, max: u32) -> Result<Option<u32>, &'static str> {
  if !is_own {
    return Ok(None);
  }
  match text.trim().parse::<u32>() {
    Ok(value) if (1..=max).contains(&value) => Ok(Some(value)),
    _ => Err("retention-invalid"),
  }
}

#[cfg(test)]
mod tests {
  use teamcraft_proto::RetentionSetting;

  use super::*;

  const LIMITS: Limits = Limits {
    max_days: 10,
    max_messages: 100,
  };

  #[test]
  fn following_is_a_choice_and_an_empty_number_is_an_error() {
    assert_eq!(parse_limit(false, "", 10), Ok(None));
    assert_eq!(parse_limit(false, "99", 10), Ok(None));
    assert_eq!(parse_limit(true, " 7 ", 10), Ok(Some(7)));
    assert_eq!(parse_limit(true, "", 10), Err("retention-invalid"));
    assert_eq!(parse_limit(true, "0", 10), Err("retention-invalid"));
    assert_eq!(parse_limit(true, "11", 10), Err("retention-invalid"));
    assert_eq!(parse_limit(true, "1.5", 10), Err("retention-invalid"));
  }

  #[test]
  fn the_policy_limit_caps_the_choices() {
    let open = Limits::of_policy(policy(RetentionSetting::default()));
    assert_eq!(
      open,
      Limits {
        max_days: MAX_DAYS,
        max_messages: MAX_MESSAGES
      }
    );
    let limited = Limits::of_policy(policy(RetentionSetting {
      maybe_max_age_days: Some(90),
      maybe_max_messages: None,
    }));
    assert_eq!(limited.max_days, 90);
    assert_eq!(limited.max_messages, MAX_MESSAGES);
  }

  mod drafts {
    use super::*;

    #[test]
    fn a_row_as_stored_is_no_change_and_a_reset_is_one() {
      let stored = own(Some(5), None);
      assert!(!OwnDraft::of(stored).is_changed(LIMITS));
      let reset = OwnDraft::with(stored, RetentionOverride::default());
      assert!(reset.is_changed(LIMITS));
      assert_eq!(reset.parsed(LIMITS), Some(RetentionOverride::default()));
    }

    #[test]
    fn a_wrong_field_counts_as_a_change_so_it_is_never_dropped_silently() {
      let mut draft = OwnDraft::of(RetentionOverride::default());
      draft.messages = LimitDraft {
        is_own: true,
        text: "lots".into(),
      };
      assert!(draft.is_changed(LIMITS));
      assert_eq!(draft.parsed(LIMITS), None);
    }

    #[test]
    fn a_bulk_change_keeps_each_channels_stored_setting_to_compare_with() {
      let first = own(Some(5), None);
      let second = RetentionOverride::default();
      let wanted = own(None, Some(50));
      let drafts: Vec<OwnDraft> = [first, second]
        .into_iter()
        .map(|stored| OwnDraft::with(stored, wanted))
        .collect();
      assert_eq!(drafts[0].stored, first);
      assert_eq!(drafts[1].stored, second);
      assert!(drafts.iter().all(|draft| draft.parsed(LIMITS) == Some(wanted)));
    }
  }

  fn own(days: Option<u32>, messages: Option<u32>) -> RetentionOverride {
    RetentionOverride {
      maybe_days: days,
      maybe_max_messages: messages,
    }
  }

  fn policy(limit: RetentionSetting) -> RetentionPolicy {
    RetentionPolicy {
      default: RetentionSetting::default(),
      limit,
      keep_pinned: true,
    }
  }
}
