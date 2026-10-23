use leptos::html;
use leptos::prelude::*;

use crate::clock::format_date_time;
use crate::components::icons::{Icon, IconName};
use crate::i18n::use_i18n;
use crate::model::ConversationKind;
use crate::search::{SearchHit, TokenKind, parse_query};
use crate::state::use_app_state;

/// Filters offered as buttons; each inserts its token and leaves the cursor in the search box.
const FILTER_BUTTONS: [(&str, &str); 8] = [
  ("search-filter-from", "from:"),
  ("search-filter-in", "in:"),
  ("search-filter-space", "space:"),
  ("search-filter-file", "has:file"),
  ("search-filter-link", "has:link"),
  ("search-filter-before", "before:"),
  ("search-filter-after", "after:"),
  ("search-filter-thread", "in-thread"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pane {
  Results,
  Mentions,
}

/// Search page: query with filters, result list with highlighted snippets, saved searches and recent
/// mentions. Everything runs through [`crate::backend::SearchBackend`], so it works offline once the
/// index is local.
#[component]
pub fn SearchView() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let input = NodeRef::<html::Input>::new();
  let pane = RwSignal::new(Pane::Results);
  let query = state.search_query;
  let parsed = Memo::new(move |_| parse_query(&query.get()));
  let results = Memo::new(move |_| state.read(|backend| backend.search(&parsed.get())));
  let mentions = Memo::new(move |_| state.read(|backend| backend.recent_mentions()));
  let saved = Memo::new(move |_| state.read(|backend| backend.saved_searches()));
  let spaces = Memo::new(move |_| state.read(|backend| backend.spaces()));
  let scope = move || parsed.get().space_scope().unwrap_or_default().to_owned();
  let has_invalid = move || {
    parsed
      .get()
      .tokens
      .iter()
      .any(|token| token.kind == TokenKind::Invalid)
  };

  Effect::new(move |_| {
    if let Some(element) = input.get() {
      drop(element.focus());
    }
  });

  let insert = move |token: &'static str| {
    pane.set(Pane::Results);
    query.update(|text| {
      if !text.is_empty() && !text.ends_with(' ') {
        text.push(' ');
      }
      text.push_str(token);
      // Filters that take a value leave the cursor right after the colon; others are complete.
      if !token.ends_with(':') {
        text.push(' ');
      }
    });
    if let Some(element) = input.get_untracked() {
      drop(element.focus());
    }
  };

  let status = move || match pane.get() {
    Pane::Mentions => i18n.t_count("search-count", mentions.get().len()),
    Pane::Results if parsed.get().is_empty() => i18n.t("search-start"),
    Pane::Results => {
      let total = results.get().total;
      if total == 0 {
        i18n.t("search-empty")
      } else {
        i18n.t_count("search-count", total)
      }
    },
  };
  let shown_hits = move || match pane.get() {
    Pane::Mentions => mentions.get(),
    Pane::Results => results.get().hits,
  };

  view! {
    <section class="search-page" aria-label=move || i18n.t("search-title")>
      <aside class="search-side">
        <h2>{move || i18n.t("search-saved-title")}</h2>
        <ul class="saved-list">
          <For each=move || saved.get() key=|item| item.id let:item>
            {
              let id = item.id;
              let run = item.query.clone();
              let delete_label = item.name.clone();
              view! {
                <li>
                  <button
                    class="saved-run"
                    on:click=move |_| {
                      pane.set(Pane::Results);
                      query.set(run.clone());
                    }
                  >
                    <strong translate="no">{item.name.clone()}</strong>
                    <span class="muted-text small mono" translate="no">{item.query.clone()}</span>
                  </button>
                  <button
                    class="icon-btn icon-btn-small"
                    aria-label=move || i18n.t_with("search-saved-delete", &[("name", &delete_label)])
                    on:click=move |_| state.write(|backend| backend.delete_saved_search(id))
                  >
                    <Icon name=IconName::Trash />
                  </button>
                </li>
              }
            }
          </For>
          <Show when=move || saved.get().is_empty() fallback=|| ()>
            <li class="muted-text small">{move || i18n.t("search-saved-empty")}</li>
          </Show>
        </ul>
        <h2>{move || i18n.t("search-mentions-title")}</h2>
        <button
          class="saved-run"
          aria-pressed=move || (pane.get() == Pane::Mentions).to_string()
          on:click=move |_| pane.set(Pane::Mentions)
        >
          <strong>{move || i18n.t("search-mentions-open")}</strong>
          <span class="muted-text small">{move || i18n.t_count("search-count", mentions.get().len())}</span>
        </button>
      </aside>
      <div class="search-main">
        <form
          class="search-form"
          role="search"
          on:submit=move |ev| {
            ev.prevent_default();
            pane.set(Pane::Results);
          }
        >
          <div class="search-box">
            <Icon name=IconName::Search />
            <label class="visually-hidden" for="search-input">{move || i18n.t("search-input-label")}</label>
            <input
              id="search-input"
              type="search"
              name="q"
              autocomplete="off"
              spellcheck="false"
              placeholder=move || i18n.t("search-input-placeholder")
              node_ref=input
              prop:value=move || query.get()
              on:input=move |ev| {
                pane.set(Pane::Results);
                query.set(event_target_value(&ev));
              }
            />
            <button
              type="button"
              class="btn btn-small"
              disabled=move || parsed.get().is_empty()
              on:click=move |_| {
                let text = query.get_untracked();
                state.write(|backend| backend.save_search(&text, &text));
              }
            >
              {move || i18n.t("search-save")}
            </button>
          </div>
          <div class="search-scope">
            <label for="search-scope">{move || i18n.t("search-scope-label")}</label>
            <select
              id="search-scope"
              class="input"
              prop:value=scope
              on:change=move |ev| {
                pane.set(Pane::Results);
                let chosen = event_target_value(&ev);
                let maybe_space = (!chosen.is_empty()).then_some(chosen);
                query.set(parsed.get_untracked().with_space_scope(maybe_space.as_deref()));
              }
            >
              <option value="">{move || i18n.t("search-scope-all")}</option>
              <For each=move || spaces.get() key=|space| space.id let:space>
                <option value=space.name.clone() selected=move || scope() == space.name translate="no">
                  {space.name.clone()}
                </option>
              </For>
            </select>
          </div>
          <div class="filter-buttons" role="group" aria-label=move || i18n.t("search-filters-label")>
            {FILTER_BUTTONS
              .into_iter()
              .map(|(label, token)| {
                view! {
                  <button type="button" class="chip-btn" on:click=move |_| insert(token)>
                    <span>{move || i18n.t(label)}</span>
                    <code class="mono">{token}</code>
                  </button>
                }
              })
              .collect_view()}
          </div>
          <ul class="token-chips" aria-label=move || i18n.t("search-active-filters")>
            {move || {
              parsed
                .get()
                .tokens
                .into_iter()
                .enumerate()
                .filter(|(_, token)| !matches!(token.kind, TokenKind::Term(_)))
                .map(|(index, token)| {
                  let is_invalid = token.kind == TokenKind::Invalid;
                  let raw = token.raw.clone();
                  let label_raw = token.raw.clone();
                  view! {
                    <li class="token-chip" class:invalid=is_invalid>
                      <span class="mono" translate="no">{raw}</span>
                      {is_invalid.then(|| view! { <span class="small">{move || i18n.t("search-invalid")}</span> })}
                      <button
                        type="button"
                        class="icon-btn icon-btn-small"
                        aria-label=move || i18n.t_with("search-remove-filter", &[("filter", &label_raw)])
                        on:click=move |_| query.set(parsed.get_untracked().without_token(index))
                      >
                        <Icon name=IconName::Close />
                      </button>
                    </li>
                  }
                })
                .collect_view()
            }}
          </ul>
          <p class="search-syntax muted-text small">{move || i18n.t("search-syntax")}</p>
        </form>
        <p class="search-status" role="status" aria-live="polite">
          {status}
          <Show when=has_invalid fallback=|| ()>
            <span class="warn-text">{move || i18n.t("search-invalid-hint")}</span>
          </Show>
        </p>
        <ul class="hit-list">
          <For each=shown_hits key=|hit| hit.message.id let:hit>
            <HitCard hit=hit />
          </For>
        </ul>
      </div>
    </section>
  }
}

#[component]
fn HitCard(hit: SearchHit) -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let author = state
    .read(|backend| backend.user(hit.message.author))
    .map(|user| user.display_name)
    .unwrap_or_default();
  let when = move || format_date_time(hit.message.sent_at, i18n.locale().tag());
  let conversation = hit.message.conversation;
  let maybe_thread = hit.message.maybe_thread;
  let is_reply = maybe_thread.is_some();
  let message_id = hit.message.id;
  // Channels read "# name"; direct and group messages are just the names.
  let prefix = match state
    .read(|backend| backend.conversation(conversation))
    .map(|conv| conv.kind)
  {
    Some(ConversationKind::Direct | ConversationKind::Group) => "",
    _ => "# ",
  };

  view! {
    <li class="hit">
      <div class="hit-meta">
        <strong translate="no">{prefix}{hit.conversation_name}</strong>
        {hit.maybe_space_name.map(|space| view! { <span class="muted-text" translate="no">{space}</span> })}
        <span translate="no">{author}</span>
        <time class="muted-text">{when}</time>
        {is_reply.then(|| view! { <span class="badge">{move || i18n.t("search-reply-badge")}</span> })}
      </div>
      <p class="hit-snippet">
        {hit
          .snippet
          .into_iter()
          .map(|part| if part.is_match { view! { <mark>{part.text}</mark> }.into_any() } else { part.text.into_any() })
          .collect_view()}
      </p>
      <div class="hit-actions">
        <button class="btn btn-small" on:click=move |_| state.open_message(conversation, message_id, None)>
          {move || i18n.t("search-open-conversation")}
        </button>
        {maybe_thread
          .map(|thread| {
            view! {
              <button
                class="btn btn-small"
                on:click=move |_| state.open_message(conversation, message_id, Some(thread))
              >
                {move || i18n.t("search-open-thread")}
              </button>
            }
          })}
      </div>
    </li>
  }
}
