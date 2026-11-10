//! Several accounts in the desktop app (#214): the other accounts in the account menu with their
//! unread badges, a badge on the account button, the dialog that adds and removes accounts, and on
//! the server entry and sign-in screens the way back to the accounts already on this computer.
//!
//! The shell does the work (`apps/desktop/src/auth/accounts.rs`). After a switch, an add, or the
//! removal of the active account the page loads afresh, so nothing of one account stays in the
//! page's memory for the next one. The browser has one account per tab and shows none of this.

use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::auth::tauri::{self, AccountView, AccountsView, BackgroundState, ServerProblem};
use crate::components::icons::{Icon, IconName};
use crate::components::modal::Modal;
use crate::i18n::{I18n, use_i18n};
use crate::reactive::SetIfAlive;
use crate::state::{Overlay, use_app_state};

/// The largest number a badge shows.
const MAX_BADGE: u32 = 99;

/// The accounts the shell keeps: read when shown, and again on every change it reports.
#[derive(Clone, Copy)]
struct Accounts {
  view: RwSignal<AccountsView>,
}

/// Keeps the accounts for everything below: one read, one subscription to the shell's changes.
pub fn provide_accounts() {
  if tauri::is_available() {
    provide_context(Accounts::load());
  }
}

/// The accounts provided above, or a list of its own where none is.
fn use_accounts() -> Accounts {
  use_context::<Accounts>().unwrap_or_else(Accounts::load)
}

impl Accounts {
  fn load() -> Self {
    let view = RwSignal::new(AccountsView::default());
    if tauri::is_available() {
      let refresh = move || {
        spawn_local(async move {
          match tauri::accounts_list().await {
            Ok(list) => view.set_if_alive(list),
            Err(problem) => tracing::debug!(detail = %problem.detail, "could not list the accounts"),
          }
        });
      };
      refresh();
      tauri::listen(tauri::ACCOUNTS_CHANGED_EVENT, refresh);
    }
    Self { view }
  }

  /// The accounts that are not active, in their order.
  fn others(self) -> Vec<AccountView> {
    self.view.with(|view| {
      view
        .accounts
        .iter()
        .filter(|account| !account.is_active)
        .cloned()
        .collect()
    })
  }

  fn can_add(self) -> bool {
    self.view.with(|view| view.can_add)
  }
}

/// The other accounts in the account menu, "Add account" and "Manage accounts" (desktop only).
#[component]
pub fn AccountsMenuSection() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let accounts = use_accounts();
  let problem = RwSignal::new(None::<ServerProblem>);
  Effect::new(move |_| {
    if let Some(problem) = problem.get() {
      state.show_error(i18n.t(&problem.key), problem.detail);
    }
  });
  let switch_to = move |origin: String| {
    state.close_overlay();
    switch_account(origin, problem);
  };
  let add = move |_| {
    state.close_overlay();
    add_account(problem);
  };
  view! {
    <div class="menu-sub" role="group" aria-label=move || i18n.t("accounts-title")>
      <For
        each=move || accounts.others()
        key=|account| (account.origin.clone(), account.background)
        children=move |account| {
          let origin = account.origin.clone();
          let name = account.display_name.clone();
          view! {
            <button
              class="menu-item account-item"
              role="menuitem"
              tabindex="-1"
              title=move || i18n.t_with("accounts-switch-to", &[("name", &name)])
              on:click=move |_| switch_to(origin.clone())
            >
              <Icon name=IconName::Users />
              <span class="menu-label account-label">
                <span translate="no">{account.display_name.clone()}</span>
                <small class="muted-text" translate="no">{account.host.clone()}</small>
              </span>
              {account_badge(account.background, i18n)}
            </button>
          }
        }
      />
      <Show when=move || accounts.can_add() fallback=|| ()>
        <button class="menu-item" role="menuitem" tabindex="-1" on:click=add>
          <Icon name=IconName::UserPlus />
          <span class="menu-label">{move || i18n.t("accounts-add")}</span>
        </button>
      </Show>
      <button class="menu-item" role="menuitem" tabindex="-1" on:click=move |_| state.overlay.set(Overlay::Accounts)>
        <Icon name=IconName::Users />
        <span class="menu-label">{move || i18n.t("accounts-manage")}</span>
      </button>
    </div>
  }
}

/// On the account button: whether another account has something unread (desktop only). Mentions
/// count; anything else is a dot.
#[component]
pub fn OtherAccountsBadge() -> impl IntoView {
  let i18n = use_i18n();
  let accounts = use_accounts();
  let totals = Memo::new(move |_| {
    accounts
      .others()
      .iter()
      .fold((0_u32, 0_u32), |(mentions, unread), account| {
        match account.background {
          BackgroundState::Unread {
            mentions: more_mentions,
            unread: more_unread,
          } => (
            mentions.saturating_add(more_mentions),
            unread.saturating_add(more_unread),
          ),
          _ => (mentions, unread),
        }
      })
  });
  move || {
    let (mentions, unread) = totals.get();
    if mentions > 0 {
      let label = i18n.t_count("accounts-others-mentions", count(mentions));
      Some(
        view! {
          <span class="account-others-badge badge-count" role="img" aria-label=label>
            {badge_text(mentions)}
          </span>
        }
        .into_any(),
      )
    } else if unread > 0 {
      Some(
        view! { <span class="account-others-badge unread-dot" role="img" aria-label=move || i18n.t("accounts-others-unread")></span> }
          .into_any(),
      )
    } else {
      None
    }
  }
}

/// Every account on this computer: switch, add, remove (desktop only).
#[component]
pub fn AccountsDialog() -> impl IntoView {
  let state = use_app_state();
  let i18n = use_i18n();
  let accounts = use_accounts();
  let problem = RwSignal::new(None::<ServerProblem>);
  // The account whose removal waits for the user's confirmation, by origin.
  let confirming = RwSignal::new(None::<String>);
  let is_busy = RwSignal::new(false);
  let remove = move |origin: String| {
    is_busy.set(true);
    spawn_local(async move {
      match tauri::account_remove(&origin).await {
        Ok(removed) => {
          for user in removed.user_ids {
            crate::settings::forget_account_on_device(user);
          }
          if removed.was_active {
            crate::bundle::reload();
          } else {
            confirming.set_if_alive(None);
            is_busy.set_if_alive(false);
          }
        },
        Err(failure) => {
          problem.set_if_alive(Some(failure));
          is_busy.set_if_alive(false);
        },
      }
    });
  };
  view! {
    <Modal labelled_by="accounts-title" class="modal-notice accounts-dialog">
      <header class="modal-head">
        <h2 id="accounts-title">{move || i18n.t("accounts-title")}</h2>
        <button class="icon-btn" aria-label=move || i18n.t("close") on:click=move |_| state.close_overlay()>
          <Icon name=IconName::Close />
        </button>
      </header>
      <div class="modal-body">
        <p class="hint">{move || i18n.t("accounts-hint")}</p>
        <ul class="accounts-list">
          <For
            each=move || accounts.view.with(|view| view.accounts.clone())
            key=|account| (account.origin.clone(), account.is_active, account.background)
            children=move |account| {
              let origin = StoredValue::new(account.origin.clone());
              let name = account.display_name.clone();
              let is_confirming = move || confirming.with(|maybe| maybe.as_deref() == Some(origin.get_value().as_str()));
              view! {
                <li class="accounts-row">
                  <div class="accounts-name">
                    <span translate="no">{account.display_name.clone()}</span>
                    <small class="muted-text" translate="no">{account.host.clone()}</small>
                  </div>
                  {if account.is_active {
                    view! { <span class="badge">{move || i18n.t("accounts-active")}</span> }.into_any()
                  } else {
                    account_badge(account.background, i18n).into_any()
                  }}
                  <div class="accounts-actions">
                    <Show
                      when=is_confirming
                      fallback=move || {
                        view! {
                          {(!account.is_active).then(|| view! {
                            <button class="btn btn-small" on:click=move |_| switch_account(origin.get_value(), problem)>
                              {move || i18n.t("accounts-switch")}
                            </button>
                          })}
                          <button
                            class="btn btn-small"
                            prop:disabled=move || is_busy.get()
                            on:click=move |_| confirming.set(Some(origin.get_value()))
                          >
                            {move || i18n.t("accounts-remove")}
                          </button>
                        }
                      }
                    >
                      <span class="accounts-confirm" role="alert">
                        {i18n.t_with("accounts-remove-confirm", &[("name", &name)])}
                      </span>
                      <button class="btn btn-small" prop:disabled=move || is_busy.get() on:click=move |_| confirming.set(None)>
                        {move || i18n.t("cancel")}
                      </button>
                      <button
                        class="btn btn-small btn-danger"
                        prop:disabled=move || is_busy.get()
                        aria-busy=move || is_busy.get().to_string()
                        on:click=move |_| remove(origin.get_value())
                      >
                        {move || i18n.t("accounts-remove")}
                      </button>
                    </Show>
                  </div>
                </li>
              }
            }
          />
        </ul>
        {move || problem.get().map(|problem| view! {
          <p class="field-error" role="alert">{i18n.t(&problem.key)}</p>
        })}
      </div>
      <footer class="modal-foot">
        <button class="btn" on:click=move |_| state.close_overlay()>
          {move || i18n.t("close")}
        </button>
        <Show when=move || accounts.can_add() fallback=|| ()>
          <button class="btn btn-primary" on:click=move |_| add_account(problem)>
            <Icon name=IconName::UserPlus />
            <span>{move || i18n.t("accounts-add")}</span>
          </button>
        </Show>
      </footer>
    </Modal>
  }
}

/// On the server entry and the sign-in screen: the other accounts on this computer, to go back to
/// one instead of signing in here (desktop only).
#[component]
pub fn AuthAccountsLine() -> impl IntoView {
  let i18n = use_i18n();
  let accounts = use_accounts();
  let problem = RwSignal::new(None::<ServerProblem>);
  move || {
    let others = accounts.others();
    (!others.is_empty()).then(|| {
      view! {
        <div class="auth-accounts" role="group" aria-label=move || i18n.t("accounts-other-title")>
          <p class="muted-text small">{move || i18n.t("accounts-other-title")}</p>
          {others
            .into_iter()
            .map(|account| {
              let origin = account.origin.clone();
              view! {
                <button class="btn auth-account" on:click=move |_| switch_account(origin.clone(), problem)>
                  <span translate="no">{account.display_name.clone()}</span>
                  <small class="muted-text" translate="no">{account.host.clone()}</small>
                  {account_badge(account.background, i18n)}
                </button>
              }
            })
            .collect_view()}
          {move || problem.get().map(|problem| view! {
            <p class="field-error" role="alert">{i18n.t(&problem.key)}</p>
          })}
        </div>
      }
    })
  }
}

/// The badge of an account that is not active: its mentions, a dot for other unread messages, or
/// that it is signed out.
fn account_badge(state: BackgroundState, i18n: I18n) -> impl IntoView {
  match state {
    BackgroundState::Unread { mentions, .. } if mentions > 0 => Some(
      view! {
        <span class="badge-count" role="img" aria-label=i18n.t_count("accounts-mentions", count(mentions))>
          {badge_text(mentions)}
        </span>
      }
      .into_any(),
    ),
    BackgroundState::Unread { unread, .. } if unread > 0 => Some(
      view! { <span class="unread-dot" role="img" aria-label=i18n.t("accounts-unread")></span> }.into_any(),
    ),
    BackgroundState::SignedOut => {
      Some(view! { <small class="muted-text">{i18n.t("accounts-signed-out")}</small> }.into_any())
    },
    BackgroundState::Unread { .. } | BackgroundState::Unknown => None,
  }
}

/// The number on a badge: up to "99+", so it keeps its size.
fn badge_text(count: u32) -> String {
  if count > MAX_BADGE {
    format!("{MAX_BADGE}+")
  } else {
    count.to_string()
  }
}

fn count(value: u32) -> usize {
  usize::try_from(value).unwrap_or(usize::MAX)
}

/// Makes the account of `origin` the active one; the page then loads afresh for it.
fn switch_account(origin: String, problem: RwSignal<Option<ServerProblem>>) {
  spawn_local(async move {
    match tauri::account_switch(&origin).await {
      Ok(()) => crate::bundle::reload(),
      Err(failure) => problem.set_if_alive(Some(failure)),
    }
  });
}

/// Leaves the active account for the server entry of another one.
fn add_account(problem: RwSignal<Option<ServerProblem>>) {
  spawn_local(async move {
    match tauri::account_add().await {
      Ok(()) => crate::bundle::reload(),
      Err(failure) => problem.set_if_alive(Some(failure)),
    }
  });
}

#[cfg(test)]
mod tests {
  use super::*;

  /// The list as the desktop shell serializes it (`apps/desktop/src/auth/accounts.rs`).
  #[test]
  fn the_shells_list_is_read() {
    let json = serde_json::json!({
      "accounts": [
        { "origin": "https://a.example.com", "host": "a.example.com", "display_name": "A", "is_active": true,
          "background": { "state": "unknown" } },
        { "origin": "https://b.example.com", "host": "b.example.com", "display_name": "B", "is_active": false,
          "background": { "state": "unread", "mentions": 2, "unread": 5 } },
        { "origin": "https://c.example.com", "host": "c.example.com", "display_name": "C", "is_active": false,
          "background": { "state": "signed_out" } },
        { "origin": "https://d.example.com", "host": "d.example.com", "display_name": "D", "is_active": false,
          "background": { "state": "a_state_of_a_newer_shell" } }
      ],
      "can_add": true
    });
    let view: AccountsView = serde_json::from_value(json).unwrap();
    let states: Vec<_> = view.accounts.iter().map(|account| account.background).collect();
    assert_eq!(
      states,
      [
        BackgroundState::Unknown,
        BackgroundState::Unread {
          mentions: 2,
          unread: 5
        },
        BackgroundState::SignedOut,
        BackgroundState::Unknown,
      ]
    );
    assert!(view.can_add);
    assert!(view.accounts[0].is_active);
  }

  #[test]
  fn badges_keep_their_size() {
    assert_eq!(badge_text(7), "7");
    assert_eq!(badge_text(99), "99");
    assert_eq!(badge_text(1000), "99+");
  }
}
